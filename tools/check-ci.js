// 仓库卫生自检：CI 配置 + 变异清单 + 编码。三块都只能靠机器，靠人记等于没记。
//
//   1. `.github/workflows/ci.yml` 还能不能解析（加 job 最容易在这里缩进出错，
//      而肉眼看 YAML 的缩进是不可靠的）。
//   2. `tools/mutate-check.js` / `tools/mutate-rust.js` 的**变异清单有没有过期**。
//      变异串靠 `from` 在源码里做字面匹配；源码一改 `from` 就可能对不上，脚本会报
//      「变异未生效」——但那意味着**这条变异已经形同虚设**，守护它的检查从此再没人
//      验证过。CI 里跑 `git diff --exit-code` 只能保证脚本自己还原了源码，发现不了这个。
//   3. 全仓库**编码卫生**：不许有 U+FFFD 替换符 / 非法 UTF-8 / BOM。
//      本轮扫出 4 处既有损坏，其中一处在 handlers.rs 的**用户可见字符串**里
//      （导出的增值税申报表标题），用户直接看得见。
//
// 清单用 `node tools/mutate-*.js --list` 取真值，不在外部重新解析 JS 字符串
// （两层转义必错，第一版就是这么把一条有效变异误报成失效的）。
//
// 本脚本自己也有 12 个变异探针（`--selftest`）——不然它也只是"看着像在检查"。
const fs = require("fs");
const { execFileSync } = require("child_process");
const path = require("path");
const os = require("os");

let bad = 0;

// 仓库根解析放在最前面：第 2.5 块与第 4 块都要用。
//
// ⚠️ 「文件找不到」必须算**失败**。第一版路径算错，`existsSync` 返回 false 就当无事
//    发生 —— 检查自己失效却报喜。已有自检探针专门覆盖这一条。
function resolveRepoRoot() {
  const cands = [path.join(__dirname, ".."), process.cwd()];
  for (const c of cands) {
    if (fs.existsSync(path.join(c, "crates", "finweb", "tests", "api.rs"))) return c;
  }
  return null;
}
const FIX_ROOT = resolveRepoRoot();

// 跑一个守门脚本的**自检**，用退出码判它过没过。
//
// 判据必须是退出码，不是输出文本。三个理由（每一条都踩过）：
//   · 自检的**预期输出里就有 `FAIL ...`** —— 那是「变异被正确拦下」的证据。
//     早先用 `/FAIL/` 判，把自己的成功输出判成了失败。
//   · 把探针数量写进正则（`/全部 3 个探针/`）会随内容腐烂：我给
//     selftest-customers-balance 加到第 5 个探针，检查就报「自检没通过」，
//     而自检本身 5/5 全过。判据写在散文里，内容一改就失效。
//   · 连全角问号都得原样写：`\？` 是「转义问号」，等于半角 `?`，永不匹配。
//
// 输出仍然打出来（人要看），但不参与判断。
function runGuardSelfTest(label, scriptName) {
  let out = "";
  let code = 0;
  try {
    out = execFileSync(process.execPath, [path.join(__dirname, scriptName)], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });
  } catch (e) {
    // 非 0 退出是「自检判定失败」；脚本崩/缺失也一样 —— 两种都算失败，
    // 但输出照样要打出来，否则只剩一句「未能运行」，无从查起。
    out = String((e && e.stdout) || "") + String((e && e.stderr) || "");
    code = 1;
  }
  process.stdout.write(out);
  if (code !== 0) {
    console.log(label + "的自检没通过（退出码 " + code + "）—— 探针没拦下，或文件没还原");
    return false;
  }
  return true;
}

// 取某个函数的完整函数体（顶格到配对的大括号）。多个检查都用它，所以定义在最前面。
//
// 允许行首空白：`test_opts` 缩进在 `mod tests` 里，而 `test_state` 在顶层。
// 但**不能**写成「任意位置含 fn xxx(」—— 那会匹配到注释里提到函数名的地方，
// 然后从一个空 body 上得出「夹具没写闸门」的错误结论。
function fnBodyOf(src, name) {
  const ls = src.split(/\r?\n/);
  const st = ls.findIndex((l) =>
    new RegExp("^\\s*(?:pub(?:\\(crate\\))?\\s+)?(?:async\\s+)?fn\\s+" + name + "\\s*\\(").test(l)
  );
  if (st < 0) return null;
  let depth = 0;
  let sawOpen = false;
  const out = [];
  for (let i = st; i < ls.length; i++) {
    out.push(ls[i]);
    for (const ch of ls[i]) {
      if (ch === "{") {
        depth++;
        sawOpen = true;
      }
      else if (ch === "}") depth--;
    }
    // 必须**先见过一个 `{`** 才可能在 depth 回到 0 时收尾。
    // 原来只看 depth：多行函数签名（如 `async fn post_login(\n  State(state): ...`）
    // 在第一行还没有 `{`，于是 out.length > 1 一满足就 break，
    // 抽出来只有签名两行 —— 而签名里当然找不到任何方法调用，
    // 于是所有用它的检查都报「一个都没用」。
    if (sawOpen && depth === 0 && out.length > 1) break;
  }
  return out.join("\n");
}
if (!FIX_ROOT) {
  bad++;
  console.log("FAIL 定位不到仓库根，无法检查测试夹具");
}

// ---- 1. CI 配置 ----------------------------------------------------------
const ymlPath = ".github/workflows/ci.yml";
const s = fs.readFileSync(ymlPath, "utf8");
const lines = s.split(/\r?\n/);

