const { test, expect } = require("@playwright/test");
const { newBook } = require("../helpers");

// 到岸成本：把运费/关税摊进存货成本
//
// 断言的是**口径**而不是界面好不好看：
//  · 未收货的货不能摊（运费到了货还没到是常事）
//  · 「入存货 / 转费用」要到过账时才算 —— 草稿阶段算不出来
//  · 过账后存货金额真的被抬高
//
// 后端口径由集成测试（landed_cost_end_to_end / landed_cost_on_sold_goods_goes_to_expense /
// landed_cost_shows_up_in_purchase_variance）覆盖；本用例守 UI 接线与文案。
test("到岸成本：按采购单取已收货行→录运费→过账", async ({ page }) => {
  await newBook(page, `E2E到岸${Date.now()}`);

  // 建采购订单并入库（用 API 建单：这条路径由集成测试覆盖，
  // 本用例要守的是「到岸成本」这一段的 UI 接线与文案）
  const po = await page.request.post("/api/procure/po", {
    data: {
      period: 202601,
      date: "2026-01-03",
      status: "Confirmed",
      supplier_code: "S001",
      supplier_name: "供应商甲",
      lines: [
        { item_code: "140301", item_name: "材料甲", qty_ordered: "100", unit_price: "8", tax_rate: "0.13" },
      ],
    },
  });
  expect(po.ok(), "建采购订单应成功").toBeTruthy();
  const po_id = (await po.json()).id;
  const rc = await page.request.post("/api/procure/receipt", {
    data: { po_id, period: 202601, date: "2026-01-05", qty: "100" },
  });
  expect(rc.ok(), "采购入库应成功").toBeTruthy();

  // 空态必须说清这个模块是干什么的
  await page.click('.nav-item[data-view="landed-cost"]');
  await expect(page.locator("#main .cards")).toBeVisible({ timeout: 15_000 });
  let text = await page.locator("#main").textContent();
  expect(text).toContain("不改原采购入库单");
  expect(text).toContain("转当期费用");
  expect(text).toContain("幽灵资产");

  // 打开编辑器，按采购单取行
  await page.click("#lc-new");
  await expect(page.locator("#le-save")).toBeVisible({ timeout: 10_000 });
  await page.fill("#le-po", String(po_id));
  await page.click("#le-load");
  await expect(page.locator("#le-lines")).toContainText("140301", { timeout: 10_000 });
  // 取到的是「已收货」量
  await expect(page.locator("#le-lines")).toContainText("100");

  // 录运费并保存草稿
  await page.fill("#le-freight", "100");
  // 贷方默认是银行存款，而银行存款核算银行账户 —— 付款账户必填
  await page.fill("#le-bank", "100201");
  await page.click("#le-save");
  // 单号前缀是账套配置的 doc_prefix（默认 LC）
  await expect(page.locator("#main")).toContainText("LC", { timeout: 10_000 });

  // 草稿行显示「摊入存货 0.00」—— 那要过账才算，界面必须如实显示 0 而不是估一个数
  await expect(page.locator("#main")).toContainText("草稿", { timeout: 10_000 });
  const draftText = await page.locator("#main").textContent();
  expect(draftText).toContain("摊入存货0.00");

  // 过账
  await page.click("[data-lc-post]");
  // 捕获响应体：只断言 UI 上的文字，过账失败时 toast 会自动消失，
  // 报错就变成「等不到已过账」而看不出原因。
  const [resp] = await Promise.all([
    page.waitForResponse((r) => /\/landed-cost\/\d+\/post/.test(r.url())),
    page.click("#cf-ok"),
  ]);
  expect(resp.ok(), `过账请求应成功：${await resp.text()}`).toBeTruthy();
  await expect(page.locator("#main")).toContainText("已过账", { timeout: 15_000 });
  text = await page.locator("#main").textContent();
  expect(text).toContain("100.00");

  // 明细里能看到「入存货 / 转费用」两列
  await page.click("[data-lc-view]");
  await expect(page.locator("#lc-detail")).toContainText("入存货", { timeout: 10_000 });
  await expect(page.locator("#lc-detail")).toContainText("转费用", { timeout: 10_000 });
});

test("存货成本差异：实际单价已含到岸成本（不显示就看不出差异被运费吃掉）", async ({ page }) => {
  await newBook(page, `E2E差异口径${Date.now()}`);
  await page.click('.nav-item[data-view="cost-variance"]');
  // 等数据到达的信号（.cards 只存在于加载完成后的 HTML 里）
  await expect(page.locator("#main .cards")).toBeVisible({ timeout: 15_000 });
  const text = await page.locator("#main").textContent();
  expect(text).toContain("已含");
  expect(text).toContain("到岸成本");
});
