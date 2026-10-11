// 把「文档里的代码引用不许漂移」变成常驻检查。
//
// 为什么值得：设计文档里写了 `scm.rs:1160`、`sales.rs:757` 这类引用，
// 半年后没人知道它们还指不指对的地方。文档里一个过期的行号，
// 比没有引用更坏 —— 它会让人**自信地**去改错的地方。
//
// 判据必须精确到「那一行」，不能容忍 ±3：容忍漂移等于没有校验力
// （我第一版就给了 ±3 容差，结果把注释里提到的函数名当成了真实调用）。
const fs = require("fs");
const path = require("path");

const ROOT = path.join(__dirname, "..");
const DOC = "docs/客户管理模块设计.md";

let bad = 0;
const fail = (m) => { console.log("FAIL " + m); bad++; };
const ok = (m) => console.log("OK   " + m);

// ---- 1. 文档存在 ----
const docPath = path.join(ROOT, DOC);
if (!fs.existsSync(docPath)) {
  console.log("FAIL 找不到 " + DOC);
  process.exit(1);
}
const doc = fs.readFileSync(docPath, "utf8");

// ---- 2. 每个「文件:行号」引用都要指向期望内容 ----
// 期望内容写死在这里：改了代码这里也要改，这是有意的 ——
// 它逼着改动者确认「文档说的和代码做的是同一件事」。
const REFS = [
  { file: "crates/finweb/src/handlers.rs", line: 7310, re: /^\s+findb::sales::so_payment_add\(/, why: "销售收款同时写 so_payment" },
  { file: "crates/finweb/src/handlers.rs", line: 7311, re: /^\s+let doc_id = findb::receipt::receipt_create\(/, why: "同一笔钱写两张表" },
  { file: "crates/findb/src/scm.rs", line: 1162, re: /credit_check\(db, &customer, period\)/, why: "订单流转时的信用拦截" },
  { file: "crates/findb/src/sales.rs", line: 742, re: /^pub fn customer_credit_limit/, why: "额度从 props_json 读" },
  { file: "crates/findb/src/sales.rs", line: 757, re: /^pub fn credit_check/, why: "占用/额度/是否超限" },
  { file: "crates/findb/src/settle.rs", line: 350, re: /^pub fn open_entries\b/, why: "余额唯一口径" },
  { file: "crates/findb/src/settle.rs", line: 366, re: /^pub fn open_entries_with\b/, why: "含子科目的 SQL" },
  { file: "crates/findb/src/settle.rs", line: 380, re: /account_code LIKE/, why: "传一级科目即含子科目" },
  { file: "crates/findb/src/settle.rs", line: 609, re: /^pub fn aging\b/, why: "账龄桶" },
  { file: "crates/findb/src/settle.rs", line: 601, re: /^pub fn party_kind\b/, why: "往来维度由科目首位定（写死 customer 会让应付账龄空白）" },
  { file: "crates/findb/src/auxs.rs", line: 188, re: /^pub fn check_parent\b/, why: "档案分级环校验" },
];

for (const r of REFS) {
  const p = path.join(ROOT, r.file);
  if (!fs.existsSync(p)) {
    // 「被检查的文件找不到」必须**报错**，不能崩、也不能跳过。
    // 崩 = 栈信息淹没真正的错误；跳过 = 变成永真通过。
    fail(`${r.file} 不存在 —— 这条引用无法校验（守门人自己坏了）`);
    continue;
  }
  const s = fs.readFileSync(p, "utf8").split(/\r?\n/);
  const line = s[r.line - 1] || "";
  const short = r.file.split("/").pop();
  if (r.re.test(line)) {
    ok(`${short}:${r.line}  ${r.why}`);
  } else {
    fail(`${short}:${r.line} 不再匹配 ${r.re}（${r.why}）`);
    console.log("       那一行现在是：" + line.trim().slice(0, 78));
    // 指出真实位置，让改动者知道该更新文档还是该修代码
    s.forEach((l, i) => { if (r.re.test(l)) console.log(`       → 真实位置在第 ${i + 1} 行`); });
  }
}

// ---- 3. 文档里出现的 `文件.rs:数字` 必须在上面这张表里 ----
// 理由：文档可以新增引用，但新增时要同时在这里登记，
// 否则「文档提到但没人校验的引用」会越积越多。
const docRefs = [...doc.matchAll(/([a-z_]+\.rs):(\d+)/g)].map((m) => ({
  file: m[1], line: parseInt(m[2], 10), ctx: doc.slice(Math.max(0, m.index - 60), m.index + 40).replace(/\s+/g, " "),
}));
for (const d of docRefs) {
  const known = REFS.some((r) => r.file.endsWith(d.file) && r.line === d.line);
  if (!known) {
    fail(`文档里的 ${d.file}:${d.line} 没有登记到本检查里 —— 登记后才能被校验`);
    console.log("       上下文：" + d.ctx);
  }
}

// ---- 4. 文档必须写明「不新增主数据」与「不做信用管理」 ----
// 这两条是你给的约束，也是最容易被后来者「顺手加一下」的地方。
for (const must of ["不新增第二份客户主数据", "不新增信用管理", "不做信用管理", "不做项目管理", "不做移动端"]) {
  if (doc.indexOf(must) < 0) fail(`文档缺约束声明：「${must}」`);
  else ok(`约束已写明：${must}`);
}

if (bad === 0) {
  console.log("\nOK: 客户管理模块设计文档的引用全部有效");
}
process.exit(bad === 0 ? 0 : 1);
