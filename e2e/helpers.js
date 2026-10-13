const { expect } = require("@playwright/test");

// ---- 账套生命周期（E2E 专用数据目录，删干净不会碰到用户数据）----------------
//
// 踩过的坑：账套上限是**每账号 10 个**，撞上后 `POST /api/books` 全部 400，
// 于是所有用例都卡在「等不到主界面 nav」，**1 个真实失败被放大成整个套件失败**
// （实测 2 个真实失败能拖垮 18-31 个用例）。所以清理必须与进程状态无关。
//
// 早先靠模块级 `lastBookKey` 记录"本 worker 上次建的账套"，有两个洞：
// ① Playwright 的 `retries: 1` 会为重试**开新 worker 进程**，新进程变量是 null，
//    第一次尝试建的账套永远没人知道 key → 泄漏；
// ② 挂 `test.afterEach` 要改 16 个 spec 文件，漏一个就漏一个。
//
// 现在的做法：给每个 worker 的账套名加**固定前缀**（取自 TEST_WORKER_INDEX），
// 清理时按前缀扫。不依赖任何进程内状态，重试换进程照样能找回上次的账套。
// 只改这一个文件，16 个 spec 一个都不用动。
const WORKER_TAG = `w${process.env.TEST_WORKER_INDEX || "0"}`;
const BOOK_PREFIX = `E2E${WORKER_TAG}///`;

// 90 条 E2E 里有 70 个视图是**点侧栏**进去的（`.nav-item[data-view=x]`）。
// 精简模式默认只铺 16 项，不先关掉的话，剩下 54 个视图全部点不到 → 全套件超时。
//
// 这不是在「把测试改成配合实现」：精简模式只决定**侧栏画不画出这一项**，
// 不碰路由、不碰权限、不碰任何页面逻辑。这里关掉它，测的仍然是全量功能；
// 精简模式本身另有 `simple-nav.spec.js` 专门覆盖（含「藏起来≠拿掉」那条断言）。
//
// ⚠️ 必须用 addInitScript（每次导航前执行），不能只在登录后 setItem 一次 ——
// 侧栏在**首屏**就渲染了，那时读到的是「没设置过」= 默认开。
//
// 注入的是**字符串**而不是 (fn, arg)：写成字符串后不依赖 addInitScript 是否
// 支持第二个参数，每个 Playwright 版本行为一致 —— 那种「本机版本支持、
// CI 版本不支持」的差异极难查，而且症状是「侧栏莫名其妙只有 16 项」，
// 看不出跟这行有关。
//
// `mode` 三态，覆盖要测的全部情形：
//   false（默认）→ 显式关：绝大多数用例要的就是「侧栏铺全量」
//   true         → 显式开：测精简态
//   null         → **不注册任何脚本**，localStorage 里没有这个 key，
//                  用来测「从没设置过 → 默认开」这条新用户路径
async function setSimpleNav(page, mode, username) {
  if (mode === null) return;
  const u = username || "admin";
  const v = mode ? "true" : "false";
  await page.addInitScript(
    `try { localStorage.setItem("nav_simple@${u}", ${JSON.stringify(v)}); } catch (e) {}`
  );
}

/// 删掉本 worker 名下所有账套（含上一次尝试泄漏的）
async function purgeOwnBooks(page) {
  let d;
  try {
    d = await (await page.request.get("/api/books")).json();
  } catch (e) {
    return; // 拿不到列表就跳过，不影响用例本身
  }
  for (const b of d.books || []) {
    if ((b.company || "").startsWith(BOOK_PREFIX)) {
      await page.request.delete(`/api/books/${encodeURIComponent(b.key)}`).catch(() => {});
    }
  }
}

