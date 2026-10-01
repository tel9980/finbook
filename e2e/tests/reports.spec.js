const { test, expect } = require("@playwright/test");
const { newBook, postVoucher } = require("../helpers");

// 两张凭证：现金收付 + 管理费用；账簿（多栏账/摘要汇总）只统计已记账，故一并记账
async function seed(page) {
  await postVoucher(page, {
    date: "2026-01-15",
    rows: [
      { code: "1001", summary: "报表取数", debit: "100" },
      { code: "2001", summary: "报表取数", credit: "100" },
    ],
  });
  await postVoucher(page, {
    date: "2026-01-16",
    rows: [
      { code: "660201", summary: "办公费", debit: "30" },
      { code: "1001", summary: "办公费", credit: "30" },
    ],
  });
  const list = await (await page.request.get("/api/vouchers?period=202601")).json();
  const resp = await page.request.post("/api/vouchers/batch-post", {
    data: { ids: list.map((v) => v.id) },
  });
  expect(resp.ok(), "批量记账应成功").toBeTruthy();
}

test("报表中心：科目余额表→辅助账→数量金额账", async ({ page }) => {
  await newBook(page, `E2E报心${Date.now()}`);
  await seed(page);
  await page.click('.nav-item[data-view="reports"]');
  await page.click("#r-go");
  await expect(page.locator("#r-table tbody")).toContainText("1001", { timeout: 15_000 });
  await page.click("#r-aux");
  await expect(page.locator("#r-extra")).toContainText("辅助账", { timeout: 15_000 });
  await page.click("#r-qty");
  await expect(page.locator("#r-extra")).toContainText("数量金额账", { timeout: 15_000 });
});

test("三大报表：资产负债表/利润表/现金流量表", async ({ page }) => {
  await newBook(page, `E2E三表${Date.now()}`);
  await seed(page);

  await page.click('.nav-item[data-view="balance-sheet"]');
  await expect(page.locator("#bs-result")).toContainText("货币资金", { timeout: 15_000 });

  await page.click('.nav-item[data-view="income-statement"]');
  await expect(page.locator("#is-result")).toContainText("营业收入", { timeout: 15_000 });

  await page.click('.nav-item[data-view="cash-flow"]');
  await expect(page.locator("#cf-result")).toContainText("经营活动产生的现金流量", { timeout: 15_000 });
});

test("多栏账/摘要汇总/财务指标/权益变动/报表对比/科目日报", async ({ page }) => {
  await newBook(page, `E2E账表${Date.now()}`);
  await seed(page);

  await page.click('.nav-item[data-view="multi-column"]');
  await page.fill("#mc-cols", "660201");
  await page.click("#mc-run");
  await expect(page.locator("#mc-result")).toContainText("办公费", { timeout: 15_000 });

  await page.click('.nav-item[data-view="summary-table"]');
  await page.click("#st-run");
  await expect(page.locator("#st-result")).toContainText("办公费", { timeout: 15_000 });

  await page.click('.nav-item[data-view="ratios"]');
  await page.click("#rt-run");
  await expect(page.locator("#rt-result table")).toHaveCount(1, { timeout: 15_000 });

  await page.click('.nav-item[data-view="equity"]');
  await page.click("#eq-run");
  await expect(page.locator("#eq-result")).toContainText("勾稽", { timeout: 15_000 });

  await page.click('.nav-item[data-view="compare"]');
  await page.click("#cp-run");
  await expect(page.locator("#cp-result table")).toHaveCount(1, { timeout: 15_000 });

  await page.click('.nav-item[data-view="daily"]');
  await page.fill("#dl-code", "1001");
  await page.click("#dl-run");
  await expect(page.locator("#dl-result")).toContainText("2026-01-15", { timeout: 15_000 });
});
