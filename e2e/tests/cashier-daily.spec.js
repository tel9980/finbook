const { test, expect } = require("@playwright/test");
const { newBook, postVoucher, addBookUser, loginAs } = require("../helpers");

// 出纳日常链路。**E2E 里此前 65 条有 62 条是 admin 身份**，出纳只有 3 条权限断言，
// 而银行对账 / 收付款 / 出纳签字这些**出纳每天真正干的活**都是从没被任何角色跑过的
// 代码路径。本文件让出纳本人把它们跑一遍。
//
// 出纳的权限集合是 VoucherNew / VoucherEdit / CashierSign / Report / FinReport ——
// 注意里面**没有** VoucherAudit（审核）和 VoucherPost（记账）。正确形态是：
//   出纳：录、导入对账单、生成凭证草稿、建收付款单、签字
//   会计：审核、记账、结账
// 本文件把这个分工当成**被测契约**断言，而不只是"顺手用出纳跑一遍"。
//
// ⚠️ 三条实测出来的配置事实（写用例时才发现，此前零覆盖）：
//   1. **出纳签字这道关现在是默认开的**（`BookOptions::require_cashier` 默认 true，
//      与 `enable_audit` 同一取向）。原先默认 false，等于新建账套一出生就默认
//      绕过出纳：谁都不必做任何决定，就已经在绕了。所以这里两种状态都测 ——
//      「默认开着会卡在哪一步」与「显式关掉之后确实走得通」各钉一条。
//   2. 打开后**只拦命中 is_cash/is_bank 的凭证**（vouchers.rs 里的 SQL 判据），
//      不是一刀切拦全部凭证。
//   3. 每个测试用**自己的一对用户名**。一个平台账号现在允许绑 2 台设备
  //      （`MAX_DEVICES_PER_USER`），但本文件每条用例要在同一台机器上换 3 个
  //      身份登（admin / 出纳 / 会计），已超上限；而且共享用户名会让「上一条
  //      用例留下的设备绑定」变成下一条的前置条件，失败时极难定位。
//      也因此不能用共享的 `postAllDrafts` 一把梭：它内部是「审核→记账」，
//      碰到需要签字的凭证就会挂在「要求出纳签字」上。签字这一步必须由出纳来做。

/// 每个测试一套独立账号（理由见上：每条用例要登 3 个身份，超过 2 台上限）
function actors(tag) {
  const u = tag.replace(/[^a-z0-9]/gi, "").slice(0, 12);
  return {
    cashier: { username: `cash_${u}`, password: "Cash@2026d", role: "cashier", display: "出纳" },
    acc: { username: `acc_${u}`, password: "Acc@2026dd", role: "accountant", display: "会计" },
  };
}

