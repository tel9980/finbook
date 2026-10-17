const { test, expect } = require("@playwright/test");
const { newBook, postVoucher, postAllDrafts } = require("../helpers");

// 跨月连续结转结账。**此前只测过单月**（2026-01）——
// smoke.spec 的"会计月度闭环"、settle-period 的"期末处理"都只走一个月，
// 于是这些从来没被跑过：
//   · 逐月结账、closed_upto 推进
//   · 已结账期间录不进去
//   · 反结账后能补录再重结
//   · 重复结转的提示（这条 api.rs 有测试，但没走过"跨月 + 已结转"这条真实路径）
//
// 期末结算是"能把一本账做完"的最后一道关口，只测单月等于没测。
//
// 两条实测出来的规则（写用例时才发现，之前没有任何测试覆盖它们）：
//   1. **首次结账必须从启用期间开始**，不能跳月 —— 账必须按顺序封。
//      实测直接结 202602 → 400「首次结账必须从启用期间 2026年01期 开始」。
//   2. `postAllDrafts` 内部是「审核→记账」逐张做；分成"先全审、再全记"两轮，
//      第二轮就捞不到草稿了（审完状态已是 audited），结账预检会以
//      「本期还有 N 张凭证未记账」拒掉。

// 启用期间是 2026-01，所以要连着结 1~12 月
const MONTHS = [
  "202601", "202602", "202603", "202604", "202605", "202606",
  "202607", "202608", "202609", "202610", "202611", "202612",
];
const dateOf = (ymm) => `${ymm.slice(0, 4)}-${ymm.slice(4, 6)}-15`;
/// 端点的 yymm（202601）与 precheck 返回的 closed_upto（2026-01）格式不同 ——
/// 写成同一个就得到一条毫无信息量的假失败。
const ym = (ymm) => `${ymm.slice(0, 4)}-${ymm.slice(4, 6)}`;

/// 一个月走完：录一笔费用 → 结转损益 → 审核+记账 → 结账。
/// 每月都留一笔费用，否则 carry-forward 会以「净发生额均为 0」被拒 ——
/// 那不是 bug，是账结法的真实前提（没损益发生额就不用结转）。
async function doMonth(page, ymm) {
  await postVoucher(page, {
    date: dateOf(ymm),
    rows: [
      { code: "660201", summary: `${ymm} 办公费`, debit: "100" },
      { code: "1001", summary: `${ymm} 付`, credit: "100" },
    ],
  });
  const carry = await page.request.post(`/api/periods/${ymm}/carry-forward`, { data: {} });
  expect(carry.ok(), `${ymm} 结转损益应成功：${carry.status()} ${await carry.text()}`).toBe(true);
  // 一轮搞定：内部逐张「审核→记账」
  await postAllDrafts(page, ymm);
  const close = await page.request.post(`/api/periods/${ymm}/close`, {
    data: { require_carry: true },
  });
  expect(close.ok(), `${ymm} 结账应成功：${close.status()} ${await close.text()}`).toBe(true);
}

test("跨月：1 月到 12 月连着结，closed_upto 逐月推进", async ({ page }) => {
  await newBook(page, `E2E跨月${Date.now()}`);

  for (const ymm of MONTHS) {
    await doMonth(page, ymm);
    const pre = await (await page.request.get(`/api/periods/${ymm}/precheck`)).json();
    expect(pre.closed_upto, `${ymm} 结完后 closed_upto 应正好是本月`).toBe(ym(ymm));
  }
});

test("不能跳月结账：未结的月之前有未结账月会被挡", async ({ page }) => {
  await newBook(page, `E2E跳月${Date.now()}`);
  // 2 月有业务但 1 月没结 → 直接结 2 月应当被挡
  await postVoucher(page, {
    date: "2026-02-15",
    rows: [
      { code: "660201", summary: "办公费", debit: "100" },
      { code: "1001", summary: "付", credit: "100" },
    ],
  });
  const jumped = await page.request.post("/api/periods/202602/close", {
    data: { require_carry: true },
  });
  expect(jumped.status(), "跳过启用期间直接结 2 月应被拒").toBe(400);
  expect(await jumped.text()).toContain("首次结账必须从启用期间");
});

test("已结账期间录不进凭证；反结账后可补录，增量结转不重复结、重复点被拒且提示说清原因", async ({ page }) => {
  await newBook(page, `E2E反结账${Date.now()}`);

  await doMonth(page, "202601");
  await doMonth(page, "202602");

  const voucherBody = (memo) => ({
    id: 0, period: 202602, date: "2026-02-20", word: "记", no: 99, attachments: 0, memo,
    entries: [
      { line: 1, account_code: "660201", summary: memo, debit: "50", credit: "0" },
      { line: 2, account_code: "1001", summary: memo, debit: "0", credit: "50" },
    ],
  });

  // 往已结账的 2 月补一笔 → 必须被拒
  const late = await page.request.post("/api/vouchers", { data: voucherBody("补记") });
  expect(late.status(), "已结账期间不应能录入凭证").toBe(400);

  // 反结账 2 月
  const un = await page.request.post("/api/periods/202602/unclose", { data: {} });
  expect(un.ok(), `反结账应成功：${un.status()} ${await un.text()}`).toBe(true);
  const pre = await (await page.request.get("/api/periods/202602/precheck")).json();
  expect(pre.closed_upto, `反结账后 2 月不该还在已结账序列里：${JSON.stringify(pre.closed_upto)}`)
    .toBe("2026-01");

  // 现在能补录了
  const retry = await page.request.post("/api/vouchers", { data: voucherBody("补记") });
  expect(retry.ok(), `反结账后补录应成功：${retry.status()} ${await retry.text()}`).toBe(true);

  // 补录后本月损益多了 50 → 这时**应该**能再结一次（账结法的幂等基础：上一张结转
  // 凭证已把原有金额清零，所以这次结的是增量，不是重复结 250）。
  const again = await page.request.post("/api/periods/202602/carry-forward", { data: {} });
  expect(again.ok(), `增量结转应成功：${again.status()} ${await again.text()}`).toBe(true);
  const newId = (await again.json()).id;
  const ents = await (await page.request.get(`/api/vouchers/${newId}`)).json();
  const moved = (ents.entries || []).reduce(
    (s, e) => s + Math.abs(parseFloat(e.debit || 0)) + Math.abs(parseFloat(e.credit || 0)), 0
  ) / 2;
  expect(moved, `这次只应结增量 50，本月原已结过 100+100，重复结会变成 250：${JSON.stringify(ents.entries)}`)
    .toBe(50);

  // 但再点一次（期间没新增任何损益）必须被拒，且提示要说清是"已结转过"
  const third = await page.request.post("/api/periods/202602/carry-forward", { data: {} });
  expect(third.status(), "期间无新增损益时重复结转应被拒").toBe(400);
  const txt = await third.text();
  expect(txt, `重复结转的提示必须说清是已结转过：${txt}`).toContain("已结转过");
  expect(txt, `不该再出现"导入数据有误"：${txt}`).not.toContain("导入数据有误");
  expect(txt, `不该说"损益发生额均为零"（本月明明有损益）：${txt}`).not.toContain("损益发生额均为零");
});
