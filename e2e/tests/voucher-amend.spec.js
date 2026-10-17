const { test, expect } = require("@playwright/test");
const { newBook, auditSignPost } = require("../helpers");

// 凭证更正链（Cancel -> Amend）
//
// 守的是**审计可追溯性**：两张凭证互相认得，才查得出「哪张在冲哪张」。
// finbook 有反记账 + 取消审核，所以错凭证能改 —— 但改完的两张凭证若没有链接，
// 事后只能靠时间和金额去猜。「不能改」反而更安全：它逼人走更正流程。
//
// 后端口径由集成测试（voucher_amend_links_old_and_new）覆盖；
// 本用例守 UI 接线：更正按钮的显隐、原因必填、链上的可读单号。

/** 用 API 建一张已记账凭证，返回 id */
async function postedVoucher(page, amount) {
  const r = await page.request.post("/api/vouchers", {
    data: {
      id: 0,
      period: 202601,
      date: "2026-01-15",
      word: "记",
      no: 0,
      attachments: 0,
      memo: "更正链 E2E",
      entries: [
        { line: 1, account_code: "1001", summary: "更正测试", debit: String(amount), credit: "0" },
        { line: 2, account_code: "2001", summary: "更正测试", debit: "0", credit: String(amount) },
      ],
    },
  });
  expect(r.ok(), `建凭证应成功：${await r.text()}`).toBeTruthy();
  const id = (await r.json()).id;
  // 两道闸门**默认都开**：草稿不能直接记账，必须先审核；涉及现金/银行科目的
  // 还要先出纳签字。
  // （引擎层的单测用 `tests::test_opts()` 夹具，它把两道闸门都显式关掉了，
  //   所以「默认开着」这件事只有 E2E 守得住 —— 少了这步 4 个用例会一起红。）
  await auditSignPost(page, id);
  return id;
}

test("已记账凭证：更正按钮可用，草稿凭证没有更正按钮", async ({ page }) => {
  await newBook(page, `E2E更正${Date.now()}`);
  const vid = await postedVoucher(page, 1000);

  // 已记账凭证的弹窗里应该有「更正」
  await page.evaluate((id) => openVoucherEditor(id), vid);
  await expect(page.locator("#v-amend")).toBeVisible({ timeout: 15_000 });
  // 已记账的凭证本来就有「反记账」「作废」不在此处（作废只给未记账的），
  // 这里只守更正按钮在。
  await expect(page.locator("#v-post")).toHaveCount(0);
});

test("更正原因必填；空原因不能提交", async ({ page }) => {
  await newBook(page, `E2E更正原因${Date.now()}`);
  const vid = await postedVoucher(page, 1000);
  await page.evaluate((id) => openVoucherEditor(id), vid);
  await expect(page.locator("#v-amend")).toBeVisible({ timeout: 15_000 });

  await page.click("#v-amend");
  // 原因对话框要真的弹出来
  await expect(page.locator("#rd-text")).toBeVisible({ timeout: 10_000 });
  // 提示要说清「为什么必须填原因」—— 否则用户会当成多余步骤直接绕过
  await expect(page.locator(".modal-mask").last()).toContainText("审计");

  // 直接点确定：空原因必须被前端拦住并给出反馈，不能静默无反应
  await page.click("#rd-ok");
  await expect(page.locator(".modal-mask").last()).toContainText("原因", { timeout: 5_000 });
  // 对话框仍在（没被关掉）
  await expect(page.locator("#rd-text")).toBeVisible();

  // Esc 取消
  await page.keyboard.press("Escape");
  await expect(page.locator("#rd-text")).toHaveCount(0);
});

