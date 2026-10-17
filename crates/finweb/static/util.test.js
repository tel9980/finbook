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