lines.forEach((l, i) => {
  if (/^\s*\t/.test(l)) {
    console.log(`FAIL ${ymlPath} 第 ${i + 1} 行有制表符缩进`);
    bad++;
  }
  const q = (l.match(/"/g) || []).length;
  if (q % 2) {
    console.log(`FAIL ${ymlPath} 第 ${i + 1} 行引号不成对：${l.trim().slice(0, 90)}`);
    bad++;
  }
});

const jobs = [];
lines.forEach((l, i) => {
  const m = /^ {2}([a-z][\w-]*):\s*$/.exec(l);
  if (m && i > 17) jobs.push(m[1]);
});
const indentBad = checkBlockScalarIndent(s);
if (indentBad === 0) {
  console.log("块标量结构：块内无「缩进不足且不像 YAML 结构」的行");
}
const yamlVerdict = authoritativeYamlParse(ymlPath);
if (yamlVerdict === null) {
  bad++;
  console.log(
    `FAIL 无法解析 ${ymlPath}（python3 / python / py / js-yaml 都不可用），且启发式检查已跑过` +
      `\n    「无法校验」不等于「校验通过」——我改坏 ci.yml 的那一刻，本检查打印的就是 OK。` +
      `\n    修法（按本机实际情况挑一条，别三条都做）：` +
      `\n      · 没装 Python        → 到 python.org 装，或让 runner 用带 python3 的镜像` +
      `\n      · 有 Python 没 PyYAML → python -m pip install pyyaml` +
      `\n      · 有 Python 有 PyYAML 但不在 PATH → 把它加进 PATH（本机就是这个：` +
      `%LOCALAPPDATA%\\Programs\\Python\\Python312\\python.exe）` +
      `\n        否则「装 PyYAML」装完检查照样跑不通（PATH 上只有 Store 占位符别名）` +
      `\n      · 都不想要           → npm i -D js-yaml（本检查会自动退回它）`
  );
} else {
  if (!/可解析/.test(yamlVerdict)) {
    bad++;
    console.log(`FAIL ${yamlVerdict}`);
  } else {
    console.log(yamlVerdict + "（权威判据）");
  }
}

const dup = jobs.filter((j, k) => jobs.indexOf(j) !== k);
if (dup.length) {
  console.log(`FAIL ${ymlPath} job 名重复：${dup.join(", ")}`);
  bad++;
}

try {
  const yaml = require("js-yaml");
  const doc = yaml.load(s);
  const parsed = Object.keys(doc.jobs);
  if (parsed.length !== jobs.length) {
    console.log(`FAIL 解析出 ${parsed.length} 个 job，静态扫出 ${jobs.length} 个`);
    bad++;
  }
  for (const [name, j] of Object.entries(doc.jobs)) {
    if (!j.steps || !j["runs-on"]) {
      console.log(`FAIL job ${name} 缺 runs-on 或 steps`);
      bad++;
    }
  }
  // 光有文件不够：mutation job 必须真的执行它。删掉那一步后 CI 仍会绿，
  // 变异测试就从门禁里消失了。
  const mutation = doc.jobs && doc.jobs.mutation;
  if (!mutation) {
    console.log("FAIL ci.yml 没有 mutation job —— 变异测试已不在门禁里");
    bad++;
  } else {
    if (!fs.existsSync("tools/mutate-rust.js")) {
      console.log("FAIL 有 mutation job 但 tools/mutate-rust.js 不存在");
      bad++;
    }
    const mutationRuns = (mutation.steps || [])
      .map((s) => String((s && s.run) || ""))
      .join("\n");
    if (!mutationRuns.includes("node tools/mutate-rust.js")) {
      console.log("FAIL mutation job 没有执行 node tools/mutate-rust.js");
      bad++;
    }
  }
  console.log(`YAML 解析通过（${parsed.length} 个 job）：${parsed.join(", ")}`);
} catch (e) {
  if (e.code !== "MODULE_NOT_FOUND") {
    console.log(`FAIL YAML 解析失败：${e.message.split("\n")[0]}`);
    bad++;
  } else {
    console.log("（无 js-yaml，跳过结构解析；静态检查已覆盖制表符/引号/重名）");
  }
}

/**
 * workflow YAML 的**权威**校验：能解析就解析。
 *
 * 优先 python3 / python（本机与 ubuntu-latest runner 都有），其次 js-yaml。
 * 都不可用时跑一个**明确标注为启发式**的兜底检查 —— 但绝不静默跳过。
 *
 * 为什么这么较劲：今天的教训不是「判据写错了」，而是「检查在工具缺失时
 * 假装通过」。我改坏 ci.yml 的那一刻，check-ci.js 打印的是 OK（它正在输出
 * 「无 js-yaml，跳过结构解析」）。**一条会静默降级成空转的检查比没有检查更危险** ——
 * 它让人以为 CI 配置被验证过了。
 *
 * 附带一个独立的结构性检查（不依赖任何解析器）：块标量内缩进不足基准、
 * 且看起来不像 YAML 结构（列表项 / key: / 注释 / 右括号）的行，就是块被提前结束。
 * 覆盖的正是今天这个错：bash 单引号可以跨行 → 长命令被拆成多行 →
 * 后两行缩进比基准浅 → shell 合法、YAML 非法。
 */
function looksLikeYamlStructure(line) {
  const t = line.trim();
  return /^-\s/.test(t) || /^[\w.\-"'\u4e00-\u9fa5]+\s*:/.test(t) || /^#/.test(t) || t === "}" || t === "]" || t === "---";
}

function checkBlockScalarIndent(src) {
  const ls = src.split(/\r?\n/);
  let bad = 0;
  for (let i = 0; i < ls.length; i++) {
    const m = /^(\s*)[\w-]+:\s*[|>][-+]?\s*$/.exec(ls[i]);
    if (!m) continue;
    const base = m[1].length + 1;
    for (let j = i + 1; j < ls.length; j++) {
      const l = ls[j];
      if (l.trim() === '') continue;
      const ind = l.length - l.trimStart().length;
      if (ind >= base) continue;
      // 缩进不足：要么这是合法的块结束（下一个 YAML 结构），要么就是提前结束
      if (looksLikeYamlStructure(l)) break;
      bad++;
      console.log(
        `FAIL ${ymlPath} 第 ${j + 1} 行缩进 ${ind} < 块标量基准 ${base}，且不像 YAML 结构` +
          `\n    块标量起于第 ${i + 1} 行：${ls[i].trim()}` +
          `\n    这一行既不是列表项、也不是 key:、也不是注释 —— 说明块在这里被**提前结束**，` +
          `\n    YAML 会转头把它当映射键解析 → workflow 非法 → GitHub 一个 job 都不建` +
          `\n    （run 在同一秒内 failure，jobs 数组为空，看起来像「CI 挂了但没有失败步骤」）。` +
          `\n    常见成因：bash 单引号可以跨行，于是把一条长命令拆成多行 —— shell 合法，YAML 非法。` +
          `\n    修法：写在一行里，或每续行都与首行同缩进。` +
          `\n    出错行：${l.trim().slice(0, 80)}`
      );
      break;
    }
  }
  return bad;
}

/**
 * 权威解析：依次试 python3 / python / py，调用 tools/yamlcheck.py（需 PyYAML），
 * 最后退回 js-yaml。返回 null = 都不可用 —— 调用方必须把「不可用」当**失败**。
 *
 * 为什么 Python 单独成文件：把 Python 源码塞进 JS 字符串字面量需要多层引号转义，
 * 我在一轮里连续踩了三次（漏引号让 Python 行被当 JS 执行、Python 的 chr(10) 留在
 * JS 里、单双引号嵌套 SyntaxError），而症状都出现在 check-ci.js 里，排查方向被带偏。
 * 独立文件把这整类问题消掉了：Python 用 Python 语法，JS 用 JS 语法。
 *
 * 两个必须处理的「假可用」：
 *   1. Windows 的 `python3` 常常只是 **Microsoft Store 占位符** —— 它存在、能启动，
 *      但打印一段提示并非零退出。把它当成「Python 不可用」才能继续试下一个，
 *      否则会得出「YAML 非法」的错误结论（实测踩过）。
 *   2. 没装 PyYAML 时脚本返回 9 并打印 NOPYyaml —— 同样要继续，而不是当成通过。
 *
 * 第三种「找不到」：**真解释器装了但不在 PATH 上**。本机就是如此 —— PATH 上
 * 只有 Store 占位符，真解释器在 `%LOCALAPPDATA%\Programs\Python\Python312`。
 * 只试 PATH 上的名字会得出「本机没有 Python」，而事实是「PATH 里没有」。
 * 所以候选列表里要带上 Windows 官方安装位置的绝对路径。
 */
/** Windows 上真解释器常在官方安装目录但不在 PATH；Linux/macOS 上返回空。 */
function winPythonCandidates() {
  const out = [];
  for (const base of [process.env.LOCALAPPDATA, process.env.APPDATA]) {
    if (!base) continue;
    // Python312 是本机实测的版本；不写死单一版本号 —— 探测失败时报错文案
    // 会把这个目录打出来，人能自己看到实际版本。
    for (const ver of ["Python312", "Python313", "Python311"]) {
      const p = path.join(base, "Programs", "Python", ver, "python.exe");
      if (fs.existsSync(p)) out.push(p);
    }
  }
  return out;
}

function authoritativeYamlParse(file) {
  const script = path.join(__dirname, "yamlcheck.py");
  const STUB = /Microsoft Store|was not found|not recognized as an internal/i;
  const CANDS = ["python3", "python", "py", ...winPythonCandidates()];
  for (const bin of CANDS) {
    const args = bin === "py" ? ["-3", script, file] : [script, file];
    let out = "";
    try {
      // stderr 也要收进管道：Windows 的 python3 占位符会往 stderr 写一段
      // 「Python was not found…Microsoft Store」——用默认 stdio 的话它会直接
      // 漏到控制台，让一条成功的检查看起来像出了错。
      out = String(execFileSync(bin, args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }));
    } catch (e) {
      if (e && e.code === "ENOENT") continue; // 真的没这个可执行文件
      out = String((e && e.stdout) || "") + String((e && e.message) || "");
      if (STUB.test(out)) continue; // Store 占位符 / 未注册，等于不可用
      if (out.indexOf("NOPYyaml") >= 0) continue; // 没装 PyYAML
      const y = /YAMLERR line (\d+): *([^\r\n]*)/.exec(out);
      if (y) return bin + " 判定非法 —— 第 " + y[1] + " 行: " + y[2].trim();
      const io = /IOERR: *([^\r\n]*)/.exec(out);
      if (io) return bin + " 读文件失败 —— " + io[1].trim();
      return bin + " 调用失败 —— " + out.split(/\r?\n/)[0].trim().slice(0, 120);
    }
    return bin + "（tools/yamlcheck.py）：YAML 可解析";
  }
  try {
    require("js-yaml").load(fs.readFileSync(file, "utf8"));
    return "js-yaml：YAML 可解析";
  } catch (e) {
    if (e && e.code !== "MODULE_NOT_FOUND") {
      return "js-yaml 判定非法 —— " + String(e.message).split("\n")[0];
    }
  }
  return null;
}
// ---- 2. 变异清单是否还对得上源码 -----------------------------------------
function listed(script) {
  return JSON.parse(
    execFileSync(process.execPath, [script, "--list"], { encoding: "utf8", maxBuffer: 8 << 20 })
  );
}
/** 复用脚本自己的换行归一化规则，避免这里再实现一遍 */
function stillMatches(src, from) {
  return ["\n", "\r\n"].some((nl) => src.includes(from.replace(/\n/g, nl)));
}

