const { test, expect } = require("@playwright/test");
const { newBook } = require("../helpers");

// 开放 API 密钥的端到端：走真实 UI 签发 → 用明文调 v1 → 停用 → 401
test("开放 API：签发密钥→调用只读接口→停用即失效", async ({ page }) => {
  await newBook(page, `E2E密钥${Date.now()}`);

  // 「全部账套」= 平台总览页，密钥管理挂在这一页
  await page.click('.nav-item[data-view="platform-books"]');
  await expect(page.locator("#ak-list")).toBeVisible({ timeout: 15_000 });
  await expect(page.locator("#ak-list")).toContainText("还没有签发任何密钥", { timeout: 15_000 });

  await page.click("#ak-new");
  await expect(page.locator("#ak-name")).toBeVisible({ timeout: 10_000 });
  await page.fill("#ak-name", "E2E 仓库对接");

  // 绑定**本次测试刚建的**账套。不能取「第一个非平台级选项」——
  // 全量跑时目录里还留着前面 spec 的账套（workers:1 下残留很正常），
  // 第一个未必是这个用例的。按公司名里的唯一标记挑，命中不到就直接失败。
  const bookKey = await page
    .locator("#ak-book option")
    .evaluateAll((els) => {
      const hit = els.find((e) => (e.textContent || "").includes("E2E密钥"));
      return hit ? hit.value : null;
    });
  expect(bookKey, "账套下拉里应能找到本用例的账套（E2E密钥*）").toBeTruthy();
  await page.selectOption("#ak-book", bookKey);
  expect(await page.locator("#ak-book").inputValue()).toBe(bookKey);

  await page.click("#ak-do");
  // 明文只显示一次
  await expect(page.locator("#ak-plain-val")).toBeVisible({ timeout: 10_000 });
  const secret = (await page.locator("#ak-plain-val").textContent()).trim();
  expect(secret.startsWith("fbk_")).toBeTruthy();
  expect(secret.length).toBeGreaterThan(30);

  // 列表里能看到，但不能有明文
  await expect(page.locator("#ak-list")).toContainText("E2E 仓库对接", { timeout: 10_000 });
  const listText = await page.locator("#ak-list").textContent();
  expect(listText).not.toContain(secret);

  // 用这把密钥调 v1（从页面上下文 fetch，验证真实跨接口链路）
  const probe = await page.evaluate(async (sec) => {
    const call = async (path, key) => {
      const r = await fetch(path, { headers: { Authorization: "Bearer " + (key || sec) } });
      const text = await r.text();
      let json = null;
      try { json = JSON.parse(text); } catch (e) { /* 非 JSON 就留 null */ }
      return { status: r.status, json };
    };
    return {
      me: await call("/api/v1/me"),
      accounts: await call("/api/v1/accounts"),
      report: await call("/api/v1/trial-balance"),
      bad: await call("/api/v1/accounts", "fbk_" + "0".repeat(64)),
    };
  }, secret);

  // 绑定账套的密钥不需要 X-Book-Key
  expect(probe.me.status).toBe(200);
  expect(probe.me.json.readonly).toBe(true);
  // 确认拿到的就是刚建那个账套（防止绑错套还自认为成功）
  expect(probe.me.json.book_key).toBe(bookKey);
  // 默认只读：基础档案可读，报表被挡
  expect(probe.accounts.status).toBe(200);
  expect(probe.report.status).toBe(403);
  // 错密钥 → 401
  expect(probe.bad.status).toBe(401);

  // 停用 → 立刻 401
  await page.click('#ak-list button[data-ak-toggle]');
  await expect(page.locator("#ak-list")).toContainText("已停用", { timeout: 10_000 });
  const after = await page.evaluate(async (sec) => {
    const r = await fetch("/api/v1/accounts", { headers: { Authorization: "Bearer " + sec } });
    return r.status;
  }, secret);
  expect(after).toBe(401);
});
