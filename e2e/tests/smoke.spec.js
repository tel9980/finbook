const { test, expect } = require("@playwright/test");
const { newBook } = require("../helpers");

test("会计月度闭环：建账→录凭证→记账→结账→报表", async ({ page }) => {
  // 曾经绕过 newBook 自己手写登录+建账。后果同 aux-attach-reports.spec.js：
  // 账套名没有 worker 段 → purgeOwnBooks 清不掉 → 额度占满 → 全套件失败。
  // 行为与 newBook 一致，直接用它。
  await newBook(page, `闭环${Date.now()}`);

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

  // 打开刚保存的凭证，走完整流程：审核 → 记账
  //
  // 新建账套的审核环节**默认开**（三权分离），所以「记账」按钮只对已审核凭证显示
  // （见 app.js openVoucher 的 canPost）。这里刻意点两次而不是直接调 API：
  // 这样才真的验到「未审核不给记账按钮、审核后才给」这条 UI 行为。
  const openBtn = page.locator("#v-table tbody [data-edit]").first();
  await expect(openBtn).toBeVisible({ timeout: 10_000 });
  await openBtn.click();
  await expect(page.locator("#v-audit")).toBeVisible({ timeout: 10_000 });
  await page.click("#v-audit");
  await expect(page.locator("#v-table tbody")).toContainText("已审核", { timeout: 10_000 });

  await page.locator("#v-table tbody [data-edit]").first().click();
  // 出纳签字（默认开）：涉及现金/银行科目的凭证记账前要点一下。
  // 不点的话「记账」会被后端拒，症状是**弹窗不关**（错误 toast 留在里面）。
  await expect(page.locator("#v-sign")).toBeVisible({ timeout: 10_000 });
  await page.click("#v-sign");
  // 签字也会关闭弹窗（closeModal），所以要再打开一次才点得到「记账」。
  await expect(page.locator("#v-table tbody")).toContainText("已记账", { timeout: 5_000 }).catch(() => {});
  await page.locator("#v-table tbody [data-edit]").first().click();

  await expect(page.locator("#v-post")).toBeVisible({ timeout: 10_000 });
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
