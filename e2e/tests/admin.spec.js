const { test, expect } = require("@playwright/test");
const { newBook } = require("../helpers");

/// 期初表里某科目的金额（.bg-yb 的值）。
///
/// 不能用 toContainText 判科目编码 —— 编码在 <input class="bg-code"> 的 **value**
/// 里，不是文本节点，toContainText 永远看不到（我第一版就是这么写的，白等 15 秒超时）。
/// 金额同理，但金额框还带 `data-i`，可以按行取。
async function beginAmountOf(page, code) {
  return page.evaluate((c) => {
    const rows = Array.from(document.querySelectorAll("#main table.grid tbody tr"));
    for (const tr of rows) {
      const inp = tr.querySelector(".bg-code");
      if (inp && inp.value.trim() === c) {
        const yb = tr.querySelector(".bg-yb");
        return yb ? yb.value.trim() : null;
      }
    }
    return null;
  }, code);
}

/// 期初表里还剩下哪些科目编码
async function beginCodes(page) {
  return page.evaluate(() =>
    Array.from(document.querySelectorAll("#main table.grid tbody .bg-code"))
      .map((i) => i.value.trim())
      .filter(Boolean)
  );
}

test("会计科目：查询→编辑备注", async ({ page }) => {
  await newBook(page, `E2E科目${Date.now()}`);
  await page.click('.nav-item[data-view="accounts"]');
  await page.fill("#acct-kw", "1001");
  await page.click("#acct-search");
  await expect(page.locator("#main table.grid tbody")).toContainText("库存现金", { timeout: 15_000 });

  await page.locator('[data-act="edit"][data-code="1001"]').click();
  await page.fill("#ac-memo", "E2E备注");
  await page.click("#ac-save");
  await expect(page.locator("#main table.grid tbody")).toContainText("E2E备注", { timeout: 15_000 });
});

test("期初建账：录入→试算平衡→保存", async ({ page }) => {
  await newBook(page, `E2E期初${Date.now()}`);
  await page.click('.nav-item[data-view="begin"]');
  const rows = page.locator("#main table.grid tbody tr");

  // 试算平衡必须**随输入实时刷新**。
  // 这里刻意不点「添加行」来触发重绘：改造前合计只在 render() 里算，而 render()
  // 只在增删行时触发，所以这个测试当时是靠「再加一行空行」才看到数字的。
  // 一旦合计退回只在重绘时更新，下面这条断言就会失败（原样是恒显示 0.00/✓平衡）。
  await page.click("#bg-add");
  await rows.last().locator(".bg-code").fill("1001");
  await rows.last().locator(".bg-yb").fill("1000");
  await expect(page.locator("#bg-totals")).toContainText("1,000.00", { timeout: 5_000 });

  // 只填借方 → 必须报不平衡，且给出差额
  await expect(page.locator("#bg-balance")).toContainText("不平衡", { timeout: 5_000 });
  await expect(page.locator("#bg-balance")).toContainText("1,000.00", { timeout: 5_000 });

  await page.click("#bg-add");
  await rows.last().locator(".bg-code").fill("2001");
  await rows.last().locator(".bg-dir").selectOption("credit");
  await rows.last().locator(".bg-yb").fill("1000");
  await expect(page.getByText("✓ 平衡")).toBeVisible({ timeout: 5_000 });

  await page.click("#bg-save");
  await expect(page.locator("#main table.grid tbody")).toContainText("已有", { timeout: 15_000 });
});

test("期初建账：移除一行会真的从库里删掉", async ({ page }) => {
  await newBook(page, `E2E期初删除${Date.now()}`);
  await page.click('.nav-item[data-view="begin"]');
  const rows = page.locator("#main table.grid tbody tr");

  await page.click("#bg-add");
  await rows.last().locator(".bg-code").fill("1001");
  await rows.last().locator(".bg-yb").fill("1000");
  await page.click("#bg-add");
  await rows.last().locator(".bg-code").fill("2001");
  await rows.last().locator(".bg-dir").selectOption("credit");
  await rows.last().locator(".bg-yb").fill("1000");
  await expect(page.getByText("✓ 平衡")).toBeVisible({ timeout: 5_000 });
  await page.click("#bg-save");
  await expect(page.locator("#main table.grid tbody")).toContainText("已有", { timeout: 15_000 });
  expect(await beginCodes(page)).toEqual(["1001", "2001"]);

  // 移除 2001（贷方 1,000）→ 只剩借方 1,000，必须变成不平衡
  // 改造后端只有 upsert、没有删除语义，这一步在界面上看着行没了，库里纹丝不动，
  // 重新加载又冒出来。
  //
  // 按行号定位而不是 `.bg-code[value="2001"]`：value 是 DOM **属性**不是 HTML 属性，
  // 那个选择器在这里永远匹配不到（我第一版就这么写的，白等 60 秒超时）。
  const i2001 = (await beginCodes(page)).indexOf("2001");
  expect(i2001, "应能找到 2001 那一行").toBeGreaterThanOrEqual(0);
  await page.locator("#main table.grid tbody tr").nth(i2001).locator("[data-rm]").click();
  await expect(page.locator("#bg-balance")).toContainText("不平衡", { timeout: 5_000 });
  expect(await beginCodes(page)).toEqual(["1001"]);
  await page.click("#bg-save");
  // 移除贷方后借贷不平，保存前会先问一句（防不平衡的期初落库）—— 必须点确定，
  // 忘了点的话下面 reload 回来两行都在，会误判成"删除没生效"
  await expect(page.locator("#cf-ok")).toBeVisible({ timeout: 5_000 });
  await page.click("#cf-ok");
  // 保存后重新加载：2001 必须不再出现（原来它还在）
  await page.reload();
  await page.click('.nav-item[data-view="begin"]');
  await expect(page.locator("#main table.grid tbody")).toContainText("已有", { timeout: 15_000 });
  expect(await beginCodes(page)).toEqual(["1001"]);
  // 金额是 fmt 过的文本（整千不带小数位）。关键是**不能翻倍**：原来移除+重存
  // 会让同一行既 upsert 一遍又留着旧的，金额变成 2,000。
  expect(await beginAmountOf(page, "1001")).toBe("1,000");
});

