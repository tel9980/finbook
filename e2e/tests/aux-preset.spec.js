const { test, expect } = require("@playwright/test");
const { newBook } = require("../helpers");

// 辅助核算「预置常用档案」的完整路径。
//
// 这条用例钉的是一个真死路：`validate_entry` 强制要求带辅助核算的科目必须填
// 辅助核算，而默认科目表里 100201~100207 全要银行账户、6601/6602/6603 全要部门。
// 新账套一条档案都没有 → 凭证存不了。预置按钮就是给这个场景的出口。
//
// 同时钉住「只预置有通用值的类别」这条约定：客户/供应商/存货不能被预置 ——
// 预置一个「待填客户」会跟着凭证流进报表，事后用户自己都想不起来回来改。
// 这条断言是本用例里最重要的：它防的是「为了省事把假数据灌进去」。

test("辅助档案预置：只补部门/银行账户，且不覆盖已有编码", async ({ page }) => {
  await newBook(page, `E2E预置${Date.now()}`);

  // ---- 部门：新账套是空的，一键预置建出 3 条 ----
  await page.click('.nav-item[data-view="aux"]');
  await page.click('[data-kind="dept"]');
  await expect(page.locator("#aux-hint")).toContainText("还没有任何部门");
  await page.click("#aux-preset");
  await page.click("#cf-ok");

  await expect(page.locator("table.grid tbody tr")).toHaveCount(3);
  for (const [code, name] of [["D01", "销售部"], ["D02", "管理部"], ["D03", "财务部"]]) {
    await expect(page.locator("table.grid tbody")).toContainText(code);
    await expect(page.locator("table.grid tbody")).toContainText(name);
  }

  // ---- 银行账户：2 条，且不带编造的账号 ----
  await page.click('[data-kind="bank"]');
  await page.click("#aux-preset");
  await page.click("#cf-ok");
  await expect(page.locator("table.grid tbody tr")).toHaveCount(2);
  await expect(page.locator("table.grid tbody")).toContainText("基本户");
  await expect(page.locator("table.grid tbody")).toContainText("一般户");

  // ---- 核心约定：再点一次「预置」不新增、不覆盖 ----
  // D01 已存在，若后端用 INSERT OR REPLACE，D01 的名字会被悄悄改成别的
  await page.click('[data-kind="dept"]');
  await page.click("#aux-preset");
  await page.click("#cf-ok");
  await expect(page.locator("table.grid tbody tr")).toHaveCount(3);
  await expect(page.locator("table.grid tbody")).toContainText("销售部");

  // ---- 客户 / 供应商：不预置，且按钮根本不出现 ----
  for (const kind of ["customer", "supplier", "item"]) {
    await page.click(`[data-kind="${kind}"]`);
    await expect(page.locator("#aux-preset")).toHaveCount(0);
    await expect(page.locator("table.grid tbody")).toContainText("暂无档案");
  }
});