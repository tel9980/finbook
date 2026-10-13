// 给 check-design-doc.js 加自检：证明它真的会拦，而不是永真通过。
//
// 「检查通过」不等于「检查有用」。四个探针对应四种失效：
//   1. 锚点内容被改坏（调用被换掉）—— 证明窗口不是「随便一行都算过」
//   2. 锚点被挪出容差窗口（上面插 30 行）—— 证明容差有界，不是无限大
//   3. 文档删掉约束声明（「不做信用管理」这行被删了）
//   4. 文档引用了一个没登记的位置（等于「不受校验的引用」）
//
// 外加一条**反向**断言：锚点只漂 3 行时检查应当仍然通过。
// 这条必须有 —— 否则探针 1、2 变红可能只是因为检查对任何改动都报错，
// 那是噪声不是校验。两条一起才说明「它在该红的时候红，该绿的时候绿」。
const fs = require("fs");
const path = require("path");
const { execFileSync } = require("child_process");

// 仓库根 = tools/ 的上一级。**不能写死绝对路径** —— 同一份脚本要能在
// 任何机器上跑（本地 Windows、CI 的 Linux）。第一版写死了本机路径，
// 结果 Test job 在 Linux 上 ENOENT 直接崩。
const ROOT = path.join(__dirname, "..");
const CHECK = path.join(ROOT, "tools", "check-design-doc.js");
const DOC = path.join(ROOT, "docs", "客户管理模块设计.md");
const SCM = path.join(ROOT, "crates", "findb", "src", "scm.rs");

const docOrig = fs.readFileSync(DOC, "utf8");
const scmOrig = fs.readFileSync(SCM, "utf8");

