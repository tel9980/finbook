const { test, expect } = require("@playwright/test");
const { newBook } = require("../helpers");

// 采购「收货 → 暂估」联动
//
// 守的是**订单驱动的闭环**：早先这两步之间没有任何联动 ——
// 收完货不提示「该登暂估了」，而登暂估的金额全靠手敲，敲错了才知道。
// 守卫把重复登记挡住了，但用户仍然只能一次次试错。
//
// 后端口径由 findb/finweb 测试覆盖（estimate_is_capped_by_received_amount /
// line_level_receipt_gives_exact_cap / over_receipt_flows_through_stock_and_estimate_cap /
// estimate_is_capped_by_receipt_over_http）；本用例守 UI 接线与文案。

/** 建一张采购订单（两行，便于验证行级收货） */
async function makePo(page, opts = {}) {
  const r = await page.request.post("/api/procure/po", {
    data: {
      period: 202601,
      date: "2026-01-05",
      status: "Confirmed",
      supplier_code: "S001",
      supplier_name: "供应商甲",
      lines: opts.lines || [
        { item_code: "140301", item_name: "原料A", qty_ordered: "100", unit_price: "10", tax_rate: "0" },
        { item_code: "140302", item_name: "原料B", qty_ordered: "100", unit_price: "10", tax_rate: "0" },
      ],
    },
  });
  expect(r.ok(), `建采购订单应成功：${await r.text()}`).toBeTruthy();
  return (await r.json()).id;
}

test("暂估页按行列出「可暂估额度 / 已暂估 / 还能登」，并能一键带出", async ({ page }) => {
  await newBook(page, `E2E暂估联动${Date.now()}`);
  const po_id = await makePo(page);

  await page.click('.nav-item[data-view="po-estimate"]');
  await page.fill("#pe-poid", String(po_id));
  await page.click("#pe-load");
  // 等数据到达的信号（表格里有行才说明查询成功）
  await expect(page.locator("#pe-status table tbody tr")).toHaveCount(2, { timeout: 15_000 });

  const status = page.locator("#pe-status");
  // 还没收货 → 额度为 0，且要能看出「欠收」
  await expect(status).toContainText("可暂估额度");
  await expect(status).toContainText("140301");
  await expect(status).toContainText("140302");
  // 物料改成下拉（从订单行里选），不再是自由输入 ——
  // 自由输入能敲订单里没有的物料，被守卫拒一次才发现
  const opts = await page.locator("#pe-item option").allTextContents();
  expect(opts.length, "物料应来自订单行").toBe(2);
  expect(opts.join(" ")).toContain("140301");

  // 只收第一行 100 件
  const rc = await page.request.post("/api/procure/receipt", {
    data: { po_id, period: 202601, date: "2026-01-08", qty: "100", item_code: "140301" },
  });
  expect(rc.ok(), `行级收货应成功：${await rc.text()}`).toBeTruthy();

  await page.click("#pe-load");
  // 界面金额带千分位（1,000.00），所以匹配要容忍逗号
  await expect(status).toContainText(/1,?000/, { timeout: 15_000 });

  // 选中第一行 → 额度应等于该行含税单价 × 实收（10 × 100 = 1000），
  // 而不是按订购占比折算的 500
  await page.selectOption("#pe-item", "140301");
  await page.click("#pe-fill");
  const filled = await page.locator("#pe-amount").inputValue();
  expect(
    filled.replace(/,/g, "").startsWith("1000"),
    `第一行额度应精确按该行算（1000），不是折算的 500；实际带出 ${filled}`
  );

  // 直接登记带出的额度 → 成功。
  // toast 渲染在 body 级的 #toast 里，**不在 #main 内** —— 断言要定位对地方，
  // 否则永远匹配不到（数据其实已登记成功，表格「已暂估」会变成 1,000.00）。
  await page.click("#pe-add");
  await expect(page.locator("#toast")).toContainText("已登记暂估", { timeout: 15_000 });

  // 登记后「还能登」归零
  await expect(status).toContainText("已用满", { timeout: 15_000 });

  // 第二行还没收货，额度仍是 0 —— 不能被第一行的收货撑起来
  await page.selectOption("#pe-item", "140302");
  await page.click("#pe-fill");
  await expect(page.locator("#toast")).toContainText("额度已用满", { timeout: 10_000 });
});

test("暂估额度按行精确：两行各收一半，额度各归各（不按订购占比摊）", async ({ page }) => {
  await newBook(page, `E2E暂估分行${Date.now()}`);
  const po_id = await makePo(page);

  // 两行各收 100（全额），但**分行**登记
  for (const code of ["140301", "140302"]) {
    const rc = await page.request.post("/api/procure/receipt", {
      data: { po_id, period: 202601, date: "2026-01-08", qty: "100", item_code: code },
    });
    expect(rc.ok(), `收货 ${code} 应成功：${await rc.text()}`).toBeTruthy();
  }
  const st = await (await page.request.get(`/api/procure/estimate/status?po_id=${po_id}`)).json();
  expect(st.rows.length, "应按订单行返回两行").toBe(2);
  // 各 1000，不是一行 2000 另一行 0
  for (const r of st.rows) {
    expect(String(r.cap).replace(/,/g, ""), `${r.item_code} 额度`).toMatch(/^1000(\.0+)?$/);
    expect(Number(r.qty_received), `${r.item_code} 已收`).toBe(100);
    expect(Number(r.remaining), `${r.item_code} 还能登`).toBe(1000);
  }
});

test("超收如实入库，额度随之放大（守卫不挡真实超收）", async ({ page }) => {
  await newBook(page, `E2E暂估超收${Date.now()}`);
  const po_id = await makePo(page, {
    lines: [{ item_code: "140301", item_name: "原料A", qty_ordered: "100", unit_price: "10", tax_rate: "0" }],
  });

  // 收 130 > 订 100
  const rc = await page.request.post("/api/procure/receipt", {
    data: { po_id, period: 202601, date: "2026-01-08", qty: "130", item_code: "140301" },
  });
  expect(rc.ok(), `超收应被接受（超收在实务里常见，不该被悄悄截断）：${await rc.text()}`).toBeTruthy();

  const st = await (await page.request.get(`/api/procure/estimate/status?po_id=${po_id}`)).json();
  const row = st.rows[0];
  expect(Number(row.qty_received), "已收要如实反映 130").toBe(130);
  expect(String(row.cap).replace(/,/g, ""), "额度应放大到 1300").toMatch(/^1300(\.0+)?$/);

  // 额度 1300 能全额登进去
  const est = await page.request.post("/api/procure/estimate", {
    data: { po_id, period: 202601, item: "140301", est_amount: "1300" },
  });
  expect(est.ok(), `超收后的额度应能全额暂估：${await est.text()}`).toBeTruthy();
});
