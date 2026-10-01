const { test, expect } = require("@playwright/test");
const { newBook } = require("../helpers");

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

  await page.click("#bg-add");
  await rows.last().locator(".bg-code").fill("1001");
  await rows.last().locator(".bg-yb").fill("1000");
  await page.click("#bg-add");
  await rows.last().locator(".bg-code").fill("2001");
  await rows.last().locator(".bg-dir").selectOption("credit");
  await rows.last().locator(".bg-yb").fill("1000");

  // 试算卡片只在重绘时更新：再加一行空行触发重绘（空行保存时会被过滤）
  await page.click("#bg-add");
  await expect(page.getByText("✓ 平衡")).toBeVisible({ timeout: 15_000 });
  await page.click("#bg-save");
  await expect(page.locator("#main table.grid tbody")).toContainText("已有", { timeout: 15_000 });
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
