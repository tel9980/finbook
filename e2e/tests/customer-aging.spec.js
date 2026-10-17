const { test, expect } = require("@playwright/test");
const { newBook, loginAs } = require("../helpers");

// 客户详情的「账龄」子视图 E2E。
//
// 守四件事：
//   ① 账龄子视图能打开，且与**账龄接口**逐档一致（桶 + 金额 + 合计）
//   ② 两笔落在**不同区间** —— 否则「逐档一致」是退化的（全在一档也能对上）
//   ③ 只有期初挂账的客户：期初要进账龄（与余额不同，余额不含期初）
//   ④ 出纳也能看，且无余额时显示「无未核销余额」而不是一张全 0 的表
//
// 为什么必须跨两个期间：账龄区间最窄的一档是 0-30 天，而**一个期间内**
// 日期最多只跨 30 天 —— 两笔必然落在同一档，「区间对不对」根本验不出来。
// 账套起始期是 2026-01（helpers.newBook 写死 #cb-start），所以用 202601 + 202602，
// 基准日取 202602 期末 2026-02-28：01-08 → 51 天（31-60），02-20 → 8 天（0-30）。

async function mkCustomer(page, code, name) {
  await page.click('.nav-item[data-view="aux"]');
  await expect(page.locator('[data-kind="customer"]')).toBeVisible({ timeout: 15_000 });
  await page.click('[data-kind="customer"]');
  await page.click("#aux-new");
  await page.fill("#au-code", code);
  await page.fill("#au-name", name);
  await page.click("#au-save");
  await expect(page.locator("#main")).toContainText(name, { timeout: 15_000 });
}

async function postReceivable(page, date, code, amount) {
  await page.click('.nav-item[data-view="vouchers"]');
  await page.click("#new-v");
  await page.fill("#v-date", date);
  const rows = page.locator("#v-entries tbody tr:has(select.acct-sel)");
  await rows.nth(0).locator("select.acct-sel").selectOption("112201");
  await rows.nth(0).locator(".e-sum").fill("应收 " + code);
  await rows.nth(0).locator(".e-d").fill(amount);
  await rows.nth(0).locator(".e-aux").click();
  await page.fill('.aux-in[data-k="customer"]', code);
  await rows.nth(1).locator("select.acct-sel").selectOption("1001");
  await rows.nth(1).locator(".e-sum").fill("收款");
  await rows.nth(1).locator(".e-c").fill(amount);
  await page.click("#v-save");
  await expect(page.locator(".modal-mask")).toHaveCount(0, { timeout: 10_000 });
}

/// 把某个期间里**全部**草稿凭证推到已记账。
///
/// 必须按期间分别调用：`/api/vouchers?period=` 只返回该期间的，
/// 而两个期间的凭证都要记账，漏一个就有一笔是草稿（H-3 会把它排除）。
async function postAllInPeriod(page, period) {
  const list = await (await page.request.get(`/api/vouchers?period=${period}`)).json();
  expect((list || []).length, period + " 应有草稿凭证可记账").toBeGreaterThan(0);
  for (const v of list) {
    for (const step of ["audit", "sign", "post"]) {
      const r = await page.request.post(`/api/vouchers/${v.id}/${step}`, { data: {} });
      expect(r.ok(), step + " 应成功：" + (await r.text())).toBe(true);
    }
  }
  return list.length;
}

/// 打开客户详情并切到账龄子视图，返回账龄面板。
///
/// ⚠️ 不要用 `.catch(() => {})` 之类的兜底 selector：selector 写错会表现为
/// 「测试通过」，那是最坏的一种失败。所以每一步都断言，且失败信息指得到人。
async function openAging(page, period) {
  await page.click('.nav-item[data-view="customers"]');
  await expect(page.locator("#cu-list")).not.toContainText("加载中", { timeout: 15_000 });
  // 「只看有欠款」默认勾着，而**期初挂账不进余额** —— 于是只有期初的客户
  // 会被这个过滤器筛掉，测试点不出详情，然后报「[data-cu-open] 数量不是 1」。
  // 报错完全指不到真原因（漏了一步取消勾选）。所以这里无条件取消。
  await page.uncheck("#cu-only-open");
  await page.fill("#cu-period", period);
  await page.click("#cu-load");
  await expect(page.locator("[data-cu-open]")).toHaveCount(1, { timeout: 10_000 });
  await page.locator("[data-cu-open]").first().click();
  await expect(page.locator("#cd-body")).not.toContainText("加载中", { timeout: 15_000 });
  const tab = page.locator('#cd-subtabs .subtab[data-cd="aging"]');
  await expect(tab, "详情弹窗里应有「账龄」子视图页签").toHaveCount(1, { timeout: 5_000 });
  await tab.click();
  const panel = page.locator("#cd-aging");
  await expect(panel).toBeVisible({ timeout: 10_000 });
  await expect(panel).not.toContainText("账龄加载中", { timeout: 15_000 });
  return panel;
}

/// 把接口给的金额串转成界面上该显示的样子（补千分位），与 util.js 的 fmt 一致。
function fmtLike(s) {
  const neg = s.startsWith("-");
  const body = neg ? s.slice(1) : s;
  const parts = body.split(".");
  const int = parts[0].replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  return (neg ? "-" : "") + int + (parts[1] != null ? "." + parts[1] : "");
}

