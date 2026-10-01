// util.js 纯函数单元测试（node:test，无需框架）
const test = require("node:test");
const assert = require("node:assert");
const { esc, fmt, fmtMoney, ymm } = require("./util.js");

test("esc 转义 HTML 特殊字符", () => {
  assert.strictEqual(esc(null), "");
  assert.strictEqual(esc(undefined), "");
  assert.strictEqual(esc("<a>&\"'"), "&lt;a&gt;&amp;&quot;'");
  assert.strictEqual(esc("普通文本"), "普通文本");
  assert.strictEqual(esc(0), "0");
});

test("fmt 千分位（字符串层，不 parseFloat）", () => {
  assert.strictEqual(fmt("1000000"), "1,000,000");
  assert.strictEqual(fmt("1234.5"), "1,234.5");
  assert.strictEqual(fmt("100"), "100");
  assert.strictEqual(fmt("999"), "999");
  assert.strictEqual(fmt("1000"), "1,000");
  // 负数
  assert.strictEqual(fmt("-1234567.89"), "-1,234,567.89");
  // 空值
  assert.strictEqual(fmt(null), "");
  assert.strictEqual(fmt(""), "");
  // 保留原小数位数（不强制两位，避免数量被误格式化）
  assert.strictEqual(fmt("0.5"), "0.5");
  assert.strictEqual(fmt("1234567890"), "1,234,567,890");
});

test("fmtMoney 强制两位小数 + 千分位", () => {
  assert.strictEqual(fmtMoney("1000000"), "1,000,000.00");
  assert.strictEqual(fmtMoney("1234.5"), "1,234.50");
  assert.strictEqual(fmtMoney("1234.567"), "1,234.56"); // 截断到两位
  assert.strictEqual(fmtMoney("-999.9"), "-999.90");
  assert.strictEqual(fmtMoney(""), "");
  assert.strictEqual(fmtMoney(null), "");
});

test("ymm 期间字符串转整型", () => {
  assert.strictEqual(ymm("2026-01"), 202601);
  assert.strictEqual(ymm("2026-12"), 202612);
  assert.strictEqual(ymm("202601"), 202601); // 无连字符也可（split 无 - 时 m 为 undefined → NaN，此处仅保底）
});

// ===========================================================================
// 通用粘贴解析器测试（期初 + 凭证）
// ===========================================================================
const { parseGrid, normNum, parseBeginText, parseVoucherText } = require("./util.js");

const ACCS = [
  { code: "1001", name: "库存现金", disabled: false },
  { code: "100202", name: "建行一般户", disabled: false },
  { code: "660201", name: "工资", disabled: false },
  { code: "2001", name: "短期借款", disabled: false },
  { code: "9999", name: "", disabled: false },
  { code: "8888", name: "已停用", disabled: true },
];

test("parseGrid 按制表符切分（Excel）", () => {
  assert.deepStrictEqual(parseGrid("a\tb\tc\n1\t2\t3"), [["a","b","c"],["1","2","3"]]);
  // 丢弃空行与 CR
  assert.deepStrictEqual(parseGrid("a\tb\r\n\r\n1\t2\r\n"), [["a","b"],["1","2"]]);
  // 无制表符时按逗号
  assert.deepStrictEqual(parseGrid("a,b,c"), [["a","b","c"]]);
  assert.deepStrictEqual(parseGrid(""), []);
});

test("normNum 金额归一", () => {
  assert.strictEqual(normNum("1,234.56"), 1234.56);
  assert.strictEqual(normNum("¥1000"), 1000);
  assert.strictEqual(normNum(""), null);      // 空 ≠ 0
  assert.strictEqual(normNum("-500"), -500);
  assert.ok(isNaN(normNum("abc")));           // 非法
});

test("parseBeginText 6 列格式（含科目名称）", () => {
  const { rows, bad } = parseBeginText(
    "1001\t库存现金\t借\t10,000\t0\t0\n100202\t建行一般户\t贷\t8,000\t0\t0", ACCS);
  assert.strictEqual(rows.length, 2);
  assert.strictEqual(rows[0].account_code, "1001");
  assert.strictEqual(rows[0].dir, "debit");
  assert.strictEqual(rows[0].yb, "10,000");
  assert.strictEqual(rows[1].dir, "credit");
  assert.strictEqual(bad.length, 0);
});

