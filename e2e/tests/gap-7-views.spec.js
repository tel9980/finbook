const { test, expect } = require("@playwright/test");
const { newBook, postVoucher, postAllDrafts } = require("../helpers");

// 本文件覆盖的 7 个页面此前只有接口层测试，UI 层零覆盖。
// 每条用例都真的点页面，不走 page.request 抄近路；接口只用来造前置数据。

/// 先备一点库存，否则「批次台账」「分仓库存」这些页面只能验空表。
async function seedStock(page, item, qty) {
  const r = await page.request.post("/api/inventory/adjust", {
    data: { period: 202601, date: "2026-01-05", item, qty: "1", delta: qty, memo: "E2E备货" },
  });
  expect(r.ok(), "备库存应成功：" + r.status() + " " + (await r.text())).toBe(true);
}

test("仓库档案：默认仓只读 → 新增 → 设默认 → 改回 → 删除", async ({ page }) => {
  await newBook(page, `E2E仓库${Date.now()}`);
  await page.click('.nav-item[data-view="warehouses"]');

  // 建账后应自动有默认仓 01
  await expect(page.locator("#wh-list")).toContainText("主仓", { timeout: 15_000 });
  // 默认仓不提供「删除」按钮（被流水/被设为默认的仓不可删）
  await expect(page.locator('[data-wh-del="01"]')).toHaveCount(0);

  await page.click("#wh-new");
  await page.fill("#wh-code", "02");
  await page.fill("#wh-name", "成品仓");
  await page.click("#wh-save");
  await expect(page.locator("#wh-list")).toContainText("成品仓", { timeout: 15_000 });

  // 设默认 → 默认标记要跟着挪走，且**任何时刻只能有一个**。
  //
  // 这里刻意不判 toast：「已设为默认仓」是 `#toast` 里的提示，而表格本身
  // 才是状态的权威。写 `expect(#wh-list).toContainText("已设为默认仓")`
  // 会永远失败 —— 我第一版就是这么写的（toast 早就消失了，表格里根本没有这句话）。
  const defaultsOf = () =>
    page.evaluate(() =>
      Array.from(document.querySelectorAll("#wh-list tbody tr"))
        .filter((tr) => tr.querySelector(".tag.ok"))
        .map((tr) => tr.querySelector("td").textContent.trim())
    );
  // 等**状态**而不是等固定时间：`click` 只保证点击已发出，不保证重绘完成。
  // 第一版 click 完立刻读，读到的是上一次渲染的旧表格 —— 于是这条用例
  // 在全量跑（8.7 分钟、机器忙）时 flaky、单跑时又稳定通过。
  // flaky 的用例和失败的用例一样没用：它要么一直绿、要么偶尔红，没人能靠它判断。
  const expectDefault = async (code) =>
    expect
      .poll(() => defaultsOf(), { timeout: 15_000 })
      .toEqual([code]);

  await page.locator('[data-wh-def="02"]').click();
  await expectDefault("02");

  // 改回 01 为默认 → 02 才拿到删除按钮（默认仓不可删）
  await page.locator('[data-wh-def="01"]').click();
  await expectDefault("01");
  await expect(page.locator('[data-wh-del="02"]')).toBeVisible({ timeout: 15_000 });

  await page.locator('[data-wh-del="02"]').click();
  await page.click("#cf-ok");
  await expect(page.locator("#wh-list")).not.toContainText("成品仓", { timeout: 15_000 });
});

