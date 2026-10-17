const { test, expect } = require("@playwright/test");
const { newBook } = require("../helpers");

test("数据导入：缺失科目映射后导入凭证", async ({ page }) => {
  await newBook(page, `E2E导入${Date.now()}`);
  await page.click('.nav-item[data-view="imports"]');
  // 视图是异步渲染的：必须等控件真的在 DOM 里再操作。
  // 少了这个等待，selectOption/fill 会在控件出现前执行，事件监听器尚未绑定，
  // 点「预检」无人应答，#imp-result 停在空串——看起来像产品 bug，其实是时序。
  await expect(page.locator("#imp-kind")).toBeVisible({ timeout: 15_000 });
  // 等 viewImports 的监听器真正绑好。
  // viewImports 是 async：先 innerHTML，再 `await api("/accounts")`，**之后**才 addEventListener。
  // 所以「#imp-kind 可见」不等于「按钮能点」——此刻点「预检」可能无人应答，#imp-result 停在空串。
  // 等待信号用 #imp-text 的 placeholder：syncKind() 绑定后立刻写入列头说明，
  // 而 innerHTML 模板里 textarea 是没有 placeholder 的。
  await expect(page.locator("#imp-text")).toHaveAttribute("placeholder", /./, { timeout: 15_000 });
  await page.selectOption("#imp-kind", "voucher");
  await page.selectOption("#imp-template", "generic");
  await page.fill(
    "#imp-text",
    "2026-01-18,记,导入收款,9999,300,0\n2026-01-18,记,导入收款,2001,0,300"
  );

  await page.click("#imp-analyze");
  await expect(page.locator("#imp-result")).toContainText("1 个缺失科目", { timeout: 10_000 });
  await page.selectOption('.imp-map[data-code="9999"]', "1001");

  await page.click("#imp-run");
  await expect(page.locator("#imp-result")).toContainText("成功导入 1 条", { timeout: 10_000 });

  await page.click('.nav-item[data-view="vouchers"]');
  await expect(page.locator("#v-table tbody")).toContainText("导入收款");
});

test("凭证模板：新建每月模板→本期到期生成凭证", async ({ page }) => {
  await newBook(page, `E2E模板${Date.now()}`);
  await page.click('.nav-item[data-view="templates"]');
  await page.click("#tpl-new");
  await page.fill("#tp-name", "月度房租");
  await page.selectOption("#tp-freq", "monthly");
  await page.fill("#tp-start", "202601");
  await page.fill("#tp-end", "202612");

  const rows = page.locator("#tp-entries tbody tr");
  await rows.nth(0).locator(".te-sum").fill("房租");
  await rows.nth(0).locator("select.acct-sel").selectOption("660201");
  await rows.nth(0).locator(".te-amt").fill("1000");
  await page.click("#tp-add");
  await rows.nth(1).locator(".te-sum").fill("房租");
  await rows.nth(1).locator("select.acct-sel").selectOption("1001");
  await rows.nth(1).locator(".te-dir").selectOption("credit");
  await rows.nth(1).locator(".te-amt").fill("1000");
  await page.click("#tp-save");
  await expect(page.locator("#tpl-body")).toContainText("月度房租", { timeout: 10_000 });
  await expect(page.locator("#tpl-body")).toContainText("每月");

  await page.click("#tpl-tab-due");
  await expect(page.locator("#tpl-body")).toContainText("月度房租", { timeout: 10_000 });
  await page.locator("[data-gen]").first().click();
  await page.click("#cf-ok");
  await expect(page.locator("#tpl-body")).toContainText("202601", { timeout: 10_000 });

  await page.click('.nav-item[data-view="vouchers"]');
  await expect(page.locator("#v-table tbody")).toContainText("房租");
});

test("数据导入：选「银行对账单」跳走后，回来不会再被拽去银行对账", async ({ page }) => {
  await newBook(page, `E2E导入返回${Date.now()}`);
  await page.click('.nav-item[data-view="imports"]');
  await expect(page.locator("#imp-kind")).toBeVisible({ timeout: 15_000 });
  // 等 viewImports 的监听器绑好（innerHTML 先出，`await api("/accounts")` 之后才 addEventListener）
  await expect(page.locator("#imp-text")).toHaveAttribute("placeholder", /./, { timeout: 15_000 });

  // 选哨兵项「银行对账单 →（转到银行对账）」→ 跳到银行对账页
  await page.selectOption("#imp-kind", "__bank");
  await expect(page).toHaveURL(/#\/bank/, { timeout: 10_000 });

  // 回到数据导入：不能又被视图快照里的 __bank + restoreViewState 300ms 后派发的 change 拽走
  await page.click('.nav-item[data-view="imports"]');
  await expect(page.locator("#imp-kind")).toBeVisible({ timeout: 15_000 });
  await expect(page.locator("#imp-kind")).toHaveValue("aux");
  // 必须跨过 restoreViewState 的 300ms change 派发窗口 —— 没修好的话是在这之后跳走的
  await page.waitForTimeout(700);
  await expect(page).toHaveURL(/#\/imports/);
  await expect(page.locator("#imp-kind")).toHaveValue("aux");
});
