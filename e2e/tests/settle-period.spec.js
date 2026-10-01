const { test, expect } = require("@playwright/test");
const { newBook, postVoucher } = require("../helpers");

test("往来核销：两笔应收/收款自动核销→账龄→核销记录", async ({ page }) => {
  await newBook(page, `E2E核销${Date.now()}`);
  await postVoucher(page, {
    date: "2026-01-10",
    rows: [
      { code: "112201", summary: "销售应收", debit: "100", aux: { customer: "C01" } },
      { code: "1001", summary: "销售应收", credit: "100" },
    ],
  });
  await postVoucher(page, {
    date: "2026-01-20",
    rows: [
      { code: "1001", summary: "收回货款", debit: "100" },
      { code: "112201", summary: "收回货款", credit: "100", aux: { customer: "C01" } },
    ],
  });

  await page.click('.nav-item[data-view="settle"]');
  await page.click("#st-load");
  await expect(page.locator("#st-sum")).toContainText("未核销 2 笔", { timeout: 10_000 });
  await expect(page.locator("#st-table")).toContainText("C01");

  await page.click("#st-auto");
  await page.click("#cf-ok");
  await expect(page.locator("#st-sum")).toContainText("未核销 0 笔", { timeout: 10_000 });

  await page.click("#st-aging");
  await expect(page.locator("#st-extra")).toContainText("账龄分析", { timeout: 10_000 });

  await page.click("#st-records");
  await expect(page.locator("#st-extra")).toContainText("核销记录", { timeout: 10_000 });
  await expect(page.locator("#st-extra")).toContainText("C01");
});

test("期末处理：结转损益→记账→结账，期间状态推进", async ({ page }) => {
  await newBook(page, `E2E期末${Date.now()}`);
  await postVoucher(page, {
    date: "2026-01-15",
    rows: [
      { code: "660201", summary: "办公费", debit: "100" },
      { code: "1001", summary: "办公费", credit: "100" },
    ],
  });
  await page.locator("#v-table tbody [data-edit]").first().click();
  await page.click("#v-post");
  await expect(page.locator("#v-table tbody")).toContainText("已记账", { timeout: 10_000 });

  await page.click('.nav-item[data-view="period-end"]');
  await expect(page.locator("#pe-status")).toContainText("损益科目", { timeout: 10_000 });
  await page.click("#pe-carry");
  await page.click("#cf-ok");
  // 结转凭证已生成但未记账 → 预检查应提示存在未记账凭证
  await expect(page.locator("#pe-issues")).toContainText("未记账", { timeout: 10_000 });

  // 结转生成的凭证未记账：批量记账后才能结账
  await page.click('.nav-item[data-view="vouchers"]');
  await page.check("#v-all");
  await page.click("#v-batch");
  await expect(page.locator("#v-table tbody")).not.toContainText("未记账", { timeout: 10_000 });

  await page.click('.nav-item[data-view="period-end"]');
  await page.click("#pe-check");
  await page.click("#pe-close");
  await page.click("#cf-ok");
  await expect(page.locator("#pe-status")).toContainText("已结账至：2026-01", { timeout: 10_000 });
});
