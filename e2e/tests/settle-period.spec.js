const { test, expect } = require("@playwright/test");
const { newBook, postVoucher, auditAllDrafts } = require("../helpers");

test("往来核销：两笔应收/收款自动核销→账龄→核销记录", async ({ page }) => {
  await newBook(page, `E2E核销${Date.now()}`);
  // 往来核销只认已记账分录（H-3 口径），所以两张凭证录完必须记账
  await postVoucher(page, {
    date: "2026-01-10",
    post: true,
    rows: [
      { code: "112201", summary: "销售应收", debit: "100", aux: { customer: "C01" } },
      { code: "1001", summary: "销售应收", credit: "100" },
    ],
  });
  await postVoucher(page, {
    date: "2026-01-20",
    post: true,
    rows: [
      { code: "1001", summary: "收回货款", debit: "100" },
      { code: "112201", summary: "收回货款", credit: "100", aux: { customer: "C01" } },
    ],
  });

  await page.click('.nav-item[data-view="settle"]');
  await page.click("#st-load");
  await expect(page.locator("#st-sum")).toContainText("未核销 2 笔", { timeout: 10_000 });
  await expect(page.locator("#st-table")).toContainText("C01");

  await page.click("#st-auto");
  await page.click("#cf-ok");
  await expect(page.locator("#st-sum")).toContainText("未核销 0 笔", { timeout: 10_000 });

  await page.click("#st-aging");
  await expect(page.locator("#st-extra")).toContainText("账龄分析", { timeout: 10_000 });

  await page.click("#st-records");
  await expect(page.locator("#st-extra")).toContainText("核销记录", { timeout: 10_000 });
  await expect(page.locator("#st-extra")).toContainText("C01");
});

test("期末处理：结转损益→记账→结账，期间状态推进", async ({ page }) => {
  await newBook(page, `E2E期末${Date.now()}`);
  await postVoucher(page, {
    date: "2026-01-15",
    rows: [
      { code: "660201", summary: "办公费", debit: "100" },
      { code: "1001", summary: "办公费", credit: "100" },
    ],
  });
  // 结转损益生成的凭证同样要走审核 → 记账（新建账套的审核环节默认开）
  await page.locator("#v-table tbody [data-edit]").first().click();
  await expect(page.locator("#v-audit")).toBeVisible({ timeout: 10_000 });
  await page.click("#v-audit");
  await expect(page.locator("#v-table tbody")).toContainText("已审核", { timeout: 10_000 });
  await page.locator("#v-table tbody [data-edit]").first().click();
  // 出纳签字（默认开）：这一步也是记账的前置。
  await expect(page.locator("#v-sign")).toBeVisible({ timeout: 10_000 });
  await page.click("#v-sign");
  // 签字也会关闭弹窗（closeModal），所以要再打开一次才点得到「记账」。
  await expect(page.locator("#v-table tbody")).toContainText("已记账", { timeout: 5_000 }).catch(() => {});
  await page.locator("#v-table tbody [data-edit]").first().click();

  await expect(page.locator("#v-post")).toBeVisible({ timeout: 10_000 });
  await page.click("#v-post");
  await expect(page.locator("#v-table tbody")).toContainText("已记账", { timeout: 10_000 });

  await page.click('.nav-item[data-view="period-end"]');
  await expect(page.locator("#pe-status")).toContainText("损益科目", { timeout: 10_000 });
  await page.click("#pe-carry");
  await page.click("#cf-ok");
  // 结转凭证已生成但未记账 → 预检查应提示存在未记账凭证
  await expect(page.locator("#pe-issues")).toContainText("未记账", { timeout: 10_000 });

  // 结转生成的凭证未记账：审核 + 批量记账后才能结账。
  //
  // 批量记账（/vouchers/batch-post）与单张 post 共用同一道审核闸门，所以必须先把
  // 结转凭证审核掉。之前这里直接点「批量记账」，结果第一张（已审核过）记上了、
  // 结转那张（未审核）被拒，页面上仍留一条「未记账」——看起来像结账流程坏了。
  await auditAllDrafts(page, "202601");
  await page.click('.nav-item[data-view="vouchers"]');
  await page.check("#v-all");
  await page.click("#v-batch");
  await expect(page.locator("#v-table tbody")).not.toContainText("未记账", { timeout: 10_000 });

  await page.click('.nav-item[data-view="period-end"]');
  await page.click("#pe-check");
  await page.click("#pe-close");
  await page.click("#cf-ok");
  await expect(page.locator("#pe-status")).toContainText("已结账至：2026-01", { timeout: 10_000 });
});

