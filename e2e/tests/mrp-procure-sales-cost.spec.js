const { test, expect } = require("@playwright/test");
const { newBook } = require("../helpers");

test("MRP：最近结果空表不报错→运行需求出结果", async ({ page }) => {
  await newBook(page, `E2EMRP${Date.now()}`);
  await page.click('.nav-item[data-view="mrp"]');
  // 空表时 MAX(run_at) 为 NULL：曾经 500，现在应正常渲染空表
  await page.click("#mrp-latest");
  await expect(page.locator("#mrp-result table")).toHaveCount(1, { timeout: 15_000 });

  await page.fill("#mrp-item", "140301");
  await page.fill("#mrp-qty", "10");
  await page.click("#mrp-run");
  await expect(page.locator("#mrp-result")).toContainText("140301", { timeout: 15_000 });
  await expect(page.locator("#mrp-result")).toContainText("采购");
});

test("采购单据：请购单保存→审批", async ({ page }) => {
  await newBook(page, `E2E采购${Date.now()}`);
  await page.click('.nav-item[data-view="po-doc"]');
  await page.fill("#pd-item", "140301");
  await page.fill("#pd-qty", "5");
  await page.fill("#pd-memo", "E2E请购");
  await page.click("#pd-save");
  await expect(page.locator("#pd-list")).toContainText("140301", { timeout: 15_000 });

  await page.locator("[data-req-approve]").first().click();
  await expect(page.locator("#pd-list")).not.toContainText("draft", { timeout: 15_000 });
});

test("销售单据：报价单保存→审批", async ({ page }) => {
  await newBook(page, `E2E销售${Date.now()}`);
  await page.click('.nav-item[data-view="so-doc"]');
  await page.fill("#sd-cust", "C01");
  await page.fill("#sd-item", "140501");
  await page.fill("#sd-qty", "2");
  await page.fill("#sd-price", "10");
  await page.click("#sd-save");
  await expect(page.locator("#sd-list")).toContainText("C01", { timeout: 15_000 });

  await page.locator("[data-quo-approve]").first().click();
  await expect(page.locator("#sd-list")).not.toContainText("draft", { timeout: 15_000 });
});

test("成本核算：配置计价方式→期末结价试算", async ({ page }) => {
  await newBook(page, `E2E成本${Date.now()}`);
  // 造一笔入库流水，期末结价才有数据
  const resp = await page.request.post("/api/inventory/adjust", {
    data: { period: 202601, date: "2026-01-10", item: "140301", delta: "5", memo: "E2E" },
  });
  expect(resp.ok(), "入库调整应成功").toBeTruthy();

  await page.click('.nav-item[data-view="cost"]');
  await page.click("#cost-new");
  await page.fill("#c-item", "140301");
  await page.selectOption("#c-method", "fifo");
  await page.click("#c-save");
  await expect(page.locator("#cost-list")).toContainText("140301", { timeout: 15_000 });

  await page.click("#ct-close");
  await page.click("#ce-preview");
  await expect(page.locator("#ce-list")).toContainText("140301", { timeout: 15_000 });
  await expect(page.locator("#ce-list")).toContainText("试算");
});

test("审批中心：待审批→通过", async ({ page }) => {
  await newBook(page, `E2E审批${Date.now()}`);
  const resp = await page.request.post("/api/approvals", {
    data: { biz_kind: "test", biz_id: 1, title: "E2E审批单", approvers: ["admin"] },
  });
  expect(resp.ok(), "发起审批应成功").toBeTruthy();

  await page.click('.nav-item[data-view="approval"]');
  await expect(page.locator("#ap-list")).toContainText("E2E审批单", { timeout: 15_000 });
  await page.locator('[data-ap][data-act="1"]').first().click();
  await expect(page.locator("#ap-list")).toContainText("没有待审批的单据", { timeout: 15_000 });
});