test("期初建账：不存在的科目被拒，不会静默吞掉金额", async ({ page }) => {
  await newBook(page, `E2E期初错科目${Date.now()}`);
  await page.click('.nav-item[data-view="begin"]');
  const rows = page.locator("#main table.grid tbody tr");

  await page.click("#bg-add");
  await rows.last().locator(".bg-code").fill("1001");
  await rows.last().locator(".bg-yb").fill("1000");
  await page.click("#bg-add");
  await rows.last().locator(".bg-code").fill("999999");
  await rows.last().locator(".bg-yb").fill("1000");
  // 用接口断言错误本身（toast 会过期，轮询式断言容易错过）
  const res = await page.request.post("/api/begin", {
    data: {
      rows: [
        { account_code: "1001", dir: "debit", yb: "1000", ad: "0", ac: "0", qty: null },
        { account_code: "999999", dir: "debit", yb: "1000", ad: "0", ac: "0", qty: null },
      ],
      delete_ids: [],
    },
  });
  expect(res.status(), "不存在的科目应 400").toBe(400);
  // 报错要点名是哪个科目，否则会计不知道改哪一位
  expect(await res.text()).toContain("999999");

  // 且整批不能落地：走真实界面点保存，1001 也不该被顺带写进去
  // （两行都是借方 → 借贷不平 → 先弹确认框；不点确定的话这里等于什么都没保存，
  //  断言会**假通过**，所以必须点确定，让请求真的发出去撞 400）
  await page.click("#bg-save");
  await expect(page.locator("#cf-ok")).toBeVisible({ timeout: 5_000 });
  await page.click("#cf-ok");
  await expect(page.locator("#toast")).toContainText("999999", { timeout: 10_000 });

  await page.reload();
  await page.click('.nav-item[data-view="begin"]');
  // 「尚未填写任何金额」在试算卡片下面（#bg-balance-note），不在表格里
  await expect(page.locator("#bg-balance-note")).toContainText("尚未填写", { timeout: 15_000 });
  expect(await beginCodes(page)).toEqual([]);
});

test("操作日志与备份恢复", async ({ page }) => {
  await newBook(page, `E2E日志${Date.now()}`);
  await page.click('.nav-item[data-view="logs"]');
  await expect(page.locator("#log-rows")).not.toContainText("加载中", { timeout: 15_000 });

  await page.click('.nav-item[data-view="backup"]');
  await page.click("#bk-new");
  await expect(page.locator("#bk-rows")).toContainText(".fbk", { timeout: 15_000 });
});

test("安全中心：新建账套用户→删除", async ({ page }) => {
  await newBook(page, `E2E安全${Date.now()}`);

  // 账套内子账号必须对应同名平台账号，先开通
  await page.click('.nav-item[data-view="platform-users"]');
  await page.click("#pu-add");
  await page.fill("#nc-u", "bookuser1");
  await page.fill("#nc-p", "Passw0rd!");
  await page.click("#nc-save");
  await expect(page.locator("#pu-list")).toContainText("bookuser1", { timeout: 15_000 });

  await page.click('.nav-item[data-view="security"]');
  await expect(page.locator("#u-table")).toContainText("admin", { timeout: 15_000 });

  await page.click("#new-user");
  await page.fill("#nu-u", "bookuser1");
  await page.fill("#nu-n", "账套用户");
  await page.fill("#nu-p", "Passw0rd!");
  await page.click("#nu-save");
  await expect(page.locator("#u-table")).toContainText("bookuser1", { timeout: 15_000 });

  await page.locator('[data-del="bookuser1"]').click();
  await page.click("#cf-ok");
  await expect(page.locator("#u-table")).not.toContainText("bookuser1", { timeout: 15_000 });

  // 清理平台账号
  await page.click('.nav-item[data-view="platform-users"]');
  await page.locator('[data-act="del"][data-u="bookuser1"]').click();
  await page.click("#cf-ok");
  await expect(page.locator("#pu-list")).not.toContainText("bookuser1", { timeout: 15_000 });
});