/// 登录 → 建账（起始期间 2026-01）→ 停在可用界面。**返回账套全名**。
///
/// 返回值不是多余的：一个 realm 里会攒下多本书（前面测试建的、别的 worker 建的），
/// `loginAs` 不按名字选就会进错账套 —— 症状是「上一段刚拿到的凭证 id 一访问就 404」，
/// 排查方向会被带偏到"凭证被删了"。我为此白查了一轮。
/// `opts.simpleNav` 见 setSimpleNav 的注释（false / true / null，默认 false）
async function newBook(page, company, opts) {
  // 变量名不能叫 o —— 函数下半段已有 `const o = …options.json()`（回读账套参数），
  // 同名会直接报 "Identifier 'o' has already been declared"，整个 helpers.js
  // 语法检查不过、连带 90 条 E2E 全跑不起来。
  await setSimpleNav(page, opts && opts.simpleNav !== undefined ? opts.simpleNav : false);
  await page.goto("/");
  await expect(page.locator("#u")).toBeVisible({ timeout: 15_000 });
  await page.fill("#u", "admin");
  await page.fill("#p", "Admin!2026");
  await page.click('#login-form button[type="submit"]');
  // 必须等登录真正完成（账套选择界面出现）再删旧账套：click 只保证点击已发出，
  // 立刻用 page.request 会因还没有会话 cookie 而 401。
  await expect(page.locator("#new-book")).toBeVisible({ timeout: 15_000 });

  await purgeOwnBooks(page);

  const name = `${BOOK_PREFIX}${company}`;
  await page.click("#new-book");
  await page.fill("#cb-company", name);
  await page.fill("#cb-start", "2026-01");
  await page.click("#cb-save");

  await expect(page.locator('.nav-item[data-view="vouchers"]')).toBeVisible({ timeout: 15_000 });

  // 回读确认这是**出厂默认**的配置，而不是碰巧能用。
  // 整套 E2E 跑在这个配置上，所以必须显式断言：哪天有人改了默认值，
  // 这里立刻红，而不是让 70 多条用例集体以别的理由挂掉、把人引向错误方向。
  const o = await (await page.request.get("/api/options")).json();
  if (o.require_cashier !== true) {
    throw new Error(
      `新建账套的出纳签字应为默认开启，实际 ${o.require_cashier} —— ` +
        "整套 E2E 跑在出厂默认上，默认值被改了必须先弄清为什么"
    );
  }
  if (o.enable_audit !== true) {
    throw new Error(
      `新建账套的审核环节应为默认开启，实际 ${o.enable_audit}`
    );
  }
  return name;
}

/// 审核并记账该期间全部草稿凭证。
///
/// 往来核销 / 账龄 / 催款只认**已记账**分录（全仓 H-3 口径，`settle::open_entries`
/// 取 `status='posted'`），所以凡是要验证核销/报表口径的用例，录完凭证必须记账，
/// 否则列表会是空的——那是正确行为，不是 bug。
///
/// 【为什么是「审核 → 签字 → 记账」三步】新建账套**两道闸门默认都开**：
/// · `enable_audit`（审核环节）：草稿直接记账被拒（400「该账套启用了审核环节…」）
/// · `require_cashier`（出纳签字）：**涉及现金/银行科目的**凭证没签字被拒
///
/// 这里刻意走**真实生产流程**，而不是在 E2E 里把两个开关都关掉：关掉虽然能跑过，
/// 但那样整套 E2E 就跑在一个**出厂不存在的配置**上，等于把回归网自己剪了。
/// 两个默认值本身由 api.rs 的 `audit_default_on_for_new_books` 盯住，
/// `newBook` 也会在建完账套后回读确认。
///
/// 签字这一步由 admin 代劳：`Role::Admin => Perm::all()` 含 `CashierSign`。
/// 这**削弱**了出纳签字的控制力（签字人 = 记账人），但对单管理员的测试环境是唯一
/// 可行解；真正要验「出纳与会计分离」的用例在 `cashier-daily.spec.js`，那里换的是
/// 真的出纳账号。
async function postAllDrafts(page, period) {
  const r = await page.request.get(
    `/api/vouchers?period=${encodeURIComponent(period)}&status=draft&limit=200`
  );
  if (!r.ok()) throw new Error(`列草稿凭证失败：${r.status()}`);
  const list = await r.json();
  let n = 0;
  for (const v of list || []) {
    // 先审核（默认账套开着审核环节，未审核不能记账）
    const a = await page.request.post(`/api/vouchers/${v.id}/audit`, { data: {} });
    if (!a.ok()) throw new Error(`审核凭证 ${v.id} 失败：${a.status()} ${await a.text()}`);
    // 再出纳签字（默认账套开着出纳签字；涉及现金/银行的凭证没签字不能记账）。
    // 对不涉及资金科目的凭证这一步是无害的幂等操作，不必先查是不是资金凭证。
    const s = await page.request.post(`/api/vouchers/${v.id}/sign`, { data: {} });
    if (!s.ok()) throw new Error(`出纳签字 ${v.id} 失败：${s.status()} ${await s.text()}`);
    // 最后记账
    const p = await page.request.post(`/api/vouchers/${v.id}/post`, { data: {} });
    if (!p.ok()) throw new Error(`记账凭证 ${v.id} 失败：${p.status()} ${await p.text()}`);
    n++;
  }
  return n;
}

