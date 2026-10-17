const { test, expect } = require("@playwright/test");
const { newBook } = require("../helpers");

// 回归：CI run 36223455217 —— 建账后工作台（async 视图）迟到写入把用户刚切的视图整体覆盖，
// 导致 #new-v / #pu-add / #ft-bill 等工具条元素消失、E2E 超时。
// 这里人为把 /api/workbench 延迟 1.5s（放大竞态窗口），断言切换后的视图不被覆盖。
test("UX：建账后立即切换视图，不被慢返回的仪表盘覆盖", async ({ page }) => {
  await page.addInitScript(() => {
    const of = window.fetch.bind(window);
    window.fetch = (input, init) => {
      const url = typeof input === "string" ? input : (input && input.url) || "";
      if (String(url).includes("/api/workbench")) {
        return new Promise((res, rej) => setTimeout(() => of(input, init).then(res, rej), 1500));
      }
      return of(input, init);
    };
  });

  await newBook(page, `E2E竞态${Date.now()}`);

  // 工作台的 3 个 API 还在返回路上，立刻切到凭证页
  await page.click('.nav-item[data-view="vouchers"]');
  await expect(page.locator("#new-v")).toBeVisible({ timeout: 10_000 });
  await expect(page.locator("#main h2")).toContainText("记账凭证");

  // 等到被延迟的仪表盘响应落地之后，再断言一次：新视图仍未被覆盖
  await page.waitForTimeout(2500);
  await expect(page.locator("#new-v")).toBeVisible();
  await expect(page.locator("#main h2")).toContainText("记账凭证");

  // 再切一个 async 视图，同样不被覆盖
  await page.click('.nav-item[data-view="accounts"]');
  await expect(page.locator("#main h2")).toContainText("会计科目", { timeout: 10_000 });
});

// 回归：列表操作列（含按钮）不得被表格省略号样式裁切，否则点击会命中 <td> 而非按钮。
test("UX：列表操作列按钮可点击（不被 overflow 裁切）", async ({ page }) => {
  await newBook(page, `E2E操作列${Date.now()}`);
  await page.click('.nav-item[data-view="accounts"]');
  const edit = page.locator('[data-act="edit"][data-code="1001"]');
  await expect(edit).toBeVisible({ timeout: 15_000 });
  // 按钮中心点必须能被命中（Playwright 点击自带可点击性检查：被裁切/被覆盖会超时）
  await edit.click({ trial: true, timeout: 10_000 });
});
