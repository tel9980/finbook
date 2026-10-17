// 变异验证：四个校验各去掉一个，测试必须变红。
// 「测试全绿」不能证明「校验有用」—— 可能校验根本没执行。
const fs = require("fs");
const { execFileSync } = require("child_process");
const path = require("path");

const ROOT = path.join(__dirname, "..");
const P = path.join(ROOT, "crates", "findb", "src", "auxs.rs");
const orig = fs.readFileSync(P, "utf8");

function run() {
  try {
    execFileSync("cargo", ["test", "-p", "findb", "--lib", "parent_tests"], {
      cwd: ROOT, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"],
      maxBuffer: 32 * 1024 * 1024,
    });
    return { ok: true, out: "" };
  } catch (e) {
    return { ok: false, out: String((e && e.stdout) || "") + String((e && e.stderr) || "") };
  }
}

console.log("=== 基线 ===");
const b = run();
console.log(b.ok ? "OK   5 条测试全过" : "FAIL 基线就红：\n" + b.out.split(/\r?\n/).filter((l) => /panicked|FAILED|error/.test(l)).slice(0, 5).join("\n"));
if (!b.ok) process.exit(1);

// 变异 1：去掉「上级必须存在」这一问
// 注意换行：crates/** 大多是 CRLF，用 \n 拼锚点会匹配 0 次 ——
// 而「匹配 0 次」如果被静默跳过，就等于这个变异没验过（我第一版就是这样）。
const eol = orig.indexOf("\r\n") >= 0 ? "\r\n" : "\n";
const M1 = [
  "    // ① 上级必须存在",
  "    if !map.contains_key(p) {",
].join(eol);
const M1_NEW = [
  "    // ① 上级必须存在（变异：这一问被去掉了）",
  "    if false {",
].join(eol);

// 变异 2：去掉自环判断
const M2 = "    if p == code.trim() {";
const M2_NEW = "    if false {";

// 变异 3：去掉环判断
const M3 = "    if reaches(&map, p, code.trim()) {";
const M3_NEW = "    if false {";

const MUTS = [
  ["① 上级存在性", M1, M1_NEW, /parent_must_exist|parent_tests/],
  ["② 自环", M2, M2_NEW, /parent_cannot_be_self/],
  ["③ 多级环", M3, M3_NEW, /parent_cycle_of_length_two_rejected/],
];

let allGood = true;
for (const [name, from, to, mustFail] of MUTS) {
  if (orig.indexOf(from) < 0) {
    // 锚点找不到 = 这个变异**没验过**。必须当失败，不能 continue 后继续跑。
    // 第一版就是 continue，结果报告「3 个变异里 2 个拦下」而没人发现第三个没跑。
    console.log("FAIL 找不到变异锚点：" + name + "（该变异未验证）");
    allGood = false;
    fs.writeFileSync(P, orig, "utf8");
    continue;
  }
  fs.writeFileSync(P, orig.replace(from, to), "utf8");
  const r = run();
  const failedTests = [...r.out.matchAll(/panicked at[^\n]*/g)].length;
  const okMut = !r.ok;
  console.log(
    (okMut ? "PASS 拦下  " : "FAIL 漏过  ") + "去掉校验 " + name +
    "  → 测试" + (okMut ? "变红" : "仍然全绿（= 这条校验没被任何测试覆盖）")
  );
  fs.writeFileSync(P, orig, "utf8");
  if (!okMut) allGood = false;
}

const after = run();
console.log("\n还原后基线：" + (after.ok ? "OK（绿）" : "FAIL（仍红）"));
const pass = allGood && after.ok;
console.log(pass ? "\n全部 3 个变异都被拦下，文件已还原" : "\n有变异漏过");
process.exit(pass ? 0 : 1);