test("更正后原凭证被红冲，红冲与更正两张各自可追溯", async ({ page }) => {
  await newBook(page, `E2E更正链${Date.now()}`);
  const vid = await postedVoucher(page, 1000);

  await page.evaluate((id) => openVoucherEditor(id), vid);
  await expect(page.locator("#v-amend")).toBeVisible({ timeout: 15_000 });
  await page.click("#v-amend");
  await expect(page.locator("#rd-text")).toBeVisible({ timeout: 10_000 });
  await page.fill("#rd-text", "金额录错，应为 1200");

  const [resp] = await Promise.all([
    page.waitForResponse((r) => /\/vouchers\/\d+\/amend/.test(r.url())),
    page.click("#rd-ok"),
  ]);
  expect(resp.ok(), `更正请求应成功：${await resp.text()}`).toBeTruthy();
  const new_id = (await resp.json()).new_id;
  expect(new_id).toBeGreaterThan(0);
  expect(new_id).not.toBe(vid);

  // 更正后自动打开新凭证：横幅要说清「更正自谁」和原因
  await expect(page.locator(".modal-mask").last()).toContainText("更正自", { timeout: 15_000 });
  const banner = await page.locator(".modal-mask").last().textContent();
  expect(banner).toContain("金额录错，应为 1200");
  // 可读单号（记-00000N），不是光一个数字 id —— 光数字在对账时没用
  expect(banner).toMatch(/记-\d{6}/);
  // 新凭证是草稿，在等会计审核
  await expect(page.locator(".modal-mask").last()).toContainText("未记账");

  // 回看原凭证：仍显示已记账（靠红冲抵消，不是被删掉），并标注被谁红冲 + 谁重做
  await page.keyboard.press("Escape");
  await page.evaluate((id) => openVoucherEditor(id), vid);
  await expect(page.locator(".modal-mask").last()).toContainText("红冲", { timeout: 15_000 });
  const oldBanner = await page.locator(".modal-mask").last().textContent();
  expect(oldBanner).toContain("金额录错，应为 1200");
  expect(oldBanner).toMatch(/记-\d{6}/g);
  await expect(page.locator(".modal-mask").last()).toContainText("已记账");
  // 已被更正过的凭证不再给「更正」按钮（链不能分叉）
  await expect(page.locator("#v-amend")).toHaveCount(0);
  // 三张成套生效，反记账 / 再红冲一次都不给（亮着却用不了最容易被当成 bug）
  await expect(page.locator("#v-unpost")).toHaveCount(0);
  await expect(page.locator("#v-reverse")).toHaveCount(0);
});

test("红冲凭证借贷互换且摘要带冲销标记（账上不能看着像重复录入）", async ({ page }) => {
  await newBook(page, `E2E红冲形态${Date.now()}`);
  const vid = await postedVoucher(page, 1000);

  const r = await page.request.post(`/api/vouchers/${vid}/amend`, {
    data: { reason: "科目录错" },
  });
  expect(r.ok(), `更正应成功：${await r.text()}`).toBeTruthy();
  const new_id = (await r.json()).new_id;

  // 从原凭证的链上取到红冲凭证
  const old = await (await page.request.get(`/api/vouchers/${vid}`)).json();
  const red_id = old.amend.amended_by;
  expect(red_id, "原凭证要标注被谁红冲").toBeGreaterThan(0);
  expect(old.amend.amended_by_new, "同批的更正凭证要单独列出").toBe(new_id);

  const red = await (await page.request.get(`/api/vouchers/${red_id}`)).json();
  // 原凭证：借 1001 / 贷 2001 —— 红冲必须反过来
  // （金额列以 TEXT 存、序列化带两位小数，用 startsWith 而非精确等值）
  expect(red.entries[0].credit, "红冲第一行应是贷方").toMatch(/^1000(\.0+)?$/);
  expect(red.entries[1].debit, "红冲第二行应是借方").toMatch(/^1000(\.0+)?$/);
  // 摘要要带冲销标记
  expect(red.entries[0].summary, "红冲摘要要带「冲销」").toContain("冲销");
  expect(red.memo).toContain("红冲");
  expect(red.memo).toContain("科目录错");

  // 更正凭证：分录原样复制
  const nw = await (await page.request.get(`/api/vouchers/${new_id}`)).json();
  expect(nw.entries[0].debit, "更正凭证要原样复制分录").toMatch(/^1000(\.0+)?$/);
  expect(nw.entries[1].credit).toMatch(/^1000(\.0+)?$/);
});

