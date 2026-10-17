const { test, expect } = require("@playwright/test");
const { newBook, postVoucher, auditSignPost } = require("../helpers");

test("工资：职员档案→工资行个税→计提凭证", async ({ page }) => {
  await newBook(page, `E2E工资${Date.now()}`);
  await page.click('.nav-item[data-view="aux"]');
  await page.click('[data-kind="employee"]');
  await page.click("#aux-new");
  await page.fill("#au-code", "E001");
  await page.fill("#au-name", "张三");
  await page.click("#au-save");
  await expect(page.locator("table.grid tbody")).toContainText("张三", { timeout: 10_000 });

  await page.click('.nav-item[data-view="payroll"]');
  await page.click("#py-new");
  await page.selectOption("#pw-emp", "E001");
  await page.fill("#pw-gross", "10000");
  await page.fill("#pw-social", "1000");
  await page.fill("#pw-housing", "500");
  await page.click("#pw-save");
  // 10000 − 5000 − 1500 = 3500 × 3% = 105；实发 10000 − 1000 − 500 − 105 = 8395
  await expect(page.locator("#py-body")).toContainText("8,395.00", { timeout: 10_000 });
  await expect(page.locator("#py-body")).toContainText("105.00");

  await page.click("#py-tab-voucher");
  await page.click("#pv-accrue");
  await page.click("#cf-ok");
  await expect(page.locator("#pv-result")).toContainText("已生成凭证", { timeout: 10_000 });
  await page.click('.nav-item[data-view="vouchers"]');
  await expect(page.locator("#v-table tbody")).toContainText("计提");
});

/// 社保缴纳按钮必须真的能点出凭证
///
/// 回归背景：#pv-social 这个 id **同时**用在「应付社保科目输入框」和
/// 「生成社保缴纳凭证按钮」上，两个都渲染在同一个 DOM 里。`$("#pv-social")`
/// 只返回第一个（输入框），于是按钮的 onclick 从未绑上——**点了没反应、不报错、
/// 没有任何控制台输出**。而 /api/payroll/social-pay 的集成测试一直是绿的
/// （端子没问题），所以这类错只有 UI 层的 E2E 才拦得住。
test("工资：社保缴纳按钮能点出凭证（id 不与科目输入框撞车）", async ({ page }) => {
  await newBook(page, `E2E社保${Date.now()}`);
  await page.click('.nav-item[data-view="aux"]');
  await page.click('[data-kind="employee"]');
  await page.click("#aux-new");
  await page.fill("#au-code", "E002");
  await page.fill("#au-name", "李四");
  await page.click("#au-save");
  await expect(page.locator("table.grid tbody")).toContainText("李四", { timeout: 10_000 });

  await page.click('.nav-item[data-view="payroll"]');
  await page.click("#py-new");
  await page.selectOption("#pw-emp", "E002");
  await page.fill("#pw-gross", "8000");
  await page.fill("#pw-social", "800");
  await page.click("#pw-save");
  await expect(page.locator("#py-body")).toContainText("李四", { timeout: 10_000 });

  await page.click("#py-tab-voucher");
  await page.click("#pv-accrue");
  await page.click("#cf-ok");
  await expect(page.locator("#pv-result")).toContainText("已生成凭证", { timeout: 10_000 });

  // 这一步以前是死的：按钮没绑事件，点下去什么都不发生
  await page.click("#pv-social-btn");
  await page.click("#cf-ok");
  await expect(page.locator("#pv-result")).toContainText("已生成凭证", { timeout: 10_000 });
  await page.click('.nav-item[data-view="vouchers"]');
  await expect(page.locator("#v-table tbody")).toContainText("社保");
});

