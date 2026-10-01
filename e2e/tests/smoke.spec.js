const { test, expect } = require("@playwright/test");

test("会计月度闭环：建账→录凭证→记账→结账→报表", async ({ page }) => {
  const company = `E2E公司${Date.now()}`;

  // 登录（平台管理员）
  await page.goto("/");
  await expect(page.locator("#u")).toBeVisible({ timeout: 15_000 });
  await page.fill("#u", "admin");
  await page.fill("#p", "Admin!2026");
  await page.click('#login-form button[type="submit"]');

  // 选账套页 → 新建账套
  await expect(page.locator("#new-book")).toBeVisible({ timeout: 15_000 });
  await page.click("#new-book");
  await page.fill("#cb-company", company);
  await page.fill("#cb-start", "2026-01");
  await page.click("#cb-save");

  // 进入账套后录制一张凭证：借 1001 100 / 贷 2001 100
  await expect(page.locator('.nav-item[data-view="vouchers"]')).toBeVisible({ timeout: 15_000 });
  await page.click('.nav-item[data-view="vouchers"]');
  await page.click("#new-v");
  // 日期改到账套期间内（默认是运行当天，可能不属于启用期间）
  await page.fill("#v-date", "2026-01-15");
  const rows = page.locator("#v-entries tbody tr");
  await expect(rows.first()).toBeVisible();
  await rows.nth(0).locator("select.acct-sel").selectOption("1001");
  await rows.nth(0).locator(".e-sum").fill("E2E 收款");
  await rows.nth(0).locator(".e-d").fill("100");
  await rows.nth(1).locator("select.acct-sel").selectOption("2001");
  await rows.nth(1).locator(".e-sum").fill("E2E 借款");
  await rows.nth(1).locator(".e-c").fill("100");
  await page.click("#v-save");
  // 摘要/日期校验失败时弹窗不会关闭：先确认保存成功
  await expect(page.locator(".modal-mask")).toHaveCount(0, { timeout: 10_000 });

  // 打开刚保存的凭证并记账
  const openBtn = page.locator("#v-table tbody [data-edit]").first();
  await expect(openBtn).toBeVisible({ timeout: 10_000 });
  await openBtn.click();
  await page.click("#v-post");
  await expect(page.locator(".modal-mask")).toHaveCount(0, { timeout: 10_000 });

  // 期末处理：直接结账（不要求先结转）
  await page.click('.nav-item[data-view="period-end"]');
  await expect(page.locator("#pe-period")).toBeVisible({ timeout: 10_000 });
  await page.uncheck("#pe-reqcarry");
  await page.click("#pe-close");
  await page.click("#cf-ok");
  await expect(page.locator("#pe-status")).toContainText("2026-01", { timeout: 10_000 });

  // 资产负债表可出数
  await page.click('.nav-item[data-view="balance-sheet"]');
  await page.fill("#bs-from", "2026-01");
  await page.fill("#bs-to", "2026-01");
  await page.click("#bs-run");
  await expect(page.locator("#bs-result table")).toBeVisible({ timeout: 10_000 });
  await expect(page.locator("#bs-result")).toContainText("资 产 总 计");
});