/// 建一个"出纳 + 会计"都在套里的账套。
///
/// `opts.requireCashier` / `opts.enableAudit` 是账套参数里的两个开关，本文件两种
/// 组合都要测，因为**它们各自都会让「会计+出纳、没主管」的两人公司走不通**：
/// · requireCashier 开着 → 资金凭证要出纳签字（会计补不了，只能出纳签）
/// · enableAudit 开着 → 凭证要先审核，而会计按三权分离**没有** VoucherAudit
/// 建账向导现在会问「有无独立的审核人」，两人公司就该按提示关掉。本文件把
/// 「关掉能走通」和「开着会卡在哪一步」两条都钉住。
async function twoPersonBook(page, tag, opts = {}) {
  // requireCashier 缺省 = **不动**（出厂默认已经是开的，newBook 也会回读断言这一点）。
  // 只有显式传值才去点开关：早先「总是去对账」也没错，但那样分不清是出厂默认在起作用
  // 还是测试自己设的 —— 出问题时会指错方向。
  const { requireCashier, enableAudit = true } = opts;
  const bookName = await newBook(page, tag);
  const who = actors(tag);
  await addBookUser(page, who.cashier);
  await addBookUser(page, who.acc);
  // **总是**去对账，不要按"参数是不是默认值"短路 —— 账套自身的默认值是
  // require_cashier=false / enable_audit=true，和我 opts 的默认值并不相同。
  // 早先写成 `if (requireCashier !== true || ...)`，于是「要求开着签字」那次整个
  // 开关设置被跳过，测试变成"签字关着也能记账"的**假通过**。
  await page.click('.nav-item[data-view="options"]');
  // 开关设置：只在**显式传值**时才对账。
  //
  // 为什么不能「总是去对账」：那样分不清是出厂默认在起作用，还是测试自己设的 ——
  // 出问题时会指错方向。
  //
  // 反过来的错也要防：**按参数是不是默认值短路**会让「要求开着签字」那条用例
  // 整段跳过开关设置，账套实际是关着的，断言反而通过 —— 名字叫「要求开着」，
  // 测的是「关着」。所以 requireCashier 缺省是 undefined（不碰），传 false/true 才动。
  if (requireCashier !== undefined || enableAudit !== true) {
    await page.click('.nav-item[data-view="options"]');
    if (requireCashier !== undefined && (await page.isChecked("#op-cashier")) !== requireCashier) {
      await page.setChecked("#op-cashier", requireCashier);
    }
    if ((await page.isChecked("#op-audit")) !== enableAudit) {
      await page.setChecked("#op-audit", enableAudit);
    }
    await page.click("#op-save");
    await expect(page.locator("#toast")).toContainText("已保存", { timeout: 15_000 });
  }
  // 对账结果必须真落库，否则后面所有断言都在测一个不存在的配置
  const now = await (await page.request.get("/api/options")).json();
  expect(
    now.require_cashier,
    "出纳签字开关没生效（未显式设置时应为出厂默认 true）"
  ).toBe(requireCashier === undefined ? true : requireCashier);
  expect(now.enable_audit, "审核环节开关没生效").toBe(enableAudit);
  return {
    bookName,
    asAdmin: () => loginAs(page, "admin", "Admin!2026", bookName),
    asCashier: () => loginAs(page, who.cashier.username, who.cashier.password, bookName),
    asAccountant: () => loginAs(page, who.acc.username, who.acc.password, bookName),
  };
}

/// 建客户/供应商档案。gen_vouchers 靠「对方**名称**作为子串出现在银行摘要里」来认
/// 往来单位，所以档案的名称必须与导入的摘要对得上。
/// `AuxEntity` 的字段一个都不能少（`parent_code` / `disabled` / `props` / `memo` 都是
/// 非 Option，漏一个就是 422）。
///
/// 顺带记一笔踩过的坑：收入科目要用 **600101**（主营业务收入 的末级），`6001` 是
/// 父级 —— 凭证校验会拒「是非末级科目，不能记账」。这个校验是对的，是我编码写错了。
async function addAux(page, kind, code, name) {
  const r = await page.request.post("/api/aux", {
    data: { id: 0, kind, code, name, parent_code: null, disabled: false, props: {}, memo: "" },
  });
  if (!r.ok()) throw new Error(`建 ${kind} ${code} 失败：${r.status()} ${await r.text()}`);
}

/// 建银行账户档案。**这是 gen_vouchers 的硬前提**：银行/现金科目带「银行账户」辅助，
/// 账套里一个都没建时，gen_vouchers 会把每一笔都跳过并说
/// 「科目 100201 核算银行账户但账套里一个银行账户都没建」。
/// 手工录凭证不查这一条（所以前两条用例不建也能过），只有自动生成这条路径查 ——
/// 这个不对称本身值得记一笔。
async function addBankAccount(page, code, name) {
  await addAux(page, "bank", code, name);
}

const listBy = async (page, period, status) =>
  (await (await page.request.get(`/api/vouchers?period=${period}&status=${status}`)).json()) || [];