test("存货档案：新增 → 搜索 → 行内编辑回显计划参数 → 只看低库存", async ({ page }) => {
  await newBook(page, `E2E存货档案${Date.now()}`);
  await page.click('.nav-item[data-view="items-master"]');
  await expect(page.locator("#im-list")).not.toContainText("加载中", { timeout: 15_000 });

  // 新增：保质期 + 安全库存 + 前置期 + 批量
  await page.click("#im-new");
  await page.fill("#au-code", "IT01");
  await page.fill("#au-name", "测试原料");
  await page.fill("#au-shelf", "30");
  await page.fill("#au-safety", "100");
  await page.fill("#au-lead", "5");
  await page.fill("#au-lot", "20");
  await page.click("#au-save");
  await expect(page.locator("#im-list")).toContainText("测试原料", { timeout: 15_000 });

  // 搜索过滤
  await page.fill("#im-q", "IT01");
  await expect(page.locator("#im-list")).toContainText("IT01", { timeout: 5_000 });
  await expect(page.locator("#im-list")).not.toContainText("不存在的东西");
  await page.fill("#im-q", "");
  await expect(page.locator("#im-list")).toContainText("测试原料", { timeout: 5_000 });

  // 只看低库存：现量 0 < 安全库存 100 → 应命中
  await page.check("#im-low");
  await expect(page.locator("#im-list")).toContainText("低库存", { timeout: 5_000 });
  await page.uncheck("#im-low");

  // 行内编辑：保质期等字段要能回显，且存的是计划参数那三栏
  await page.locator("[data-im-edit]").first().click();
  await expect(page.locator("#au-code")).toHaveValue("IT01", { timeout: 10_000 });
  await expect(page.locator("#au-name")).toHaveValue("测试原料");
  await expect(page.locator("#au-shelf")).toHaveValue("30", { timeout: 5_000 });
  // 计划参数是异步回显的：保存时另发一次 /item-plan，这里等它到位
  await expect(page.locator("#au-safety")).toHaveValue("100", { timeout: 10_000 });
  await expect(page.locator("#au-lead")).toHaveValue("5");
  await expect(page.locator("#au-lot")).toHaveValue("20");
  await page.fill("#au-name", "测试原料改");
  await page.click("#au-save");
  await expect(page.locator("#im-list")).toContainText("测试原料改", { timeout: 15_000 });
});

test("批次库位：登记入库 → 保质期推失效 → 临期 → FEFO → 出库 → 库位主数据", async ({ page }) => {
  await newBook(page, `E2E批次${Date.now()}`);

  // 先在**存货档案页**建档并配保质期 30 天。
  // 走界面而不是打接口：① 跨页数据联动（档案页建的东西批次页要用）
  // 本来就该验；② 我第一版用 POST /api/aux 建档，props 没落库，
  // 结果失效日期是「—」—— 那是**我的用例错了**，不是产品的锅。
  await page.click('.nav-item[data-view="items-master"]');
  await page.click("#im-new");
  await page.fill("#au-code", "RM01");
  await page.fill("#au-name", "带保质期原料");
  await page.fill("#au-shelf", "30");
  await page.click("#au-save");
  await expect(page.locator("#im-list")).toContainText("带保质期原料", { timeout: 15_000 });

  await page.click('.nav-item[data-view="inv-batch"]');
  await expect(page.locator("#bt-list")).not.toContainText("加载中", { timeout: 15_000 });

  // 登记入库（批号留空 → 自动生成）
  await page.fill("#bt-item", "RM01");
  await page.fill("#bt-prod", "2026-01-05");
  await page.fill("#bt-qty", "100");
  await page.click("#bt-reg");
  await expect(page.locator("#bt-list")).toContainText("RM01", { timeout: 15_000 });

  // 生产日期 + 保质期 → 失效日期必须是 2026-02-04（不是空，也不是今天）
  await expect(page.locator("#bt-list")).toContainText("2026-02-04", { timeout: 5_000 });

  // 再入一个更早失效的批次，FEFO 必须先推它
  await page.fill("#bt-item", "RM01");
  await page.fill("#bt-no", "EARLY");
  await page.fill("#bt-prod", "2026-01-01");
  await page.fill("#bt-qty", "50");
  await page.click("#bt-reg");
  await expect(page.locator("#bt-list")).toContainText("EARLY", { timeout: 15_000 });
  await expect(page.locator("#bt-list")).toContainText("2026-01-31", { timeout: 5_000 });

  await page.fill("#bt-item", "RM01");
  await page.fill("#bt-fq", "60");
  await page.click("#bt-fefo");
  await expect(page.locator("#bt-fefo-out")).toContainText("EARLY", { timeout: 15_000 });

  // 临期：窗口放到 9999 天，两条都该命中
  await page.fill("#bt-days", "9999");
  await page.click("#bt-exp");
  await expect(page.locator("#bt-list")).toContainText("临期", { timeout: 15_000 });

  // 出库 20 → EARLY 余额 30
  await page.selectOption("#bt-dir", "out");
  await page.fill("#bt-item", "RM01");
  await page.fill("#bt-no", "EARLY");
  await page.fill("#bt-qty", "20");
  await page.click("#bt-reg");
  await expect(page.locator("#bt-list")).toContainText("EARLY", { timeout: 15_000 });

  // 库位主数据：新增后要出现在下拉与列表里
  await page.click("#bt-loc-new");
  await page.fill("#lo-code", "A01");
  await page.fill("#lo-name", "A区货位");
  await page.selectOption("#lo-kind", "storage");
  await page.click("#lo-save");
  await expect(page.locator("#bt-locs")).toContainText("A01", { timeout: 15_000 });
  await expect(page.locator("#bt-loc")).toContainText("A01", { timeout: 5_000 });

  // 批次成本勾稽区块也要能渲染出数（不是空白）
  await expect(page.locator("#bt-cost")).toContainText("批次", { timeout: 15_000 });
});

