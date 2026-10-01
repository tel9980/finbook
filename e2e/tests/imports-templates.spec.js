const { test, expect } = require("@playwright/test");
const { newBook } = require("../helpers");

test("数据导入：缺失科目映射后导入凭证", async ({ page }) => {
  await newBook(page, `E2E导入${Date.now()}`);
  await page.click('.nav-item[data-view="imports"]');
  await page.selectOption("#imp-kind", "voucher");
  await page.selectOption("#imp-template", "generic");
  await page.fill(
    "#imp-text",
    "2026-01-18,记,导入收款,9999,300,0\n2026-01-18,记,导入收款,2001,0,300"
  );

  await page.click("#imp-analyze");
  await expect(page.locator("#imp-result")).toContainText("1 个缺失科目", { timeout: 10_000 });
  await page.selectOption('.imp-map[data-code="9999"]', "1001");

  await page.click("#imp-run");
  await expect(page.locator("#imp-result")).toContainText("成功导入 1 条", { timeout: 10_000 });

  await page.click('.nav-item[data-view="vouchers"]');
  await expect(page.locator("#v-table tbody")).toContainText("导入收款");
});

test("凭证模板：新建每月模板→本期到期生成凭证", async ({ page }) => {
  await newBook(page, `E2E模板${Date.now()}`);
  await page.click('.nav-item[data-view="templates"]');
  await page.click("#tpl-new");
  await page.fill("#tp-name", "月度房租");
  await page.selectOption("#tp-freq", "monthly");
  await page.fill("#tp-start", "202601");
  await page.fill("#tp-end", "202612");

  const rows = page.locator("#tp-entries tbody tr");
  await rows.nth(0).locator(".te-sum").fill("房租");
  await rows.nth(0).locator("select.acct-sel").selectOption("660201");
  await rows.nth(0).locator(".te-amt").fill("1000");
  await page.click("#tp-add");
  await rows.nth(1).locator(".te-sum").fill("房租");
  await rows.nth(1).locator("select.acct-sel").selectOption("1001");
  await rows.nth(1).locator(".te-dir").selectOption("credit");
  await rows.nth(1).locator(".te-amt").fill("1000");
  await page.click("#tp-save");
  await expect(page.locator("#tpl-body")).toContainText("月度房租", { timeout: 10_000 });
  await expect(page.locator("#tpl-body")).toContainText("每月");

  await page.click("#tpl-tab-due");
  await expect(page.locator("#tpl-body")).toContainText("月度房租", { timeout: 10_000 });
  await page.locator("[data-gen]").first().click();
  await page.click("#cf-ok");
  await expect(page.locator("#tpl-body")).toContainText("202601", { timeout: 10_000 });

  await page.click('.nav-item[data-view="vouchers"]');
  await expect(page.locator("#v-table tbody")).toContainText("房租");
});