test("发票：新增→认证→作废", async ({ page }) => {
  await newBook(page, `E2E发票${Date.now()}`);
  await page.click('.nav-item[data-view="invoices"]');
  await page.click("#inv-new");
  await page.fill("#inv-number", "INV001");
  await page.fill("#inv-buyer", "供应商A");
  await page.fill("#inv-seller", "本公司");
  await page.fill("#inv-amt", "1130");
  await page.fill("#inv-amount", "1000");
  await page.fill("#inv-tax", "130");
  await page.fill("#inv-rate", "0.13");
  await page.click("#inv-save");

  const row = page.locator("table.grid tbody tr").first();
  await expect(row).toContainText("INV001", { timeout: 10_000 });
  await expect(row).toContainText("待认证");
  await row.locator('[data-act="verify"]').click();
  await expect(page.locator("table.grid tbody tr").first()).toContainText("已认证", { timeout: 10_000 });
  await page.locator("table.grid tbody tr").first().locator('[data-act="reject"]').click();
  await page.click("#cf-ok");
  await expect(page.locator("table.grid tbody tr").first()).toContainText("已作废", { timeout: 10_000 });
});

test("自定义报表：建表保存→按期间生成取数", async ({ page }) => {
  await newBook(page, `E2E报表${Date.now()}`);
  await postVoucher(page, {
    date: "2026-01-15",
    rows: [
      { code: "1001", summary: "报表取数", debit: "100" },
      { code: "2001", summary: "报表取数", credit: "100" },
    ],
  });
  // H-3：自定义报表 QM/LFS 按已记账取数，先记账
  const list = await (await page.request.get("/api/vouchers?period=202601")).json();
  // 默认账套开着审核环节：先审核再记账
  await auditSignPost(page, list[0].id);

  await page.click('.nav-item[data-view="custom-reports"]');
  await page.click("#cr-new");
  await page.fill("#cr-name", "费用表");
  await page.fill(".cr-lname", "现金");
  await page.fill('.cr-f[data-ci="0"]', 'QM("1001")');
  await page.fill('.cr-f[data-ci="1"]', 'LFS("1001")');
  await page.click("#cr-save");
  await expect(page.locator("#cr-err")).toHaveText("", { timeout: 10_000 });
  await page.click("#cr-preview");
  await expect(page.locator("#cr-preview-box")).toContainText("现金", { timeout: 10_000 });
  await expect(page.locator("#cr-preview-box")).toContainText("100.00");
});

test("期末对账：试算平衡通过", async ({ page }) => {
  await newBook(page, `E2E对账${Date.now()}`);
  await postVoucher(page, {
    date: "2026-01-15",
    rows: [
      { code: "1001", summary: "对账", debit: "100" },
      { code: "2001", summary: "对账", credit: "100" },
    ],
  });
  // 试算平衡只看已记账，凭证要走审核 → 记账（新建账套的审核环节默认开）
  await page.locator("#v-table tbody [data-edit]").first().click();
  await expect(page.locator("#v-audit")).toBeVisible({ timeout: 10_000 });
  await page.click("#v-audit");
  await expect(page.locator("#v-table tbody")).toContainText("已审核", { timeout: 10_000 });
  await page.locator("#v-table tbody [data-edit]").first().click();
  // 出纳签字（默认开）：记账的前置闸门之一。
  await expect(page.locator("#v-sign")).toBeVisible({ timeout: 10_000 });
  await page.click("#v-sign");
  // 签字也会关闭弹窗（closeModal），所以要再打开一次才点得到「记账」。
  await expect(page.locator("#v-table tbody")).toContainText("已记账", { timeout: 5_000 }).catch(() => {});
  await page.locator("#v-table tbody [data-edit]").first().click();

  await expect(page.locator("#v-post")).toBeVisible({ timeout: 10_000 });
  await page.click("#v-post");
  await expect(page.locator("#v-table tbody")).toContainText("已记账", { timeout: 10_000 });

  await page.click('.nav-item[data-view="reconcile"]');
  await page.click("#rc-run");
  await expect(page.locator("#rc-result")).toContainText("通过", { timeout: 10_000 });
});
