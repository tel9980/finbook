// 证明 check-customers-balance 真的会拦「自己写 SQL 算余额」与「自己分账龄桶」。
//
// 余额侧三种破例都要被拦：
//   ① 直接查 voucher_entry
//   ② 用 SUM( ... ) 聚合
//   ③ 完全不用 open_entries（换成别的方式）—— 只靠前两条守不住
//
// 账龄侧两种（桶定义同样只有一处，`settle::aging`）：
//   ④ 完全不调 settle::aging
//   ⑤ 自己写桶天数字面量（"0-30天"）—— ④ 只看「有没有调那个函数」，
//      调了但又自己另分一套档的形状要靠 ⑤ 拦
const fs = require("fs");
const path = require("path");
const { execFileSync } = require("child_process");

const ROOT = path.join(__dirname, "..");
const F = path.join(ROOT, "crates", "findb", "src", "customers.rs");
const C = path.join(ROOT, "tools", "check-customers-balance.js");
const orig = fs.readFileSync(F, "utf8");

function run() {
  try {
    const o = execFileSync(process.execPath, [C], { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
    return { ok: true, out: o };
  } catch (e) {
    return { ok: false, out: String((e && e.stdout) || "") + String((e && e.stderr) || "") };
  }
}

console.log("=== 基线 ===");
const b = run();
console.log(b.ok ? "OK   当前实现通过" : "FAIL 基线就红：\n" + b.out);
if (!b.ok) process.exit(1);

// 找一个稳定锚点：open_lines 函数体
const ANCHOR = "    let all = settle::open_entries(db, AR_ACCOUNT, period, false)?;";
if (orig.indexOf(ANCHOR) < 0) { console.log("FAIL 锚点找不到"); process.exit(1); }

// 账龄侧的锚点必须**独立校验**。第一版我打算复用上面那个 `orig.replace(ANCHOR, …)`，
// 但账龄代码在 `aging_of` 里、余额锚点在 `open_lines` 里，两处不相干；
// 真复用的话变异会插到 `open_lines` 里去 —— 而 ④ 只看 `aging_of`，于是
// 「变异生效但检查正确地不报红」，输出会变成一个骗人的 PASS。
//
// 这正是之前踩过的「锚点匹配 0 次却 continue」的同款：变异必须落在**它要验证的那段代码**里。
const ANCHOR_AGING = "    let all = settle::aging(db, AR_ACCOUNT, period, as_of, buckets)?;";
if (orig.indexOf(ANCHOR_AGING) < 0) {
  console.log("FAIL 账龄锚点找不到 —— 探针会变成空变异（空变异 = 永真）");
  process.exit(1);
}

const MUTS = [
  [
    "① 自己查 voucher_entry",
    `    let mut all = db.conn().prepare(
        "SELECT aux_key, SUM(debit) - SUM(credit) FROM voucher_entry WHERE account_code LIKE '1122%'"
    )?;
    let _ = all;`,
  ],
  [
    "② 用 SQL 聚合算余额",
    `    let _sum: String = db.conn().query_row(
        "SELECT SUM(debit) - SUM(credit) FROM voucher_entry WHERE account_code='1122'",
        [],
        |r| r.get(0),
    )?;`,
  ],
  [
    "③ 完全不用 open_entries（自己写个函数名）",
    `    let all = my_own_balance_query(db, AR_ACCOUNT, period)?;`,
  ],
  [
    "④ 完全不调 settle::aging（自己分档）",
    `    let all = my_own_aging(db, AR_ACCOUNT, period, as_of, buckets)?;`,
    ANCHOR_AGING,
  ],
  [
    "⑤ 调了 settle::aging 但自己又写了一组桶天数字面量",
    `    let all = settle::aging(db, AR_ACCOUNT, period, as_of, buckets)?;
    let _my_own_buckets = ["0-30天", "31-60天", "61-90天"];`,
    ANCHOR_AGING,
  ],
];

let allGood = true;
for (const [name, injected, anchor] of MUTS) {
  const a = anchor || ANCHOR;
  const mutated = orig.replace(a, injected);
  if (mutated === orig) { console.log("FAIL 变异没写进去：" + name); allGood = false; continue; }
  fs.writeFileSync(F, mutated, "utf8");
  const r = run();
  const bad = !r.ok;
  console.log(
    (bad ? "PASS 拦下  " : "FAIL 漏过  ") + name + "  → 检查" + (bad ? "报红" : "仍然通过")
  );
  if (!bad) console.log("       " + r.out.trim().slice(0, 160));
  fs.writeFileSync(F, orig, "utf8");
  if (!bad) allGood = false;
}

const after = run();
console.log("\n还原后基线：" + (after.ok ? "OK（绿）" : "FAIL（仍红）"));
const pass = allGood && after.ok;
console.log(pass ? `\n全部 ${MUTS.length} 个变异都被拦下，文件已还原` : "\n有变异漏过");
process.exit(pass ? 0 : 1);