test("更正链三张成套：任何一张都不能被单独反记账（撤一张净额就错）", async ({ page }) => {
  await newBook(page, `E2E更正防翻倍${Date.now()}`);
  const vid = await postedVoucher(page, 1000);

  const r = await page.request.post(`/api/vouchers/${vid}/amend`, {
    data: { reason: "金额录错" },
  });
  expect(r.ok(), `更正应成功：${await r.text()}`).toBeTruthy();
  const new_id = (await r.json()).new_id;
  const old = await (await page.request.get(`/api/vouchers/${vid}`)).json();
  const red_id = old.amend.amended_by;

  // 三张最终都要是「已记账」，但**起点不同**：
  //   · 原凭证 vid ——postedVoucher() 已经记好了，别再碰
  //     （对它再审核会被拒：「已记账凭证不能审核」）
  //   · 红冲 red_id / 更正 new_id ——更正链产出的**新凭证**（草稿态），
  //     两道闸门都要重新走一遍
  //
  // 原来这里逐张 if/else 分支，正是「原凭证已审核、红冲未审核」这种状态最容易
  // 漏掉一道闸门的地方；现在分成两句：已记账的只断言，新草稿的一律走完整三步。
  for (const [id, what] of [[vid, "原凭证"], [red_id, "红冲"], [new_id, "更正"]]) {
    try {
      if (id === vid) {
        const v0 = await (await page.request.get(`/api/vouchers/${vid}`)).json();
        expect(v0.status, `${what}应仍保持已记账（不该被这条用例改动）`).toBe("posted");
      } else {
        await auditSignPost(page, id);
      }
    } catch (e) {
      throw new Error(`${what}：${e.message}`);
    }
  }

  // 任何一张被单独反记账都必须被拒
  for (const [id, what] of [[vid, "原凭证"], [red_id, "红冲凭证"], [new_id, "更正凭证"]]) {
    const u = await page.request.post(`/api/vouchers/${id}/unpost`);
    expect(u.status(), `${what} 不能被单独反记账（三张必须成套）`).toBe(400);
    expect(await u.text(), "错误信息要指向「成套」").toContain("成套");
  }

  // 界面同步不给这些按钮（亮着却用不了最容易被当成 bug）
  await page.evaluate((id) => openVoucherEditor(id), red_id);
  await expect(page.locator(".modal-mask").last()).toBeVisible({ timeout: 15_000 });
  await expect(page.locator("#v-unpost")).toHaveCount(0);
  await expect(page.locator("#v-reverse")).toHaveCount(0);

  // 未被更正的凭证仍能正常走「反记账 -> 作废 -> 恢复」，守卫不能误伤老路。
  // （已记账的凭证不能直接作废 —— 引擎既有规则 `validate_void` 拒绝 Posted，
  //   所以对照组必须先反记账。）
  const plain = await postedVoucher(page, 500);
  const un = await page.request.post(`/api/vouchers/${plain}/unpost`);
  expect(un.ok(), `反记账应成功：${await un.text()}`).toBeTruthy();
  const v1 = await page.request.post(`/api/vouchers/${plain}/void`, { data: { void: true } });
  expect(v1.ok(), `作废应成功：${await v1.text()}`).toBeTruthy();
  const v2 = await page.request.post(`/api/vouchers/${plain}/void`, { data: { void: false } });
  expect(v2.ok(), `恢复作废应成功：${await v2.text()}`).toBeTruthy();
  // 恢复后回到**审核态**而不是已记账：`unpost` 已经清掉 posted_by，
  // 而 `audited_by` 还在 —— 引擎既有语义，守卫不该改它。
  const after = await page.request.get(`/api/vouchers/${plain}`);
  expect((await after.json()).status, "恢复作废后应回到已审核").toBe("audited");
});

test("重复更正被拒，且错误信息指向「已被更正过」而不是笼统的状态", async ({ page }) => {
  await newBook(page, `E2E更正链分叉${Date.now()}`);
  const vid = await postedVoucher(page, 1000);

  const r = await page.request.post(`/api/vouchers/${vid}/amend`, {
    data: { reason: "第一次更正" },
  });
  expect(r.ok()).toBeTruthy();

  // 再更正一次必须被拒
  const r2 = await page.request.post(`/api/vouchers/${vid}/amend`, {
    data: { reason: "再改一次" },
  });
  expect(r2.status(), "重复更正应被拒").toBe(400);
  const msg = await r2.text();
  expect(msg, "错误信息应指向「已被更正过」").toContain("更正过");
});
