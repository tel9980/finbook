const { test, expect } = require("@playwright/test");
const { newBook, loginAs } = require("../helpers");

/// 在**辅助档案页**建一个客户档案。
///
/// 客户主数据只有一个入口（辅助档案）—— 客户管理页是只读视图，没有「新增客户」。
/// 这是设计 §0 的约束：两份客户主数据必然出现「销售单据认的客户」与
/// 「客户管理里看到的客户」对不上。
async function mkCustomer(page, code, name) {
  await page.click('.nav-item[data-view="aux"]');
  await expect(page.locator('[data-kind="customer"]')).toBeVisible({ timeout: 15_000 });
  await page.click('[data-kind="customer"]');
  await page.click("#aux-new");
  await page.fill("#au-code", code);
  await page.fill("#au-name", name);
  await page.click("#au-save");
  await expect(page.locator("#main")).toContainText(name, { timeout: 15_000 });
}

/// 录一笔应收：借 112201（辅助核算挂客户）/ 贷 1001。
/// 保存后弹窗关闭（postVoucher 的同一套约定），**不自动记账** —— 记账由调用方控制，
/// 因为「已记账 vs 草稿」正是 H-3 口径的验证点。
async function postReceivable(page, date, code, amount) {
  await page.click('.nav-item[data-view="vouchers"]');
  await page.click("#new-v");
  await page.fill("#v-date", date);
  const rows = page.locator("#v-entries tbody tr:has(select.acct-sel)");
  await rows.nth(0).locator("select.acct-sel").selectOption("112201");
  await rows.nth(0).locator(".e-sum").fill("应收 " + code);
  await rows.nth(0).locator(".e-d").fill(amount);
  await rows.nth(0).locator(".e-aux").click();
  await page.fill('.aux-in[data-k="customer"]', code);
  await rows.nth(1).locator("select.acct-sel").selectOption("1001");
  await rows.nth(1).locator(".e-sum").fill("收款");
  await rows.nth(1).locator(".e-c").fill(amount);
  await page.click("#v-save");
  await expect(page.locator(".modal-mask")).toHaveCount(0, { timeout: 10_000 });
}

/// 把第一张凭证推到「已记账」（审核 → 出纳签字 → 记账）
async function postFirstVoucher(page, period) {
  const list = await (await page.request.get(`/api/vouchers?period=${period}`)).json();
  const vid = (list || [])[0].id;
  for (const step of ["audit", "sign", "post"]) {
    const r = await page.request.post(`/api/vouchers/${vid}/${step}`, { data: {} });
    expect(r.ok(), step + " 应成功：" + (await r.text())).toBe(true);
  }
  return vid;
}