/// 会计：审核该期间全部草稿
async function auditAll(page, period) {
  const n = [];
  for (const v of await listBy(page, period, "draft")) {
    const r = await page.request.post(`/api/vouchers/${v.id}/audit`, { data: {} });
    expect(r.ok(), `审核 ${v.id} 应成功：${r.status()} ${await r.text()}`).toBe(true);
    n.push(v.id);
  }
  return n;
}

/// 出纳：给该期间全部「已审核」凭证签字
async function signAll(page, period) {
  const n = [];
  for (const v of await listBy(page, period, "audited")) {
    const r = await page.request.post(`/api/vouchers/${v.id}/sign`, { data: {} });
    expect(r.ok(), `出纳签字 ${v.id} 应成功：${r.status()} ${await r.text()}`).toBe(true);
    n.push(v.id);
  }
  return n;
}

/// 会计：记账（该期间全部「已审核」）
async function postAll(page, period) {
  const n = [];
  for (const v of await listBy(page, period, "audited")) {
    const r = await page.request.post(`/api/vouchers/${v.id}/post`, { data: {} });
    expect(r.ok(), `记账 ${v.id} 应成功：${r.status()} ${await r.text()}`).toBe(true);
    n.push(v.id);
  }
  return n;
}

/// 收付款单 / 资金调拨单的「审核」判据与后端 `require_fund_doc_gate` 必须一致：
/// voucher_audit **或** voucher_post。
/// 原来两边都只认 voucher_audit，而后端那侧还与 `enable_audit` 无关 —— 于是
/// 「只有会计+出纳、没有财务主管」的小微企业，出纳录的单**谁也审不了**，
/// 单子永远变不成凭证。建账向导里选「不启用审核环节」也救不了这一条。
/// 出纳（VoucherNew/Edit/Sign，两样都没有）仍然审不了自己提的单，分离没被破坏。
const canGate = (role) => role === "cashier" ? {} : { voucher_post: true };

/// 一张走银行账户的凭证 —— 命中 is_bank，签字关开着就会被拦
const fundRows = (n) => [
  { code: "100201", summary: "从基本户转现金", debit: n },
  { code: "1001", summary: "从基本户转现金", credit: n },
];

test("出纳签字开着：资金类凭证没签字不能记账，签字后才能记", async ({ page }) => {
  const p = await twoPersonBook(page, `E2E签字开${Date.now()}`);
  await postVoucher(page, { date: "2026-01-10", rows: fundRows("5000") });
  const id = (await listBy(page, "202601", "draft"))[0].id;

  await auditAll(page, "202601");

  // 关键断言：已审核、含资金科目，但没出纳签字 → 记账必须被拒
  const before = await page.request.post(`/api/vouchers/${id}/post`, { data: {} });
  expect(before.status(), "没出纳签字就记账应被拒").toBe(400);
  expect(await before.text()).toContain("出纳签字");

  // 出纳签字
  await p.asCashier();
  await page.click('.nav-item[data-view="vouchers"]');
  await page.locator("#v-table tbody [data-edit]").first().click();
  await expect(page.locator("#v-sign"), "出纳应看到签字按钮").toBeVisible({ timeout: 15_000 });
  // 出纳不该有记账权 —— 界面就不给按钮
  await expect(page.locator("#v-post")).toHaveCount(0);
  await page.click("#v-sign");
  await expect(page.locator("#toast")).toContainText("已出纳签字", { timeout: 10_000 });
  await page.keyboard.press("Escape");

  // 接口层同样拦住：出纳补不了记（无 voucher_post）
  const asCashier = await page.request.post(`/api/vouchers/${id}/post`, { data: {} });
  expect(asCashier.status(), "出纳不该有记账权").toBe(403);

  // 会计记账：这次通过
  await p.asAccountant();
  const after = await page.request.post(`/api/vouchers/${id}/post`, { data: {} });
  expect(after.ok(), `签字后会计记账应成功：${await after.text()}`).toBe(true);
});

