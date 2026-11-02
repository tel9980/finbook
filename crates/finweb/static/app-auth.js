// FinBook Web 前端 — 登录、选账本、首次建账向导
//
// 这一份是 `crates/finweb/static/app.js` 拆分出来的（原单文件 14k 行、295 个顶层函数）。
// 拆分方式：**经典 script，按原始行序切分，不重排任何代码**——顶层 function/const 的
// 提升与声明顺序完全保持原样，所以跨文件不存在 TDZ/未定义问题；只有 `VIEWS` 路由表
// 因为引用全部视图函数，被挪到了最后加载的 app-hr.js。
// 加载顺序由 `index.html` 的 <script> 顺序决定（也是 `main.rs` 里 asset_version 的清单顺序）。
// 新增文件必须同步三处：index.html、main.rs 的 asset_version 清单、tools/lib-app-src.js 的解析（自动）。
// ===========================================================================
// 登录（平台级：登录后再选择/新建账套）
// ===========================================================================
async function showLogin() {
  const app = document.getElementById("app");
  app.innerHTML = `
    <div class="login-wrap">
      <div class="login-card">
        <h1>FinBook 财务管理系统</h1>
        <div class="sub">多用户 · 多账套</div>
        <div class="banner set">请输入账号登录。登录后选择账套进入（仅管理员可新建账套）。</div>
        <form id="login-form">
          <div class="field"><label>账号</label><input id="u" autocomplete="username" required /></div>
          <div class="field"><label>口令</label><input id="p" type="password" autocomplete="current-password" required /></div>
          <button class="btn block" type="submit">登录</button>
        </form>
        <div id="login-err" class="muted" style="color:var(--err);margin-top:10px;min-height:18px"></div>
      </div>
    </div>`;
  $("#login-form").addEventListener("submit", async (e) => {
    e.preventDefault();
    const username = $("#u").value.trim();
    const password = $("#p").value;
    try {
      const r = await api("/login", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ username, password, device_id: deviceId(), device_name: deviceName(), book_key: "" }),
      });
      session.user = r.user;
      session.platformAdmin = !!r.user.is_admin;
      if (r.must_change_pwd) {
        openChangePwd(true, () => showBookPicker());
      } else {
        toast(`欢迎，${esc(r.user.display_name)}`, "ok");
        showBookPicker({ user: r.user, books: r.books });
      }
    } catch (err) {
      $("#login-err").textContent = err.message;
    }
  });
}

// ---------------------------------------------------------------------------
// 账套选择 / 新建（登录成功后、进入账套前）
// ---------------------------------------------------------------------------

async function loadMyBooks() {
  const r = await api("/books");
  return { user: r.user || null, books: (r && r.books) || [] };
}

async function showBookPicker(pre, force) {
  let data = pre || null;
  if (!data) {
    try { data = await loadMyBooks(); } catch (e) { return; }
  }
  // force：从账套内退回选择页时，用平台身份覆盖账套内身份
  if (data.user && (!session.user || force)) session.user = data.user;
  if (data.user) session.platformAdmin = !!data.user.is_admin;
  const books = data.books || [];
  const app = document.getElementById("app");
  const u = session.user || {};
  const isPlatformAdmin = !!session.platformAdmin;
  app.innerHTML = `
    <div class="login-wrap">
      <div class="login-card wide">
        <h1>选择账套</h1>
        <div class="sub">${esc(u.display_name || "")}${isPlatformAdmin ? "（管理员）" : ""}</div>
        <div class="muted" style="margin:6px 0 14px">管理员可创建账套并进入全部账套；普通账号由管理员开通并邀请进入账套工作。</div>
        <div id="book-list" class="book-list">${books.length ? books.map((b) => `
          <div class="book-item">
            <button class="book-enter" data-key="${esc(b.key)}" data-company="${esc(b.company || b.key)}">
              <span class="book-name">${esc(b.company || b.key)}</span>
              <span class="book-meta">${isPlatformAdmin ? `归属：${esc(b.owner)} · ` : ""}${esc(b.key)}</span>
            </button>
            ${(isPlatformAdmin || b.owner === u.username) ? `<button class="btn sm ghost book-del" data-del="${esc(b.key)}" title="删除该账套（数据不可恢复）">删除</button>` : ""}
          </div>`).join("") : `<div class="muted" style="padding:18px 0">${isPlatformAdmin ? "还没有账套，点击下方「新建账套」开始记账。" : "暂无可用账套，请让管理员创建并邀请你加入。"}</div>`}</div>
        <div style="display:flex;gap:10px;margin-top:16px">
          ${isPlatformAdmin ? `<button class="btn primary" id="new-book">＋ 新建账套</button>` : ""}
          ${isPlatformAdmin ? `<button class="btn ghost" id="import-book">导入账套</button>` : ""}
          <span class="grow"></span>
          <button class="btn ghost" id="picker-logout">退出登录</button>
        </div>
      </div>
    </div>`;
  $all(".book-enter").forEach((el) => {
    el.addEventListener("click", () => enterBook(el.dataset.key, el.dataset.company));
  });
  $all(".book-del").forEach((el) => {
    el.addEventListener("click", () => deleteBook(el.dataset.del, el.dataset.del));
  });
  if ($("#new-book")) $("#new-book").addEventListener("click", openCreateBook);
  if ($("#import-book")) $("#import-book").addEventListener("click", openImportBook);
  $("#picker-logout").addEventListener("click", async () => {
    try { await api("/logout", { method: "POST" }); } catch (e) {}
    session.user = null;
    showLogin();
  });
}