test("parseBeginText 3 列格式（无名称，向后兼容）", () => {
  const { rows, bad } = parseBeginText("1001\t借\t5000\n2001\t贷\t5000", ACCS);
  assert.strictEqual(rows.length, 2);
  assert.strictEqual(rows[0].dir, "debit");
  assert.strictEqual(rows[0].yb, "5,000");
  assert.strictEqual(rows[1].dir, "credit");
  assert.strictEqual(bad.length, 0);
});

test("parseBeginText 跳过表头行，报问题行带行号", () => {
  const { rows, bad } = parseBeginText(
    "科目编码\t方向\t金额\n9999\t借\t100\n8888\t贷\t100", ACCS);
  assert.strictEqual(rows.length, 2, "停用科目仍进 rows，由前端二次拦截");
  assert.strictEqual(bad.length, 1);
  assert.strictEqual(bad[0].line, 3, "表头是第 1 行；第 2 行 9999 存在；8888 在第 3 行");
  assert.match(bad[0].why, /停用/);
});

test("parseBeginText 金额非法只报该行", () => {
  const { rows, bad } = parseBeginText("1001\t借\tabc\n100202\t贷\t100", ACCS);
  assert.strictEqual(rows.length, 1);
  assert.strictEqual(bad.length, 1);
  assert.match(bad[0].why, /不是数字/);
});

test("parseBeginText 方向空且金额为负 → 视为贷方", () => {
  const { rows } = parseBeginText("1001\t\t-500", ACCS);
  assert.strictEqual(rows[0].dir, "credit");
  assert.strictEqual(rows[0].yb, "500", "金额取绝对值，方向单独表达");
});

test("parseVoucherText 通用 4 列（摘要,科目,借,贷）", () => {
  const { rows, bad, date } = parseVoucherText(
    "差旅费\t660201\t1,000.00\t0\n差旅费\t100202\t0\t1,000.00", ACCS);
  assert.strictEqual(rows.length, 2);
  assert.strictEqual(rows[0].summary, "差旅费");
  assert.strictEqual(rows[0].account_code, "660201");
  assert.strictEqual(rows[0].debit, "1,000.00");
  assert.strictEqual(rows[0].credit, "0.00");
  assert.strictEqual(date, "", "无日期列时留空，沿用编辑器头部");
  assert.strictEqual(bad.length, 0);
});

test("parseVoucherText 金蝶 8 列（日期,凭证字,凭证号,摘要,科目,名称,借,贷）", () => {
  const { rows, date, word } = parseVoucherText(
    "2026-09-01\t记\t1\t差旅\t660201\t工资\t1000\t0\n2026-09-01\t记\t1\t差旅\t100202\t建行一般户\t0\t1000", ACCS);
  assert.strictEqual(rows.length, 2);
  assert.strictEqual(date, "2026-09-01");
  assert.strictEqual(word, "记");
  assert.strictEqual(rows[0].summary, "差旅");
  assert.strictEqual(rows[0].account_code, "660201");
});

test("parseVoucherText 金蝶无凭证号（7 列）也能认", () => {
  const { rows, date } = parseVoucherText(
    "2026-09-01\t记\t差旅\t660201\t工资\t1000\t0", ACCS);
  assert.strictEqual(rows.length, 1);
  assert.strictEqual(date, "2026-09-01");
  assert.strictEqual(rows[0].summary, "差旅", "有凭证号时会错位，此处应走无号分支");
  assert.strictEqual(rows[0].account_code, "660201");
});

test("parseVoucherText 精简 3 列（科目,借,贷）", () => {
  const { rows } = parseVoucherText("1001\t100\t0\n100202\t0\t100", ACCS);
  assert.strictEqual(rows.length, 2);
  assert.strictEqual(rows[0].account_code, "1001");
  assert.strictEqual(rows[1].credit, "100.00");
});

test("parseVoucherText 跳过表头，问题行带原因", () => {
  const { rows, bad } = parseVoucherText("摘要\t科目\t借\t贷\n差旅\t8888\t100\t0", ACCS);
  assert.strictEqual(rows.length, 1);
  assert.strictEqual(bad.length, 1);
  assert.match(bad[0].why, /停用或非末级/);
});