function run() {
  try {
    execFileSync(process.execPath, [CHECK], { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
    return { ok: true, out: "" };
  } catch (e) {
    return { ok: false, out: String((e && e.stdout) || "") + String((e && e.stderr) || "") };
  }
}

function probe(name, apply, restore, mustSee) {
  apply();
  const r = run();
  restore();
  const good = !r.ok && mustSee.test(r.out);
  console.log(
    (good ? "PASS 拦下  " : "FAIL 漏过  ") + name +
    (good ? "" : "  （退出=" + (r.ok ? "0" : "非0") + "，输出里找不到 " + mustSee + "）")
  );
  if (!good && r.out) console.log("       " + r.out.split(/\r?\n/).filter((l) => /FAIL/.test(l)).slice(0, 3).join("\n       "));
  return good;
}

let all = true;

// 先确认基线是绿的（否则下面的探针没有意义 —— 基线红着的话「拦下」是假的）
const base = run();
console.log((base.ok ? "OK   基线通过" : "FAIL 基线就不通过，下面探针无意义"));
if (!base.ok) { console.log(base.out); process.exit(1); }

// 1. 内容被改坏：把信用检查那一行的真实调用改掉
//
// 判据用的是「锚点行 ±TOLERANCE 窗口」，所以**插一行是不该变红的** ——
// 那正是不容差窗口存在的理由（见 check-design-doc.js 里的说明）。
// 这里必须破坏的是**内容**：把 `credit_check(db, &customer, period)` 换成别的调用，
// 证明窗口不是「随便一行都算过」。
//
// 锚点**动态定位**，不写死。写死会有两个问题：
//   ① 代码一演进（哪怕只是前面加了一行注释）这个探针就失效 —— 它会去找
//      早已不存在的行号，于是「探针没生效」和「探针生效了」都看不出来；
//   ② 上一版还把写回的行尾写成 "\r\n"，等于每跑一次就把整个 scm.rs 转成 CRLF。
//      现在按原样 split/join，行尾跟着文件走。
const ANCHOR = /credit_check\(db, &customer, period\)/;
const lines = scmOrig.split(/\r?\n/);
const anchorAt = lines.findIndex((l) => ANCHOR.test(l));
if (anchorAt < 0) {
  console.log("FAIL 锚点没找到：scm.rs 里已无 credit_check(db, &customer, period)");
  process.exit(1);
}
const eol = scmOrig.indexOf("\r\n") >= 0 ? "\r\n" : "\n";
// 行尾跟着原文件走：写死 "\r\n" 会把 LF 仓库整个转成 CRLF，
// 还原时又是另一个 EOL，一次自检就在仓库里留下两份脏文件。
const write = (arr) => fs.writeFileSync(SCM, arr.join(eol), "utf8");

all &= probe(
  "内容被改坏：scm.rs 的信用检查调用被换掉，锚点窗口内找不到",
  () => {
    const s = scmOrig.split(/\r?\n/);
    s[anchorAt] = s[anchorAt].replace(ANCHOR, "credit_check_OLD(db, &customer, period)");
    write(s);
  },
  () => fs.writeFileSync(SCM, scmOrig, "utf8"),
  // 只认「不再匹配」这个信号，不认具体行号：行号本来就可能随代码演进变化
  /scm\.rs:\d+ 不再匹配/
);

// 1b. 容差有界：把锚点**整体挪出**窗口（上面插 30 行），必须变红。
//
// 这是 TOLERANCE 唯一的刹车：证明容差不是无限大的。
// 只插 30 行是为了压过 check-design-doc.js 里的 TOLERANCE=20，
// 数值故意不同步 —— 一旦有人调大容差，这里应该立刻失败，
// 那才是「容差被改大了」该有的反应。
all &= probe(
  "容差有界：scm.rs 锚点被推到容差窗口外，必须报红",
  () => {
    const s = scmOrig.split(/\r?\n/);
    const pad = [];
    for (let i = 0; i < 30; i++) pad.push("// 漂移探针：把锚点顶出容差窗口");
    s.splice(anchorAt, 0, ...pad);
    write(s);
  },
  () => fs.writeFileSync(SCM, scmOrig, "utf8"),
  /scm\.rs:\d+ 不再匹配/
);

// 1c. 小幅漂移**不该**变红（这条不是 probe，是断言）
// 反向验证容差有用：只插 3 行，检查必须仍然通过。
{
  const s = scmOrig.split(/\r?\n/);
  s.splice(anchorAt, 0, "// 小幅漂移探针", "// 第二行", "// 第三行");
  write(s);
  const r = run();
  fs.writeFileSync(SCM, scmOrig, "utf8");
  const good = r.ok;
  console.log(
    (good ? "PASS 容差有用  " : "FAIL 容差太紧  ") +
    "锚点只下移 3 行，检查应当仍然通过" +
    (good ? "" : "（容差 20 行连 3 行都容不下，说明窗口根本没生效）")
  );
  if (!good) console.log("       " + r.out.split(/\r?\n/).filter((l) => /FAIL/.test(l)).slice(0, 3).join("\n       "));
  all &= good ? 1 : 0;
}

// 2. 约束声明被删
all &= probe(
  "约束被删：文档里去掉「不做信用管理」",
  () => fs.writeFileSync(DOC, docOrig.replace(/不做信用管理/g, "信用管理"), "utf8"),
  () => fs.writeFileSync(DOC, docOrig, "utf8"),
  /文档缺约束声明/
);

// 3. 未登记的引用
all &= probe(
  "未登记引用：文档里加一个 scm.rs:99999",
  () => fs.writeFileSync(DOC, docOrig + "\n\n见 `scm.rs:99999`。\n", "utf8"),
  () => fs.writeFileSync(DOC, docOrig, "utf8"),
  /没有登记到本检查里/
);

// 还原确认
const after = run();
console.log("\n还原后基线：" + (after.ok ? "OK（绿）" : "FAIL（仍红，探针自己破坏了文件）"));
if (!after.ok) console.log(after.out);

const pass = all && after.ok;
console.log(pass ? "\n全部 4 个探针都生效且都被拦下，文件已还原" : "\n有探针没生效");
process.exit(pass ? 0 : 1);