async function enterBook(key, name) {
  try {
    await api(`/books/${encodeURIComponent(key)}/select`, { method: "POST", headers: { "Content-Type": "application/json" }, body: "{}" });
    state.bookKey = key;
    state.bookOptions = null; // 换账套必须重新拉，不能沿用上一套的审核/出纳开关
    const me = await api("/me");
    session.user = me;
    shellBuilt = false; // 身份从平台层切换为账套层，顶栏与侧边栏需重建
    resetToDashboard();
    toast(`已进入账套「${esc(name || key)}」`, "ok");
    await afterLogin();
  } catch (e) {
    toast(e.message, "err");
    if (String(e.message).indexOf("登录") >= 0 || String(e.message).indexOf("停用") >= 0) showLogin();
    else if (session.user) showBookPicker();
  }
}

/// 删除账套（管理员 或 账套归属者）；删除后回到账套选择页
async function deleteBook(key, name) {
  if (!(await confirmDialog(`确定删除账套「${name || key}」？该账套内的全部凭证、科目与设置将被永久删除，不可恢复。`, true))) return;
  try {
    await api(`/books/${encodeURIComponent(key)}`, { method: "DELETE" });
    toast("账套已删除", "ok");
    if (key === state.bookKey) {
      // 删掉的正是当前所在账套：退回选择页并用平台身份重建
      state.bookKey = "";
      shellBuilt = false;
      await showBookPicker(null, true);
    } else {
      await showBookPicker();
    }
  } catch (e) { toast(e.message, "err"); }
}

/// 导入账套：桌面版建的 .fbk / 别的服务器上的账套 / 本系统「备份下载」得到的 .zip
///
/// 这里**不做任何客户端预检**（不查文件头、不看大小）：判断一个文件是不是
/// 能用的账套要在服务端跑 `PRAGMA integrity_check` 并核对科目表与 schema 版本，
/// 前端拿到的前 16 字节说明不了任何问题。上传后由服务端给出明确拒绝理由。
function openImportBook() {
  const mask = modal(`
    <h3>导入账套</h3>
    <p class="muted" style="font-size:12px;line-height:1.7;margin:0 0 14px">
      支持两种文件：<b>.fbk</b> 账套文件（桌面版建的、或从别的服务器搬来的），
      以及本系统「下载备份」得到的 <b>.zip</b>（带附件）。
      上传后服务端会先做完整性校验 —— 不是 FinBook 账套的文件会被拒绝，不会留下半个账套。
    </p>
    <label style="display:block;margin-bottom:10px">账套名称
      <input id="ib-name" placeholder="留空则用文件名" style="width:100%;margin-top:4px" />
    </label>
    <label style="display:block;margin-bottom:12px">选择文件
      <input id="ib-file" type="file" accept=".fbk,.zip" style="width:100%;margin-top:4px" />
    </label>
    <div class="muted" id="ib-msg" style="min-height:18px;font-size:12px;line-height:1.5"></div>
    <div class="foot"><button class="btn primary" id="ib-ok">开始导入</button><button class="btn ghost" id="ib-cancel">取消</button></div>
  `);
  const msg = (t) => { const el = $("#ib-msg", mask); if (el) el.innerHTML = t; };
  $("#ib-cancel", mask).onclick = () => mask.remove();
  $("#ib-ok", mask).onclick = async () => {
    const input = $("#ib-file", mask);
    const f = input.files && input.files[0];
    if (!f) { msg('<span style="color:var(--err)">先选一个 .fbk 或 .zip 文件</span>'); return; }
    const okBtn = $("#ib-ok", mask);
    okBtn.disabled = true;
    msg(`正在上传 ${esc(f.name)}（${(f.size / 1048576).toFixed(1)} MB），大账套要等一会…`);
    const fd = new FormData();
    fd.append("name", ($("#ib-name", mask).value || "").trim());
    fd.append("file", f);
    try {
      // 上传超时单独放宽到 10 分钟：几百 MB 的账套走 30s 默认值必然失败，
      // 而失败时用户看到的是「连接超时」，会以为网络坏了。
      const r = await api("/books/import", { method: "POST", body: fd, timeout: 10 * 60 * 1000 });
      mask.remove();
      toast(`已导入账套「${r.company || r.key}」${r.attachments ? `，含 ${r.attachments} 个附件` : ""}`, "ok");
      await showBookPicker();
    } catch (e) {
      okBtn.disabled = false;
      msg(`<span style="color:var(--err)">导入失败：${esc(e.message)}</span>`);
    }
  };
}

