const { test, expect } = require("@playwright/test");
const { newBook } = require("../helpers");

// 税务申报：开票 → 认证 → 出表 → 导出 CSV
//
// 核心断言是**口径**而不是界面好不好看：
//  · 销项含「待认证」的发票（开票了销项义务就发生）
//  · 进项只认「已认证」
//  · 附加税费按法定三档税率算，且是 2 位小数
// 这三条错一条，申报表上的数就是错的，而错的方向是多缴税。
test("税务申报：进项只认已认证，应纳税额与附加税费算对", async ({ page }) => {
  await newBook(page, `E2E税务${Date.now()}`);

  const addInv = async (kind, number, net, tax) => {
    await page.click('.nav-item[data-view="invoices"]');
    await expect(page.locator("#inv-new")).toBeVisible({ timeout: 15_000 });
    await page.click("#inv-new");
    // 编辑器是弹窗：等控件出现（不是「视图已渲染」——见 app.js 里的竞态守卫）
    await expect(page.locator("#inv-number")).toBeVisible({ timeout: 10_000 });
    await page.selectOption("#inv-kind2", kind);
    await page.fill("#inv-number", number);
    await page.fill("#inv-date", "2026-01-15");
    await page.fill("#inv-buyer", kind === "out" ? "客户甲" : "本企业");
    await page.fill("#inv-seller", kind === "out" ? "本企业" : "供应商甲");
    await page.fill("#inv-amount", net);
    await page.fill("#inv-tax", tax);
    await page.fill("#inv-amt", String(Number(net) + Number(tax)));
    await page.fill("#inv-rate", "0.13");
    await page.click("#inv-save");
    await expect(page.locator("#inv-list")).toContainText(number, { timeout: 10_000 });
  };

  await addInv("out", "FP000001", "1000", "130");
  await addInv("in", "FP000002", "500", "65");

  // ---- 认证之前：验证两个方向的法定口径不同 ----
  //   销项含「待认证」（开票了销项义务就发生）
  //   进项**排除**「待认证」（未认证不得抵扣）
  await page.click('.nav-item[data-view="tax-decl"]');
  // 同样等「数据到了」的信号（#tx-export），不等 h2 外壳
  await expect(page.locator("#tx-export")).toBeVisible({ timeout: 15_000 });
  // 用 #tx-warn 而不是 .banner.unset —— 页面上还有别的 .banner.unset
  // （期间未初始化/首登改密），类选择器会命中它们，断言就成了「某个横幅非空」。
  await expect(page.locator("#tx-warn")).toContainText("尚未认证", { timeout: 15_000 });

  let text = await page.locator("#main").textContent();
  expect(text).toContain("130.00"); // 销项照计（pending 也算）
  expect(text).not.toContain("65.00"); // 进项未认证，一个都不能抵扣

  // ---- 认证之后：进项才进得来 ----
  await page.click('.nav-item[data-view="invoices"]');
  await expect(page.locator("#inv-list")).toContainText("FP000002", { timeout: 15_000 });
  await page
    .locator("#inv-list tr", { hasText: "FP000002" })
    .first()
    .locator('button[data-act="verify"]')
    .click();
  await expect(page.locator("#inv-list")).toContainText("已认证", { timeout: 10_000 });

  await page.click('.nav-item[data-view="tax-decl"]');
  // 等 `#tx-export`（导出按钮）而不是 h2：viewTaxDecl 会**先**渲染一个
  // 「<h2>税务申报…加载中…</h2>」的外壳，取到数之后再整体替换。
  // 所以 h2 含「税务申报」只证明视图画出来了，不证明数据到了——紧接着读
  // textContent 会读到加载态，于是 `toContain("130.00")` 偶发失败。
  // 这个 flake 之前被 retry 盖住了，看起来像偶发，实际是断言选错了信号。
  await expect(page.locator("#tx-export")).toBeVisible({ timeout: 15_000 });
  text = await page.locator("#main").textContent();
  expect(text).toContain("130.00"); // 销项 130
  expect(text).toContain("65.00"); // 进项 65（已认证）
  expect(text).toContain("7.80"); // 附加税费 65 × (7%+3%+2%)
  expect(text).not.toContain("尚未认证"); // 认证后不该再提示
  // 界面必须明说边界，否则用户不知道哪些数能信
  expect(text).toContain("不连网");
  expect(text).toContain("人工填报");

  // 导出 CSV：BOM + 口径说明随表走
  const [download] = await Promise.all([
    page.waitForEvent("download"),
    page.click("#tx-export"),
  ]);
  expect(download.suggestedFilename()).toContain(".csv");
  const fs = require("fs");
  const buf = fs.readFileSync(await download.path());
  expect([buf[0], buf[1], buf[2]]).toEqual([0xef, 0xbb, 0xbf]);
  const csv = buf.toString("utf8");
  expect(csv).toContain("销项税额");
  expect(csv).toContain("城市维护建设税");
  // 拿到 CSV 的人不一定是出表的会计，口径说明必须跟着走
  expect(csv).toContain("口径说明");
});