const jsList = listed("tools/mutate-check.js");
const appJs = fs.readFileSync(jsList.file, "utf8");

// 数量下限：防的是最阴的一种退化 —— **有人把变异清单删掉/清空**，此后每条检查
// 都不再有人验证，CI 依然全绿。逐条比对查不出"清单本身没了"这件事。
// 新增检查时把下限一起调高；确实要减少条目时，改这里并写清原因。
// 下限 = 当前实际条数，所以「删掉几条」和「清单被清空」都会被立刻发现。
// （之前写的是 10/7，而实际已到 16/17 —— 下限形同虚设，删掉 6 条都不会有人知道。）
const MIN_CHECK_MUTATIONS = 22;
const MIN_RUST_MUTATIONS = 17;
if (jsList.cases.length < MIN_CHECK_MUTATIONS) {
  console.log(
    `FAIL mutate-check 只剩 ${jsList.cases.length} 条变异（下限 ${MIN_CHECK_MUTATIONS}）—— ` +
      `多半是清单被删过，此后这些行为检查就再没人验证了`
  );
  bad++;
}

for (const [from, , name] of jsList.cases) {
  if (!stillMatches(appJs, from)) {
    console.log(
      `FAIL mutate-check 的变异已失效：${name}\n      源码改了但 from 串对不上，` +
        `\n      这条变异从此形同虚设（脚本会报"未生效"并返回非 0，但没人知道它已经名存实亡）。\n` +
        `      from: ${JSON.stringify(from.slice(0, 90))}`
    );
    bad++;
  }
}
console.log(`mutate-check：${jsList.cases.length} 条变异，全部仍能在 app.js 里定位`);

const rustList = listed("tools/mutate-rust.js");
if (rustList.cases.length < MIN_RUST_MUTATIONS) {
  console.log(
    `FAIL mutate-rust 只剩 ${rustList.cases.length} 条变异（下限 ${MIN_RUST_MUTATIONS}）—— ` +
      `清单被删过，这些集成测试就再没人验证是否真的会失败`
  );
  bad++;
}
const testSrc =
  fs.readFileSync("crates/finweb/tests/api.rs", "utf8") +
  fs.readFileSync("crates/fincore/src/user.rs", "utf8") +
  fs.readFileSync("crates/findb/src/users.rs", "utf8");
for (const m of rustList.cases) {
  const src = fs.readFileSync(m.file, "utf8");
  if (!stillMatches(src, m.from)) {
    console.log(
      `FAIL mutate-rust 的变异已失效：${m.name}\n      file: ${m.file}\n      from: ${JSON.stringify(
        m.from.slice(0, 90)
      )}`
    );
    bad++;
  }
  // 指向的测试必须真的存在，否则跑出来是「0 passed」并被读成 PASS。
  // 直接取命令数组最后一项；旧正则要求命令里有引号，而 join(" ") 后永远没有引号，
  // 于是这条检查从未生效。以 `::` 结尾的是模块前缀（如 user::tests::），跳过。
  const testName = m.cmd[m.cmd.length - 1];
  if (testName && !testName.endsWith("::") && !testSrc.includes(testName)) {
    console.log(`FAIL mutate-rust 指向的测试 ${testName} 已不存在（${m.name}）`);
    bad++;
  }
}
console.log(`mutate-rust：${rustList.cases.length} 条变异，全部仍能在源码里定位`);