/// 删除账套（管理员 或 账套归属者）；删除后回到账套选择页
function openCreateBook() {
  const now = new Date();
  const mask = modal(`
    <h3>新建账套</h3>
    <p class="muted" style="margin:0 0 14px;line-height:1.6">
      每个账套都是独立隔离的一套账。仅管理员可创建；创建后可在「账号管理」开通成员并邀请入套、分配岗位。
    </p>
    <div class="field"><label>公司名称</label><input id="cb-company" placeholder="例如：某某贸易有限公司" /></div>
    <div class="field"><label>启用期间（YYYY-MM）</label><input id="cb-start" value="${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}" /></div>
    <div class="field"><label>账套标识（可选，留空自动生成）</label><input id="cb-key" placeholder="字母/数字/下划线" /></div>
    <div class="foot"><button class="btn" id="cb-cancel">取消</button><button class="btn primary" id="cb-save">创建</button></div>`);
  $("#cb-cancel", mask).onclick = closeModal;
  $("#cb-save", mask).onclick = async () => {
    const company = $("#cb-company", mask).value.trim();
    if (!company) { toast("请输入公司名称", "err"); return; }
    const ym = $("#cb-start", mask).value.trim().replace(/[^0-9]/g, "");
    let start_period = 0;
    if (ym.length === 6) start_period = parseInt(ym, 10);
    else if (/^\d{4}$/.test(ym)) start_period = parseInt(ym, 10) * 100 + 1;
    if (start_period <= 0) { toast("启用期间格式应为 YYYY-MM", "err"); return; }
    try {
      const r = await api("/books", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ key: $("#cb-key", mask).value.trim(), company, start_period }) });
      closeModal();
      toast(`账套「${esc(company)}」创建成功`, "ok");
      await enterBook(r.key, company);
    } catch (e) { toast(e.message, "err"); }
  };
}

async function afterLogin() {
  // 加载期间列表与当前期间
  try {
    const p = await api("/periods");
    state.periods = p.list || [];
    state.current = p.current;
  } catch (e) {}
  // 账套参数：凭证弹窗的「记账」按钮要按 enable_audit 决定给不给（见 openVoucher）。
  // 拿不到就退回 null，前端按「未开审核」处理——顶多按钮亮着被后端拒，
  // 不会把按钮错误地藏掉。
  try {
    state.bookOptions = await api("/options");
  } catch (e) {
    state.bookOptions = null;
  }
  resetToDashboard();
  render();
  // 未建账（尚未设置公司名）→ 弹出建账向导
  try {
    const st = await api("/setup/status");
    if (st && st.needs_setup) showSetupWizard();
  } catch (e) {}
}