test("存货盘点：新建 → 账面快照 → 应用出凭证 → 已应用不可再删", async ({ page }) => {
  await newBook(page, `E2E盘点${Date.now()}`);
  // 差异 × 标准价 = 凭证金额，所以要先配标准价，否则只调流水不出凭证
  const cfg = await page.request.post("/api/cost/configs", {
    data: { item: "140301", method: "moving_average", standard_cost: "10" },
  });
  expect(cfg.ok(), "配标准价应成功：" + (await cfg.text())).toBe(true);

  await page.click('.nav-item[data-view="inv-count"]');
  await expect(page.locator("#ic-list")).not.toContainText("加载中", { timeout: 15_000 });
  await expect(page.locator("#ic-list")).toContainText("暂无盘点单", { timeout: 5_000 });

  await page.click("#ic-new");
  await page.fill("#cn-date", "2026-01-20");
  await page.fill("#cn-memo", "E2E 盘点");
  await page.locator('#cn-tbl input[data-f="item"]').first().fill("140301");
  await page.locator('#cn-tbl input[data-f="count_qty"]').first().fill("5");
  await page.click("#cn-save");
  await expect(page.locator("#ic-list")).toContainText("PD", { timeout: 15_000 });
  await expect(page.locator("#ic-list")).toContainText("草稿", { timeout: 5_000 });
  // 账面 0 → 实盘 5，差异 +5 必须显示出来
  await expect(page.locator("#ic-list")).toContainText("+5", { timeout: 5_000 });

  // 应用 → 出盘盈凭证（差异 5 × 标准价 10 = 50）
  await page.locator("[data-ic-apply]").first().click();
  await expect(page.locator("#ic-list")).toContainText("已应用", { timeout: 15_000 });
  await expect(page.locator("#ic-list")).toContainText("凭证 #", { timeout: 5_000 });

  // 凭证链接要能真的打开那张凭证
  await page.locator("[data-ic-v]").first().click();
  await expect(page.locator("#v-entries")).toContainText("140301", { timeout: 15_000 });
  await page.click("#v-close");

  // 已应用：不再有删除/应用按钮
  await expect(page.locator("[data-ic-apply]")).toHaveCount(0, { timeout: 5_000 });
  await expect(page.locator("[data-ic-del]")).toHaveCount(0);

  // 接口层也要拒（页面不给按钮了，但直接打接口仍须被拒）
  const list = await (await page.request.get("/api/inventory/counts")).json();
  const cid = list.rows[0].id;
  const del = await page.request.post(`/api/inventory/count/${cid}/delete`, { data: {} });
  expect(del.status(), "已应用的盘点单不应能删").toBe(400);
  const again = await page.request.post(`/api/inventory/count/${cid}/apply`, { data: {} });
  expect(again.status(), "已应用的盘点单不应能重复应用").toBeGreaterThanOrEqual(400);
});

