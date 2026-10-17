const { test, expect } = require("@playwright/test");
const { newBook, setSimpleNav } = require("../helpers");

// 精简模式：69 个功能平铺 16 组，新用户建完账第一眼看到的是 MRP、工艺路线、
// 预算预警这类跟他完全无关的东西。桌面端已经这么做了，这里对齐。
//
// 这个模式最容易**做坏**，坏法有三个，全都在下面被钉住：
//   ① 藏起来变成了拿掉 —— 搜「MRP」搜不到了，用户永远进不去那个页面
//   ② 用户关掉后下次进来又自己开回来（没区分「没设置过」和「显式关掉」）
//   ③ 精简顺手拦了路由 —— 直接敲 URL 也进不去，那才是真的功能没了
//
// ⚠️ 三种状态必须用 `newBook(page, name, {simpleNav: …})` 的三态来构造，
// 不能靠「先建账再 localStorage.removeItem 再 reload」：
// setSimpleNav 用的是 addInitScript，**每次导航前都会重写**一遍，
// removeItem 的效果在 reload 时就被它抹掉了 —— 测到的是「显式关掉」，
// 不是「从没设置过」。这两种是不同的代码路径，混起来测等于没测。

// ---- 默认开：localStorage 里根本没有这个 key（新用户路径）----
test("精简默认开：只铺日常功能，其余的搜得到", async ({ page }) => {
  await newBook(page, `E2E精简默认${Date.now()}`, { simpleNav: null });
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
  expect(laidOut.length, "默认侧栏应该是精简后的一小撮").toBeLessThanOrEqual(20);

  // 不变量①：藏起来 ≠ 拿掉。打「MRP」必须搜得到并能跳进去。
  await page.fill("#nav-q", "MRP");
  await expect(page.locator('.nav-item[data-view="mrp"]')).toHaveCount(1);
  await page.click('.nav-item[data-view="mrp"]');
  await expect(page).toHaveURL(/#\/mrp/);

  // 换个冷门的也照样搜得到 —— 证明这不是只对第一页生效
  await page.fill("#nav-q", "批次");
  await expect(page.locator('.nav-item[data-view="inv-batch"]')).toHaveCount(1);
});

// ---- 不变量②：显式关掉后不会被自己开回来 ----
//
// ⚠️ 这条**绝不能用 `newBook(…, {simpleNav: true})`**：那会注册一个往
// localStorage 写 "true" 的 addInitScript，而 **addInitScript 不能注销** ——
// 后面再调 `setSimpleNav(page, null)` 只是「不再新增」，已注册的那个照样每次
// 导航前执行，值被写回 "true"。于是断言测到的根本不是持久化，而是「脚本又写
// 了一遍」。我第一版就是这么写的，CI 报 "Expected: not checked / Received: checked"。
//
// 正确做法：`simpleNav: null` 起步，全程不注入任何脚本，只靠用户真实的点击
// 改变状态，reload 后看值还在不在。这才是持久化该有的测法。
test("精简开关：关掉后刷新仍是关的，切回来也保持", async ({ page }) => {
  await newBook(page, `E2E精简开关${Date.now()}`, { simpleNav: null });

  // 起点：没设置过 → 默认开
  await expect(page.locator("#nav-simple")).toBeChecked();
  await expect(page.locator('.nav-item[data-view="mrp"]')).toHaveCount(0);

  // 用户关掉 → 全量
  await page.uncheck("#nav-simple");
  await expect(page.locator('.nav-item[data-view="mrp"]')).toHaveCount(1);

  // 刷新后仍然是关的 —— 此时 localStorage 里是用户上一次点击真正写下的 false
  await page.reload();
  await expect(page.locator("#nav-simple")).not.toBeChecked();
  await expect(page.locator('.nav-item[data-view="mrp"]')).toHaveCount(1);

  // 再切回精简，也是持久的
  await page.check("#nav-simple");
  await page.reload();
  await expect(page.locator("#nav-simple")).toBeChecked();
  await expect(page.locator('.nav-item[data-view="mrp"]')).toHaveCount(0);
  // 切回精简后搜索仍然找得到（不变量①在两种状态下都成立）
  await page.fill("#nav-q", "MRP");
  await expect(page.locator('.nav-item[data-view="mrp"]')).toHaveCount(1);
});

// ---- 不变量③：精简只管侧栏画不画，不管路由 ----
// 独立成条：万一以后有人在 goView 里也加了精简判断，只有这条能抓住。
// 这里可以用 simpleNav:true —— 它只要求「精简态下能敲 URL 进去」，不涉及持久化。
test("精简不拦路由：直接访问 URL 仍能进到未铺开的页面", async ({ page }) => {
  await newBook(page, `E2E精简路由${Date.now()}`, { simpleNav: true });
  await expect(page.locator('.nav-item[data-view="mrp"]')).toHaveCount(0);

  await page.goto("/#/mrp");
  await expect(page).toHaveURL(/#\/mrp/);
  await expect(page.locator("h2").first()).toBeVisible();

  // 到了未铺开的页面，侧栏里已铺开的当前页仍要标 .active ——
  // 不标的话用户在精简侧栏里会失去「我在哪」的位置感。
  await page.goto("/#/vouchers");
  await expect(page.locator('.nav-item[data-view="vouchers"].active')).toHaveCount(1);
});