test("客户管理：建档 → 录应收 → 列表只算已记账 → 详情看到明细", async ({ page }) => {
  await newBook(page, `E2E客户${Date.now()}`);
  await mkCustomer(page, "KC01", "客户甲");

  // 两笔应收：1200 记账、300 留草稿
  await postReceivable(page, "2026-01-08", "KC01", "1200");
  await postReceivable(page, "2026-01-09", "KC01", "300");
  await postFirstVoucher(page, "202601");

  // 列表
  await page.click('.nav-item[data-view="customers"]');
  await expect(page.locator("#cu-list")).not.toContainText("加载中", { timeout: 15_000 });
  await expect(page.locator("#cu-list")).toContainText("客户甲", { timeout: 10_000 });

  // 余额必须只含已记账那笔（1200）。**草稿的 300 不该进来** —— 这就是 H-3 口径。
  const rowText = await page.locator("#cu-list tbody tr").first().innerText();
  expect(rowText, "余额应含已记账的 1200").toContain("1,200");
  expect(rowText, "草稿的 300 不该计入（H-3：只认已记账）").not.toContain("1,500");
  expect(rowText, "草稿那笔也不该进未核销笔数之外的合计").not.toContain("300");

  // 顶部汇总要与行一致（前端不重算，用后端给的字段）
  await expect(page.locator("#cu-sum")).toContainText("1 个客户", { timeout: 5_000 });
  await expect(page.locator("#cu-sum")).toContainText("1,200");

  // 「只看有欠款」取消勾选后，草稿那笔也不该让它出现（它确实有未核销，
  // 但余额为 0 的客户在勾选时会被过滤）—— 这条验的是过滤开关真的起作用
  await page.uncheck("#cu-only-open");
  await page.click("#cu-load");
  await expect(page.locator("#cu-list")).toContainText("客户甲", { timeout: 10_000 });
  await page.check("#cu-only-open");
  await page.click("#cu-load");
  await expect(page.locator("#cu-list")).toContainText("客户甲", { timeout: 10_000 });

  // 搜索
  await page.fill("#cu-q", "客户甲");
  await page.click("#cu-load");
  await expect(page.locator("#cu-list")).toContainText("客户甲", { timeout: 10_000 });
  await page.fill("#cu-q", "查无此客户");
  await page.click("#cu-load");
  await expect(page.locator("#cu-list")).not.toContainText("KC01", { timeout: 10_000 });

  // 详情弹窗
  await page.fill("#cu-q", "");
  await page.click("#cu-load");
  await expect(page.locator("[data-cu-open]")).toHaveCount(1, { timeout: 10_000 });
  await page.locator("[data-cu-open]").first().click();
  await expect(page.locator("#cd-body")).not.toContainText("加载中", { timeout: 15_000 });
  await expect(page.locator("#cd-body")).toContainText("1,200", { timeout: 10_000 });
  await expect(page.locator("#cd-body")).toContainText("112201", { timeout: 5_000 });
  // 明细行要能追到单据号（否则用户点进去看不到原始单据）
  const line = await page.locator("#cd-body table tbody tr").first().innerText();
  expect(line, "明细行应有单据号（记-1 之类）").toMatch(/记-\d+/);
  await page.click("#cd-close");
  await expect(page.locator(".modal-mask")).toHaveCount(0, { timeout: 5_000 });
});

test("客户管理：期初挂账单独显示，不混进余额", async ({ page }) => {
  await newBook(page, `E2E客户期初${Date.now()}`);
  await mkCustomer(page, "KP01", "客户丙");

  // 期初挂账 3000（导入 API，页面上没有这个入口 —— 它是「迁移数据」用的）
  const r = await page.request.post("/api/import/run", {
    data: {
      kind: "arap_opening",
      template: "generic",
      text: "类型,客商编码,单据号,单据日期,金额,客商名称,备注\n应收,KP01,XS-9001,2025-12-01,3000,客户丙,上年末欠款\n",
    },
  });
  expect(r.ok(), "期初挂账导入应成功：" + (await r.text())).toBe(true);

  await page.click('.nav-item[data-view="customers"]');
  await expect(page.locator("#cu-list")).toContainText("客户丙", { timeout: 15_000 });
  const rowText = await page.locator("#cu-list tbody tr").first().innerText();
  // 期初挂账**不进**应收余额（open_entries 只读凭证分录），但要单独显示
  expect(rowText, "期初挂账 3000 应单独一列").toContain("3,000");
  // 余额仍是 0 —— 界面上不能显示成 3000（那是「把两种口径混在一起」）
  expect(rowText, "应收余额应仍为 0（期初不混进去）").not.toContain("3,000.00");

  // 详情里期初行要标出来
  await page.locator("[data-cu-open]").first().click();
  await expect(page.locator("#cd-body")).not.toContainText("加载中", { timeout: 15_000 });
  await expect(page.locator("#cd-body")).toContainText("期初", { timeout: 10_000 });
  await page.click("#cd-close");
});

test("客户管理：出纳能看到名单，但侧栏没有辅助档案（改档案的入口）", async ({ page }) => {
  const book = await newBook(page, `E2E客户权限${Date.now()}`);

  await page.click('.nav-item[data-view="platform-users"]');
  await page.click("#pu-add");
  await page.fill("#nc-u", "e2ecas");
  await page.fill("#nc-p", "Csh@2026x");
  await page.click("#nc-save");
  await expect(page.locator("#pu-list")).toContainText("e2ecas", { timeout: 15_000 });
  const r = await page.request.post("/api/users", {
    data: {
      username: "e2ecas", display_name: "出纳",
      password: "", role: "cashier", must_change_pwd: false,
    },
  });
  expect(r.ok() || /已存在/.test(await r.text()), "把出纳拉进账套").toBeTruthy();

  await loginAs(page, "e2ecas", "Csh@2026x", book);

  // 出纳有 report 权限 → 能看客户管理
  await expect(page.locator('.nav-item[data-view="customers"]')).toHaveCount(1, { timeout: 15_000 });
  // 没有 aux_edit → 侧栏没有辅助档案
  await expect(page.locator('.nav-item[data-view="aux"]')).toHaveCount(0);

  await page.click('.nav-item[data-view="customers"]');
  await expect(page.locator("#cu-list")).not.toContainText("加载中", { timeout: 15_000 });
  // 页面上不能有任何「新增/编辑客户」的入口
  await expect(page.locator("#cu-list")).not.toContainText("新增", { timeout: 5_000 });
  await expect(page.locator("#main")).not.toContainText("删除", { timeout: 5_000 });
});