/// 录一张凭证并保存。rows: [{ code, summary, debit, credit, aux? }]
/// opts.post=true 时保存后立即记账（核销/报表类用例必须）。
async function postVoucher(page, { date, rows, post = false }) {
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
  if (post) {
    // date 形如 "2026-01-10" → 期间 202601
    await postAllDrafts(page, date.slice(0, 7).replace("-", ""));
  }
}

/// 批量审核该期间全部草稿凭证。
///
/// 新建账套的审核环节**默认开**（`BookOptions::enable_audit`，三权分离），草稿不能
/// 直接记账——`/post` 与 `/batch-post` 都会被 `vouchers::post` 的同一道闸门拒掉。
/// 配套 `postAllDrafts` 使用：先审核全部，再记账全部。
async function auditAllDrafts(page, period) {
  const r = await page.request.get(
    `/api/vouchers?period=${encodeURIComponent(period)}&status=draft&limit=200`
  );
  if (!r.ok()) throw new Error(`列草稿凭证失败：${r.status()}`);
  const list = await r.json();
  for (const v of list || []) {
    const a = await page.request.post(`/api/vouchers/${v.id}/audit`, { data: {} });
    if (!a.ok()) throw new Error(`审核凭证 ${v.id} 失败：${a.status()} ${await a.text()}`);
  }
  return (list || []).length;
}

/// 把一张草稿凭证推到「已记账」：审核 → 出纳签字 → 记账。
///
/// 两道闸门都是**生产默认开**（`enable_audit` / `require_cashier`），所以任何要
/// 「这张凭证进总账」的用例都得走这三步。签字这一步由 admin 代劳
/// （`Role::Admin => Perm::all()` 含 `CashierSign`）—— 对单管理员的测试环境这是
/// 唯一可行解；真正验「出纳与会计分离」的用例在 cashier-daily.spec.js，
/// 那里换的是真的出纳账号。
///
/// 失败时把三步各自的响应体都报出来：单看「记账 400」猜不出是审核还是签字拦的。
async function auditSignPost(page, id, { sign = true } = {}) {
  const a = await page.request.post(`/api/vouchers/${id}/audit`, { data: {} });
  if (!a.ok()) {
    throw new Error(`审核凭证 ${id} 失败：${a.status()} ${await a.text()}`);
  }
  if (sign) {
    const g = await page.request.post(`/api/vouchers/${id}/sign`, { data: {} });
    if (!g.ok()) {
      throw new Error(`出纳签字 ${id} 失败：${g.status()} ${await g.text()}`);
    }
  }
  const p = await page.request.post(`/api/vouchers/${id}/post`, { data: {} });
  if (!p.ok()) {
    throw new Error(`记账凭证 ${id} 失败：${p.status()} ${await p.text()}`);
  }
  return true;
}