test("客户账龄子视图：桶与金额都与账龄接口一致（跨接口口径）", async ({ page }) => {
  await newBook(page, `E2E账龄${Date.now()}`);
  await mkCustomer(page, "KA01", "客户甲");

  await postReceivable(page, "2026-01-08", "KA01", "3000");
  await postReceivable(page, "2026-02-20", "KA01", "500");
  await postAllInPeriod(page, "202601");
  await postAllInPeriod(page, "202602");

  // 基准：账龄接口在 as_of=2026-02-28（202602 期末）时给出什么
  const aging = await (
    await page.request.get("/api/settle/aging?account=1122&upto=202602&as_of=2026-02-28")
  ).json();
  const row = (aging.rows || []).find((r) => r.key === "KA01");
  expect(row, "账龄接口应有 KA01 一行：" + JSON.stringify(aging)).toBeTruthy();

  // 前置断言：两笔真的落在两个不同区间（否则后面的比对是退化的）
  const nz = row.amounts.filter((a) => Number(String(a).replace(/,/g, "")) !== 0);
  expect(nz.length, "两笔应落在两个不同区间，实际 amounts=" + JSON.stringify(row.amounts)).toBe(2);

  const panel = await openAging(page, "202602");
  const text = await panel.innerText();

  // 桶标签逐个对上（前端不许自己分档）
  for (const b of aging.buckets) {
    expect(text, "账龄面板应显示区间「" + b + "」").toContain(b);
  }
  // 非零的两档金额要出现，且带千分位（与账龄页同格式）
  for (const amt of nz) {
    const shown = fmtLike(String(amt));
    expect(text, "账龄面板应显示金额 " + shown).toContain(shown);
  }
  expect(text, "应有合计行").toContain("合计");
  expect(text, "合计应等于账龄接口那一行").toContain(row.total);

  // 跨接口：客户页那个接口必须与账龄接口同一行完全一致
  const mine = await (await page.request.get("/api/customers/KA01/aging?period=202602")).json();
  expect(mine.as_of, "as_of 缺省应是**期间期末**，不是今天").toBe("2026-02-28");
  expect(mine.aging.total, "客户页账龄合计应等于账龄页那一行").toBe(row.total);
  expect(mine.aging.amounts, "各档金额应逐档相等").toEqual(row.amounts);
  expect(mine.aging.buckets, "桶标签应完全一致").toEqual(aging.buckets);

  await page.click("#cd-close");
  await expect(page.locator(".modal-mask")).toHaveCount(0, { timeout: 5_000 });
});

test("客户账龄子视图：只有期初挂账的客户，期初要进账龄", async ({ page }) => {
  await newBook(page, `E2E账龄期初${Date.now()}`);
  await mkCustomer(page, "KP01", "客户丙");

  const r = await page.request.post("/api/import/run", {
    data: {
      kind: "arap_opening",
      template: "generic",
      text:
        "类型,客商编码,单据号,单据日期,金额,客商名称,备注\n" +
        "应收,KP01,XS-9001,2025-12-01,3000,客户丙,上年末欠款\n",
    },
  });
  expect(r.ok(), "期初挂账导入应成功：" + (await r.text())).toBe(true);

  const panel = await openAging(page, "202601");
  const text = await panel.innerText();
  // 余额不含期初，但账龄**含**期初 —— 这两件事不一样，别混
  expect(text, "期初挂账应出现在账龄里（账龄含期初，余额不含）").toContain("3,000");
  expect(text, "不该显示「无未核销余额」—— 期初也是钱").not.toContain("无未核销余额");

  await page.click("#cd-close");
});

test("客户账龄子视图：出纳也能看；无余额时显示「无未核销余额」", async ({ page }) => {
  const book = await newBook(page, `E2E账龄权限${Date.now()}`);

  // 先建一个客户：出纳进来是空列表的话，点不出详情，测的就不是「出纳能不能看」
  await mkCustomer(page, "KC01", "客户甲");

  await page.click('.nav-item[data-view="platform-users"]');
  await page.click("#pu-add");
  await page.fill("#nc-u", "e2eag");
  await page.fill("#nc-p", "Csh@2026z");
  await page.click("#nc-save");
  await expect(page.locator("#pu-list")).toContainText("e2eag", { timeout: 15_000 });
  const r = await page.request.post("/api/users", {
    data: {
      username: "e2eag", display_name: "出纳",
      password: "", role: "cashier", must_change_pwd: false,
    },
  });
  expect(r.ok() || /已存在/.test(await r.text()), "把出纳拉进账套").toBeTruthy();

  await loginAs(page, "e2eag", "Csh@2026z", book);
  await expect(page.locator('.nav-item[data-view="customers"]')).toHaveCount(1, { timeout: 15_000 });

  const panel = await openAging(page, "202601");
  // 无余额：必须显示「无未核销余额」，而不是一张全 0 的表
  //（全 0 的表看起来像「算过了，确实是 0」，与「没有数据」是两件事）
  await expect(panel).toContainText("无未核销余额", { timeout: 5_000 });
  await page.click("#cd-close");
});

module.exports = {};