#!/usr/bin/env node
// D 金额 / 账号约定可执行门禁。
//
// 两条核心约定（本脚本存在的理由）：
//   1. 金额一律存 TEXT，在 Rust 侧用 Money(pub Decimal) 累加。
//      禁止在 SQL 里对金额列直接 SUM / CAST 求和：SQLite 对 TEXT 列 SUM 返回
//      Integer/Real，`get::<String>` 会 InvalidColumnType，或出现分位精度漂移。
//      历史教训：v1.8.113 修过 `SELECT COALESCE(SUM(qty),'0')` + `get::<String>` 的坑。
//      注意：qty 是数量不是金额，合法 SUM(qty) 不在此列（本脚本故意不查 qty）。
//   2. 科目代码（account code）是字符串（如 "112201"），不是整数：
//      禁止把 code / account 相关字段声明成 i32/i64/u32/u64。
//      account_id: i64 是数据库主键，不算违规。
//   3. 金额禁止用 f64/f32 存储或运算（Money 用 Decimal）。
//
// 只扫 crates/{findb,finweb,fincore}/src/**/*.rs，不含 tests/、benches/ 目录。
// 注释行（//、///、//!）按行跳过 —— 文档里讲「绝不写 SUM(...)」不算违规。
//
// 判据一律看退出码：有问题 exit(1)，干净 exit(0)。
const fs = require("fs");
const path = require("path");

const ROOT = path.join(__dirname, "..");
const SCAN_DIRS = ["crates/findb/src", "crates/finweb/src", "crates/fincore/src"];
const SKIP_DIRS = new Set(["tests", "benches"]);

// 金额列名：qty 明确不在其中（数量列 SUM 是合法的）。
const RE_SQL_SUM = /\b(?:SUM|COALESCE\s*\(\s*SUM)\s*\(/g;
const RE_MONEY_COL = /\b(?:amount|tax_amount|total_amount|balance|debit|credit|price|cost)\b/i;
// 浮点与金额同现（同行或相邻 2 行）。
const RE_FLOAT = /\b(?:f64|f32)\b/g;
const RE_FLOAT_CTX = /\b(?:amount|money|price|balance|tax|cost)\b/i;
// 科目代码被声明为整数（let / 结构体字段 / 函数参数统一命中）。
const RE_INT_CODE = /(\w*(?:code|account)\w*)\s*:\s*(i32|i64|u32|u64)\b/g;

// 已知合法例外。锚定到**代码文字**而不是行号：文件改了行号会漂，代码改了
// 锚点找不到时会明确报「白名单过期」，不会静默放行。
const EXCEPTIONS = [
  {
    rule: "sql-sum",
    file: "crates/findb/src/workbench.rs",
    anchor: "SUM(CAST(e.debit AS REAL)) as sum_debit",
    why: "工作台趋势图专用：SQL 里聚合后 get::<f64>（仅画图，不参与账务），不是 get::<String>",
  },
  {
    rule: "sql-sum",
    file: "crates/findb/src/workbench.rs",
    anchor: "SUM(CAST(e.credit AS REAL)) as sum_credit",
    why: "工作台趋势图专用：SQL 里聚合后 get::<f64>（仅画图，不参与账务），不是 get::<String>",
  },
  {
    rule: "float-money",
    file: "crates/findb/src/manufacturing.rs",
    anchor: "pub variance_pct: f64,",
    why: "差异率（百分比），不是金额本身；同结构体的金额字段都是 Money",
  },
  {
    rule: "float-money",
    file: "crates/findb/src/procurement.rs",
    anchor: "let v: f64 = st.query_row([po_id], |r| r.get(0))?;",
    why: "qty 是数量不是金额；读出的 f64 立即转成 Money::parse_or_zero 再参与业务",
  },
  {
    rule: "float-money",
    file: "crates/findb/src/workbench.rs",
    anchor: "let mut cost_m: BTreeMap<i32, f64> = BTreeMap::new();",
    why: "工作台趋势图专用的按期间累加容器（仅画图）；紧邻的 SQL 读的是 pc.amount 文本，",
  },
  {
    rule: "float-money",
    file: "crates/findb/src/workbench.rs",
    anchor: "pub fn money_f64(m: Money) -> f64 {",
    context: "m.inner().to_string().parse::<f64>()",
    why: "money_f64() 整个函数：Money → f64 只给趋势图打点用，账务计算全程 Decimal",
  },
];

const problems = [];
const report = (rel, line, why) => problems.push({ rel, line, why });

function walk(dir, out = []) {
  if (!fs.existsSync(dir)) return out;
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) {
      if (SKIP_DIRS.has(e.name)) continue;
      walk(p, out);
    } else if (e.name.endsWith(".rs")) out.push(p);
  }
  return out;
}

