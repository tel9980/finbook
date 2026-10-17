const { test, expect } = require("@playwright/test");
const { newBook } = require("../helpers");

test("资金管理：票据新增→贴现→资金预测", async ({ page }) => {
  await newBook(page, `E2E资金${Date.now()}`);
  await page.click('.nav-item[data-view="funds"]');
  await page.click("#ft-bill");
  await page.click("#bill-new");
  await page.fill("#b-no", "BP001");
  await page.fill("#b-cp", "客户A");
  await page.fill("#b-bank", "工商银行");
  await page.fill("#b-amt", "5000");
  await page.click("#b-save");
  await expect(page.locator("#bill-list")).toContainText("BP001", { timeout: 15_000 });
  await expect(page.locator("#bill-list")).toContainText("在库");

  await page.locator('[data-bill-act][data-to="discounted"]').first().click();
  await expect(page.locator("#bill-list")).toContainText("已贴现", { timeout: 15_000 });

  await page.click("#ft-forecast");
  await expect(page.locator("#funds-body")).toContainText("预计资金头寸", { timeout: 15_000 });

  // 融资：新增借款 → 结清
  await page.click("#ft-loan");
  await page.click("#loan-new");
  await page.fill("#l-no", "LN001");
  await page.fill("#l-bank", "工商银行");
  await page.fill("#l-pr", "100000");
  await page.fill("#l-rate", "4.5");
  await page.click("#l-save");
  await expect(page.locator("#loan-list")).toContainText("LN001", { timeout: 15_000 });
  await page.locator("[data-loan-settle]").first().click();
  await page.click("#cf-ok");
  await expect(page.locator("#loan-list")).toContainText("已结清", { timeout: 15_000 });
});

test("预算版本：新建版本→设为当前", async ({ page }) => {
  await newBook(page, `E2E预算${Date.now()}`);
  await page.click('.nav-item[data-view="budget-versions"]');
  await page.fill("#bv-key", "v2");
  await page.fill("#bv-name", "2026调整版");
  await page.click("#bv-save");
  await expect(page.locator("#bv-list")).toContainText("2026调整版", { timeout: 15_000 });

  await page.locator('[data-bv-act="v2"]').click();
  await expect(page.locator("#bv-list")).toContainText("当前版本", { timeout: 15_000 });
});

test("报表附注：新增→列表→删除", async ({ page }) => {
  await newBook(page, `E2E附注${Date.now()}`);
  await page.click('.nav-item[data-view="notes"]');
  await page.fill("#nt-title", "货币资金说明");
  await page.fill("#nt-content", "期末货币资金构成说明。");
  await page.click("#nt-save");
  await expect(page.locator("#nt-list")).toContainText("货币资金说明", { timeout: 15_000 });

  await page.locator("[data-nt-del]").first().click();
  await page.click("#cf-ok");
  await expect(page.locator("#nt-list")).toContainText("暂无附注", { timeout: 15_000 });
});

test("电子档案：归档→列表", async ({ page }) => {
  await newBook(page, `E2E档案${Date.now()}`);
  await page.click('.nav-item[data-view="archive"]');
  await page.fill("#ar-new-title", "2026-01 凭证册");
  await page.fill("#ar-new-payload", '{"vouchers":1}');
  await page.click("#ar-save");
  await expect(page.locator("#ar-list")).toContainText("2026-01 凭证册", { timeout: 15_000 });
  // 归档即封存（后端 archive_create 固定 sealed=1）
  await expect(page.locator("#ar-list")).toContainText("封存");
});