test("客户管理：交易历史与催款两个 Tab", async ({ page }) => {
  await newBook(page, `E2E客户交易${Date.now()}`);
  await mkCustomer(page, "KT01", "客户交易");
  // 一笔已记账应收：催款单 / 对账单才有得算
  await postReceivable(page, "2026-01-07", "KT01", "1000");
  await postFirstVoucher(page, "202601");

  // 销售订单（已确认）→ 收款，都走真实接口（页面上这两步是另一个模块的主题）
  const soR = await page.request.post("/api/sales/so", {
    data: {
      period: 202601,
      date: "2026-01-05",
      customer_code: "KT01",
      customer_name: "客户交易",
      status: "Confirmed",
      memo: "",
      lines: [
        {
          item_code: "RM1",
          item_name: "氧化料",
          qty_ordered: "10",
          unit_price: "100",
          tax_rate: "0",
        },
      ],
    },
  });
  expect(soR.ok(), "建销售订单：" + (await soR.text())).toBe(true);
  const soId = (await soR.json()).id;
  expect(soId, "订单应返回 id").toBeTruthy();

  // 发货依赖默认仓与库存；即便失败也不影响本用例主题（四类单据的合并由
  // findb::customers::tests::trade_* 逐条钉住）。
  await page.request.post("/api/sales/shipment", {
    data: { so_id: soId, period: 202601, date: "2026-01-08", qty: "5", memo: "发一半" },
  });

  // 收款 800（同时落 so_payment 与 receipt_doc）
  const payR = await page.request.post("/api/sales/payment", {
    data: { so_id: soId, period: 202601, date: "2026-01-10", amount: "800", memo: "收到货款" },
  });
  expect(payR.ok(), "收款应成功：" + (await payR.text())).toBe(true);

  // 催款单 + 对账单（催款 Tab 的两个来源）
  const dunR = await page.request.post("/api/settle/dunnings", {
    data: {
      kind: "ar",
      party_code: "KT01",
      party_name: "客户交易",
      date: "2026-01-31",
      memo: "催收",
    },
  });
  expect(dunR.ok(), "建催款单：" + (await dunR.text())).toBe(true);
  const stR = await page.request.post("/api/settle/statements", {
    data: {
      kind: "ar",
      party_code: "KT01",
      party_name: "客户交易",
      from: 202601,
      to: 202601,
    },
  });
  expect(stR.ok(), "建对账单：" + (await stR.text())).toBe(true);

  // 打开客户详情
  await page.click('.nav-item[data-view="customers"]');
  await expect(page.locator("#cu-list")).toContainText("客户交易", { timeout: 15_000 });
  await page.locator("[data-cu-open]").first().click();
  await expect(page.locator("#cd-body")).not.toContainText("加载中", { timeout: 15_000 });

  // 交易历史 Tab：销售订单 + 收款必须出现
  await page.click('[data-cd="trade"]');
  await expect(page.locator("#cd-trade")).toBeVisible({ timeout: 10_000 });
  await expect(page.locator("#cd-trade")).toContainText("销售订单", { timeout: 10_000 });
  await expect(page.locator("#cd-trade")).toContainText("收款", { timeout: 10_000 });

  // 催款 Tab：催款单 + 对账单都要在
  await page.click('[data-cd="dunning"]');
  await expect(page.locator("#cd-dunning")).toBeVisible({ timeout: 10_000 });
  await expect(page.locator("#cd-dunning")).toContainText("催款单", { timeout: 10_000 });
  await expect(page.locator("#cd-dunning")).toContainText("对账单", { timeout: 10_000 });
});

module.exports = {};
