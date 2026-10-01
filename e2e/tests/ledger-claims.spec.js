const { test, expect } = require("@playwright/test");
const { newBook, postVoucher } = require("../helpers");

// 记账后的账套：账簿只统计已记账凭证
async function postedBook(page, name) {
  await newBook(page, name);
  await postVoucher(page, {
    date: "2026-01-15",
    rows: [
      { code: "1001", summary: "账簿取数", debit: "100" },
      { code: "2001", summary: "账簿取数", credit: "100" },
    ],
  });
  const list = await (await page.request.get("/api/vouchers?period=202601")).json();
  const resp = await page.request.post("/api/vouchers/batch-post", {
    data: { ids: list.map((v) => v.id) },
  });
  expect(resp.ok(), "批量记账应成功").toBeTruthy();
}

test("账簿查询：明细账/总账/日记账", async ({ page }) => {
  await postedBook(page, `E2E账簿${Date.now()}`);
  await page.click('.nav-item[data-view="ledger"]');
  await page.fill("#l-code", "1001");
  await page.click("#l-go");
  await expect(page.locator("#l-table")).toContainText("账簿取数", { timeout: 15_000 });

  await page.click('[data-ltab="general"]');
  await page.click("#l-go");
  await expect(page.locator("#l-table")).toContainText("本期发生额", { timeout: 15_000 });

  await page.click('[data-ltab="journal"]');
  await page.click("#l-go");
  await expect(page.locator("#l-table")).toContainText("账簿取数", { timeout: 15_000 });
});

test("费用报销：草稿→提交→审批通过→支付→自动出凭证", async ({ page }) => {
  await newBook(page, `E2E报销${Date.now()}`);
  await page.click('.nav-item[data-view="claims"]');
  await page.click("#cl-new");
  await page.fill("#cm-applicant", "张三");
  await page.fill("#cm-dept", "财务部");
  await page.fill("#cm-reason", "差旅费");
  await page.fill("#cm-amount", "200");
  await page.locator("#cm-items tbody select.acct-sel").first().selectOption("660201");
  await page.locator("#cm-items tbody .ci-amt").first().fill("200");
  await page.click("#cm-save");
  await expect(page.locator("#cl-body")).toContainText("张三", { timeout: 15_000 });

  await page.locator('[data-trans][data-to="submitted"]').first().click();
  await expect(page.locator("#cl-body")).toContainText("待审批", { timeout: 15_000 });
  await page.locator('[data-trans][data-to="approved"]').first().click();
  await expect(page.locator("#cl-body")).toContainText("已批准", { timeout: 15_000 });
  await page.locator('[data-trans][data-to="paid"]').first().click();
  await expect(page.locator("#cl-body")).toContainText("已付款", { timeout: 15_000 });

  // 支付即自动落账：付款凭证草稿随支付生成（H-3：草稿不入余额），无需再手动出凭证
  await expect(page.locator("#cl-body [data-voucher]")).toHaveCount(1, { timeout: 15_000 });
});
