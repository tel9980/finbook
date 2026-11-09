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
  // 保存被拒时界面**故意不关窗口**（留着让人改数字），但遮罩会挡住背后的工具栏，
  // 所以要先自己关掉再点查询——不关就是等点击超时，报错还指在下一行上。
  await page.click("#cs-cancel");
  await page.click("#cs-load");
  await expect(page.locator("#cs-docs tbody tr")).toHaveCount(2, { timeout: 15_000 });
  // 再问一次服务端：列表条数只说明渲染了几行，入库了没有得看接口
  const afterOver = await (await page.request.get("/api/consign/docs")).json();
  expect(
    (afterOver.docs || []).map((d) => d.no),
    "超交被拒后不该多出一张单据",
  ).toHaveLength(2);

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

test("加工费单价：矩阵、折算、起步价兜底、加急费", async ({ page }) => {
  await newBook(page, `E2E加工费${Date.now()}`);
  await page.click('.nav-item[data-view="price-rules"]');
  // v1.8.87 起这页是「单价矩阵」：一行物料、一列口径。空账套先给一句人话。
  await expect(page.locator("#pr-matrix")).toContainText("还没有任何单价", { timeout: 15_000 });

  // 氧化厂典型规则：18 元/㎡，起步 30 元，加急 20 元 + 10%。
  // 矩阵里改价点格子，但「起步价 / 加急」这些小格子放不下，走「新增规则（高级）」。
  await page.click("#pr-new");
  await page.fill("#pr-name", "氧化-普通");
  await page.fill("#pr-unit", "㎡");
  await page.fill("#pr-price", "18");
  await page.fill("#pr-min", "30");
  await page.fill("#pr-rfee", "20");
  await page.fill("#pr-rrate", "10");
  await page.click("#pr-save");
  // 通配规则（不填物料）在矩阵里单独一行
  await expect(page.locator("#pr-matrix")).toContainText("任意物料（通配价）", { timeout: 15_000 });
  await expect(page.locator("#pr-matrix .pr-cell").first()).toHaveValue("18", { timeout: 15_000 });

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
  await page.uncheck("#pr-q-rush");

  // 折算系数：1 只 = 0.0234 ㎡。开单填 300 只，系统自己折成 7.02 ㎡。
  await page.click("#pr-new-unit");
  await page.fill("#pu-item", "MB01");
  await page.fill("#pu-unit", "㎡");
  await page.fill("#pu-factor", "0.0234");
  await page.fill("#pu-scale", "2");
  await page.click("#pu-save");
  await expect(page.locator("#pr-units")).toContainText("MB01", { timeout: 15_000 });
  await expect(page.locator("#pr-matrix")).toContainText("MB01", { timeout: 15_000 });

  await page.fill("#pr-q-item", "MB01");
  await page.fill("#pr-q-qty", "300");
  await page.selectOption("#pr-q-unit", "㎡");
  await page.click("#pr-q-run");
  // 300 × 0.0234 = 7.02；7.02 × 18 = 126.36
  await expect(page.locator("#pr-q-out")).toContainText("7.02", { timeout: 15_000 });
  await expect(page.locator("#pr-q-out")).toContainText("126.36", { timeout: 15_000 });

  // 加急率填 120% → 保存要被拦，且提示里说清合法区间
  await page.click("#pr-new");
  await page.fill("#pr-name", "乱填");
  await page.fill("#pr-price", "1");
  await page.fill("#pr-rrate", "120");
  await page.click("#pr-save");
  await expect(page.locator("#toast")).toContainText("0-100", { timeout: 15_000 });
  // 被拒时窗口不关，遮罩挡着后面的按钮，先关窗
  await page.click("#pr-cancel2");
  // 规则数仍是 1（没存进去）：矩阵里的通配行仍在，且没有第二行通配
  await expect(page.locator("#pr-matrix tbody tr")).toHaveCount(2, { timeout: 15_000 });
  const afterBad = await (await page.request.get("/api/price-rules")).json();
  expect(
    (afterBad.rules || []).map((r) => r.name),
    "加急率越界的规则不该存进去",
  ).toHaveLength(1);
});


test("批量导入单价：粘一张表进来（表头跳过、坏行只跳自己）", async ({ page }) => {
  await newBook(page, `E2E导入${Date.now()}`);
  await page.click('.nav-item[data-view="price-rules"]');
  await expect(page.locator("#pr-matrix")).toContainText("还没有任何单价", { timeout: 15_000 });

  await page.click("#pr-import");
  // 模拟「从 Excel 里复制三行粘进来」：带表头 + 两行正常 + 一行坏数据
  await page.fill(
    "#pi-text",
    "物料编码,客户编码,计价口径,单价,起步价\nMB09,C01,㎡,20,50\nMB09,C01,只,0.5,\n坏行,abc\n",
  );
  await page.click("#pi-run");
  await expect(page.locator("#pi-out")).toContainText("新增", { timeout: 15_000 });
  await expect(page.locator("#pi-out")).toContainText("跳过", { timeout: 15_000 });
  // 坏行要说清是第几行、坏在哪 —— 只报「有 1 行失败」用户没法修
  await expect(page.locator("#pi-out")).toContainText("第 4 行", { timeout: 15_000 });
  await page.click("#pi-cancel");

  // 导进来的价要立刻出现在矩阵里，而且价是对的那个数
  await expect(page.locator("#pr-matrix")).toContainText("MB09", { timeout: 15_000 });
  // 一行两个口径，价都要对（列的顺序按「只 / 米 / kg / ㎡」固定，别按位置断言）
  const row = page.locator("#pr-matrix tbody tr", { hasText: "MB09" }).first();
  const vals = await row.locator("input.pr-cell").evaluateAll((els) => els.map((e) => e.value));
  expect(vals.sort(), "MB09 的两个口径价").toEqual(["0.5", "20"]);
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

  // 报工流水的期间跟**订单**走（v1.8.69 起），不再是"今天所在期间"。
  // 所以这里必须用账套当前期间，否则计件汇总查的是一个没有流水的期间。
  const prodList = await (await page.request.get("/api/prod")).json();
  const period = prodList.period;
  expect(period, "账套当前期间：" + JSON.stringify(prodList)).toMatch(/^\d{4}-\d{2}$/);

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
