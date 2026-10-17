const { test, expect } = require("@playwright/test");
const { newBook, postVoucher, auditSignPost } = require("../helpers");

test("固定资产：建卡→计提折旧→台账出数", async ({ page }) => {
  await newBook(page, `E2E固资${Date.now()}`);
  await page.click('.nav-item[data-view="assets"]');
  await page.click("#as-new");
  await page.fill("#ae-code", "SB0001");
  await page.fill("#ae-name", "测试设备");
  await page.fill("#ae-orig", "12000");
  await page.click("#ae-save");
  await expect(page.locator("#as-table")).toContainText("SB0001", { timeout: 10_000 });

  await page.click("#as-accrue");
  await page.click("#cf-ok");
  await expect(page.locator("#as-summary")).toContainText("本期已计提记录 1 条", { timeout: 10_000 });
  // 12000 × 95% / 36 = 316.67
  await expect(page.locator("#as-table")).toContainText("316.67");
  await expect(page.locator("#as-table")).toContainText("11,683.33");
});

test("银行对账：导入对账单→自动勾对→余额调节表一致", async ({ page }) => {
  await newBook(page, `E2E银行${Date.now()}`);
  await postVoucher(page, {
    date: "2026-01-10",
    rows: [
      { code: "100201", summary: "收到货款", debit: "500" },
      { code: "1001", summary: "收到货款", credit: "500" },
    ],
  });
  // H-3：余额与报表只统计已记账，先记账再对账
  const list = await (await page.request.get("/api/vouchers?period=202601")).json();
  // 默认账套开着审核环节：先审核再记账
  await auditSignPost(page, list[0].id);

  await page.click('.nav-item[data-view="bank"]');
  await page.fill("#bk-acct", "100201");
  await page.click("#bk-load");
  await page.click("#bk-import");
  await page.fill("#bi-text", "2026-01-10,收到货款,SN100,500.00,0.00,500.00");
  await page.click("#bi-ok");
  await expect(page.locator("#bk-sum")).toContainText("银行流水 1 条", { timeout: 10_000 });

  await page.click("#bk-auto");
  await expect(page.locator("#bk-sum")).toContainText("已勾 1", { timeout: 10_000 });
  await expect(page.locator("#bk-recon")).toContainText("调节后一致");
});

test("审核环节：启用后未审核不能记账，审核后可记账", async ({ page }) => {
  await newBook(page, `E2E审核${Date.now()}`);
  await page.click('.nav-item[data-view="options"]');
  await page.check("#op-audit");
  await page.click("#op-save");

  await postVoucher(page, {
    date: "2026-01-12",
    rows: [
      { code: "1001", summary: "借备用金", debit: "100" },
      { code: "2001", summary: "借备用金", credit: "100" },
    ],
  });

  const list = await (await page.request.get("/api/vouchers?period=202601")).json();
  const id = list[0].id;

  let resp = await page.request.post(`/api/vouchers/${id}/post`);
  expect(resp.status(), "未审核直接记账应被拒").toBe(400);

  await page.locator("#v-table tbody [data-edit]").first().click();
  await page.click("#v-audit");
  await expect(page.locator("#v-table tbody")).toContainText("已审核", { timeout: 10_000 });

  // 审核过了还要过出纳签字那道闸（生产默认开）。这条凭证借记 1001 库存现金，
  // 会被出纳闸门命中。
  resp = await page.request.post(`/api/vouchers/${id}/sign`);
  expect(resp.ok(), `出纳签字应成功：${await resp.text()}`).toBeTruthy();

  resp = await page.request.post(`/api/vouchers/${id}/post`);
  expect(resp.status(), "审核 + 签字后记账应成功").toBe(200);

  await page.click('.nav-item[data-view="vouchers"]');
  await expect(page.locator("#v-table tbody")).toContainText("已记账", { timeout: 10_000 });
});
