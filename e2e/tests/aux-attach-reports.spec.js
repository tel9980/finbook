const { test, expect } = require("@playwright/test");
const { auditSignPost, newBook } = require("../helpers");

test("辅助/数量凭证、附件上传与数量金额账", async ({ page }) => {
  // 曾经绕过 newBook 自己手写登录+建账。两个后果：
  //   ① 账套名没有 worker 段 → purgeOwnBooks 清不掉 → 额度被占满，
  //      实测 98 条里 65 条一起挂，每条都报「等不到 .nav-item[data-view=vouchers]」，
  //      与各自的测试内容毫无关系；
  //   ② 拿不到 newBook 里的 setSimpleNav → 精简模式下侧栏只有 16 项。
  // 这里跟 newBook 的行为本来一模一样，直接用它。
  await newBook(page, `辅${Date.now()}`);

  // 借 140301（存货辅助 + 数量核算）5×20=100 / 贷 1001 100
  await page.click('.nav-item[data-view="vouchers"]');
  await page.click("#new-v");
  await page.fill("#v-date", "2026-01-15");
  // 只匹配"分录行"：辅助面板展开后会插入一行明细 tr，用 :has 排除它
  const entryRows = page.locator("#v-entries tbody tr:has(select.acct-sel)");
  await expect(entryRows.first()).toBeVisible();
  await entryRows.nth(0).locator("select.acct-sel").selectOption("140301");
  await entryRows.nth(0).locator(".e-sum").fill("E2E 入库");
  await entryRows.nth(0).locator(".e-d").fill("100");
  await entryRows.nth(1).locator("select.acct-sel").selectOption("1001");
  await entryRows.nth(1).locator(".e-sum").fill("E2E 付款");
  await entryRows.nth(1).locator(".e-c").fill("100");
  await entryRows.nth(0).locator(".e-aux").click();
  await page.fill('.aux-in[data-k="item"]', "RM01");
  await page.fill('.aux-in[data-k="qty"]', "5");
  await page.fill('.aux-in[data-k="price"]', "20");
  await page.click("#v-save");
  await expect(page.locator(".modal-mask")).toHaveCount(0, { timeout: 10_000 });

  // 重新打开：上传附件
  await page.locator("#v-table tbody [data-edit]").first().click();
  await page.setInputFiles("#v-attfile", {
    name: "receipt.txt",
    mimeType: "text/plain",
    buffer: Buffer.from("hello attachment"),
  });
  await page.click("#v-attup");
  await expect(page.locator("#v-attach")).toContainText("receipt.txt", { timeout: 10_000 });
  await page.click("#v-close");

  // H-3：数量金额账只统计已记账，先记账再查报表
  const list = await (await page.request.get("/api/vouchers?period=202601")).json();
  // 默认账套开着审核环节：先审核再记账
  await auditSignPost(page, list[0].id);

  // 数量金额账能出数
  await page.click('.nav-item[data-view="reports"]');
  await page.click("#r-qty");
  await expect(page.locator("#r-qty-table")).toContainText("140301", { timeout: 10_000 });
  await expect(page.locator("#r-qty-table")).toContainText("5");

  // 导出接口在浏览器会话下可用
  const resp = await page.request.get("/api/export/vouchers?period=202601");
  expect(resp.status()).toBe(200);
  const csv = await resp.text();
  expect(csv).toContain("凭证号");
});
