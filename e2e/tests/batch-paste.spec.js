const { test, expect } = require("@playwright/test");
const { newBook } = require("../helpers");

// Web 端「批量粘贴」的两条主路径：辅助档案 + 期初余额。
//
// 与 §4.61 的 aux-preset 不同，这条测的是**解析**。解析本身在 fincore 里有 42
// 条单测、HTTP 层有 11 条集成测试 —— 这里只补「浏览器里点得动、看得见」那一段，
// 端到端验的其实是**接线**：按钮 → 弹窗 → 预览表格 → 导入 → 列表刷新。
//
// 特别盯住「先预览再导入」这个两步流程：它是这个功能的核心约定
// （直接点导入会把一坨文本当数据写库，粘贴错位就是静默的脏数据）。

// ⚠️ 粘贴文本用 Tab 分隔，模拟 Excel 复制的真实输出（\t）。
// 用逗号的话，金额里的千分位会被当分隔符切坏（fincore 的 begin 测试里有钉住这条）。
const AUX_TEXT = "编码\t名称\nC01\t杭州明远贸易\nC02\t宁波华信电子";
const BEGIN_TEXT = "科目编码\t金额\n1001\t58000\n1002\t125000\n2001\t80000";

test("辅助档案批量粘贴：先预览再导入，已存在的跳过不覆盖", async ({ page }) => {
  await newBook(page, `E2E粘档案${Date.now()}`);

  // 先手工建一个 C01，用来验证「已存在」这条
  await page.click('.nav-item[data-view="aux"]');
  await page.click('[data-kind="customer"]');
  await page.click("#aux-new");
  await page.fill("#ae-code", "C01");
  await page.fill("#ae-name", "我的老客户");
  await page.click("#ae-save");
  await expect(page.locator("table.grid tbody")).toContainText("我的老客户");

  await page.click("#aux-batch");
  await expect(page.locator("#bp-text")).toBeVisible();
  await page.fill("#bp-text", AUX_TEXT);

  // 未预览前「导入」必须禁用 —— 两步流程的第一道闸
  await expect(page.locator("#bp-ok")).toBeDisabled();

  await page.click("#bp-preview-btn");
  await expect(page.locator("#bp-preview")).toContainText("解析到 3 条");
  // 表头被跳过，剩下 3 行：1 条已存在 + 2 条新
  await expect(page.locator("#bp-preview")).toContainText("其中 2 条将导入");
  await expect(page.locator("#bp-preview")).toContainText("编码已存在");
  // 预览阶段**没有写库**
  await expect(page.locator("#bp-preview .grid tbody tr")).toHaveCount(3);

  await page.click("#bp-ok");
  await expect(page.locator("#modal-root")).toHaveCount(0);
  await expect(page.locator("table.grid tbody tr")).toHaveCount(3);
  await expect(page.locator("table.grid tbody")).toContainText("杭州明远贸易");
  // 已存在的那条**不能被改名** —— 「不覆盖」是这个功能的硬约定
  await expect(page.locator("table.grid tbody")).toContainText("我的老客户");
});

test("辅助档案批量粘贴：改文本后预览作废，必须重新预览", async ({ page }) => {
  await newBook(page, `E2E粘改${Date.now()}`);
  await page.click('.nav-item[data-view="aux"]');
  await page.click('[data-kind="supplier"]');
  await page.click("#aux-batch");
  await page.fill("#bp-text", "苏州精诚\n");
  await page.click("#bp-preview-btn");
  await expect(page.locator("#bp-ok")).toBeEnabled();

  // 预览之后改文本 → 预览过期，导入必须被禁用。
  // 不这样的话，用户会导入他已经删掉的那几行 —— 两步流程就白做了。
  await page.fill("#bp-text", "");
  await expect(page.locator("#bp-ok")).toBeDisabled();
  await expect(page.locator("#bp-msg")).toContainText("重新点");
});

test("期初批量粘贴：方向按科目自动归位，预览带试算", async ({ page }) => {
  await newBook(page, `E2E粘期初${Date.now()}`);

  await page.click('.nav-item[data-view="begin"]');
  await page.click("#bg-batch");
  await page.fill("#bp-text", BEGIN_TEXT);
  await page.click("#bp-preview-btn");

  // 试算：Web 端粘贴后立刻提交，没有草稿区慢慢看，不给试算用户就完全不知道平不平
  await expect(page.locator("#bp-preview")).toContainText("借方合计");
  await expect(page.locator("#bp-preview")).toContainText("贷方合计");
  await expect(page.locator("#bp-preview")).toContainText("差额");
  // 方向不用填：1001/1002 是资产（借）、2001 是负债（贷），由科目表决定
  await expect(page.locator("#bp-preview")).toContainText("借方合计");
  const trial = await page.locator("#bp-preview").innerText();
  expect(trial).toContain("183,000.00"); // 58000 + 125000
  expect(trial).toContain("80,000.00");  // 2001
  expect(trial).toContain("103,000.00"); // 差额，不平要看得见

  await page.click("#bp-ok");
  await expect(page.locator("#modal-root")).toHaveCount(0);
  await page.waitForTimeout(800);
  await expect(page.locator("table.grid tbody tr")).toHaveCount(3);
  // 导入后必须点「保存全部」才落库 —— 弹窗文案里说了这件事
  await page.click("#bg-save");
  await page.waitForTimeout(800);
  await page.reload();
  await page.waitForTimeout(800);
  await expect(page.locator("table.grid tbody tr")).toHaveCount(3);
});

test("期初批量粘贴：未知科目给提示，不静默丢行", async ({ page }) => {
  await newBook(page, `E2E粘抄错${Date.now()}`);
  await page.click('.nav-item[data-view="begin"]');
  await page.click("#bg-batch");
  // 10011 是 1001 的笔误（多打一个 1）
  await page.fill("#bp-text", "1001,5000\n10011,3000");
  await page.click("#bp-preview-btn");

  await expect(page.locator("#bp-preview")).toContainText("无此科目");
  await expect(page.locator("#bp-preview")).toContainText("像不像 1001");
  await expect(page.locator("#bp-preview")).toContainText("其中 1 条将导入");
  // 抄错那行不该进试算（否则用户会以为「差一点」）
  const trial = await page.locator("#bp-preview").innerText();
  expect(trial).toContain("5,000.00");
  expect(trial).not.toContain("3,000.00");
});