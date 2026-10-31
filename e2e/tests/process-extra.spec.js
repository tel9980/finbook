const { test, expect } = require("@playwright/test");
const { newBook } = require("../helpers");

// v1.8.63 的三块「受托加工现场」功能此前只有单测 + API 用例守着，UI 层零覆盖。
// 这类功能的失效形态恰好是接口层看不见的：按钮点了没反应、模态框绑错字段、
// 保存后列表不刷新、超交只弹一句 toast 但单据其实存进去了。
// 所以每条用例都真的点页面；接口只用来造前置数据（工艺路线、报工这种多步表单）。

async function fillConsign(page, kind, qty, item) {
  await page.click("#cs-new");
  await page.fill("#cs-customer", "华东氧化");
  await page.selectOption("#cs-kind", kind);
  await page.fill("#cs-date", "2026-01-05");
  await page.locator('#cs-lines [data-f="item"]').first().fill(item || "MB01");
  await page.locator('#cs-lines [data-f="item_name"]').first().fill("铝型材");
  await page.locator('#cs-lines [data-f="qty"]').first().fill(qty);
  await page.click("#cs-save");
}

test("客供料台账：来料 → 交货 → 超交被拦 → 结存与对账单", async ({ page }) => {
  await newBook(page, `E2E客供料${Date.now()}`);
  await page.click('.nav-item[data-view="consign"]');
  await expect(page.locator("#cs-docs")).toContainText("暂无客供料单据", { timeout: 15_000 });

  // 来料 100：单号按类型分前缀，来料是 LR
  await fillConsign(page, "in", "100");
  await expect(page.locator("#cs-docs")).toContainText("LR-202601-001", { timeout: 15_000 });

  await page.click("#cs-bal");
  await expect(page.locator("#cs-bals")).toContainText("100", { timeout: 15_000 });

  // 交货 60：JH 前缀
  await fillConsign(page, "out", "60");
  await expect(page.locator("#cs-docs")).toContainText("JH-202601-001", { timeout: 15_000 });

  await page.click("#cs-bal");
  await expect(page.locator("#cs-bals")).toContainText("40", { timeout: 15_000 });

  // 再交 50（只剩 40）→ 必须拦下，且**单据不能进库**
  //
  // 这里刻意同时判 toast 和列表条数：只判 toast 的话，「提示报错但单据已经存进去」
  // 这个 bug 会被判成通过（toast 只是个 DOM 元素，存没存要看列表）。
  await fillConsign(page, "out", "50");
  await expect(page.locator("#toast")).toContainText("超出可核销数量", { timeout: 15_000 });
  await page.click("#cs-load");
  await expect(page.locator("#cs-docs tbody tr")).toHaveCount(2, { timeout: 15_000 });

  // 客户对账单：期初 0 / 来料 100 / 交货 60 / 期末 40
  await page.click("#cs-stmt");
  await page.fill("#cs-st-cust", "华东氧化");
  await page.fill("#cs-from", "2026-01");
  await page.fill("#cs-to", "2026-01");
  await page.click("#cs-st-run");
  const stmt = page.locator("#cs-st-out");
  await expect(stmt).toContainText("MB01", { timeout: 15_000 });
  await expect(stmt).toContainText("100");
  await expect(stmt).toContainText("60");
  await expect(stmt).toContainText("40");
});

test("加工费规则：面积计价 → 最低收费兜底 → 加急费", async ({ page }) => {
  await newBook(page, `E2E加工费${Date.now()}`);
  await page.click('.nav-item[data-view="price-rules"]');
  await expect(page.locator("#pr-list")).toContainText("还没有加工费规则", { timeout: 15_000 });

  // 氧化厂典型规则：18 元/㎡，起步 30 元，加急 20 元 + 10%
  await page.click("#pr-new");
  await page.fill("#pr-name", "氧化-普通");
  await page.fill("#pr-unit", "㎡");
  await page.fill("#pr-price", "18");
  await page.fill("#pr-min", "30");
  await page.fill("#pr-rfee", "20");
  await page.fill("#pr-rrate", "10");
  await page.click("#pr-save");
  await expect(page.locator("#pr-list")).toContainText("氧化-普通", { timeout: 15_000 });

  // 12.5 ㎡ × 18 = 225，不触起步价
  await page.fill("#pr-q-qty", "12.5");
  await page.click("#pr-q-run");
  await expect(page.locator("#pr-q-out")).toContainText("225", { timeout: 15_000 });

  // 0.5 ㎡ × 18 = 9 → 补到起步价 30（补差 21）
  await page.fill("#pr-q-qty", "0.5");
  await page.click("#pr-q-run");
  await expect(page.locator("#pr-q-out")).toContainText("21", { timeout: 15_000 });
  await expect(page.locator("#pr-q-out")).toContainText("30");

  // 加急：225 + 22.5(10%) + 20 = 267.5
  await page.fill("#pr-q-qty", "12.5");
  await page.check("#pr-q-rush");
  await page.click("#pr-q-run");
  await expect(page.locator("#pr-q-out")).toContainText("267.5", { timeout: 15_000 });

  // 加急率填 120% → 保存要被拦，且提示里说清合法区间
  await page.click("#pr-new");
  await page.fill("#pr-name", "乱填");
  await page.fill("#pr-price", "1");
  await page.fill("#pr-rrate", "120");
  await page.click("#pr-save");
  await expect(page.locator("#toast")).toContainText("0-100", { timeout: 15_000 });
  // 规则数仍是 1（没存进去）
  await page.click("#pr-reload");
  await expect(page.locator("#pr-list tbody tr")).toHaveCount(1, { timeout: 15_000 });
});