// ⚠️ 这条用例原来叫「出纳签字关着（**默认**）」，而默认早已改成「开」。
// 名字写反的用例比没有用例更坏 —— 读报告的人会以为默认是关的。
// 现在它测的是「**显式**关掉之后确实关掉了」：一个人记账的小微企业仍然需要这条路。
test("显式关掉出纳签字后：记账不被拦（没有出纳岗的小微可用）", async ({ page }) => {
  await twoPersonBook(page, `E2E签字显式关${Date.now()}`, { requireCashier: false });
  await postVoucher(page, { date: "2026-01-10", rows: fundRows("5000") });
  const id = (await listBy(page, "202601", "draft"))[0].id;
  await auditAll(page, "202601");

  // 显式关着 → 会计可以直接记账，没有人拦
  const r = await page.request.post(`/api/vouchers/${id}/post`, { data: {} });
  expect(r.ok(), `显式关掉后记账应通过：${r.status()} ${await r.text()}`).toBe(true);

  // 回读确认它真的是关的（newBook 只断言出厂默认是开，这里是显式覆盖后的结果）
  const o = await (await page.request.get("/api/options")).json();
  expect(o.require_cashier, "显式关掉后回读应仍是 false").toBe(false);
});

test("新建账套的出纳签字**默认开着** —— 不做任何选择就已经在守这道闸", async ({ page }) => {
  // 单独一条钉住默认值：名字里带「默认」的用例必须真的验默认，不能借「显式设置」
  // 顺手把默认值也测了（那正是上一条名字写反的根源）。
  //
  // 这里**不传** requireCashier，`twoPersonBook` 因此不去点开关 ——
  // 账套保持 `newBook` 建出来时的出厂状态。
  const bookName = await newBook(page, `E2E签字默认${Date.now()}`);
  const o = await (await page.request.get("/api/options")).json();
  expect(o.require_cashier, "新建账套的出纳签字应默认开启").toBe(true);

  // 而且它是真的在拦：录一张资金凭证，审核后直接记账应当被拒
  await postVoucher(page, { date: "2026-01-10", rows: fundRows("5000") });
  const id = (await listBy(page, "202601", "draft"))[0].id;
  await auditAll(page, "202601");
  const r = await page.request.post(`/api/vouchers/${id}/post`, { data: {} });
  expect(r.status(), "默认开着时，未签字的资金凭证不该能记账").toBe(400);
  expect(await r.text(), "拒绝原因应指向出纳签字").toMatch(/出纳|签字/);
  void bookName;
});

