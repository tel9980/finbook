const { test, expect } = require("@playwright/test");
const { newBook } = require("../helpers");

// 精简模式：69 个功能平铺 16 组，新用户建完账第一眼看到的是 MRP、工艺路线、
// 预算预警这类跟他完全无关的东西。桌面端已经这么做了，这里对齐。
//
// 这个模式最容易**做坏**，坏法有三个，全都在下面被钉住：
//   ① 藏起来变成了拿掉 —— 搜「MRP」搜不到了，用户永远进不去那个页面
//   ② 默认开 → 老用户/测试全炸 —— 90 条 E2E 里有 70 个视图靠点侧栏进去
//   ③ 用户关掉后下次进来又自己开回来（没区分「没设置过」和「显式关掉」）
//
// 这条用例**不开** newBook 的精简关闭（那正是被测对象），所以自己注入
// localStorage 控制状态。

test.beforeEach(async ({ page }) => {
  await newBook(page, `E2E精简${Date.now()}`);
});

// 点开侧栏里的「显示全部功能」，好让下面的断言都在全量状态下做（1）以外的事
async function setSimple(page, on) {
  await page.evaluate((v) => {
    localStorage.setItem("nav_simple@admin", JSON.stringify(v));
  }, on);
  await page.reload();
  await expect(page.locator(".nav-item[data-view]").first()).toBeVisible();
}

test("精简模式：默认只铺日常功能，其余的搜得到、开关能来回切", async ({ page }) => {
  // ---- 默认开（newBook 之后 localStorage 里没有这个 key）----
  await page.evaluate(() => localStorage.removeItem("nav_simple@admin"));
  await page.reload();
  await expect(page.locator(".nav-item[data-view]").first()).toBeVisible();

  const laidOut = await page.locator(".nav-body .nav-item").evaluateAll((els) =>
    Array.from(new Set(els.map((e) => e.dataset.view)))
  );
  // 生产制造 / 预算 / 库存那一大片必须**不在**默认列表里
  for (const hidden of ["mrp", "routing", "work-report", "budget-analysis", "inv-abc"]) {
    expect(laidOut, `${hidden} 不该出现在默认侧栏`).not.toContain(hidden);
  }
  // 日常的那些必须在
  for (const shown of ["vouchers", "begin", "accounts", "aux", "ledger", "reports"]) {
    expect(laidOut, `${shown} 应该出现在默认侧栏`).toContain(shown);
  }

  // ---- 不变量 ①：藏起来 ≠ 拿掉。搜「MRP」必须搜得到并能跳进去 ----
  await page.fill("#nav-q", "MRP");
  await expect(page.locator('.nav-item[data-view="mrp"]')).toHaveCount(1);
  await page.click('.nav-item[data-view="mrp"]');
  await expect(page).toHaveURL(/#\/mrp/);

  // 换个更冷门的也照样搜得到 —— 证明这不是只对第一页生效
  await page.fill("#nav-q", "批次");
  await expect(page.locator('.nav-item[data-view="inv-batch"]')).toHaveCount(1);

  // ---- 开关能切回全量 ----
  await page.fill("#nav-q", "");
  await page.uncheck("#nav-simple");
  await expect(page.locator('.nav-item[data-view="mrp"]')).toHaveCount(1);

  // ---- 不变量 ③：显式关掉后刷新仍然是关的 ----
  await page.reload();
  await expect(page.locator("#nav-simple")).not.toBeChecked();
  await expect(page.locator('.nav-item[data-view="mrp"]')).toHaveCount(1);

  // ---- 再切回精简，也是持久的 ----
  await page.check("#nav-simple");
  await page.reload();
  await expect(page.locator("#nav-simple")).toBeChecked();
  await expect(page.locator('.nav-item[data-view="mrp"]')).toHaveCount(0);
  // 但搜索仍然找得到
  await page.fill("#nav-q", "MRP");
  await expect(page.locator('.nav-item[data-view="mrp"]')).toHaveCount(1);
});

// 精简只该改侧栏**画不画出**，不该拦路由：直接敲 URL 必须照常进去。
// 这条独立于上一条 —— 万一以后有人在 goView 里也加了精简判断，只有这条能抓住。
test("精简模式不拦路由：直接访问 URL 仍能进到未铺开的页面", async ({ page }) => {
  await setSimple(page, true);
  await expect(page.locator('.nav-item[data-view="mrp"]')).toHaveCount(0);

  await page.goto("/#/mrp");
  await expect(page).toHaveURL(/#\/mrp/);
  await expect(page.locator("h2").first()).toBeVisible();
  // 到了页面，侧栏要把它标成「当前页」—— 即便那一项没画出来。
  // 不标的话用户在精简侧栏里会失去「我在哪」的位置感。
  await page.goto("/#/vouchers");
  await expect(page.locator('.nav-item[data-view="vouchers"].active')).toHaveCount(1);
});