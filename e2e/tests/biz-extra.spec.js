const { test, expect } = require("@playwright/test");
const { newBook } = require("../helpers");

test("采购/销售对账 + 订单变更 + 暂估 + 配额", async ({ page }) => {
  await newBook(page, `E2E对账表${Date.now()}`);

  await page.click('.nav-item[data-view="po-reconcile"]');
  await expect(page.locator("#pr-result")).not.toContainText("加载中", { timeout: 15_000 });

  await page.click('.nav-item[data-view="so-reconcile"]');
  await expect(page.locator("#sr-result")).not.toContainText("加载中", { timeout: 15_000 });

  await page.click('.nav-item[data-view="order-change-log"]');
  await page.fill("#ocl-id", "1");
  await page.click("#ocl-load");
  await expect(page.locator("#ocl-result")).toContainText("无变更记录", { timeout: 15_000 });

  await page.click('.nav-item[data-view="po-estimate"]');
  await page.fill("#pe-poid", "1");
  await page.click("#pe-load");
  await expect(page.locator("#pe-result")).toContainText("未冲回暂估", { timeout: 15_000 });

  await page.click('.nav-item[data-view="procure-quota"]');
  await page.fill("#pq-item", "140301");
  await page.fill("#pq-sup", "S01");
  await page.fill("#pq-qty", "100");
  await page.click("#pq-save");
  await page.click("#pq-query");
  await expect(page.locator("#pq-result")).toContainText("剩余配额", { timeout: 15_000 });
});

test("预算预警/分析 + 工艺路线 + 工序报工", async ({ page }) => {
  await newBook(page, `E2E预算表${Date.now()}`);

  await page.click('.nav-item[data-view="budget-alerts"]');
  await page.click("#ba-run");
  await expect(page.locator("#ba-result")).toContainText("无预警科目", { timeout: 15_000 });

  await page.click('.nav-item[data-view="budget-analysis"]');
  await page.fill("#ana-year", "2026");
  await page.click("#ana-run");
  await expect(page.locator("#ana-body")).toContainText("无预算数据", { timeout: 15_000 });

  const resp = await page.request.post("/api/routing/140301", {
    data: [{ seq: 1, op_code: "OP1", op_name: "车削", work_center: "WC1", std_hours: "1", rate: "10" }],
  });
  expect(resp.ok(), "维护工艺路线应成功").toBeTruthy();
  await page.click('.nav-item[data-view="routing"]');
  await page.fill("#rt-item", "140301");
  await page.click("#rt-load");
  await expect(page.locator("#rt-result")).toContainText("OP1", { timeout: 15_000 });

  await page.click('.nav-item[data-view="work-report"]');
  await expect(page.locator("#wr-ops")).toContainText("没有生产订单", { timeout: 15_000 });
});