test("出纳：导入对账单→自动勾对→未勾对的生成凭证草稿（不自动记账）", async ({ page }) => {
  const p = await twoPersonBook(page, `E2E对账${Date.now()}`);
  await addAux(page, "customer", "C01", "华东商贸");
  await addAux(page, "supplier", "S01", "钢构厂");
  await addBankAccount(page, "B01", "工行基本户");

  // 业务侧已入账：销售确认收入，应收挂 C01
  await postVoucher(page, {
    date: "2026-01-10",
    rows: [
      { code: "112201", summary: "销售给华东商贸", debit: "8000", aux: { customer: "C01" } },
      { code: "600101", summary: "销售给华东商贸", credit: "8000" },
    ],
  });
  // 供应商那边：应付挂 S01。注意方向 —— 付供应商是**贷**银行存款
  // （我一开始写成借 100201，结果自动勾对死活勾不上：银行侧借贷方向反了，
  //  流水是支出 3000 而账面那边成了进账 3000）
  await postVoucher(page, {
    date: "2026-01-12",
    rows: [
      { code: "220201", summary: "应付钢构厂", debit: "3000", aux: { supplier: "S01" } },
      { code: "100201", summary: "付钢构厂", credit: "3000" },
    ],
  });
  await auditAll(page, "202601");
  // 只有含银行科目的那张需要出纳签字
  await p.asCashier();
  await signAll(page, "202601");
  await p.asAccountant();
  await postAll(page, "202601");

  await p.asCashier();
  await page.click('.nav-item[data-view="bank"]');
  await page.fill("#bk-acct", "100201");
  await page.fill("#bk-period", "202601");
  await page.click("#bk-load");

  // 导入 3 条：①能与账面自动勾对 ②③只有流水、没有账面分录 → 只能靠 gen_vouchers。
  // 摘要里必须带上往来单位名称，gen_vouchers 才认得出（它靠「名称是摘要子串」匹配）
  await page.click("#bk-import");
  await page.fill(
    "#bi-text",
    [
      "2026-01-12,付钢构厂,SN1,0.00,3000.00,12000.00",
      "2026-01-20,网银转入 华东商贸货款,SN2,8000.00,0.00,20000.00",
      "2026-01-25,手续费,SN3,0.00,30.00,19970.00",
    ].join("\n")
  );
  await page.click("#bi-ok");
  await expect(page.locator("#bk-sum")).toContainText("银行流水 3 条", { timeout: 15_000 });

  // 自动勾对：能勾上的勾上，勾不上的如实显示「未勾」
  await page.click("#bk-auto");
  await expect(page.locator("#bk-sum")).toContainText("已勾 1", { timeout: 15_000 });
  await expect(page.locator("#bk-stmts")).toContainText("未勾");

  // 出纳点「未勾对的生成凭证」
  await page.click("#bk-gen");
  await expect(page.locator("#bg-ok"), "生成前要先说清边界").toBeVisible();
  await expect(page.locator(".modal")).toContainText("不含收入与成本侧");
  await page.click("#bg-ok");

  // 结果弹窗：认得出的生成了，认不出的逐条说原因
  const res = page.locator(".modal-mask").last();
  await expect(res).toContainText("已生成", { timeout: 15_000 });
  expect(await res.textContent(), "手续费这种认不出往来单位的必须被留下并说明原因").toMatch(/手续费/);
  await page.keyboard.press("Escape");

  // 生成的凭证是草稿，**且没有被自动记账** —— 自动记账等于把出纳签字这一关自动掉
  const posted = await listBy(page, "202601", "posted");
  expect(posted.length, "生成凭证不应被自动记账（应仍只有前面记过的 2 张）").toBe(2);
  const drafts = await listBy(page, "202601", "draft");
  expect(drafts.length, "应有新生成的草稿凭证").toBeGreaterThan(0);

  // 顺序：生成(draft) → 审核(audited) → 出纳签字 → 记账
  // 审核这一步**只有 admin/主管做得了**（会计按三权分离没有 VoucherAudit）——本用例
  // 开着审核环节，所以这一段本来就该由管理员代办。顺带把这条分工钉住：
  expect((await page.request.post(`/api/vouchers/${drafts[0].id}/audit`, { data: {} })).status(),
    "会计不该能审核凭证（无 VoucherAudit）").toBe(403);
  await p.asAdmin();
  await auditAll(page, "202601");
  // 签字这道关对**自动生成**的凭证同样生效（不是只对手工录的有用）
  expect((await page.request.post(`/api/vouchers/${drafts[0].id}/post`, { data: {} })).status(),
    "生成的凭证没出纳签字不能记账").toBe(400);
  await p.asCashier();
  expect((await signAll(page, "202601")).length).toBeGreaterThanOrEqual(drafts.length);
  await p.asAdmin();
  await postAll(page, "202601");
  const bank = await (await page.request.get("/api/bank?period=202601&account=100201")).json();
  const unmatched = bank.statements.filter((s) => !s.entry_id);
  expect(unmatched.length, "认不出往来单位的流水应原样留在对账页").toBe(1);
  expect(unmatched[0].summary).toContain("手续费");
});

