const { expect } = require("@playwright/test");

// 本 worker 上一个用例建的账套：下个用例先删掉，避免撞"单账号最多 10 个账套"的上限。
// Playwright 每个 worker 是独立进程，模块级变量天然按 worker 隔离。
let lastBookKey = null;

/// 登录 → 建账（起始期间 2026-01）→ 停在可用界面
async function newBook(page, company) {
  await page.goto("/");
  await expect(page.locator("#u")).toBeVisible({ timeout: 15_000 });
  await page.fill("#u", "admin");
  await page.fill("#p", "Admin!2026");
  await page.click('#login-form button[type="submit"]');
  // 必须等登录真正完成（账套选择界面出现）再删旧账套：click 只保证点击已发出，
  // 立刻用 page.request 会因还没有会话 cookie 而 401。
  await expect(page.locator("#new-book")).toBeVisible({ timeout: 15_000 });

  if (lastBookKey) {
    await page.request.delete(`/api/books/${encodeURIComponent(lastBookKey)}`).catch(() => {});
    lastBookKey = null;
  }

  await page.click("#new-book");
  await page.fill("#cb-company", company);
  await page.fill("#cb-start", "2026-01");
  await page.click("#cb-save");
  await expect(page.locator('.nav-item[data-view="vouchers"]')).toBeVisible({ timeout: 15_000 });

  try {
    const d = await (await page.request.get("/api/books")).json();
    const b = (d.books || []).find((x) => x.company === company);
    if (b) lastBookKey = b.key;
  } catch (e) {
    // 记录失败只影响清理，不影响用例本身
  }
}

/// 录一张凭证并保存（不记账）。rows: [{ code, summary, debit, credit, aux? }]
async function postVoucher(page, { date, rows }) {
  await page.click('.nav-item[data-view="vouchers"]');
  await page.click("#new-v");
  await page.fill("#v-date", date);
  // 辅助面板展开会插入明细 tr，用 :has 只匹配分录行
  const entryRows = page.locator("#v-entries tbody tr:has(select.acct-sel)");
  for (let i = 0; i < rows.length; i++) {
    const r = rows[i];
    await entryRows.nth(i).locator("select.acct-sel").selectOption(r.code);
    await entryRows.nth(i).locator(".e-sum").fill(r.summary);
    if (r.debit) await entryRows.nth(i).locator(".e-d").fill(r.debit);
    if (r.credit) await entryRows.nth(i).locator(".e-c").fill(r.credit);
    if (r.aux) {
      await entryRows.nth(i).locator(".e-aux").click();
      for (const [k, v] of Object.entries(r.aux)) {
        await page.fill(`.aux-in[data-k="${k}"]`, v);
      }
    }
  }
  await page.click("#v-save");
  await expect(page.locator(".modal-mask")).toHaveCount(0, { timeout: 10_000 });
}

module.exports = { newBook, postVoucher };