// ===========================================================================
// 建账向导（首次登录 / 未设置公司名时出现）
// ===========================================================================
async function showSetupWizard() {
  if (!session.user) return;
  let opts = {};
  try { opts = await api("/options"); } catch (e) {}
  const now = new Date();
  const mask = modal(`
    <h3>创建账套</h3>
    <p class="muted" style="margin:0 0 14px;line-height:1.6">
      为当前账套设置公司信息与启用期间。完成后即可开始填制凭证。
      此步骤可由管理员随时在「账套参数」中修改。
    </p>
    <div class="field">
      <label>公司名称</label>
      <input id="set-company" placeholder="例如：某某贸易有限公司" value="${esc(opts.company || "")}" />
    </div>
    <div class="field">
      <label>启用期间（YYYY-MM）</label>
      <input id="set-start" placeholder="2026-01" value="${opts.start_period || now.getFullYear() + "-01"}" />
    </div>
    <div class="field">
      <label>本位币</label>
      <input id="set-currency" value="${opts.base_currency || "CNY"}" />
    </div>
    <div class="field">
      <label>审核环节</label>
      <div class="u-row" style="gap:16px;align-items:flex-start">
        <label class="u-row tight" style="cursor:pointer">
          <input type="radio" name="set-audit" id="set-audit-on" value="1" ${opts.enable_audit ? "checked" : ""} />
          <span>启用（<b>有独立的审核人</b>，如财务主管）</span>
        </label>
        <label class="u-row tight" style="cursor:pointer">
          <input type="radio" name="set-audit" id="set-audit-off" value="0" ${opts.enable_audit ? "" : "checked"} />
          <span>不启用（<b>会计自己录自己记</b>）</span>
        </label>
      </div>
      <div class="u-sm u-dim" style="margin-top:4px">
        启用后，未审核的凭证不能记账。若本企业<b>没有</b>除会计之外能审核的人
        （如只有会计+出纳、没有财务主管），请选「不启用」——否则会计既不能记账
        也不能审核，凭证会一直卡住。此项之后可在「账套参数」里改。
      </div>
    </div>
    <div class="field">
      <label>出纳签字</label>
      <div class="u-row" style="gap:16px;align-items:flex-start">
        <label class="u-row tight" style="cursor:pointer">
          <input type="radio" name="set-cash" id="set-cash-on" value="1" ${opts.require_cashier ? "checked" : ""} />
          <span>启用（<b>有出纳岗</b>，资金进出由出纳复核）</span>
        </label>
        <label class="u-row tight" style="cursor:pointer">
          <input type="radio" name="set-cash" id="set-cash-off" value="0" ${opts.require_cashier ? "" : "checked"} />
          <span>不启用（<b>没有出纳岗</b>，会计自己管钱）</span>
        </label>
      </div>
      <div class="u-sm u-dim" style="margin-top:4px">
        启用后，<b>涉及现金/银行科目</b>的凭证记账前须出纳签字。若本企业没有出纳岗
        （一个人记账），请选「不启用」——否则现金/银行存款的凭证会一直卡在
        「需出纳签字」，而没有人能签。此项之后可在「账套参数」里改。
      </div>
    </div>
    <div class="foot">
      <button class="btn ghost" id="setup-later">稍后再说</button>
      <button class="btn primary" id="setup-save">创建账套</button>
    </div>
  `);
  $("#setup-save").addEventListener("click", async () => {
    const company = $("#set-company").value.trim();
    if (!company) { toast("请输入公司名称", "err"); return; }
    const startText = $("#set-start").value.trim();
    const ym = startText.replace(/[^0-9]/g, "");
    let start_period = 0;
    if (ym.length === 6) start_period = parseInt(ym, 10);
    else if (/^\d{4}$/.test(ym)) start_period = parseInt(ym, 10) * 100 + 1;
    if (start_period <= 0) { toast("启用期间格式应为 YYYY-MM", "err"); return; }
    try {
      const cur = await api("/options");
      const enable_audit = ($("#set-audit-on") && $("#set-audit-on").checked) || false;
      // 出纳签字同样要落到账套参数上。**不显式写就会继承出厂默认 true**，
      // 于是「一个人记账」的小微企业建完账套才发现现金凭证永远记不了账 ——
      // 那个坑正是这一问要挡掉的。
      const require_cashier = ($("#set-cash-on") && $("#set-cash-on").checked) || false;
      const merged = Object.assign({}, cur, {
        company,
        start_period,
        base_currency: $("#set-currency").value.trim() || "CNY",
        enable_audit,
        require_cashier,
      });
      await api("/options", { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify(merged) });
      // 提示要把**两道闸门**的实际状态都说出来。
      // 原来只说审核：用户看到「已启用审核环节」就以为万事大吉，
      // 结果第一次录现金凭证就被出纳签字拦下，说不出为什么。
      const notes = [];
      notes.push(enable_audit ? "已启用审核环节" : "未启用审核环节（会计可自行记账）");
      notes.push(require_cashier ? "已启用出纳签字" : "未启用出纳签字（无出纳岗）");
      toast(`账套创建成功（${notes.join("，")}）`, "ok");
      closeModal();
      resetToDashboard();
      render();
    } catch (err) { toast(err.message, "err"); }
  });
  const later = $("#setup-later");
  if (later) later.addEventListener("click", () => closeModal());
}