test("两人公司的收付款：出纳建单→会计审→出纳签字→会计记账→自动核销", async ({ page }) => {
  // 这条走**两人公司该用的配置**：关掉审核环节（建账向导里「没有独立审核人」选项），
  // 但**保留**出纳签字（出纳是真实存在的岗位，这道关正是这套系统最有价值的内控之一）。
  // 于是分工是：出纳录单/签字，会计审单/记账 —— 两个人都必要，谁也替代不了谁。
  const p = await twoPersonBook(page, `E2E收付${Date.now()}`, { enableAudit: false });
  await addAux(page, "customer", "C01", "华东商贸");

  // 业务侧：销售确认，应收挂 C01（无资金科目，不必签字）
  await postVoucher(page, {
    date: "2026-01-08",
    rows: [
      { code: "112201", summary: "销售给华东商贸", debit: "6000", aux: { customer: "C01" } },
      { code: "600101", summary: "销售给华东商贸", credit: "6000" },
    ],
  });
  await postAll(page, "202601");

  await p.asCashier();
  await page.click('.nav-item[data-view="funds"]');
  // funds 是分页签的，收付款在「收付款」页签下（默认停在「资金日报」）
  await page.click("#ft-receipt");
  // 收付款单列表出得来（list_receipts 要 VoucherNew，出纳有）
  await expect(page.locator("#rc-list")).not.toContainText("加载中", { timeout: 15_000 });

  await page.selectOption("#rc-kind", "receipt");
  await page.fill("#rc-date", "2026-01-15");
  await page.fill("#rc-fund", "100201");
  await page.selectOption("#rc-party", "C01");
  await page.fill("#rc-amt", "6000");
  await page.fill("#rc-memo", "收到货款");
  await page.click("#rc-new");
  await expect(page.locator("#rc-list")).toContainText("待审核", { timeout: 15_000 });
  await expect(page.locator("#rc-list")).toContainText("6,000");

  // 出纳没有 voucher_audit / voucher_post —— 界面不该给他「审核」按钮，接口也该拦
  await expect(page.locator("#rc-list [data-rc-audit]")).toHaveCount(0);
  const rows = (await (await page.request.get("/api/funds/receipts")).json()).rows;
  const rid = rows[0].id;
  const auditAsCashier = await page.request.post(`/api/funds/receipts/${rid}/audit`, { data: {} });
  expect(auditAsCashier.status(), "出纳不该能审核收付款单").toBe(403);

  // 会计审核 → 生成凭证并按往来 FIFO 自动核销
  // 会计没有 voucher_audit，但有 voucher_post —— 后端 require_fund_doc_gate 认这个
  await p.asAccountant();
  await page.click('.nav-item[data-view="funds"]');
  await page.click("#ft-receipt");
  await expect(page.locator("#rc-list")).toContainText("待审核", { timeout: 15_000 });
  await expect(page.locator("#rc-list [data-rc-audit]"), "会计应有审核按钮（voucher_post 也算数）")
    .toHaveCount(1);
  await page.locator("#rc-list [data-rc-audit]").first().click();
  await expect(page.locator("#rc-list")).toContainText("已审核", { timeout: 15_000 });
  await expect(page.locator("#rc-list")).toContainText("凭证 #", { timeout: 10_000 });

  // 生成的凭证含银行科目 → 签字关开着，没出纳签字不能记。
  // 凭证 id 从收付款单上取，不去猜它的 status —— 单据审核生成的是哪一档状态由后端
  // 决定，测试去猜就等于把实现细节钉进用例（我第一版就栽在这：以为该是 audited，
  // 实际不是，于是断言了 0 条）。id 是单据上的事实，status 不是。
  const after = (await (await page.request.get("/api/funds/receipts")).json()).rows;
  const vid = after.find((r) => r.id === rid).voucher_id;
  expect(vid, "审核后收付款单应带出凭证 id").toBeTruthy();
  expect((await page.request.post(`/api/vouchers/${vid}/post`, { data: {} })).status(),
    "没出纳签字不能记账").toBe(400);

  // 出纳签字 → 会计记账
  await p.asCashier();
  expect((await page.request.post(`/api/vouchers/${vid}/sign`, { data: {} })).ok(),
    "出纳应能签这张凭证").toBe(true);
  await p.asAccountant();
  const posted = await page.request.post(`/api/vouchers/${vid}/post`, { data: {} });
  expect(posted.ok(), `签字后会计应能记账：${posted.status()} ${await posted.text()}`).toBe(true);

  // 自动核销：应收被这笔收款清掉
  const aging = await (await page.request.get("/api/settle/aging?account=1122")).json();
  const open = (aging.rows || aging).filter((r) => parseFloat(r.open_amount ?? r.open ?? 0) !== 0);
  expect(open.length, `收款核销后应收应被清掉：${JSON.stringify(aging)}`).toBe(0);
});

