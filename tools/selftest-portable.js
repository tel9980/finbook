// 用这一轮**真实犯过的错**验证 check-portable：
// 它上一版就含 `C:\\Users\\Administrator\\Documents\\Default Project\\finbook`，
// 本地跑完全正常，只有 CI 的 Linux 上才崩。检查必须能抓到它。
//
// ── 为什么改成「临时文件 + --file」而不是原地改 tools/*.js ──
//
// 第一版的做法：把违规样本写进 tools/selftest-design-doc.js（改完再还原）。
// 两个后果，都是真实踩到的：
//
//   ① check-ci 先跑可移植性检查（扫全部 .js）、后跑设计文档自检。
//      顺序一变，全量扫描就撞上探针留下的中间态，日志里冒出
//         FAIL tools/selftest-design-doc.js:19 盘符绝对路径
//      ——看着像「基线本身不干净」，实际只是探针的中间态被拍到了。
//   ② 任一步推进程崩掉（断言失败 / 被打断），还原就被跳过，
//      仓库里留下一份脏源码。本地还看不出来，下次全量扫描才红。
//
// 现在：违规样本写到系统临时目录里的 .js，用 `check-portable --file` 单点问
// 「这条规则会不会拦」。仓库里的文件从头到尾一个字节都不动。
const fs = require("fs");
const path = require("path");
const os = require("os");
const { execFileSync } = require("child_process");

const ROOT = path.join(__dirname, "..");
const C = path.join(ROOT, "tools", "check-portable.js");

const TMPDIR = fs.mkdtempSync(path.join(os.tmpdir(), "finbook-portable-"));

let missed = false;

function runOn(file) {
  try {
    const o = execFileSync(process.execPath, [C, "--file", file], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });
    return { ok: true, out: o };
  } catch (e) {
    return { ok: false, out: String((e && e.stdout) || "") + String((e && e.stderr) || "") };
  }
}

/** 一个探针：样本源码 + 期望是否被拦下 */
function probe(name, src, mustBlock) {
  const f = path.join(TMPDIR, `probe-${name.replace(/[^\w]/g, "_")}.js`);
  fs.writeFileSync(f, src, "utf8");
  const r = runOn(f);
  const shown = r.out
    .split(/\r?\n/)
    .filter((l) => /FAIL|修法/.test(l))
    .map((l) => "  " + l)
    .join("\n");
  if (mustBlock && r.ok) {
    console.log(`FAIL ${name} 未被拦下 —— check-portable 存在漏网规则`);
    missed = true;
  } else if (!mustBlock && !r.ok) {
    console.log(`FAIL ${name} 被误拦 —— 检查过严，会逼人绕过它`);
    missed = true;
  } else {
    console.log(`PASS ${mustBlock ? "拦下" : "放行"}  ${name}`);
  }
  if (shown) console.log(shown);
}

/** 当前探针在起作用？给「该拦」探针用 */
function main() {
  // ── 基线：合规样本必须放行 ──
  console.log("=== 基线（合规样本，应放行）===");
  probe(
    "合规样本",
    'const path = require("path");\nconst ROOT = path.join(__dirname, "..");\nmodule.exports = ROOT;\n',
    false
  );

  // ── 变异 1：ROOT 写回硬编码用户目录（第一次真实发生的错）──
  console.log("\n=== 变异 1：ROOT 写死 C:\\Users\\... ===");
  probe(
    "用户目录",
    'const ROOT = "C:\\\\Users\\\\Administrator\\\\Documents\\\\Default Project\\\\finbook";\n',
    true
  );

  // ── 变异 2：盘符绝对路径（不带 Users）──
  // 这条正是当初把 selftest-design-doc.js 判红的那一行，必须能拦住。
  console.log("\n=== 变异 2：盘符绝对路径 D:\\tmp ===");
  probe("盘符路径", 'const ROOT = "D:\\\\tmp\\\\finbook";\n', true);

  // ── 变异 3：写死 CRLF（规则只认 .join/.split("\r\n") 这种行尾处理写法，
  //     普通字符串里出现 \r\n 不拦 —— 那是正当用法）──
  console.log("\n=== 变异 3：写死 CRLF（join） ===");
  probe("写死CRLF", 'const s = arr.join("\\r\\n");\n', true);

  // ── 变异 4：用户目录的 Unix 写法 /Users/<名> ──
  console.log("\n=== 变异 4：/Users/<名> 目录 ===");
  probe("Users目录", 'const f = "/Users/alice/project/x";\n', true);

  // ── 反向断言 1：注释里提到绝对路径不算违规（文档该说清为什么）──
  // 没有这条，check-portable 会把「解释这条规则」的注释也判红，
  // 逼着作者把说明删掉 —— 那是把检查做成敌人。
  console.log("\n=== 反向断言：注释里的路径不算违规（应放行）===");
  probe("注释含路径", '// 不能写 C:\\\\Users\\\\xxx，要用 path.join\nconst ROOT = 1;\n', false);

  // ── 反向断言 2：普通字符串里的相对路径不算违规 ──
  console.log("\n=== 反向断言：相对路径（应放行）===");
  probe("相对路径", 'const f = "crates/findb/src/periods.rs";\n', false);
}

try {
  main();
} finally {
  // 临时文件在系统 temp 下，清不掉也不污染仓库
  try {
    for (const f of fs.readdirSync(TMPDIR)) fs.unlinkSync(path.join(TMPDIR, f));
    fs.rmdirSync(TMPDIR);
  } catch (_) {
    /* 清不掉就算了 */
  }
}

if (missed) {
  console.log("\n有探针未生效 —— check-portable 存在漏网或过严");
  process.exit(1);
}
console.log("\n全部探针生效：该拦的拦下，该放的放行");