test("MPS 排产：手工需求运算 → 下达成生产订单 → 粗排写回计划日期", async ({ page }) => {
  await newBook(page, `E2E排产${Date.now()}`);
  await page.click('.nav-item[data-view="mps"]');
  await expect(page.locator("#mps-table")).not.toContainText("加载中", { timeout: 15_000 });
  await expect(page.locator("#mps-table")).toContainText("暂无结果", { timeout: 5_000 });

  // 不勾销售、纯手工需求（否则后端会因为既无手工也无销售未发量而 400）
  await page.uncheck("#mps-sales");
  await page.fill("#mps-item", "140301");
  await page.fill("#mps-qty", "6");
  await page.fill("#mps-due", "2026-02-01");
  await page.click("#mps-run");
  await expect(page.locator("#mps-table")).toContainText("140301", { timeout: 15_000 });
  // 无库存无在制 → 计划量 = 需求 6
  await expect(page.locator("#mps-table")).toContainText("待下达", { timeout: 5_000 });

  // 下达 → 生成生产订单，行状态变「已下达」，计划量转 MRP 才有东西可转
  await page.locator("[data-mps-go]").first().click();
  await expect(page.locator("#mps-table")).toContainText("已下达", { timeout: 15_000 });
  await expect(page.locator("#mps-table")).not.toContainText("待下达", { timeout: 5_000 });

  // 计划量转 MRP
  await page.locator("#mps-tomrp").click();
  await expect(page.locator("#toast")).toContainText("MRP", { timeout: 15_000 });

  // 粗排：日产 2 件 × 未完 6 件 → 需 3 天；应用后细排结果里能看到计划日期
  await page.fill("#rq-daily", "2");
  await page.click("#rq-run");
  await expect(page.locator("#rq-table")).toContainText("140301", { timeout: 15_000 });
  await page.click("#rq-apply-all");
  await expect(page.locator("#sch-table")).toContainText("140301", { timeout: 15_000 });
  await expect(page.locator("#sch-table")).toContainText("计划开工", { timeout: 5_000 });

  // 「最近一批」要能重读出同样的结果（不是只存在于内存）
  await page.click("#mps-latest");
  await expect(page.locator("#mps-table")).toContainText("140301", { timeout: 15_000 });
});

test("工作流：新建流程 → 加审批节点 → 保存草稿 → 发布 → 撤回 → 删除", async ({ page }) => {
  await newBook(page, `E2E工作流${Date.now()}`);
  await page.click('.nav-item[data-view="workflow"]');
  await expect(page.locator("#wf-body")).not.toContainText("加载中", { timeout: 15_000 });
  await expect(page.locator("#wf-body")).toContainText("暂无流程", { timeout: 5_000 });

  await page.click("#wf-new");
  await page.fill("#wf-name", "E2E 报销流程");
  await page.selectOption("#wf-biz", "claim");
  await page.locator('[data-add="approve"]').click();
  // 没连线也没关系：保存只要求至少有一个开始节点（默认就有）
  await page.click("#wf-save");
  await expect(page.locator("#wf-body")).toContainText("E2E 报销流程", { timeout: 15_000 });
  await expect(page.locator("#wf-body")).toContainText("草稿", { timeout: 5_000 });
  // 节点数 = 开始 + 新加的审批
  await expect(page.locator("#wf-body tbody tr")).toContainText("/ 0", { timeout: 5_000 });

  // 发布 → 状态变已发布，按钮换成「撤回」
  await page.locator("[data-wf-pub]").first().click();
  await expect(page.locator("#wf-body")).toContainText("已发布", { timeout: 15_000 });
  await expect(page.locator("[data-wf-unpub]")).toHaveCount(1, { timeout: 5_000 });

  // 撤回 → 恢复草稿
  await page.locator("[data-wf-unpub]").first().click();
  await expect(page.locator("#wf-body")).toContainText("草稿", { timeout: 15_000 });
  await expect(page.locator("[data-wf-pub]")).toHaveCount(1, { timeout: 5_000 });

  // 运行实例页：没跑过实例时要有明确空态，不是「加载中」卡住
  await page.click("#wf-i");
  await expect(page.locator("#wf-body")).toContainText("暂无运行实例", { timeout: 15_000 });
  await page.click("#wf-d");

  // 删除（带确认）
  await page.locator("[data-wf-del]").first().click();
  await page.click("#cf-ok");
  await expect(page.locator("#wf-body")).toContainText("暂无流程", { timeout: 15_000 });
});

