// 用这一轮**真实犯过的错**验证 check-portable：
// 它上一版就含 `C:\\Users\\Administrator\\Documents\\Default Project\\finbook`，
// 本地跑完全正常，只有 CI 的 Linux 上才崩。检查必须能抓到它。
// 注意：自检脚本自己也在这两个目录里，所以它**不许**含绝对路径
// —— 第一版写了 `const ROOT = "C:\\Users\\Administrator\\..."`，
// 结果 check-portable 把**自己**判成不可移植（第一次跑就红）。
// 一个检查脚本把自己也算进去是可接受的（它确实该守自己），
// 但前提是它自己先满足规则。
const fs = require("fs");
const path = require("path");
const { execFileSync } = require("child_process");

const ROOT = path.join(__dirname, "..");
const P = path.join(ROOT, "tools", "selftest-design-doc.js");
const C = path.join(ROOT, "tools", "check-portable.js");
const orig = fs.readFileSync(P, "utf8");

// 变异有没有被拦下。**不能就地 process.exit(1)** —— 那样会跳过下面的还原，
// 把变异过的源码留在磁盘上，下一个人接手时看到的是一个脏工作区，
// 而且脏在哪个文件都不好找（这里改的是别的文件）。
// 所以只记结果，最后统一还原再退出。
let missed = false;

function run() {
  try {
    const o = execFileSync(process.execPath, [C], { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
    return { ok: true, out: o };
  } catch (e) {
    return { ok: false, out: String((e && e.stdout) || "") + String((e && e.stderr) || "") };
  }
}

console.log("=== 基线（已修好）===");
const b = run();
console.log("  退出成功？ " + b.ok + (b.ok ? "（应为 true）" : "（不应红）"));

// 变异 1：把 ROOT 换回硬编码绝对路径（真实发生过的）
const mutated = orig.replace(
  'const ROOT = path.join(__dirname, "..");',
  'const ROOT = "C:\\\\Users\\\\Administrator\\\\Documents\\\\Default Project\\\\finbook";'
);
if (mutated === orig) { console.log("FAIL 变异没写进去（源码形态与预期不符）"); process.exit(1); }

console.log("\n=== 变异 1：ROOT 写回硬编码绝对路径 ===");
fs.writeFileSync(P, mutated, "utf8");
const r1 = run();
console.log("  退出成功？ " + r1.ok + "   ← 必须 false");
console.log(r1.out.split(/\r?\n/).filter((l) => /FAIL|修法/.test(l)).map((l) => "  " + l).join("\n"));

// 变异 2：写死 CRLF（另一个真实形状）
console.log("\n=== 变异 2：写死 CRLF ===");
fs.writeFileSync(P, orig, "utf8");
const m2 = orig.replace(
  'const scmeol = scmOrig.indexOf("\\r\\n") >= 0 ? "\\r\\n" : "\\n";',
  'const scmeol = "\\r\\n";'
);
const hasEol = m2 !== orig;
if (hasEol) {
  fs.writeFileSync(P, m2, "utf8");
  const r2 = run();
  console.log("  退出成功？ " + r2.ok + "   ← 必须 false");
  console.log(r2.out.split(/\r?\n/).filter((l) => /FAIL|修法/.test(l)).map((l) => "  " + l).join("\n"));
  // 同上：这个变异的结论必须参与判定，否则探针形同虚设
  if (r2.ok) {
    console.log("FAIL CRLF 变异未被拦下 —— check-portable 漏了写死行尾");
    missed = true;
  }
} else {
  // 兜底形状：源码里没有 scmeol 那行时（代码演进过），换一个**同样该被拦**的形状。
  //
  // 第一版这里用 `const X = "D:\\tmp"` —— 两个问题：
  //   ① 变异目标写成了一个新的、与被测代码无关的常量，check-portable 报出来的 FAIL
  //      指向 selftest-design-doc.js，读日志的人会以为**基线本身**不干净
  //      （第一版就踩过这个，见 check-ci 的 2c 段注释）；
  //   ② 更要命的是它**没进第 69 行的退出条件**（那里只判了 r1 和 after），
  //      也就是说变异 2 拦没拦住都算通过 —— 一个不被验证的探针等于没有探针。
  // 现在：变异的是 ROOT 本身（这才是 check-portable 真正要守的东西），
  // 并且把结论显式纳入判定。
  console.log("  （当前源码没有 scmeol 那行，改用等效形状：ROOT 写死盘符路径）");
  const m3 = orig.replace(
    'const ROOT = path.join(__dirname, "..");',
    'const ROOT = "D:\\\\tmp\\\\finbook";'
  );
  if (m3 === orig) {
    console.log("FAIL 兜底变异没写进去（源码形态与预期不符）");
    missed = true;
  } else {
    fs.writeFileSync(P, m3, "utf8");
    const r3 = run();
    console.log("  退出成功？ " + r3.ok + "   ← 必须 false");
    console.log(r3.out.split(/\r?\n/).filter((l) => /FAIL|修法/.test(l)).map((l) => "  " + l).join("\n"));
    // 让 r3 的结论参与最终判定（原来漏了，探针形同虚设）
    if (r3.ok) {
      console.log("FAIL 兜底变异未被拦下 —— check-portable 漏了盘符路径");
      missed = true;
    }
  }
}

fs.writeFileSync(P, orig, "utf8");
const after = run();
console.log("\n还原后基线：" + (after.ok ? "OK（绿）" : "FAIL（仍红）"));
if (missed) console.log("有变异未被拦下 —— check-portable 存在漏网规则");
process.exit(!b.ok || r1.ok || missed || !after.ok ? 1 : 0);
