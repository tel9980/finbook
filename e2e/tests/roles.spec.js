const { test, expect } = require("@playwright/test");
const { newBook, postVoucher, addBookUser, loginAs } = require("../helpers");

// 出纳（cashier）在 fincore::user::Role 里的权限只有 5 项：
//   VoucherNew / VoucherEdit / CashierSign / Report / FinReport
// 也就是：能看、能录凭证、能签字，但**不能**维护科目、不能建期初、不能期末结账。
// 下面两条用例就是钉这个差别的。

test("侧栏「最近/收藏」按当前角色过滤，且不跨账号共享", async ({ page }) => {
  const book1 = await newBook(page, `E2E侧栏权限${Date.now()}`);

  // admin（会计以上权限）访问三个出纳看不到的页面 → 进「最近」
  for (const v of ["begin", "assets", "period-end"]) {
    await page.click(`.nav-item[data-view="${v}"]`);
  }
  // admin 收藏一个出纳也没有的页面
  await page.click('.nav-item[data-view="begin"]');
  await page.click('.nav-star[data-fav="begin"]');

  const adminKeys = await page.evaluate(() =>
    Object.keys(localStorage).filter((k) => k.indexOf("nav_") === 0)
  );
  expect(adminKeys.some((k) => k.endsWith("@admin"))).toBe(true);
  await expect(page.locator('.nav-item[data-view="begin"]').first()).toBeVisible();

  // 换成出纳
  await addBookUser(page, {
    username: "e2ecash",
    password: "Cash@2026x",
    role: "cashier",
    display: "出纳",
  });
  await loginAs(page, "e2ecash", "Cash@2026x", book1);

  // 出纳先访问两页、再收藏其中一页 —— 否则「最近 / 常用」两个分区根本不渲染。
  // **空分区上的 not.toContainText 恒真**：原写法还挂在只含标题的 .nav-sec 上，
  // 标题里只有「最近 / 常用」四个字，三条断言永远通过 = 没测。
  await page.click('.nav-item[data-view="vouchers"]');
  await page.click('.nav-item[data-view="bank"]');
  await page.click('.nav-star[data-fav="bank"]');
  // 结构锚点：secHtml() 的标题和项是**兄弟节点**，必须写 `.nav-sec[...] + .nav-body`
  const recentBody = page.locator('.nav-sec[data-sec="recent"] + .nav-body');
  const favBody = page.locator('.nav-sec[data-sec="fav"] + .nav-body');
  // 正例：两个分区确实渲染了，且里面有他**该**看到的那两页（否则下面的反例是空跑）
  await expect(recentBody.locator('.nav-jump[data-view="vouchers"]')).toHaveCount(1);
  await expect(favBody.locator('.nav-item[data-view="bank"]')).toHaveCount(1);
  // 反例：他没权限的页面在这两个分区里都不许出现
  for (const sec of [recentBody, favBody]) {
    for (const v of ["begin", "assets", "period-end"]) {
      await expect(sec.locator(`[data-view="${v}"]`)).toHaveCount(0);
    }
  }
  // 主导航本来就没有（对照组：主列表一直是对的，出错的是 localStorage 那条路）
  await expect(page.locator('.nav-item[data-view="begin"]')).toHaveCount(0);
  await expect(page.locator('.nav-item[data-view="assets"]')).toHaveCount(0);

  // admin 的 localStorage 分区**仍然在**（登出不清 localStorage，这是对的），
  // 要验的是出纳既读不到它、也读不到里面的内容。
  const parts = await page.evaluate(() => {
    const out = {};
    for (const k of Object.keys(localStorage)) {
      if (k.indexOf("nav_recent") === 0 || k.indexOf("nav_fav") === 0) out[k] = localStorage.getItem(k);
    }
    return out;
  });
  // 每个 key 都必须带账号后缀 —— 共享 key（没有 @）就是没做分区
  for (const k of Object.keys(parts)) expect(k).toMatch(/@(admin|e2ecash)$/);
  // admin 那边确实记下了「期初建账」（证明上面的操作真的生效了，对照组）
  expect(parts["nav_recent@admin"] || "").toContain("begin");
  // 出纳这边不许出现他没权限的 id
  for (const k of Object.keys(parts)) {
    if (!k.endsWith("@e2ecash")) continue;
    expect(parts[k]).not.toContain("begin");
    expect(parts[k]).not.toContain("assets");
    expect(parts[k]).not.toContain("period-end");
  }
  expect(adminKeys.some((k) => k.endsWith("@admin"))).toBe(true);
});