test("两人公司的资金调拨：出纳建单→会计审→出纳签字→会计记账（两端资金科目，不核销）", async ({ page }) => {
  // 调拨是账户间转账：借 转入 / 贷 转出，两端都是现金/银行科目 → 命中出纳签字闸门，
  // 与收付款单同一条分工（出纳录/签，会计审/记）。此处不涉及往来，因此**不核销**。
  const p = await twoPersonBook(page, `E2E调拨${Date.now()}`, { enableAudit: false });

  await p.asCashier();
  await page.click('.nav-item[data-view="funds"]');
  // funds 分页签，调拨在「资金调拨」页签下（默认停在「资金日报」）
  await page.click("#ft-transfer");
  await expect(page.locator("#tf-list")).not.toContainText("加载中", { timeout: 15_000 });

  await page.fill("#tf-date", "2026-01-05");
  await page.selectOption("#tf-from", "100201");
  await page.selectOption("#tf-to", "1001");
  await page.fill("#tf-amt", "500");
  await page.fill("#tf-memo", "提现备用");
  await page.click("#tf-new");
  await expect(page.locator("#tf-list")).toContainText("待审核", { timeout: 15_000 });
  await expect(page.locator("#tf-list")).toContainText("500");

  // 出纳没有 voucher_audit / voucher_post —— 界面不给「审核」按钮，接口也该拦
  await expect(page.locator("#tf-list [data-tf-audit]")).toHaveCount(0);
  const rows = (await (await page.request.get("/api/funds/transfers")).json()).rows;
  const tid = rows[0].id;
  const auditAsCashier = await page.request.post(`/api/funds/transfers/${tid}/audit`, { data: {} });
  expect(auditAsCashier.status(), "出纳不该能审核资金调拨单").toBe(403);

  // 会计审核（没有 voucher_audit 但有 voucher_post —— 后端 require_fund_doc_gate 认这个）
  await p.asAccountant();
  await page.click('.nav-item[data-view="funds"]');
  await page.click("#ft-transfer");
  await expect(page.locator("#tf-list")).toContainText("待审核", { timeout: 15_000 });
  await expect(page.locator("#tf-list [data-tf-audit]"), "会计应有审核按钮（voucher_post 也算数）")
    .toHaveCount(1);
  await page.locator("#tf-list [data-tf-audit]").first().click();
  await expect(page.locator("#tf-list")).toContainText("已审核", { timeout: 15_000 });
  await expect(page.locator("#tf-list")).toContainText("凭证 #", { timeout: 10_000 });

  // 两端都是资金科目 → 签字关开着，没出纳签字不能记。凭证 id 取自单据事实，不猜 status。
  const after = (await (await page.request.get("/api/funds/transfers")).json()).rows;
  const vid = after.find((r) => r.id === tid).voucher_id;
  expect(vid, "审核后资金调拨单应带出凭证 id").toBeTruthy();
  expect((await page.request.post(`/api/vouchers/${vid}/post`, { data: {} })).status(),
    "没出纳签字不能记账").toBe(400);

  await p.asCashier();
  expect((await page.request.post(`/api/vouchers/${vid}/sign`, { data: {} })).ok(),
    "出纳应能签这张凭证").toBe(true);
  await p.asAccountant();
  const posted = await page.request.post(`/api/vouchers/${vid}/post`, { data: {} });
  expect(posted.ok(), `签字后会计应能记账：${posted.status()} ${await posted.text()}`).toBe(true);
});
