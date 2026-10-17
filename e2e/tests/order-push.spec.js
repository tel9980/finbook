const { test, expect } = require("@playwright/test");
const { newBook } = require("../helpers");

// 销售订单下推生产订单（产销链路上新接的一段）
//
// 这里的断言全是**口径**，不是「界面好不好看」：
//  · 草稿订单不能下推（草稿还不是承诺，所以没有「下推生产」按钮）
//  · 确认后能下推，弹窗里先摆出 ATP 的三个分量（现货/在途/已占用）
//  · 下推之后产单列表能看到来源销售订单
//
// 后端逻辑与数字口径由集成测试
// （sales_order_pushes_to_production_order / draft_sales_order_cannot_push_production
//  / atp_endpoint_excludes_qc_pending_and_draft）覆盖。本用例只守**UI 接线**——
// 端点全绿但按钮没绑事件那类错只有 UI 层拦得住（#pv-social 就是这么坏的）。
test("销售订单下推生产订单：草稿不可推、确认后可推、ATP 三分量可见", async ({ page }) => {
  await newBook(page, `E2E下推${Date.now()}`);

  // 建单走 API：本用例要守的是**UI 接线**（按钮有没有绑事件、弹窗有没有把数带出来），
  // 建单路径本身由集成测试覆盖。走 UI 建单会先撞上报价单/订单两个入口的差异，
  // 反而把「按钮死没死」这个主题淹掉。
  const mk = async (status) => {
    const resp = await page.request.post("/api/sales/so", {
      data: {
        period: 202601,
        date: "2026-01-05",
        status,
        customer_code: "C01",
        customer_name: "客户甲",
        lines: [
          { item_code: "140501", item_name: "成品甲", qty_ordered: "100", unit_price: "10", tax_rate: "0.13" },
        ],
      },
    });
    expect(resp.ok(), `建 ${status} 销售订单应成功`).toBeTruthy();
    return (await resp.json()).id;
  };

  // 草稿订单：不该出现「下推生产」
  const draftId = await mk("Draft");
  await page.click('.nav-item[data-view="so-doc"]');
  await expect(page.locator("#so-list")).toContainText("XS", { timeout: 15_000 });
  await expect(page.locator(`[data-so-prod="${draftId}"]`)).toHaveCount(0);

  // 已确认订单：按钮出现
  const confId = await mk("Confirmed");
  await page.click('.nav-item[data-view="so-doc"]');
  await expect(page.locator("#so-list")).toContainText("XS", { timeout: 15_000 });
  await expect(page.locator(`[data-so-prod="${confId}"]`)).toBeVisible({ timeout: 10_000 });

  // 点开弹窗：ATP 三个分量都要在（只给一个总数，被算错了也不知道错在哪）
  await page.locator(`[data-so-prod="${confId}"]`).click();
  await expect(page.locator("#pp-ok")).toBeVisible({ timeout: 10_000 });
  const dlg = page.locator(".modal-mask").last();
  await expect(dlg).toContainText("现有可用库存");
  await expect(dlg).toContainText("在途");
  await expect(dlg).toContainText("已占用");
  await expect(dlg).toContainText("可承诺量");
  // 无现货无在途、已占用 100 → ATP 是负数（承诺不了 100 件）
  await expect(dlg).toContainText("-100");

  // 下推成功 → 弹窗关闭
  await page.click("#pp-ok");
  await expect(page.locator("#pp-ok")).toHaveCount(0, { timeout: 10_000 });

  // 全部认领后再推必须被拒（否则产销对不上），且拒绝理由留在弹窗里
  // ——弹窗不关闭而 toast 会自动消失，错误只弹 toast 等于没告诉用户
  await page.locator(`[data-so-prod="${confId}"]`).click();
  await expect(page.locator("#pp-ok")).toBeVisible({ timeout: 10_000 });
  await page.click("#pp-ok");
  await expect(page.locator("#pp-err")).toContainText("没有可再下推", { timeout: 10_000 });

  // 下推过的那批产单**没排期** → 界面必须明说它给不出交期
  //
  // 仓里没有工作中心产能数据，系统编不出真实完工日；只有计划员排的 plan_end
  // 才是有依据的日期。所以「在途 100」不能直接拿去承诺客户 —— 界面上要把
  // 已排期/未排期分开，并说明未排期的那部分不能用来承诺交期。
  await page.click("#pp-cancel");
  await page.locator(`[data-so-prod="${confId}"]`).click();
  await expect(page.locator("#pp-ok")).toBeVisible({ timeout: 10_000 });
  const dlg2 = page.locator(".modal-mask").last();
  await expect(dlg2).toContainText("在途·已排期");
  await expect(dlg2).toContainText("在途·未排期");
  await expect(page.locator("#pp-warn")).toContainText("没有排期", { timeout: 10_000 });
  await expect(page.locator("#pp-warn")).toContainText("给不出交期");
  await expect(page.locator("#pp-warn")).toContainText("系统不会把它算进");
  // 要货日期输入：填了就现算「到该日为止可承诺量」
  await expect(page.locator("#pp-date")).toBeVisible();
  await page.fill("#pp-date", "2026-01-20");
  await page.locator("#pp-date").dispatchEvent("change");
  await expect(page.locator("#pp-until")).toContainText("可承诺", { timeout: 10_000 });
  await expect(page.locator("#pp-until")).toContainText("之前");
});

test("存货成本差异：点明不算哪几档、点明与订单级差异是两个口径", async ({ page }) => {
  await newBook(page, `E2E差异${Date.now()}`);

  await page.click('.nav-item[data-view="cost-variance"]');
  // 等 .cards（只存在于数据到达后的 HTML 里）——不等 h2 外壳：
  // 视图会先渲染「<h2>存货成本差异…加载中…</h2>」，h2 出现不证明取到数了。
  await expect(page.locator("#main .cards")).toBeVisible({ timeout: 15_000 });
  const text = await page.locator("#main").textContent();

  // 明说本表**不**包含哪几档差异（输入没有维护入口，缺输入时硬算等于编数）
  expect(text).toContain("用量差异");
  expect(text).toContain("效率差异");
  expect(text).toContain("固定制造费用差异");
  // 明说与订单级差异是两个口径，避免界面上出现两个「差异」数字对不上
  expect(text).toContain("两个不同口径");
  // 只统计已设标准成本单价的存货
  expect(text).toContain("已设标准成本单价");
});
