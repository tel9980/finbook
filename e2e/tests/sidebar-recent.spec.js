const { test, expect } = require("@playwright/test");
const { newBook } = require("../helpers");

// 2026-10-06 用户反馈：点过一个导航项，它就从原来的分组里消失、只剩「最近」里一份。
// 「最近」本该是**多给一个入口**，不是把菜单项搬走 —— 搬走之后分组少一项、组内计数
// 也跟着少，用起来像菜单项被删了。
//
// 这条用例同时钉住三个必须同时成立的点（缺任何一条都是真故障）：
//   ① 原分组里那一份必须一直在（反馈里说的「消失」）
//   ② 「最近」里那份是 nav-jump，不是第二份 nav-item —— 同一 data-view 出现两份
//      nav-item 会让 `.nav-item[data-view=x]` 一次匹配两个，Playwright 严格模式直接失败
//   ③ 点「最近」里那条要真的跳过去 —— 侧栏委托不认 nav-jump 就是「点了没反应」，
//      不报错、不跳转，是最难查的一类
//
// ⚠️ secHtml() 的结构是 `<div class="nav-sec">…标题…</div><div class="nav-body">项</div>`：
// 分区标题和项是**兄弟节点**，不是父子。所以锚点必须写 `.nav-sec[...] + .nav-body .xxx`，
// 写成后代选择器 `.nav-sec[...] .xxx` 永远匹配 0 个 —— 那样的断言恒真，等于没测。
test("侧栏「最近」是追加：访问过的页面仍在原分组，最近只是多一个入口", async ({ page }) => {
  await newBook(page, `E2E最近追加${Date.now()}`);

  await page.click('.nav-item[data-view="vouchers"]');
  await expect(page).toHaveURL(/#\/vouchers/);

  // ① 原分组那一份还在：锚点落在**分组 body** 里（data-sec^="g:"）
  await expect(page.locator('.nav-sec[data-sec^="g:"] + .nav-body .nav-item[data-view="vouchers"]')).toHaveCount(1);
  // 全站仍然只有一份 nav-item（「最近」那份不算）
  await expect(page.locator('.nav-item[data-view="vouchers"]')).toHaveCount(1);

  // ② 「最近」分区里多一个入口，class 是 nav-jump；那里不许再有 nav-item
  const recentBody = page.locator('.nav-sec[data-sec="recent"] + .nav-body');
  await expect(page.locator('.nav-sec[data-sec="recent"]')).toHaveCount(1);
  await expect(recentBody.locator('.nav-jump[data-view="vouchers"]')).toHaveCount(1);
  await expect(recentBody.locator('.nav-item[data-view="vouchers"]')).toHaveCount(0);

  // ③ 换个页面再点「最近」那一条，必须跳回 vouchers
  await page.click('.nav-item[data-view="dashboard"]');
  await expect(page).toHaveURL(/#\/dashboard/);
  await recentBody.locator('.nav-jump[data-view="vouchers"]').click();
  await expect(page).toHaveURL(/#\/vouchers/);
});