test("计件工资：报工流水 → 按人汇总 → 回填工资表（幂等）", async ({ page }) => {
  await newBook(page, `E2E计件${Date.now()}`);

  // 前置数据走接口：工艺路线 → 生产订单（自动生成工序清单）→ 派工 → 报工
  const routing = await page.request.post("/api/routing/140501", {
    data: [
      { seq: 1, op_code: "OX", op_name: "氧化", work_center: "", std_hours: "0.5", rate: "0", qc_required: false },
    ],
  });
  expect(routing.ok(), "建工艺路线：" + (await routing.text())).toBe(true);

  const prod = await page.request.post("/api/prod", { data: { item_code: "140501", qty: "100", date: "" } });
  expect(prod.ok(), "下达生产订单：" + (await prod.text())).toBe(true);
  const pid = (await prod.json()).id;

  const ops = await (await page.request.get(`/api/prod/${pid}/ops`)).json();
  expect(ops.ops && ops.ops.length, "订单下达后应自动生成工序清单：" + JSON.stringify(ops)).toBeGreaterThan(0);
  const opId = ops.ops[0].id;

  const dispatch = await page.request.post("/api/prod/op/dispatch", { data: { op_id: opId, worker: "张三" } });
  expect(dispatch.ok(), "派工：" + (await dispatch.text())).toBe(true);

  const report = await page.request.post("/api/prod/op/report", { data: { op_id: opId, qty: "100", hours: "2" } });
  expect(report.ok(), "报工：" + (await report.text())).toBe(true);

  // 报工流水的期间取**当天**，不是账套当前期间——这里跟着当天的期间走
  const now = new Date();
  const period = `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}`;

  await page.click('.nav-item[data-view="piece-pay"]');
  await page.fill("#pp-period", period);

  // 计件单价：140501 的氧化工序 1.5 元/件
  await page.click("#pp-new");
  await page.fill("#ppr-item", "140501");
  await page.fill("#ppr-op", "氧化");
  await page.fill("#ppr-rate", "1.5");
  await page.click("#ppr-save");
  await expect(page.locator("#pp-rates")).toContainText("1.5", { timeout: 15_000 });

  // 算计件：张三 100 件 × 1.5 = 150
  await page.click("#pp-run");
  await expect(page.locator("#pp-out")).toContainText("张三", { timeout: 15_000 });
  await expect(page.locator("#pp-out")).toContainText("150");
  await expect(page.locator("#pp-out")).not.toContainText("无单价");

  // 回填：应发提到 150
  await page.click("#pp-fill");
  await page.click("#cf-ok");
  await expect(page.locator("#toast")).toContainText("已回填 1 人", { timeout: 15_000 });

  // 幂等：再回填一次，应发已够 → 跳过，不重复加钱
  await page.click("#pp-fill");
  await page.click("#cf-ok");
  await expect(page.locator("#toast")).toContainText("没有需要回填的人", { timeout: 15_000 });

  // 工资表里张三的应发就是 150（不是 300）
  const payroll = await (await page.request.get(`/api/payroll?period=${period}`)).json();
  const row = (Array.isArray(payroll) ? payroll : []).find((r) => r.employee === "张三");
  expect(row, "工资表里应有张三：" + JSON.stringify(payroll)).toBeTruthy();
  // Money 序列化不带尾零（"150" 而不是 "150.00"），按数值比
  expect(Number(String(row.gross).replace(/,/g, ""))).toBe(150);
});