test("往来核销·差异核销：应收100、实收98，抹零2记入损益，被核销方一次结清", async ({ page }) => {
  await newBook(page, `E2E抹零${Date.now()}`);
  await postVoucher(page, {
    date: "2026-01-10",
    post: true,
    rows: [
      { code: "112201", summary: "销售应收", debit: "100", aux: { customer: "C01" } },
      { code: "1001", summary: "销售应收", credit: "100" },
    ],
  });
  await postVoucher(page, {
    date: "2026-01-20",
    post: true,
    rows: [
      { code: "1001", summary: "收回货款", debit: "98" },
      { code: "112201", summary: "收回货款", credit: "98", aux: { customer: "C01" } },
    ],
  });

  await page.click('.nav-item[data-view="settle"]');
  await page.click("#st-load");
  await expect(page.locator("#st-sum")).toContainText("未核销 2 笔", { timeout: 10_000 });

  // 借方（应收）选「原单」，贷方（收款）选「收/付」
  await page.locator("#st-table tbody tr").filter({ hasText: "销售应收" }).first()
    .locator("[data-from]").click();
  await page.locator("#st-table tbody tr").filter({ hasText: "收回货款" }).first()
    .locator("[data-to]").click();

  // 差异核销走原生 prompt：按顺序回填 核销额98 / 差异2 / 差异科目660305
  const answers = ["98", "2", "660305"];
  page.on("dialog", async (d) => { await d.accept(answers.length ? answers.shift() : ""); });
  await page.click("#st-manual-diff");

  // 核销 98 + 差异 2 = 100 → 应收全额结清
  await expect(page.locator("#st-sum")).toContainText("未核销 0 笔", { timeout: 10_000 });
  await expect(page.locator("#st-sum")).toContainText("合计 0.00", { timeout: 10_000 });
});

test("往来对冲：同一公司应收/应付轧差，出凭证并两端核销", async ({ page }) => {
  await newBook(page, `E2E对冲${Date.now()}`);
  // 应收 1000（客户 C01）
  await postVoucher(page, {
    date: "2026-01-05",
    post: true,
    rows: [
      { code: "112201", summary: "销售应收", debit: "1000", aux: { customer: "C01" } },
      { code: "600101", summary: "销售应收", credit: "1000" },
    ],
  });
  // 应付 600（供应商 S01）
  await postVoucher(page, {
    date: "2026-01-06",
    post: true,
    rows: [
      { code: "1001", summary: "采购付款", debit: "600" },
      { code: "220201", summary: "采购应付", credit: "600", aux: { supplier: "S01" } },
    ],
  });

  await page.click('.nav-item[data-view="settle"]');
  await expect(page.locator("#off-date")).toBeVisible({ timeout: 15_000 });
  await page.fill("#off-cust", "C01");
  await page.fill("#off-sup", "S01");
  await page.fill("#off-amt", "600");
  await page.click("#off-new");
  await expect(page.locator("#off-list")).toContainText("待审核", { timeout: 10_000 });
  await expect(page.locator("#off-list")).toContainText("600");

  // 审核 → 出凭证 + 两端核销
  await page.locator('#off-list [data-off-act="audit"]').first().click();
  await page.click("#cf-ok");
  await expect(page.locator("#off-list")).toContainText("已审核", { timeout: 15_000 });

  // 应收对冲 600 后剩 400、应付全局清
  const ar = await (await page.request.get("/api/settle/open?account=112201&upto=202601")).json();
  const arOpen = (ar.rows || []).reduce((a, r) => a + (parseFloat(String(r.open).replace(/,/g, "")) || 0), 0);
  expect(arOpen, `应收应剩 400：${JSON.stringify(ar)}`).toBeCloseTo(400, 2);
  const ap = await (await page.request.get("/api/settle/open?account=220201&upto=202601")).json();
  expect((ap.rows || []).length, `应付应被全额冲销：${JSON.stringify(ap)}`).toBe(0);
});