test("工作流：从模板创建（未选单据类型应被拒，而不是静默成功）", async ({ page }) => {
  await newBook(page, `E2E工作流模板${Date.now()}`);
  await page.click('.nav-item[data-view="workflow"]');
  await expect(page.locator("#wf-body")).not.toContainText("加载中", { timeout: 15_000 });

  await page.click("#wf-tpl");
  await expect(page.locator("#wt-list")).not.toContainText("加载中", { timeout: 15_000 });

  // 不选单据类型就点「用此模板创建」→ 必须被拒（否则会给所有单据类型各建一条流程）
  await page.locator("[data-wt-apply]").first().click();
  await expect(page.locator("#toast")).toContainText("请先选择", { timeout: 10_000 });
  await expect(page.locator(".modal-mask")).toHaveCount(1, { timeout: 5_000 });

  // 选了才建得出来
  const radio = page.locator('#wt-list input[type="radio"]').first();
  await radio.check();
  await page.locator("[data-wt-apply]").first().click();
  await expect(page.locator("#wf-body")).toContainText("草稿", { timeout: 15_000 });
  // 模板建出来的是**草稿**：不发布就不生效，业务单据仍走默认审批
  await expect(page.locator("#wf-body")).toContainText("草稿", { timeout: 5_000 });
});

