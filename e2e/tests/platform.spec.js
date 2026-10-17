const { test, expect } = require("@playwright/test");
const { newBook } = require("../helpers");

test("平台账号：开通→重置口令→重置设备→删除", async ({ page }) => {
  await newBook(page, `E2E平台${Date.now()}`);
  await page.click('.nav-item[data-view="platform-users"]');
  await page.click("#pu-add");
  await page.fill("#nc-u", "e2eplat");
  await page.fill("#nc-d", "E2E平台用户");
  await page.fill("#nc-p", "Passw0rd!");
  await page.click("#nc-save");
  await expect(page.locator("#pu-list")).toContainText("e2eplat", { timeout: 15_000 });

  await page.locator('[data-act="reset"][data-u="e2eplat"]').click();
  await page.fill("#rp-n", "NewPassw0rd!");
  await page.click("#rp-save");
  await expect(page.locator("#rp-save")).toHaveCount(0, { timeout: 15_000 });

  await page.locator('[data-act="dev"][data-u="e2eplat"]').click();
  await page.click("#cf-ok");
  await expect(page.locator("#pu-list")).toContainText("未绑定", { timeout: 15_000 });

  await page.locator('[data-act="del"][data-u="e2eplat"]').click();
  await page.click("#cf-ok");
  await expect(page.locator("#pu-list")).not.toContainText("e2eplat", { timeout: 15_000 });
});

test("全部账套：平台管理员可见并进入", async ({ page }) => {
  const company = `E2E全部账套${Date.now()}`;
  await newBook(page, company);
  await page.click('.nav-item[data-view="platform-books"]');
  await expect(page.locator("#pb-list")).toContainText(company, { timeout: 15_000 });
  await expect(page.locator("#pb-list [data-key]").first()).toBeVisible();
});