// ===========================================================================
// 应用骨架
// ===========================================================================

// 导航项配置（新增页面只改这里 + VIEWS 注册表，无需改 switch）
// admin: true 表示仅系统管理员可见（只读视角入口）
// group: 侧边栏分组（与桌面端 NavItem::group 保持一致）
const NAV_ITEMS = [
  { id: "overview", label: "账目总览", admin: true, group: "管理员" },
  { id: "platform-users", label: "账号管理", platform: true, group: "系统管理" },
  { id: "platform-books", label: "全部账套", platform: true, group: "系统管理" },
  { id: "audit-log", label: "操作日志", perm: "audit_log", platform: true, group: "系统管理" },
  { id: "consolidate", label: "合并报表", platform: true, group: "系统管理" },
  { id: "dashboard", label: "工作台", perm: null, group: "开始" },
  { id: "accounts", label: "会计科目", perm: "account_edit", group: "基础资料" },
  { id: "begin", label: "期初建账", perm: "opening", group: "基础资料" },
  { id: "aux", label: "辅助档案", perm: "aux_edit", group: "基础资料" },
  { id: "vouchers", label: "记账凭证", perm: "voucher_new", group: "凭证" },
  { id: "imports", label: "数据导入", perm: "voucher_new", group: "凭证" },
  { id: "templates", label: "凭证模板", perm: "voucher_new", group: "凭证" },
  { id: "payroll", label: "工资管理", perm: "voucher_new", group: "凭证" },
  { id: "claims", label: "费用报销", perm: "voucher_new", group: "凭证" },
  { id: "invoices", label: "发票管理", perm: "fin_report", group: "凭证" },
  { id: "tax-decl", label: "税务申报", perm: "fin_report", group: "凭证" },
  { id: "ledger", label: "明细账", perm: "fin_report", group: "账簿报表" },
  { id: "reports", label: "报表中心", perm: "fin_report", group: "账簿报表" },
  { id: "balance-sheet", label: "资产负债表", perm: "fin_report", group: "账簿报表" },
  { id: "income-statement", label: "利润表", perm: "fin_report", group: "账簿报表" },
  { id: "cash-flow", label: "现金流量表", perm: "fin_report", group: "账簿报表" },
  { id: "multi-column", label: "多栏账", perm: "fin_report", group: "账簿报表" },
  { id: "summary-table", label: "摘要汇总表", perm: "fin_report", group: "账簿报表" },
  { id: "ratios", label: "财务指标", perm: "fin_report", group: "账簿报表" },
  { id: "equity", label: "权益变动表", perm: "fin_report", group: "账簿报表" },
  { id: "compare", label: "报表对比", perm: "fin_report", group: "账簿报表" },
  { id: "daily", label: "科目日报表", perm: "fin_report", group: "账簿报表" },
  { id: "dim-profit", label: "多维损益", perm: "fin_report", group: "账簿报表" },
  { id: "month-brief", label: "月度简报", perm: "fin_report", group: "账簿报表" },
  { id: "profit-sheet", label: "利润分析表", perm: "fin_report", group: "账簿报表" },
  { id: "notes", label: "报表附注", perm: "fin_report", group: "账簿报表" },
  { id: "custom-reports", label: "自定义报表", perm: "fin_report", group: "账簿报表" },
  { id: "period-end", label: "期末处理", perm: "period_close", group: "期末" },
  { id: "assets", label: "固定资产", perm: "account_edit", group: "期末" },
  { id: "bank", label: "银行对账", perm: "voucher_new", group: "期末" },
  { id: "settle", label: "往来核销", perm: "voucher_new", group: "期末" },
  { id: "reconcile", label: "期末对账", perm: "fin_report", group: "期末" },
  { id: "mrp", label: "MRP 运算", perm: "production_ops", production: true, group: "生产制造" },
  { id: "mps", label: "MPS 排产", perm: "production_ops", production: true, group: "生产制造" },
  { id: "routing", label: "工艺路线", perm: "production_ops", production: true, group: "生产制造" },
  { id: "work-report", label: "工序报工", perm: "production_ops", production: true, group: "生产制造" },
  { id: "consign", label: "客供料台账", perm: "order_ops", production: true, group: "生产制造" },
  { id: "piece-pay", label: "计件工资", perm: "production_ops", production: true, group: "生产制造" },
  { id: "tool-ledger", label: "挂具台账", perm: "production_ops", production: true, group: "生产制造" },
  { id: "rework", label: "返工成本单", perm: "production_ops", production: true, group: "生产制造" },
  { id: "funds", label: "资金管理", perm: "fin_report", group: "资金" },
  { id: "budget-versions", label: "预算版本", perm: "fin_report", group: "管理会计" },
  { id: "budget-alerts", label: "预算预警", perm: "fin_report", group: "管理会计" },
  { id: "budget-analysis", label: "预算分析", perm: "fin_report", group: "管理会计" },
  { id: "cost", label: "成本核算", perm: "fin_report", group: "管理会计" },
  { id: "price-rules", label: "加工费规则", perm: "price_view", group: "销售" },
  { id: "po-doc", label: "采购单据", perm: "order_ops", group: "采购" },
  { id: "po-reconcile", label: "采购对账", perm: "report", group: "采购" },
  { id: "po-estimate", label: "采购暂估", perm: "order_ops", group: "采购" },
  { id: "procure-quota", label: "供应商配额", perm: "order_ops", group: "采购" },
  // 客户视图：perm 用 report（不是 aux_edit）—— 出纳要看这张名单（催款要用）、
  // 要能核销，但不该能改客户档案。详见 docs/客户管理模块设计.md §4。
  { id: "customers", label: "客户管理", perm: "report", group: "销售" },
  { id: "so-doc", label: "销售单据", perm: "order_ops", group: "销售" },
  { id: "so-reconcile", label: "销售对账", perm: "report", group: "销售" },
  { id: "order-change-log", label: "订单变更", perm: "report", group: "销售" },
  { id: "items-master", label: "存货档案", perm: "warehouse", group: "库存" },
  { id: "inv-batch", label: "批次库位", perm: "warehouse", group: "库存" },
  { id: "inv-count", label: "存货盘点", perm: "warehouse", group: "库存" },
  { id: "inv-aging", label: "库存账龄", perm: "report", group: "库存" },
  { id: "inv-abc", label: "库存ABC", perm: "report", group: "库存" },
  { id: "cost-variance", label: "存货成本差异", perm: "report", group: "库存" },
  { id: "landed-cost", label: "到岸成本", perm: "order_ops", group: "库存" },
  { id: "inv-serial", label: "序列号", perm: "warehouse", group: "库存" },
  { id: "inv-unit", label: "多单位换算", perm: "warehouse", group: "库存" },
  { id: "warehouses", label: "仓库档案", perm: "warehouse", group: "库存" },
  { id: "inv-assemble", label: "组装拆卸", perm: "warehouse", group: "库存" },
  { id: "inv-warehouse", label: "分仓库库存", perm: "report", group: "库存" },
  { id: "inv-transfer", label: "调拨报表", perm: "report", group: "库存" },
  { id: "approval", label: "审批中心", perm: "report", group: "系统" },
  { id: "workflow", label: "工作流", perm: "report", group: "系统" },
  { id: "archive", label: "电子档案", perm: "report", group: "系统" },
  { id: "options", label: "账套参数", perm: "sys_option", group: "系统" },
  { id: "logs", label: "操作日志", perm: "audit_log", group: "系统" },
  { id: "backup", label: "备份恢复", perm: "backup", group: "系统" },
  { id: "security", label: "安全中心", perm: "user_manage", group: "系统" },
];