test("合并报表：两个账套各记一笔 → 汇总表按科目跨套相加", async ({ page }) => {
  const b1 = await newBook(page, `E2E合并A${Date.now()}`);

  // b1：借 1001 / 贷 2001 各 100
  await postVoucher(page, {
    date: "2026-01-15",
    rows: [
      { code: "1001", summary: "合并造数", debit: "100" },
      { code: "2001", summary: "合并造数", credit: "100" },
    ],
    post: true,
  });

  // 建第二个账套（consolidate 是平台管理员功能，需要两套才有意义）
  const created = await page.request.post("/api/books", {
    data: { company: `E2E合并B${Date.now()}`, start_period: 202601 },
  });
  expect(created.ok(), "建第二个账套应成功：" + (await created.text())).toBe(true);
  const b2 = (await created.json()).key;

  // 用**界面**切账套（点 .book-enter），不走 /books/{k}/select 接口。
  // 顺手验一件正事：切到第二套之后，「合并报表」这个 platform 级菜单项
  // 还得在侧栏里 —— 它是平台管理员才可见的，而侧栏可见性判据读的是
  // `session.platformAdmin`。
  await page.click("#switch-book");
  await page.locator(`.book-enter[data-company*="合并B"]`).click();
  await expect(page.locator('.nav-item[data-view="vouchers"]')).toBeVisible({ timeout: 15_000 });
  await page.click('.nav-item[data-view="vouchers"]');
  await postVoucher(page, {
    date: "2026-01-15",
    rows: [
      { code: "1001", summary: "合并造数", debit: "200" },
      { code: "2001", summary: "合并造数", credit: "200" },
    ],
    post: true,
  });

  // 合并报表（平台管理员）
  await page.click('.nav-item[data-view="consolidate"]');
  await expect(page.locator("#cs-books")).not.toContainText("加载", { timeout: 15_000 });

  // `.cs-bk` 的 value 是账套 **key**，而 newBook 返回的是 **company**。
  // 两者不是一回事：`make_book_key` 会把非字母数字字符换成 `_`，
  // 所以 `E2Ew0///公司名` 的 key 是 `E2Ew0___公司名`。
  // 我第一版写 `if (v !== b1 && v !== b2)` —— 拿 key 跟 company 比，恒为真，
  // 于是 b1 被自己 uncheck 掉，表里只剩 b2 那一列。症状像产品 bug，
  // 真因是拿错了标识符。所以这里从 /api/books 取真实 key 再勾。
  const all = await (await page.request.get("/api/books")).json();
  const k1 = (all.books.find((b) => b.company === b1) || {}).key;
  expect(k1, `应能找到 ${b1} 的账套 key`).toBeTruthy();
  expect(k1, "账套 key 与公司名不相等（否则下面用 key 勾选就成了永真）").not.toBe(b1);
  // b2 的 key 是建账响应里直接给的，不用反查
  for (const cb of await page.locator(".cs-bk").all()) {
    const v = await cb.getAttribute("value");
    if (v !== k1 && v !== b2) await cb.uncheck();
  }
  expect(await page.locator(".cs-bk:checked").count(), "正好勾两个账套").toBe(2);
  await page.fill("#cs-period", "202601");

  // 表格还没生成就点打印 → 必须明确报错，而不是静默无反应。
  // （我第一版把这条断言写在**生成之后**，那时走的是正常打印路径，
  //  本来就没有 toast —— 注释与代码自相矛盾，断言在等一个永不到来的东西。）
  await page.click("#cs-print");
  await expect(page.locator("#toast")).toContainText("先执行合并汇总", { timeout: 10_000 });

  await page.click("#cs-run");
  await expect(page.locator("#cs-table")).toContainText("1001", { timeout: 15_000 });
  await expect(page.locator("#cs-table")).toContainText("库存现金", { timeout: 5_000 });
  // 合计 100（b1）+ 200（b2）= 300 —— 这一条同时证明两套都进来了：
  // 少一套的话合计就是 200，正是我第一版踩的坑（拿 company 当 key 勾选）。
  await expect(page.locator("#cs-table")).toContainText("300", { timeout: 5_000 });
  // 表头里两个账套都在（按列判，不靠合计猜）
  await expect(page.locator("#cs-grid thead")).toContainText(b1, { timeout: 5_000 });

  // 一个账套都不选 → 明确报错
  for (const cb of await page.locator(".cs-bk").all()) await cb.uncheck();
  await page.click("#cs-run");
  await expect(page.locator("#toast")).toContainText("至少选择", { timeout: 10_000 });
});

test("批次登记：已结账期间拒收（守卫的浏览器侧证据）", async ({ page }) => {
  await newBook(page, `E2E批次守卫${Date.now()}`);
  // 把 2026-01 结掉（本期无发生额，不要求先结转）
  await page.click('.nav-item[data-view="period-end"]');
  await expect(page.locator("#pe-close")).toBeVisible({ timeout: 15_000 });
  await page.uncheck("#pe-reqcarry");
  await page.click("#pe-close");
  await page.click("#cf-ok");
  await expect(page.locator("#pe-status")).toContainText("已结账至：2026-01", { timeout: 15_000 });

  // 批次登记默认落在账套当前期间（2026-01，已结账）→ 必须被拒
  await page.click('.nav-item[data-view="inv-batch"]');
  await expect(page.locator("#bt-list")).not.toContainText("加载中", { timeout: 15_000 });
  await page.fill("#bt-item", "140301");
  await page.fill("#bt-qty", "10");
  await page.click("#bt-reg");
  await expect(page.locator("#toast")).toContainText("已结账", { timeout: 15_000 });

  // 一条流水都不能落库
  const mv = await page.request.get("/api/inventory/warehouse-stats?item=140301");
  if (mv.ok()) {
    const body = await mv.json();
    expect((body.rows || []).length, "被拒的登记不应留下任何流水").toBe(0);
  }
});

module.exports = {};
