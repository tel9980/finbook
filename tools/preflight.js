// 提交前的「快检」：一分钟内告诉你这次改的东西**有没有低级错误**。
//
// 为什么不直接跑 CI，也不在这跑 cargo test：
//   · CI 一轮 40 分钟、16 个 job，拿它当 lint 是浪费；
//   · 本地 cargo test --workspace 6 分钟，且 CI 里它和其它 15 个 job 并行，
//     本地跑一遍并不比 CI 更快得到结论 —— **编译和测试交给 CI**。
//
// 所以本脚本只做「秒级 / 十秒级」的事，挡的是那些一提交就让整轮 CI 白跑的错：
//   1. 前端静态检查（语法、重复 id、行为约束、nginx 锚点、视图真渲染）
//   2. 仓库卫生（CI 配置可解析、变异清单未过期、全仓编码无损坏）
//   3. 工具脚本可移植性（不许写死本机路径 —— 本地绿、CI 红的第一大来源）
//   4. 设计文档引用未漂移
//   5. cargo fmt --check（Rust 侧唯一能在秒级判定的闸门）
//   6. cargo check --workspace（只查类型，不跑测试；有增量缓存时通常十几秒）
//
// 判据一律看**退出码**，不看输出文字 —— 见 tools/check-js.js 头部的教训。
//
// 用法：
//   node tools/preflight.js            # 全跑
//   node tools/preflight.js --fast     # 跳过 cargo 两项（改文档/前端时用，约 5 秒）
//   node tools/preflight.js --no-fmt   # 跳过 cargo fmt（本机没装 rustfmt 时）
const { spawnSync } = require("child_process");
const path = require("path");

const ROOT = path.join(__dirname, "..");
const fast = process.argv.includes("--fast");
const noFmt = process.argv.includes("--no-fmt");

// 每一项：名字、命令、参数、要不要算进「失败」。
// cargo 两项在 --fast 下整体跳过。
const steps = [
  { name: "前端静态检查", cmd: "node", args: ["tools/check-js.js"] },
  { name: "仓库卫生（CI 配置 / 变异清单 / 编码）", cmd: "node", args: ["tools/check-ci.js"] },
  { name: "工具脚本可移植性", cmd: "node", args: ["tools/check-portable.js"] },
  { name: "设计文档引用", cmd: "node", args: ["tools/check-design-doc.js"] },
  {
    name: "Rust 格式",
    cmd: "cargo",
    args: ["fmt", "--all", "--", "--check"],
    skip: fast || noFmt,
    skipWhy: fast ? "--fast" : "--no-fmt",
    hint: "修法：cargo fmt --all（会直接改文件，改完 git diff 看一眼再提交）",
  },
  {
    name: "Rust 类型检查（不跑测试）",
    cmd: "cargo",
    args: ["check", "--workspace", "--all-targets", "--locked"],
    skip: fast,
    skipWhy: "--fast",
    hint: "编译和测试交给 CI；这里只保证「能过编译」这一件事",
  },
];

const t0 = Date.now();
let failed = [];
let skipped = [];

for (const s of steps) {
  if (s.skip) {
    skipped.push(`${s.name}（${s.skipWhy}）`);
    continue;
  }
  const start = Date.now();
  const r = spawnSync(s.cmd, s.args, {
    cwd: ROOT,
    stdio: "pipe",
    encoding: "utf8",
    // Windows 上 cargo 是 cargo.exe，spawnSync 不带 shell 也能找到，但 npm 风格的
    // .cmd 包装找不到 —— 这里加 shell:true 统一行为。
    shell: process.platform === "win32",
  });
  const ms = Date.now() - start;
  if (r.error) {
    failed.push({ name: s.name, why: r.error.message, ms });
    console.log(`✗ ${s.name}  ${ms}ms  —— 命令跑不起来：${r.error.message}`);
    continue;
  }
  if (r.status === 0) {
    console.log(`✓ ${s.name}  ${ms}ms`);
    continue;
  }
  failed.push({ name: s.name, why: `退出码 ${r.status}`, ms, out: (r.stdout || "") + (r.stderr || "") });
  console.log(`✗ ${s.name}  ${ms}ms  —— 退出码 ${r.status}`);
  if (s.hint) console.log(`   ${s.hint}`);
}

const total = ((Date.now() - t0) / 1000).toFixed(1);
console.log("");

if (failed.length === 0) {
  if (skipped.length) console.log(`跳过：${skipped.join("、")}`);
  console.log(`预检通过（${total}s）。编译、测试、变异、E2E 交给 CI，提交后看结果。`);
  process.exit(0);
}

console.log(`预检失败 ${failed.length} 项（${total}s）—— 先修再提交，否则这一轮 CI 白跑：`);
for (const f of failed) {
  console.log("");
  console.log(`── ${f.name}（${f.why}）`);
  // 只回放最后 40 行：这些脚本的报错都在末尾，全量回放会淹掉结论
  const lines = (f.out || "").trimEnd().split("\n");
  console.log(lines.slice(-40).join("\n"));
}
process.exit(1);