test("无权限的页面只显示权限提示，不再叠一个永远失败的重试空态", async ({ page }) => {
  const book2 = await newBook(page, `E2E越权页面${Date.now()}`);
  await addBookUser(page, {
    username: "e2ecash2",
    password: "Cash@2026x",
    role: "cashier",
    display: "出纳",
  });
  await loginAs(page, "e2ecash2", "Cash@2026x", book2);

  // 出纳侧栏里根本没有「固定资产」，直接走 hash 路由硬闯
  await page.goto("/#assets");
  await expect(page.locator("#main")).toContainText("权限", { timeout: 15_000 });
  // 关键：不能同时出现「加载失败 / 可能是网络问题」这类会把人引向网络排查的文案
  await expect(page.locator("#main")).not.toContainText("加载失败", { timeout: 5_000 });
  await expect(page.locator("#main")).not.toContainText("重试", { timeout: 5_000 });
});

test("两人公司：会计+出纳，无财务主管也能把一个月走完", async ({ page }) => {
  const book3 = await newBook(page, `E2E两人公司${Date.now()}`);

  // ⚠️ 这条用例原来叫「会计+出纳」，却**只创建了会计**。此前没暴露是因为出纳签字
  // 默认关 —— 一个人就能把含现金的凭证记进总账，正是本轮修掉的「默认绕过出纳」。
  // 现在出纳签字默认开，所以必须真的有出纳，否则这条用例测不到它名字里那件事。
  //
  // 审核环节仍然要关：两人公司没有主管，关掉才能让会计记账（另一道闸门，
  // 由 cashier-daily.spec.js 专门钉）。
  const opts = await (await page.request.get("/api/options")).json();
  opts.enable_audit = false;
  const put = await page.request.put("/api/options", { data: opts });
  expect(put.ok(), `关审核环节失败：${put.status()} ${await put.text()}`).toBe(true);

  // 开一个会计（无额外授权，就是 Role::Accountant 本身）
  await page.click('.nav-item[data-view="platform-users"]');
  await page.click("#pu-add");
  await page.fill("#nc-u", "e2eacc");
  await page.fill("#nc-p", "Acc@2026xx");
  await page.click("#nc-save");
  await expect(page.locator("#pu-list")).toContainText("e2eacc", { timeout: 15_000 });
  const r = await page.request.post("/api/users", {
    data: {
      username: "e2eacc",
      display_name: "会计",
      password: "",
      role: "accountant",
      must_change_pwd: false,
    },
  });
  expect(r.ok(), `建会计失败：${r.status()} ${await r.text()}`).toBe(true);

  // 出纳也要真的建出来（见上面那段说明：这条用例名字里有「出纳」，
  // 但原来从头到尾只有会计）
  await addBookUser(page, {
    username: "e2ecash3",
    password: "Cash@2026x",
    role: "cashier",
    display: "出纳",
  });

  await loginAs(page, "e2eacc", "Acc@2026xx", book3);

  // 录一张凭证：贷记 1001 库存现金（命中出纳闸门）
  // （用共享的 postVoucher 助手，它断言的就是「保存后弹窗关闭」）
  await postVoucher(page, {
    date: "2026-01-10",
    rows: [
      { code: "660201", summary: "办公费", debit: "500" },
      { code: "1001", summary: "付", credit: "500" },
    ],
  });
  const list = await (await page.request.get("/api/vouchers?period=202601")).json();
  const vid = (list[0] || {}).id;
  expect(vid, "凭证应已保存").toBeTruthy();
  // ① 审核环节关着，所以**不是**审核拦的；但出纳闸门默认开，现金凭证仍要签字。
  //    这一条断言很关键：它证明「关掉审核」并不能顶替出纳签字 ——
  //    两道闸门是各自独立的，不是同一件事的两个开关。
  const blocked = await page.request.post(`/api/vouchers/${vid}/post`, { data: {} });
  expect(blocked.status(), "含现金的凭证未签字应不能记账").toBe(400);
  const why = await blocked.text();
  expect(why, "拒绝原因要指向「出纳签字」这个下一步，而不是只说失败").toMatch(/出纳|签字/);
  expect(why, "不该把没过的闸门说成审核：审核环节是关着的").not.toMatch(/审核/);

  // ② 会计自己没有 CashierSign，签不了 —— 否则「出纳复核」就是一句空话。
  const selfSign = await page.request.post(`/api/vouchers/${vid}/sign`, { data: {} });
  expect(selfSign.status(), `会计不该能自己出纳签字：${selfSign.status()} ${await selfSign.text()}`).toBe(403);

  // ③ 换真正的出纳签字
  await loginAs(page, "e2ecash3", "Cash@2026x", book3);
  const sg = await page.request.post(`/api/vouchers/${vid}/sign`, { data: {} });
  expect(sg.ok(), `出纳应能签字：${sg.status()} ${await sg.text()}`).toBe(true);

  // 签字后立刻回读，两件事要验：
  //   · 签字人记的是**出纳本人**。「有人签过了」不够 —— 出纳签字这道控制的全部意义
  //     就是记录是谁复核的，记成管理员或空着，控制就只剩形式。
  //   · 签字**不等于**记账：状态仍是草稿/已审核。这条必须在记账之前查，
  //     放到之后就变成「本来就是 posted」，成了永真断言。
  const signed = await (await page.request.get(`/api/vouchers/${vid}`)).json();
  expect(signed.cashier, `签字人应记为出纳本人，实际 ${JSON.stringify(signed.cashier)}`).toBe("e2ecash3");
  expect(signed.status, "签字不等于记账：状态不该已经是 posted").not.toBe("posted");

  // ④ 会计记账：审核环节关着，出纳签过字就该一路走通
  await loginAs(page, "e2eacc", "Acc@2026xx", book3);
  const post = await page.request.post(`/api/vouchers/${vid}/post`, { data: {} });
  expect(post.ok(), `出纳签字后会计应能记账：${post.status()} ${await post.text()}`).toBe(true);
  const after = await (await page.request.get(`/api/vouchers/${vid}`)).json();
  expect(after.status, "记账后状态应为 posted").toBe("posted");
  expect(after.cashier, "记账不应把签字人清掉").toBe("e2ecash3");

  // 期末处理页必须有「结账」按钮 —— 原来会计看到的是「没有「期末结账」权限」，
  // 两人公司里根本没有财务主管，于是永远结不了账
  await page.click('.nav-item[data-view="period-end"]');
  await expect(page.locator("#pe-close")).toBeVisible({ timeout: 15_000 });
  await expect(page.locator("#pe-carry")).toBeVisible({ timeout: 5_000 });
  // precheck 也必须通得过（它要 PeriodClose，原来对会计直接 403）
  await expect(page.locator("#pe-status")).not.toContainText("权限", { timeout: 15_000 });
  await expect(page.locator("#pe-status")).not.toContainText("加载中", { timeout: 5_000 });

  // 真的把 1 月结掉 —— 走到这一步才算「两人公司能把一个月走完」
  await page.uncheck("#pe-reqcarry"); // 本期无损益发生额，不要求先结转
  await page.click("#pe-close");
  await page.click("#cf-ok");
  await expect(page.locator("#pe-status")).toContainText("已结账至：2026-01", { timeout: 15_000 });

  // 结账确实生效：再记一笔到 202601 必须被拒
  const late = await page.request.post("/api/vouchers", {
    data: {
      id: 0, period: 202601, date: "2026-01-20", word: "记", no: 99, attachments: 0, memo: "t",
      entries: [
        { line: 1, account_code: "660201", summary: "补记", debit: "1", credit: "0" },
        { line: 2, account_code: "1001", summary: "补记", debit: "0", credit: "1" },
      ],
    },
  });
  expect(late.status(), "已结账期间不应再能录入凭证").toBe(400);
});