// ---- 2.5 测试夹具不许靠继承内控闸门的默认值 --------------------------------
//
// 扫**每一个**测试代码里的 `BookOptions` 字面量，要求显式写出 `enable_audit` 与
// `require_cashier`。
//
// 为什么是「每一个」而不是「我记得的那几个」：同一个坑这轮在三个地方各犯一次 ——
//   · finweb/tests/api.rs 的 test_state()        → 28 条集成测试变红
//   · findb/src/lib.rs 的 tests::test_opts()     → 57 条单测变红
//   · findb/tests/{core_loop,ext_loop}.rs 的夹具 → 21 条变红
// 我一开始只查了前两处，第三处立刻又炸了一遍。按名字列举 = 按记忆列举 = 迟早漏。
//
// 「字面量」必须排除函数**返回类型**：`pub fn options(&self) -> BookOptions {` 里的
// `BookOptions {` 是函数体开头，不是字面量。把它们算进来就有 2 条永久性误报 ——
// 一个会误报的检查等于没有检查。
function scanBookOptionLiterals(root) {
  const targets = [
    "crates/finweb/tests/api.rs",
    "crates/findb/src/lib.rs",
    ...fs
      .readdirSync(path.join(root, "crates", "findb", "tests"))
      .filter((f) => f.endsWith(".rs"))
      .map((f) => "crates/findb/tests/" + f),
  ];
  const found = [];
  for (const rel of targets) {
    const full = path.join(root, rel);
    if (!fs.existsSync(full)) continue;
    const ls = fs.readFileSync(full, "utf8").split(/\r?\n/);
    ls.forEach((l, i) => {
      const at = l.search(/\bBookOptions\s*\{/);
      if (at < 0) return;
      const before = l.slice(0, at);
      if (/\bfn\b/.test(before) || /->/.test(before)) return;
      let depth = 0;
      const seg = [];
      for (let k = i; k < Math.min(ls.length, i + 40); k++) {
        seg.push(ls[k]);
        for (const ch of ls[k]) {
          if (ch === "{") depth++;
          else if (ch === "}") depth--;
        }
        if (depth === 0 && k > i) break;
      }
      const body = seg.join("\n");
      const missing = ["enable_audit", "require_cashier"].filter(
        (f) => !new RegExp("\\b" + f + "\\s*:").test(body)
      );
      found.push({ rel, line: i + 1, missing });
    });
  }
  return found;
}
if (FIX_ROOT) {
  const lits = scanBookOptionLiterals(FIX_ROOT);
  const badLits = lits.filter((x) => x.missing.length);
  for (const x of badLits) {
    console.log(
    `FAIL ${x.rel}:${x.line} 的 BookOptions 字面量没有显式写 ${x.missing.join(" / ")} —— ` +
      `它在靠 ..Default::default() 继承\n` +
      `    出处：require_cashier 的默认值从 false 改成 true 那天，靠继承的三处夹具` +
      `    分别是 28 条 / 57 条 / 21 条用例集体变红，报错全指向凭证记账逻辑。\n` +
      `    修法：在字面量里写明 ${x.missing.join("、")}: <true|false>，并说明为何与生产默认值不同。`
    );
    bad++;
  }
  if (lits.length === 0) {
    bad++;
    console.log("FAIL 一个 BookOptions 字面量都没扫到 —— 判据失效，必须报错而不是「通过」");
  }
  console.log(`测试夹具：${lits.length} 个 BookOptions 字面量，${badLits.length} 个靠继承默认值`);
}

// ---- 2.6 会话清理的三种语义不许用错地方 ------------------------------------
//
// 「一个账号可绑两台」这一轮加了一种清理，于是有三种，语义不同、极易串：
//   登录              → remove_by_username_and_device  只顶掉**本设备**的旧会话
//   改密              → remove_others                 清掉**别的设备**，保留本设备
//   重置设备/停用/删号 → remove_by_username            全清
// 用错的后果：改密用前者 → 改完口令别的设备的旧会话照样能用；
//               重置设备用 remove_others → 旧设备不被踢，绑定等于没重置。
//
// 判据落在**调用点所在的函数**上，不做全文件 grep：同名方法在文件里出现多次。
const SESS_RULES = [
  { fn: "post_login", want: "remove_by_username_and_device",
    why: "登录只应顶掉本设备的旧会话。若用 remove_by_username 清全部，第 2 台设备一登录" +
         "就把第 1 台踢下线——允许绑 2 台就只剩一个数字。" },
  { fn: "post_change_password", want: "remove_others",
    why: "改密应清掉**别的设备**的会话、保留本设备。若用 remove_by_username，" +
         "本人会被踢去重新登录；若用 remove_by_username_and_device，别的设备上的" +
         "旧会话不会被清——改完口令却还有别的设备能继续用。" },
];
if (FIX_ROOT) {
  const hSrc = fs.readFileSync(path.join(FIX_ROOT, "crates", "finweb", "src", "handlers.rs"), "utf8");
  let sessBad = 0;
  for (const r of SESS_RULES) {
    const body = fnBodyOf(hSrc, r.fn);
    if (!body) {
      bad++;
      sessBad++;
      console.log(`FAIL handlers.rs 里找不到 ${r.fn}() —— 函数改名时本检查要跟着改`);
      continue;
    }
    const used = ["remove_by_username_and_device", "remove_others", "remove_by_username"].filter(
      (m) => new RegExp("\\." + m + "\\(").test(body)
    );
    if (!used.includes(r.want)) {
      bad++;
      sessBad++;
      console.log(
        `FAIL ${r.fn}() 没有调用 remove_by_${r.want === "remove_others" ? "others" : r.want === "remove_by_username" ? "username" : "username_and_device"}()（实际用了 ${used.join(" / ") || "一个都没用"}）` +
          `\n    ${r.why}`
      );
    }
  }
  // 反向：重置设备 / 停用 / 删号必须全清，否则旧设备上的会话还能用到自然过期
  for (const fn of ["reset_platform_device", "reset_user_device", "delete_platform_user"]) {
    const body = fnBodyOf(hSrc, fn);
    if (!body) { bad++; sessBad++; console.log(`FAIL handlers.rs 里找不到 ${fn}()`); continue; }
    if (!/\.remove_by_username\(/.test(body)) {
      bad++;
      sessBad++;
      console.log(
        `FAIL ${fn}() 没有全清该用户会话（remove_by_username）` +
          `\n    少了这一步，旧设备上的会话还能继续用到自然过期——重置设备绑定等于没重置`
      );
    }
  }
  // 结论行**必须**按实际结果说话：原来无条件打印「各自正确」，
  // 于是上面报了 FAIL、下面还说「各自正确」，读的人只会看最后一行。
  console.log(
    sessBad === 0
      ? "会话清理：登录/改密/重置设备三处语义各自正确"
      : `会话清理：${sessBad} 处语义用错了`
  );
}

// ---- 2.7 e2e spec 用到的 helpers 必须在 import 里 ---------------------------
//
// 判据的来历：把两处「audit 一下就 post」换成 auditSignPost 时**忘了加 import**。
// `node --check` 只做纯语法检查、不看标识符是否已定义，所以 check-js 全绿、
// cargo 全绿，直到 E2E 跑到那个文件才炸：ReferenceError: auditSignPost is not defined。
// E2E 是整套验证里最慢的一环 —— 一个拼写级别的错误要等十几分钟才现形。
//
// 名单取自 helpers.js 的 module.exports（权威来源），不猜。
if (FIX_ROOT) {
  const helpersSrc = fs.readFileSync(path.join(FIX_ROOT, "e2e", "helpers.js"), "utf8");
  const ex = /module\.exports\s*=\s*\{([\s\S]*?)\n\};/.exec(helpersSrc);
  if (!ex) {
    bad++;
    console.log("FAIL 读不出 e2e/helpers.js 的 module.exports —— 判据失效，必须报错而不是「通过」");
  } else {
    const exported = ex[1]
      .split(",")
      .map((x) => x.trim())
      .filter((x) => /^[A-Za-z_$][\w$]*$/.test(x));
    let impBad = 0;
    const testsDir = path.join(FIX_ROOT, "e2e", "tests");
    for (const f of fs.readdirSync(testsDir)) {
      if (!f.endsWith(".js")) continue;
      const src = fs.readFileSync(path.join(testsDir, f), "utf8");
      const im = /require\("\.\.\/helpers"\)/.test(src)
        ? /const\s*\{([^}]*)\}\s*=\s*require\("\.\.\/helpers"\)/.exec(src)
        : null;
      const imported = new Set(
        (im ? im[1] : "")
          .split(",")
          .map((x) => x.trim())
          .filter(Boolean)
      );
      for (const name of exported) {
        if (imported.has(name)) continue;
        if (new RegExp(`(?<![\\w$.])${name}\\s*\\(`).test(src)) {
          impBad++;
          bad++;
          console.log(
            `FAIL e2e/tests/${f} 用了 ${name}()，但 require("../helpers") 里没有它` +
              `\n    症状是 ReferenceError: ${name} is not defined —— node --check 只看语法，` +
              `\n    不看标识符是否已定义，所以这类错误要等 E2E（整套验证里最慢的一环）才现形。`
          );
        }
      }
    }
    if (impBad === 0) {
      console.log(`e2e spec：${exported.length} 个 helpers，用到的都在 import 里`);
    }

    // ---- 绕过 newBook 的 spec 必须自己关掉精简模式 ----
    //
    // 精简模式默认只铺 16 项，而 90 条 E2E 里有 70 个视图靠**点侧栏**进去。
    // 凡是绕开 `newBook(page, …)`（自己手写登录+建账）的 spec，都拿不到
    // newBook 里的 setSimpleNav，侧栏就只剩 16 项 —— 后面点不到的视图会一路
    // 超时（60s × retries 1），一个文件能拖掉几十分钟。
    //
    // 本次就是漏了 `smoke.spec.js` 和 `aux-attach-reports.spec.js` 两个文件，
    // 症状是 CI 的 E2E 跑了 50 分钟才挂（正常 16-20），而日志要 Actions 写
    // 权限才拿得到，只能靠这条检查提前拦住。
    //
    // 判据是「文件里出现 `page.goto("/")` 就必须提到 setSimpleNav 或 newBook」。
    // 宁可多报：真出现了误报，一眼就能看出该文件其实不需要。
    let navBad = 0;
    const testsDir2 = path.join(FIX_ROOT, "e2e", "tests");
    for (const f of fs.readdirSync(testsDir2)) {
      if (!f.endsWith(".js")) continue;
      const src = fs.readFileSync(path.join(testsDir2, f), "utf8");
      if (!/page\.goto\(\s*"\/"\s*\)/.test(src)) continue; // 没自己 goto 的不用管
      if (/newBook\s*\(/.test(src)) continue; // 走 newBook 的自带开关
      if (/setSimpleNav\s*\(/.test(src)) continue; // 自己处理了
      navBad++;
      bad++;
      console.log(
        `FAIL e2e/tests/${f} 自己 page.goto("/") 登录，但既没用 newBook 也没调 setSimpleNav` +
          `\n    精简模式默认只铺 16 项，没关掉的话点不到的视图会一路超时（60s×retries），` +
          `\n    一个文件就能拖掉几十分钟。在 page.goto 之前加一行 setSimpleNav(page, false)。`
      );
    }
    if (navBad === 0) {
      console.log("e2e spec：自己 goto 的都处理了精简模式开关");
    }

    // ---- 建账必须走 newBook ----
    //
    // 账套上限是**每账号 10 个**。`newBook` 里的 `purgeOwnBooks` 会把本套件建的
    // 书清掉，而它认的是 `E2E` 前缀。自己手写建账的 spec 建出来的名字没有
    // worker 段 → 清不掉 → 额度逐轮累积 → 某天 `POST /api/books` 全 400 →
    // **整个套件失败**。
    //
    // 实测：98 条里 65 条一起挂，每条都报「等不到 .nav-item[data-view=vouchers]」，
    // 与它们各自的测试内容毫无关系。一个额度问题被放大成 65 个假失败，
    // 而真因（额度满）只出现在第一条失败里，中间隔了几十行不相干的堆栈。
    //
    // 判据：spec 里出现 `#cb-save`（自己点「创建」按钮）就报红。建账这件事的
    // 全部知识（命名、清理、精简开关、默认期间、出厂配置回读）都在 newBook 里，
    // 绕过它没有任何好处，只有漏清理的风险。
    let bookBad = 0;
    for (const f of fs.readdirSync(testsDir2)) {
      if (!f.endsWith(".js")) continue;
      const src = fs.readFileSync(path.join(testsDir2, f), "utf8");
      if (!/#cb-save/.test(src)) continue;
      if (/newBook\s*\(/.test(src)) {
        console.log(
          `WARN e2e/tests/${f} 同时用了 newBook 和 #cb-save —— 若确实是绕过 newBook 自建账套，` +
            `那本账套 purgeOwnBooks 清不掉，额度满时全套件一起挂。确认是有意的就在这里留个说明。`
        );
        continue;
      }
      bookBad++;
      bad++;
      console.log(
        `FAIL e2e/tests/${f} 自己点 #cb-save 建账，没走 newBook` +
          `\n    账套上限每账号 10 个。newBook 里的 purgeOwnBooks 按 E2E 前缀清理，` +
          `\n    自己建的名字没有那个前缀 → 清不掉 → 额度占满后 POST /api/books 全 400，` +
          `\n    整个套件一起挂（实测 98 条里 65 条假失败）。改成 newBook(page, \`名称\`)。`
      );
    }
    if (bookBad === 0) {
      console.log("e2e spec：建账都走 newBook（账套能按前缀清理）");
    }

    // ---- setSimpleNav(page, null) 之后不能再声称测了「持久化」 ----
    //
    // **addInitScript 不能注销**。一旦 newBook(…, {simpleNav: true/false}) 注册过
    // 写 localStorage 的脚本，后面 `setSimpleNav(page, null)` 只是「不再新增」，
    // 已注册的那个照样在每次导航前执行、照样覆写值。所以「null 之后 reload 看值
    // 还在不在」测到的不是持久化，是脚本又写了一遍 —— 永远绿或永远红，都没意义。
    //
    // 本次 CI 的真实报错就是它：Expected: not checked / Received: checked。
    // 症状极像产品 bug（开关没存住），实际是探针自己写错了。
    //
    // 判据按 **test 块**判断，不按文件：同一个 spec 里不同 test 用不同模式是正常的
    // （「精简不拦路由」用 simpleNav:true 完全合理，它不测持久化）。
    // 第一版按文件判，立刻把 simple-nav.spec.js 误报了 —— 一个新增的检查如果
    // 在真实代码上就误报，它会很快被人注释掉，那还不如不写。
    let nullBad = 0;
    for (const f of fs.readdirSync(testsDir2)) {
      if (!f.endsWith(".js")) continue;
      const src = fs.readFileSync(path.join(testsDir2, f), "utf8");
      if (!/setSimpleNav\s*\([^)]*null/.test(src)) continue;
      // 按 test( 切开，逐块判；块首之前是文件头（require/常量），一并算第 0 块
      const blocks = src.split(/(?=\ntest\s*\()/);
      blocks.forEach((blk, bi) => {
        if (!/setSimpleNav\s*\([^)]*null/.test(blk)) return;
        const m = blk.match(/newBook\s*\(([\s\S]{0,300}?)\)\s*;/g) || [];
        if (!m.some((call) => /simpleNav\s*:\s*(true|false)/.test(call))) return;
        const name = (blk.match(/test\s*\(\s*["'`]([^"'`]+)/) || [])[1] || `第 ${bi} 块`;
        nullBad++;
        bad++;
        console.log(
          `FAIL e2e/tests/${f} 的「${name}」既用 setSimpleNav(…, null) ` +
            `又在同一个用例里传了 newBook(…, {simpleNav: true/false})` +
            `\n    addInitScript 不能注销：先注册的那个每次导航前照样覆写 localStorage，` +
            `\n    「null 之后 reload 看持久化」测到的不是持久化（症状极像产品 bug：` +
            `\n    开关没存住）。改成 newBook(page, name, { simpleNav: null }) 起步，` +
            `\n    全程只用真实点击改状态。`
        );
      });
    }
    if (nullBad === 0) {
      console.log("e2e spec：没有「null 之后却注册过覆写脚本」的组合");
    }
  }
}

// ---- 2.8 里程碑报告必须带「历史快照」声明 ----------------------------------
//
// 判据的来历：仓库转公开后，8 份报告里的「MRP运算 ❌ 0%」「测试 203 项」会被
// 任何读者当成现状。这些结论**实测已被推翻**（MRP / 工序报工 / 委外 / BOM 都已
// 实现，测试是 657 项 + 73 条 E2E），但文档不会自己更新。
//
// 不删那些报告 —— 它们是「当时做了什么」的记录，删掉是另一种信息损失。
// 关键区别从来不是「文档旧」，而是「旧文档读起来像现状」。
const REPORT_MD = [
  "COMPLETION_SUMMARY.md",
  "DELIVERY_CHECKLIST.md",
  "ERP_MANUFACTURING_GAP_ANALYSIS.md",
  "FINAL_MANUFACTURING_ERP_REPORT.md",
  "FINAL_SECURITY_REPORT.md",
  "FINAL_VERIFICATION.md",
  "MANUFACTURING_ERP_IMPLEMENTATION_REPORT.md",
  "COMPREHENSIVE_COMPARISON.md",
];
let docBad = 0;
for (const rel of REPORT_MD) {
  const f = path.join(FIX_ROOT || '.', rel);
  if (!fs.existsSync(f)) continue;
  const t = fs.readFileSync(f, "utf8");
  if (t.includes("本文是历史快照")) continue;
  // 还没加声明。先确认它是不是真的需要 —— 只在含「对照竞品」或「测试条数」时才要求，
  // 否则一份纯说明性的 md 不该被这条检查卡住
  const competitive = /金蝶|用友|覆盖度|对标/.test(t);
  const testCount = /\d+\s*\/\s*\d+\s*passed|\d+\s*项测试|测试[:：]\s*\d+/.test(t);
  if (!competitive && !testCount) continue;
  docBad++;
  bad++;
  console.log(
    `FAIL ${rel} 含对照竞品的覆盖度或测试条数声明，但没有「历史快照」声明` +
      `\n    仓库已公开，读者会把「MRP运算 ❌ 0%」「测试 203 项」当成现状。` +
      `\n    这些结论实测已被推翻（MRP / 工序报工 / 委外 / BOM 都已实现；测试 657 + 73 E2E）。` +
      `\n    修法：在一级标题后加一段声明，写清哪些结论已过时、现状去哪看。`
  );
}
if (docBad === 0) {
  console.log(`里程碑报告：${REPORT_MD.length} 份需要声明，都已带「历史快照」`);
}

// ---- 2b. 客户管理模块设计文档：代码引用不许漂移，且自检必须真的会拦 ----
//
// 设计文档里写了 `scm.rs:1161`、`sales.rs:757` 这类**行号**引用。
// 文档里一个过期的行号比没有引用更坏 —— 它会让人**自信地**去改错的地方，
// 而不像「没写」那样引人警觉。所以它得像代码一样被守。
//
// 自检是这块的关键：没有自检，「检查通过」并不能证明「检查有用」。
// 三个探针分别对着三种失效：代码漂移 / 约束声明被删 / 未登记的引用。
try {
  const r = execFileSync(process.execPath, [path.join(__dirname, "check-design-doc.js")], {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  process.stdout.write(r);
  if (/FAIL/.test(r)) {
    console.log("设计文档引用检查：有 FAIL");
    bad++;
  }
  if (!runGuardSelfTest("设计文档检查", "selftest-design-doc.js")) bad++;
} catch (e) {
  // 自检脚本缺失或崩了同样算失败：无法校验 ≠ 校验通过。
  console.log(
    "设计文档检查未能运行：" +
      String((e && e.message) || e).split("\n")[0].slice(0, 110)
  );
  bad++;
}

// ---- 2c. tools/ 与 e2e/ 的脚本必须可在任何机器上跑 ----
//
// 2026-10-01 的 Test job 就是红在这里：selftest-design-doc.js 里写死了
// `C:\Users\Administrator\...` —— 本地跑**完全正常**（路径就在那儿），
// 只有 CI 的 Linux runner 上才 ENOENT 崩。也就是说「本地绿」对这一类错误
// 完全没有证明力，所以必须有一道能在提交前拦住的检查。
try {
  const r = execFileSync(process.execPath, [path.join(__dirname, "check-portable.js")], {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  process.stdout.write(r);
  if (/FAIL/.test(r)) {
    console.log("可移植性检查：有 FAIL");
    bad++;
  }
  if (!runGuardSelfTest("可移植性检查", "selftest-portable.js")) bad++;
} catch (e) {
  console.log("可移植性检查未能运行：" + String((e && e.message) || e).split("\n")[0].slice(0, 100));
  bad++;
}
// ---- 2c-2. 精简模式的侧栏逻辑 ----
//
// 为什么单独立一条而不是并进 check-js.js：**E2E 才是它的主要验证手段，
// 而 E2E 只在 CI 上跑**。本机 Playwright 的输入注入失效（`page.fill` 之后
// `input.value` 是空的），所以开发者本地跑 E2E 只会看到「等不到 #new-book」，
// 分不清是产品坏了还是环境不行 —— 本次 CI 上的 E2E 失败就是这样卡住的。
//
// 这条检查在 Node 里抽 app.js 的**真实源码片段**执行（抽不到就报错，不许
// 静默用副本），验证三条不变量：默认只铺日常功能、搜索数据源是全量、
// 账号分区互不污染。
try {
  const r = execFileSync(process.execPath, [path.join(__dirname, "check-simple-nav.js")], {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  process.stdout.write(r);
  if (/FAIL/.test(r)) {
    console.log("精简模式侧栏：有 FAIL");
    bad++;
  }
} catch (e) {
  console.log(
    "精简模式检查未能运行：" + String((e && e.message) || e).split("\n")[0].slice(0, 100)
  );
  bad++;
}

// ---- 2d. 客户视图的余额只能有一处算法 ----
//
// docs/客户管理模块设计.md §3.2①：客户视图的所有余额必须走
// `settle::open_entries`（H-3：只认已记账）。自己写 SQL 算一遍 = 两套口径，
// 而「为什么这个客户欠款和往来核销页不一样」是最难查的一类问题。
// 「约定靠人记住」= 迟早有人在自己模块里加一句 SUM(...)，所以要机器守。
try {
  const r = execFileSync(process.execPath, [path.join(__dirname, "check-customers-balance.js")], {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  process.stdout.write(r);
  if (/FAIL/.test(r)) {
    console.log("客户余额口径检查：有 FAIL");
    bad++;
  }
  if (!runGuardSelfTest("客户余额口径检查", "selftest-customers-balance.js")) bad++;
} catch (e) {
  console.log("客户余额口径检查未能运行：" + String((e && e.message) || e).split("\n")[0].slice(0, 100));
  bad++;
}
// ---- 3. 编码卫生：全仓库不许有 U+FFFD 替换符 / 非法 UTF-8 / BOM ---------
//
// 这不是洁癖：本轮在仓库里找到 3 处**既有**的编码损坏，其中一处在
// `handlers.rs` 的**用户可见字符串**里 ——
//   "增值税一般纳税<FFFD><FFFD><FFFD>申报表（主表）"
// 也就是导出的增值税申报表标题是坏的，用户直接看得见。
//
// 为什么必须机器化：肉眼在编辑器/控制台里分不出「这是乱码」和「终端显示乱码」。
// 我自己就误判过好几次 —— PowerShell 5.1 的 GBK 控制台会把正常中文显示成
// 一串问号，第一反应是"文件被写坏了"，其实文件是好的。反过来，文件真坏了
// （UTF-8 字节序列被截断）在控制台里看起来和正常中文**一模一样**。
// 所以只能按码位判：码位 U+FFFD（替换符）只可能来自解码失败，正常文本里不会出现。
//
// 注意这行注释以前是**字面**放着那个替换符来举例的。本检查会扫本文件，所以它把自己
// 判成了损坏文件 —— 检查是对的，注释里也不该有它。这条能潜伏这么久，靠的是
// 「本文件当时还没进 git ls-files」（检查只扫受版本控制的文件）。
//
// 顺带禁 BOM：BOM 是"看不见"的差异，会让部分工具链出怪问题。
/**
 * 编码检查要搜的字符：Unicode 替换符 U+FFFD。
 *
 * 为什么按码位构造而不写字面量：字面量本身就是这个检查要禁止的东西 ——
 * 本文件也在扫描范围内（一旦它进了 git ls-files），写字面量等于让检查把自己
 * 判成损坏文件。`git add` 之前它一直潜伏着，因为检查只扫受版本控制的文件。
 *
 * 顺带一提：按码位写也更自说明 —— fromCharCode(0xfffd) 一眼就知道搜的是谁，
 * 而 REPLACEMENT_CHAR 只会让人以为是乱码。
 */
const REPLACEMENT_CHAR = String.fromCharCode(0xfffd);
// `-c core.quotepath=false` + `-z` 是必需的，不是洁癖：
//   · git 的 `core.quotepath` 默认 **true**，非 ASCII 路径会被输出成 C 转义
//     （`docs/\345\256\242...md`），那个字符串在磁盘上不存在；
//   · 于是旧的 `filter(f => fs.existsSync(f))` 会把它们**静默跳过** ——
//     实测漏掉 2 个文件：`docs/多租户套帐架构设计.md` 与
//     `docs/客户管理模块设计.md`，也就是**受机器检查的设计文档本身**。
//   · 检查报「扫描 203 个，0 个有问题」时，那 2 个文件一次都没被读过。
//     `docs/客户管理模块设计.md` 里那 3 个 U+FFFD（`额度调整日志` 被写坏成
//     `额<?><?><?>调整日志`）就是这么活下来的 —— 绿色的检查结果，
//     底下是一个空洞。
//
// 所以：**不许静默跳过**。列不出来/读不到的文件必须报错，见下面的 missing 处理。
const tracked = execFileSync(
  "git",
  ["-c", "core.quotepath=false", "ls-files", "-z"],
  { encoding: "utf8", maxBuffer: 8 << 20 }
)
  .split("\0")
  .filter(Boolean)
  .filter((f) => /\.(rs|js|ts|json|md|yml|toml|css|html|ps1)$/i.test(f));
let encBad = 0;
// 「列在版本控制里、工作区却读不到」= 无法校验。旧代码把它当不存在而跳过，
// 那是把「没检查」报成「检查通过」。现在它是一条明确的失败。
const missing = tracked.filter((f) => !fs.existsSync(f));
for (const f of missing) {
  console.log(`FAIL ${f}: 受版本控制但工作区读不到 —— 无法校验编码（不许静默跳过）`);
  encBad++;
}
for (const f of tracked.filter((f) => fs.existsSync(f))) {
  const buf = fs.readFileSync(f);
  if (buf[0] === 0xef && buf[1] === 0xbb && buf[2] === 0xbf) {
    console.log(`FAIL ${f}: 带 UTF-8 BOM`);
    encBad++;
  }
  let text;
  try {
    text = new TextDecoder("utf-8", { fatal: true }).decode(buf);
  } catch (e) {
    console.log(`FAIL ${f}: 非法 UTF-8 字节序列（${e.message}）`);
    encBad++;
    continue;
  }
  const i = text.indexOf(REPLACEMENT_CHAR);
  if (i >= 0) {
    const line = text.slice(0, i).split(/\r?\n/).length;
    console.log(
      `FAIL ${f}:${line} 含 U+FFFD 替换符（解码失败的痕迹）：` +
        JSON.stringify(text.slice(Math.max(0, i - 30), i + 30))
    );
    encBad++;
  }
}
console.log(`编码卫生：扫描 ${tracked.length} 个受版本控制的源文件，${encBad} 个有问题`);

// ---- 4. 测试夹具不许靠继承默认值 -----------------------------------------
//
// api.rs 的 `test_state()` 是 186 条集成测试共用的账套夹具。原来它写的是
// `BookOptions { enable_audit: false, ..Default::default() }` —— `require_cashier`
// 靠继承。默认值从 false 改成 true 的那天，28 条用例集体变红，报错是
// 「记账应成功 left: 400 right: 200」：指向记账逻辑，实际是夹具静默换了配置。
// 修法只花一行，排查花了一整轮。
//
// 判据落在 `test_state()` **函数体**上，不做全文件 grep：
// 「文件里出现过 require_cashier」不等于「夹具写了它」—— 第一版就是那么写的，
// 把被守的那行删掉，检查照样打印 OK。
//
// ⚠️ 「文件没找到」必须算**失败**。本检查的第一版路径算错了，`existsSync` 返回
//    false 就当无事发生 —— 检查自己失效却报喜，正是最坏的一种检查。
/** 夹具 test_state() 现在在哪个文件里。主检查与自检探针都必须问这个函数，
 *  不能各写一份 —— 各写一份就会出现「主检查改了、探针没改」这种静默失效。 */
function fixFixtureFile() {
  if (!FIX_ROOT) return "crates/finweb/tests/api.rs";
  for (const rel of [
    "crates/finweb/tests/common/mod.rs",
    "crates/finweb/tests/api.rs",
  ]) {
    const p = path.join(FIX_ROOT, ...rel.split("/"));
    if (fs.existsSync(p) && /^(pub )?fn test_state\b/m.test(fs.readFileSync(p, "utf8"))) {
      return rel;
    }
  }
  // 都没有就退回 api.rs —— 让后面的检查报「找不到夹具」，而不是静默跳过
  return "crates/finweb/tests/api.rs";
}

if (FIX_ROOT) {
  // 夹具可能落在 api.rs 或 tests/common/mod.rs（2026-10-01 提取）。
  // 两处都查：只查其中一处，夹具再搬一次就会漏 —— 而漏的表现是
  // 「对着空 body 得出『夹具没写闸门』」的错误结论，比不检查更糟。
  const fxFile = fixFixtureFile();
  const apiSrc = fs.readFileSync(path.join(FIX_ROOT, ...fxFile.split("/")), "utf8");
  // 允许行首空白：`test_opts` 缩进在 `mod tests` 里，而 `test_state` 在顶层。
  // 但**不能**写成「任意位置含 fn xxx(」—— 那会匹配到注释里提到函数名的地方，
  // 然后从一个空 body 上得出「夹具没写闸门」的错误结论。
  function fnBodyOf(src, name) {
    const ls = src.split(/\r?\n/);
    const st = ls.findIndex((l) =>
      new RegExp("^\\s*(?:pub(?:\\(crate\\))?\\s+)?(?:async\\s+)?fn\\s+" + name + "\\s*\\(").test(l)
    );
    if (st < 0) return null;
    let depth = 0;
    const out = [];
    for (let i = st; i < ls.length; i++) {
      out.push(ls[i]);
      for (const ch of ls[i]) {
        if (ch === "{") depth++;
        else if (ch === "}") depth--;
      }
      if (depth === 0 && out.length > 1) break;
    }
    return out.join("\n");
  }
  const fx = fnBodyOf(apiSrc, "test_state");
  if (!fx) {
    bad++;
    console.log(
      "FAIL 在 " + FIX_FILES.map((f) => path.relative(FIX_ROOT, f)).join(" / ") +
        " 里都找不到 test_state() —— 夹具改名或搬走时本检查要跟着改"
    );
  } else {
    console.log(`测试夹具：${fxFile} 的 test_state() 显式声明了两道闸门`);
    for (const field of ["enable_audit", "require_cashier"]) {
      if (!new RegExp("\\b" + field + "\\s*:").test(fx)) {
        bad++;
        console.log(`FAIL test_state() 的夹具没有显式写 ${field} —— 它在靠 ..Default::default() 继承`);
        console.log(`      修法：在 BookOptions 字面量里写明 ${field}: <true|false>，并说明为何与生产默认值不同`);
      }
    }
    for (const field of ["enable_audit", "require_cashier"]) {
      const m = new RegExp("\\b" + field + "\\s*:\\s*(true|false)").exec(fx);
      if (m) {
        const around = fx.slice(Math.max(0, m.index - 700), m.index);
        if (!/试[具夹]|显式|夹具|默认值/.test(around)) {
          bad++;
          console.log(`FAIL test_state() 把 ${field} 显式设成 ${m[1]}，但上面没有一句说明为什么`);
        }
      }
    }
    console.log("api.rs 夹具：两道闸门（enable_audit / require_cashier）都已显式声明");

// findb 侧的同一个夹具（`crate::tests::test_opts()`）也必须显式声明。
// 只查 api.rs 是不够的：这个坑在两处各犯了一次（findb 那边一次就让 57 条单测变红），
// 漏掉一处就只挡住一半。
  const findbSrc = fs.readFileSync(path.join(FIX_ROOT, "crates", "findb", "src", "lib.rs"), "utf8");
  const fo = fnBodyOf(findbSrc, "test_opts");
  if (!fo) {
    bad++;
    console.log("FAIL findb 里找不到 tests::test_opts() —— 夹具改名时本检查要跟着改");
  } else {
    for (const field of ["enable_audit", "require_cashier"]) {
      if (!new RegExp("\\b" + field + "\\s*:").test(fo)) {
        bad++;
        console.log(`FAIL findb 的 test_opts() 没有显式写 ${field} —— 它在靠 ..Default::default() 继承`);
        console.log(`      修法：在 BookOptions 字面量里写明 ${field}: <true|false>，并说明为何与生产默认值不同`);
      }
    }
    console.log("findb test_opts()：两道闸门（enable_audit / require_cashier）都已显式声明");
  }
  }
}
bad += encBad;

// ---- 4. 自检自身也要被验证（变异测试） ------------------------------------
//
// 一个只会在 happy path 上运行的检查，凭什么相信它？这里改坏三样东西各跑一遍：
// 源码里删掉一行（让某条变异对不上）、CI 里插一个重名 job、CI 里塞一个制表符。
// 三种都必须被本检查抓住。
const ANCHOR_MSG =
  "探针失效：ci.yml 里找不到单行的 docker inspect —— 若那条命令被改写，请同步改本探针的 one 串";

if (process.argv.includes("--selftest")) {
  /** 写文件并**自证真的改了**（from 与 after 必须不同），否则报"探针失效" */
  const poke = (file, from, to) => {
    const o = fs.readFileSync(file, "utf8");
    const f = ["\n", "\r\n"].find((nl) => o.includes(from.replace(/\n/g, nl)));
    if (!f) throw new Error(`探针失效：${file} 里找不到 ${JSON.stringify(from.slice(0, 50))}`);
    const after = o.replace(from.replace(/\n/g, f), to.replace(/\n/g, f));
    if (after === o) throw new Error(`探针失效：替换后内容没变（${file}）`);
    fs.writeFileSync(file, after);
    return () => fs.writeFileSync(file, o);
  };
  const probes = [
    {
      name: "源码改动使某条变异失效（删掉 paintTotals 调用）",
      apply: () => poke(jsList.file, ".yb = inp.value; paintTotals(); }", ".yb = inp.value; }"),
    },
    {
      name: "变异清单被清空（只剩 1 条，等于没人守）",
      apply: () => {
        const p2 = "tools/mutate-check.js";
        const o = fs.readFileSync(p2, "utf8");
        const i = o.indexOf("const cases = [");
        const j = o.indexOf("\n];", i);
        // 找不到结尾就必须抛错。原来这里是 j=-1 继续走，
        // `o.slice(j + 3)` 变成 `o.slice(2)` —— 把整个脚本从第 2 个字符起
        // 接在数组后面，文件彻底烂掉，检查却报「探针失效」以外的东西。
        // 探针把自己变成破坏源，比没有探针更糟。
        if (i < 0 || j < 0) {
          throw new Error(
            `探针失效：mutate-check.js 里找不到变异清单的边界（i=${i} j=${j}）`
          );
        }
        const keep = /const cases = \[\n\[[\s\S]*?\n\],\n/m.exec(o.slice(i, j));
        fs.writeFileSync(
          p2,
          o.slice(0, i) + "const cases = [\n  [\".yb = inp.value; paintTotals(); }\", \".yb = inp.value; }\", \"x\"],\n];" + o.slice(j + 3)
        );
        void keep;
        return () => fs.writeFileSync(p2, o);
      },
    },
    {
      name: "CI 里出现重名 job",
      apply: () => poke(ymlPath, "  clippy:\n", "  clippy:\n  clippy:\n"),
    },
    {
      name: "CI 里混入制表符缩进",
      apply: () => poke(ymlPath, "jobs:\n", "jobs:\n\t# oops\n"),
    },
    {
      // 拿一个**无关的**源文件（不是 .github/workflows/ci.yml）注入替换符 ——
      // 编码检查要覆盖全仓库，探针也得落在它真的看的地方
      name: "某个源文件被写入 U+FFFD 替换符",
      apply: () => {
        const p2 = "crates/finweb/static/util.js";
        const o = fs.readFileSync(p2, "utf8");
        fs.writeFileSync(p2, o + "\n// 故意写坏：" + REPLACEMENT_CHAR + "\n");
        return () => fs.writeFileSync(p2, o);
      },
    },
    {
      name: "某个源文件被加上 UTF-8 BOM",
      apply: () => {
        const p2 = "crates/finweb/static/util.js";
        const o = fs.readFileSync(p2);
        fs.writeFileSync(p2, Buffer.concat([Buffer.from([0xef, 0xbb, 0xbf]), o]));
        return () => fs.writeFileSync(p2, o);
      },
    },
    {
      // 这条探针守的是刚修的那个洞：`git ls-files` 的 `core.quotepath` 默认 true，
      // 中文文件名被输出成 C 转义 → `existsSync` 失败 → **静默跳过**。
      // 后果不是「少查两个文件」，是：检查报「扫描 203 个，0 个有问题」，
      // 而受机器检查的设计文档 `docs/客户管理模块设计.md` 一次都没被读过 ——
      // 它里面那 3 个 U+FFFD（`额度调整日志` 被写坏）就是这么活下来的。
      //
      // 所以探针必须打在**中文命名的文件**上。打在 util.js 上（已有那条探针）
      // 证明不了任何事：ASCII 文件名从来就没被跳过。
      name: "中文命名的源文件被写入 U+FFFD（守 core.quotepath 那个洞）",
      apply: () => {
        // 路径从 git ls-files 里**按内容特征**找出来，不写死中文名 ——
        // 写死的话，将来文件改名探针就变成空变异（空变异 = 永真）。
        const cjk = execFileSync(
          "git",
          ["-c", "core.quotepath=false", "ls-files", "-z"],
          { encoding: "utf8", maxBuffer: 8 << 20 }
        )
          .split("\0")
          .filter(Boolean)
          .filter((f) => /\.(md|rs|js)$/i.test(f) && /[^\x00-\x7f]/.test(f));
        if (cjk.length === 0) {
          throw new Error(
            "探针失效：仓库里没有中文命名的受版本控制源文件，" +
              "无法验证 core.quotepath=false 那个修复",
          );
        }
        const p2 = cjk[0];
        const o = fs.readFileSync(p2, "utf8");
        fs.writeFileSync(p2, o + "\n" + REPLACEMENT_CHAR + "\n");
        return () => fs.writeFileSync(p2, o);
      },
    },
    {
      // 夹具退回 ..Default::default() 继承：默认值一改，28 条用例会集体变红，
      // 而报错指向的是记账逻辑，人会往错的方向查。
      name: "账套测试夹具退回继承默认值（删掉 require_cashier 显式声明）",
      apply: () => {
        // 路径**必须和夹具的真实位置同源**：夹具 2026-10-01 从 api.rs 搬到了
        // tests/common/mod.rs。探针若仍指向 api.rs，那里已经没有那行可删 ——
        // 变异变成空操作，检查照旧绿，**探针静默退化成永真**。
        // 而它的名字还叫「api.rs 夹具」，从输出上完全看不出它已经失效。
        const p2 = fixFixtureFile();
        const o = fs.readFileSync(p2, "utf8");
        const nl = o.includes("\r\n") ? "\r\n" : "\n";
        const target = "        require_cashier: false," + nl;
        if (o.split(target).length - 1 !== 1) {
          throw new Error(
            "夹具文件 " + p2 + " 里找不到唯一的 require_cashier 声明 —— " +
              "探针无法变异（空变异 = 永真）",
          );
        }
        fs.writeFileSync(p2, o.split(target).join(""));
        return () => fs.writeFileSync(p2, o);
      },
    },
    {
      // 同一个坑在 findb 侧也犯过一次（test_opts 继承默认值 → 57 条单测集体变红）。
      name: "findb 的 test_opts 退回继承默认值（删掉 require_cashier 显式声明）",
      apply: () => {
        const p2 = "crates/findb/src/lib.rs";
        const o = fs.readFileSync(p2, "utf8");
        const nl = o.includes("\r\n") ? "\r\n" : "\n";
        fs.writeFileSync(p2, o.split("            require_cashier: false," + nl).join(""));
        return () => fs.writeFileSync(p2, o);
      },
    },
    {
      // 照着我今天真犯的错写：bash 单引号跨行 → 长命令拆三行 → 后两行缩进浅于块基准。
      // shell 合法、YAML 非法 → GitHub 一个 job 都不建（run 秒失败，jobs 数组为空）。
      name: "ci.yml 块标量被提前结束（bash 单引号跨行拆长命令）",
      apply: () => {
        const p2 = ".github/workflows/ci.yml";
        const o = fs.readFileSync(p2, "utf8");
        const nl = o.includes("\r\n") ? "\r\n" : "\n";
        const one =
          "          docker inspect -f 'Running={{.State.Running}} Status={{.State.Status}} ExitCode={{.State.ExitCode}} OOMKilled={{.State.OOMKilled}} Error={{.State.Error}} StartedAt={{.State.StartedAt}} FinishedAt={{.State.FinishedAt}}' finbook-ci || true";
        if (!o.includes(one)) throw new Error(ANCHOR_MSG);
        const three = [
          "          docker inspect -f 'Running={{.State.Running}}  Status={{.State.Status}}",
          "  ExitCode={{.State.ExitCode}}  OOMKilled={{.State.OOMKilled}}",
          "  Error={{.State.Error}}  StartedAt={{.State.StartedAt}}  FinishedAt={{.State.FinishedAt}}' finbook-ci || true",
        ].join(nl);
        fs.writeFileSync(p2, o.replace(one, three));
        return () => fs.writeFileSync(p2, o);
      },
    },
    {
      // 用了没 import 的 helper：node --check 是纯语法检查，抓不到；
      // 而 E2E 是整套验证里最慢的一环，这种错要等十几分钟才现形。
      name: "spec 用了没 import 的 helper（ReferenceError 藏到 E2E 才现形）",
      apply: () => {
        const p2 = "e2e/tests/voucher-amend.spec.js";
        const o = fs.readFileSync(p2, "utf8");
        fs.writeFileSync(p2, o.replace('const { newBook, auditSignPost } = require("../helpers");', 'const { newBook } = require("../helpers");'));
        return () => fs.writeFileSync(p2, o);
      },
    },
    {
      // 改密用错 → 改完口令，别的设备上的旧会话照样能用。
      name: "改密改成只清本设备会话（别的设备不被踢）",
      apply: () => {
        const p2 = "crates/finweb/src/handlers.rs";
        const o = fs.readFileSync(p2, "utf8");
        fs.writeFileSync(p2, o.replace("state.sessions.remove_others(&user.username, &user.token);", "state.sessions.remove_by_username(&user.username);"));
        return () => fs.writeFileSync(p2, o);
      },
    },
    {
      // 重置设备绑定时不全会话 → 旧设备上的会话还能用到自然过期，等于没重置。
      name: "重置设备绑定时不清该用户会话（旧设备不被踢）",
      apply: () => {
        const p2 = "crates/finweb/src/handlers.rs";
        const o = fs.readFileSync(p2, "utf8");
        const nl = o.includes("\r\n") ? "\r\n" : "\n";
        fs.writeFileSync(p2, o.split("    state.sessions.remove_by_username(&username);").join("    let _ = &username;"));
        void nl;
        return () => fs.writeFileSync(p2, o);
      },
    },
    {
      // 这个文件**不在**我手写的两条夹具检查里（那两条只管 api.rs 与 lib.rs），
      // 只能被通用的「扫每一个 BookOptions 字面量」检查拦下。
      // 探针就是用来证明这一点的 —— 按名字列举的写法会在这里漏掉。
      name: "core_loop.rs 的夹具退回继承默认值（通用检查必须拦下）",
      apply: () => {
        const p2 = "crates/findb/tests/core_loop.rs";
        const o = fs.readFileSync(p2, "utf8");
        const nl = o.includes("\r\n") ? "\r\n" : "\n";
        fs.writeFileSync(p2, o.split("        require_cashier: false," + nl).join(""));
        return () => fs.writeFileSync(p2, o);
      },
    },
    {
      // 把「文件找不到」当成通过 —— 这是检查自己失效却报喜的形态。
      // 探针必须覆盖它，否则路径算错时整套卫生检查会静默变成空转。
      name: "定位不到仓库根时必须报错而不是静默通过",
      apply: () => {
        const p2 = "tools/check-ci.js";
        const o = fs.readFileSync(p2, "utf8");
        fs.writeFileSync(p2, o.replace('if (fs.existsSync(path.join(c, "crates", "finweb", "tests", "api.rs"))) return c;', "if (false) return c;"));
        return () => fs.writeFileSync(p2, o);
      },
    },
  ];
  let leaked = 0;
  for (const pr of probes) {
    let restore;
    try {
      restore = pr.apply();
    } catch (e) {
      leaked++;
      console.log(`!! 探针本身失效：${pr.name} —— ${e.message}`);
      continue;
    }
    let out = "";
    let rc = 0;
    try {
      out = execFileSync(process.execPath, [__filename], {
        encoding: "utf8",
        stdio: ["ignore", "pipe", "pipe"],
      });
    } catch (e) {
      out = (e.stdout || "") + (e.stderr || "");
      rc = e.status || 1;
    }
    restore();
    const caught = rc !== 0 && /FAIL/.test(out);
    if (!caught) leaked++;
    console.log(`${caught ? "PASS 拦下了  " : "FAIL 放过了  "}${pr.name}`);
  }
  console.log(
    leaked === 0
      ? `\n全部 ${probes.length} 个自检探针都生效且都被拦下，文件已还原`
      : `\n${leaked} 个探针没生效或没被拦下（共 ${probes.length} 个）`
  );
  process.exit(leaked === 0 ? 0 : 1);
}

console.log(bad === 0 ? "OK: CI 配置 + 变异清单自检通过" : `FAIL: ${bad} 个问题`);
process.exit(bad === 0 ? 0 : 1);