// 把注释行替换成等长空格：保留偏移量（行号才能算对），同时让注释里的
// 「绝不写 SUM(CAST(x AS REAL))」之类的反面教材不参与匹配，也不给关键词
// 窗口供词。///、//! 都以 // 开头，一并覆盖。
function maskComments(lines) {
  return lines.map((l) => (/^\s*\/\//.test(l) ? " ".repeat(l.length) : l));
}

function lineAt(src, offset) {
  let n = 1;
  for (let i = 0; i < offset; i++) if (src[i] === "\n") n++;
  return n;
}

// `#[cfg(test)] mod tests { ... }` 的整段范围。测试里为了断言故意用 f64 算
// 近似值、故意写 SUM(debit) 造数，属于用户点名的合法例外；整段跳过比逐行
// 白名单更稳（以后测试里再加几行也不会冒出来）。
function testRanges(lines) {
  const ranges = [];
  for (let i = 0; i < lines.length; i++) {
    if (!/^\s*#\[cfg\(test\)\]\s*$/.test(lines[i])) continue;
    let j = i + 1;
    while (j < lines.length && j <= i + 5 && !/^\s*(?:pub\s+)?mod\s+\w*\s*\{/.test(lines[j])) j++;
    if (j >= lines.length || j > i + 5) continue;
    let depth = 0;
    let sawOpen = false;
    let end = j;
    for (; end < lines.length; end++) {
      for (const ch of lines[end]) {
        if (ch === "{") { depth++; sawOpen = true; }
        else if (ch === "}") depth--;
      }
      if (sawOpen && depth <= 0) break;
    }
    ranges.push([i + 1, end + 1]);
    i = end;
  }
  return ranges;
}

function exempt(rule, rel, lineNo, lines) {
  return EXCEPTIONS.some((x) => {
    if (x.rule !== rule || x.file !== rel) return false;
    const anchorIdx = lines.findIndex((l) => l.includes(x.anchor));
    if (anchorIdx < 0) return false;
    if (!x.context) return lines[lineNo - 1] !== undefined && lines[lineNo - 1].includes(x.anchor);
    const near = lines.slice(Math.max(0, anchorIdx - 3), anchorIdx + 4);
    return (
      Math.abs(lineNo - 1 - anchorIdx) <= 3 &&
      near.some((l) => l.includes(x.context))
    );
  });
}

const files = SCAN_DIRS.flatMap((d) => walk(path.join(ROOT, d)));
if (files.length === 0) {
  console.log("FAIL 一个 Rust 源文件都没扫到 —— 判据失效，必须报错而不是「通过」");
  process.exit(1);
}

for (const f of files) {
  const rel = path.relative(ROOT, f).replace(/\\/g, "/");
  const src = fs.readFileSync(f, "utf8");
  const lines = src.split(/\r?\n/);
  const code = maskComments(lines);          // 注释行已置空
  const masked = code.join("\n");            // 保留偏移，用于跨行 SQL 扫描
  const tests = testRanges(lines);
  const inTest = (n) => tests.some(([a, b]) => n >= a && n <= b);

  // --- 1. SQL 里对金额列直接 SUM / CAST 求和 -------------------------------
  RE_SQL_SUM.lastIndex = 0;
  let m;
  while ((m = RE_SQL_SUM.exec(masked)) !== null) {
    const ln = lineAt(masked, m.index);
    if (inTest(ln)) continue;
    const ctx = masked.slice(
      Math.max(0, m.index - 200),
      Math.min(masked.length, m.index + 200)
    );
    if (!RE_MONEY_COL.test(ctx)) continue;
    if (exempt("sql-sum", rel, ln, lines)) continue;
    report(
      rel,
      ln,
      "SQL 对金额列直接 SUM/CAST 求和（上下文含金额列）—— 金额必须在 Rust 侧用 Money(Decimal) 累加；" +
        "v1.8.113 的 COALESCE(SUM(qty)) + get::<String> 就是这么炸的（InvalidColumnType / 精度漂移）"
    );
  }

  // --- 2. 金额用 f64/f32 存储或运算 ---------------------------------------
  code.forEach((line, i) => {
    const ln = i + 1;
    if (inTest(ln)) return;
    RE_FLOAT.lastIndex = 0;
    if (!RE_FLOAT.test(line)) return;
    const ctx = code.slice(Math.max(0, i - 2), i + 3).join("\n");
    if (!RE_FLOAT_CTX.test(ctx)) return;
    if (exempt("float-money", rel, ln, lines)) return;
    report(
      rel,
      ln,
      "金额相关字段用 f64/f32（同行或相邻 2 行含金额关键词）—— 必须用 Money(Decimal) 存储与运算"
    );
  });

  // --- 3. 科目代码声明成整数 -----------------------------------------------
  code.forEach((line, i) => {
    const ln = i + 1;
    if (inTest(ln)) return;
    RE_INT_CODE.lastIndex = 0;
    let mm;
    while ((mm = RE_INT_CODE.exec(line)) !== null) {
      const name = mm[1];
      // 明显不是科目代码：数据库主键 / 计数 / 布尔 / 期间 / 状态 / 复数。
      if (/_id$/.test(name)) continue;
      if (/^(?:has|is|num|count)_|_count$|_num$|_period$|^period|status|s$/.test(name)) continue;
      report(
        rel,
        ln,
        `科目代码 ${name} 声明为 ${mm[2]} —— 科目代码是字符串（如 "112201"），不是整数`
      );
    }
  });
}

// 白名单锚点必须仍能在对应文件里找到，否则明确报错：一条会悄悄过期的白名单
// 等于没有门禁。
for (const x of EXCEPTIONS) {
  const full = path.join(ROOT, x.file);
  if (!fs.existsSync(full) || !fs.readFileSync(full, "utf8").includes(x.anchor)) {
    problems.push({
      rel: x.file,
      line: 0,
      why: `白名单锚点已失效：「${x.anchor}」找不到（${x.why}）—— 请复核后更新或删除该例外`,
    });
  }
}

if (problems.length === 0) {
  console.log(`OK: 扫描 ${files.length} 个 Rust 源文件，金额/账号约定无违规`);
  process.exit(0);
}
for (const p of problems) {
  console.log(`FAIL ${p.rel}:${p.line}: ${p.why}`);
}
console.log(`FAIL: ${problems.length} 处金额/账号约定违规`);
process.exit(1);