/// 在当前账套里开一个岗位账号（出纳等），返回登录后的 username。
///
/// 为什么需要：E2E 此前**全部用 admin 一个身份**跑，于是权限相关的一切
/// （侧栏可见性、越权拦截、按角色分区的东西）根本没被测过。2026-09-29 的
/// 全岗位走查就是靠手工开第二个账号才发现「侧栏『最近』不校验权限」这类问题。
///
/// 平台账号必须先开（账套内子账号只是岗位/权限分配，口令沿用平台账号）。
///
/// **账号已存在时容忍并复用**：`purgeOwnBooks` 只删账套，不删平台账号。标准跑法
/// （`e2e\run-local.ps1`）每次都重建 realm 所以看不出来，但对着一个用过的开发
/// 服务器直接 `npx playwright test` 时，第二次跑就会撞「该用户名已存在」而
/// 整条用例挂掉 —— 症状是"测试自己搞坏了自己的环境"。
async function addBookUser(page, { username, password, role, display }) {
  // 1) 平台层开通（已存在就跳过）
  const probe = await page.request.get("/api/platform/users");
  if (probe.ok()) {
    const body = await probe.json();
    const users = Array.isArray(body) ? body : body.users || [];
    if (!users.some((u) => (u.username || u) === username)) {
      await page.click('.nav-item[data-view="platform-users"]');
      await page.click("#pu-add");
      await page.fill("#nc-u", username);
      await page.fill("#nc-p", password);
      await page.click("#nc-save");
      await expect(page.locator("#pu-list")).toContainText(username, { timeout: 15_000 });
    }
  }

  // 2) 拉进当前账套，定岗（已在套里就跳过）
  const res = await page.request.post("/api/users", {
    data: {
      username,
      display_name: display || username,
      password: "",
      role,
      must_change_pwd: false,
    },
  });
  if (!res.ok()) {
    const t = await res.text();
    if (!/已存在/.test(t)) {
      throw new Error(`把 ${username} 拉进账套失败：${res.status()} ${t}`);
    }
  }
  return username;
}

/// 切换到另一个账号（同一浏览器上下文 —— 这正是验证 localStorage 按账号分区的必要条件）
///
/// `book` = 目标账套全名（`newBook` 的返回值）。**必须传**：一个 realm 里会攒下
/// 多本书，`.book-enter` 取第一个就是进错账套 —— 症状是刚拿到的凭证 id 一访问就 404，
/// 会被误读成"凭证被删了"。不传就退化成"第一个"，并在这里说明为什么不能这么用。
async function loginAs(page, username, password, book) {
  await page.request.post("/api/logout", { data: {} }).catch(() => {});
  // 同 newBook：换账号后侧栏重新渲染，精简默认开会让后续点不到非 16 项的视图。
  // key 按**登录账号**分区（acctKey）—— 写 admin 的等于没写。
  await setSimpleNav(page, false, username);
  await page.goto("/");
  await expect(page.locator("#u")).toBeVisible({ timeout: 15_000 });
  await page.fill("#u", username);
  await page.fill("#p", password);
  await page.click('#login-form button[type="submit"]');
  // 非管理员看不到「新建账套」，用账套列表出现作为「登录完成」的判据
  await expect(page.locator("#book-list")).toBeVisible({ timeout: 15_000 });
  const target = book
    ? page.locator(`.book-enter[data-company="${book}"]`)
    : page.locator(".book-enter").first();
  await expect(target, `账套选择页里找不到「${book}」—— 是不是建账时名字不一致？`).toHaveCount(1);
  await target.click();
  await expect(page.locator('.nav-item[data-view="vouchers"]')).toBeVisible({ timeout: 15_000 });
}

module.exports = {
  newBook,
  setSimpleNav,
  postVoucher,
  postAllDrafts,
  auditAllDrafts,
  auditSignPost,
  addBookUser,
  loginAs,
};
