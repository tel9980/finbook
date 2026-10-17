const { test, expect } = require("@playwright/test");
const { newBook } = require("../helpers");

test("仪表盘与账目总览", async ({ page }) => {
  await newBook(page, `E2E看板${Date.now()}`);

  await page.click('.nav-item[data-view="dashboard"]');
  await expect(page.locator("#main")).toContainText("当前会计期间", { timeout: 15_000 });
  await expect(page.locator("#main")).toContainText("凭证数");

  await page.click('.nav-item[data-view="overview"]');
  await expect(page.locator("#main")).toContainText("凭证总数（全账套）", { timeout: 15_000 });
  await expect(page.locator("#main")).toContainText("补齐新版科目表");
});
