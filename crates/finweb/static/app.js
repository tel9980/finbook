// FinBook Web 前端（原生 JS SPA，无框架依赖）
const API = "/api";

let session = { user: null };
let state = { view: "dashboard", periods: [], current: null, accounts: null, users: null, bookKey: "" };

// ---------- 工具 ----------
// esc / fmt / fmtMoney / ymm 抽到 util.js（无 DOM 依赖，可单测），此处经全局复用。
function $(sel, root) { return (root || document).querySelector(sel); }
function $all(sel, root) { return Array.from((root || document).querySelectorAll(sel)); }

// 请求竞态防护：每个视图一个请求序号，返回时才采纳最新一次的结果
const reqSeq = { v: 0 };
function nextReq(scope) { reqSeq[scope] = (reqSeq[scope] || 0) + 1; return reqSeq[scope]; }
function staleReq(scope, id) { return reqSeq[scope] !== id; }

async function api(path, opts = {}) {
  const r = await fetch(API + path, Object.assign({ credentials: "same-origin" }, opts));
  let data = null;
  try { data = await r.json(); } catch (e) {}
  if (r.status === 401) {
    if (path !== "/setup/status" && path !== "/login") { session.user = null; render(); }
    throw new Error((data && data.error) || "未登录");
  }
  if (!r.ok) {
    let msg = translateErr((data && data.error) || ("请求失败 " + r.status));
    if (r.status === 403) msg += "（权限不足，请联系管理员开通该权限或调整岗位）";
    throw new Error(msg);
  }
  // 列偏好：每次数据到达后重挂（异步填充的表格也能获得列菜单与隐藏样式）
  try { autoColPrefs(); } catch (e) {}
  // 通用列头排序 + 列宽拖拽（同点补挂，异步表也能获得）
  try { autoColSort(); } catch (e) {}
  try { autoColResize(); } catch (e) {}
  // 单元格省略号补 title（悬停可见全文）
  try { autoCellTitles(); } catch (e) {}
  return data;
}

// ===========================================================================
// 后端错误翻译：把 Rust / Serde 的原始报错翻成人话
//
// 反序列化失败会以 "Failed to deserialize the JSON body into the target type:
// missing field `xxx` at line 1 column 139" 抛给用户——既看不懂也不知怎么改。
// 这里按已知的字段中文名映射后拼成可操作的提示；未识别的原样返回。
// ===========================================================================
const FIELD_CN = {
  device_id: "设备标识", device_name: "设备名称", username: "用户名", password: "口令",
  account_code: "科目编码", debit: "借方金额", credit: "贷方金额", summary: "摘要",
  line: "行号", date: "日期", entries: "分录明细", year_begin: "年初余额",
  period: "会计期间", company: "账套名称", enable_period: "启用期间", kind: "类型",
  amount: "金额", qty: "数量", dir: "方向", aux: "辅助核算", memo: "摘要",
  name: "名称", code: "编码", old_password: "原口令", new_password: "新口令",
};
const ERR_HINT = {
  "Failed to deserialize": "提交的数据格式不对，缺少必填项或类型不符",
  "missing field": "必填项没填",
  "invalid type": "某项的数据类型不对",
  "unknown field": "提交了系统不认识的字段",
};
function translateErr(raw) {
  const s = String(raw == null ? "" : raw);
  if (!/Failed to deserialize|missing field|invalid type|unknown field/.test(s)) return s;
  const head = (s.match(/^[A-Za-z ]+?(?=[:：])/) || [""])[0].trim();
  const missing = (s.match(/missing field `([^`]+)`/) || [])[1];
  const unknown = (s.match(/unknown field `([^`]+)`/) || [])[1];
  const badPath = (s.match(/(?:at )?`?([a-z_]+(?:\[\d+\])?)\.?([a-z_]+)?`?:/) || [])[1];
  const parts = [];
  const h = ERR_HINT[head] || ERR_HINT["Failed to deserialize"] || "提交的数据格式不对";
  parts.push(h);
  if (missing) parts.push(`缺「${FIELD_CN[missing] || missing}」`);
  else if (unknown) parts.push(`多传了「${FIELD_CN[unknown] || unknown}」`);
  else if (badPath) {
    const seg = badPath.split(".");
    const last = seg[seg.length - 1];
    parts.push(`问题在「${FIELD_CN[last] || last}」`);
  }
  return parts.join("，") + "。请检查该表单的必填项后重试";
}

// ===========================================================================
// 未保存守卫：期初/凭证等"草稿在内存里"的页面，切视图或关页面前提醒
//
// 场景：录了 200 行期初，中间点去别的菜单 → state.beginDraft 还在，
// 再切回来数据还在；但如果离开整个应用（刷新/关标签）就全丢了，
// 而且没有任何提示。这里用一个统一的 dirty 标记 + beforeunload 兜底。
// ===========================================================================
let dirtyCount = 0;
const dirtyReasons = new Set();
function markDirty(reason) {
  if (reason) dirtyReasons.add(reason);
  dirtyCount = dirtyReasons.size;
  try { window.onbeforeunload = dirtyCount ? dirtyBeforeUnload : null; } catch (e) {}
}
function clearDirty(reason) {
  if (reason) dirtyReasons.delete(reason);
  dirtyCount = dirtyReasons.size;
  if (!dirtyCount) { try { window.onbeforeunload = null; } catch (e) {} }
}
function dirtyBeforeUnload(e) {
  // 提示用户：还有未保存的修改
  e.preventDefault();
  e.returnValue = "";
  return "";
}

const MAX_TOASTS = 5;
function toast(msg, kind) {
  const wrap = document.getElementById("toast");
  // 上限：挤掉最旧
  while (wrap.children.length >= MAX_TOASTS) wrap.removeChild(wrap.firstChild);
  const t = document.createElement("div");
  t.className = "toast " + (kind || "");
  const text = document.createElement("span");
  text.textContent = msg;
  const close = document.createElement("button");
  close.className = "toast-x";
  close.type = "button";
  close.textContent = "✕";
  close.setAttribute("aria-label", "关闭提示");
  const dismiss = () => { t.classList.remove("show"); setTimeout(() => t.remove(), 200); };
  close.onclick = dismiss;
  t.appendChild(text);
  t.appendChild(close);
  wrap.appendChild(t);
  requestAnimationFrame(() => t.classList.add("show"));
  setTimeout(dismiss, 2600);
}

let modalStack = [];
// Ctrl+K 全局快速搜索；Ctrl+Enter 提交当前弹窗主按钮（与画布 Ctrl+Z/Y 不冲突）
document.addEventListener("keydown", (e) => {
  if ((e.ctrlKey || e.metaKey) && e.key && e.key.toLowerCase() === "k") {
    e.preventDefault();
    openQuickSearch();
  } else if ((e.ctrlKey || e.metaKey) && e.key === "Enter" && typeof modalStack !== "undefined" && modalStack.length) {
    const top = modalStack[modalStack.length - 1];
    const btn = top.querySelector(".foot .btn.primary") || top.querySelector(".btn.primary");
    if (btn) { e.preventDefault(); btn.click(); }
  }
});

// 快速搜索面板：分组结果 → 点击/回车直达目标页面（服务端按权限与数据范围裁剪）
function openQuickSearch() {
  const mask = modal(`<h3>快速搜索 <span class="muted" style="font-size:12px;font-weight:400">Ctrl+K 唤起 · Enter 打开首条 · Esc 关闭</span></h3>
    <input id="qs-input" placeholder="单号 / 名称 / 摘要 / 关键词…" style="width:100%" />
    <div id="qs-results" class="muted" style="margin-top:8px;min-height:120px;max-height:55vh;overflow:auto">输入关键词即时搜索</div>
    <div class="foot"><button class="btn ghost" id="qs-close">关闭</button></div>`);
  $("#qs-close", mask).onclick = closeModal;
  const input = $("#qs-input", mask);
  const box = $("#qs-results", mask);
  input.focus();
  const KIND = { voucher: "凭证", po: "采购订单", so: "销售订单", req: "请购单", claim: "报销单" };
  let timer = null;
  const run = async () => {
    const kw = input.value.trim();
    if (!kw) { box.className = "muted"; box.textContent = "输入关键词即时搜索"; return; }
    try {
      const r = await api(`/quick-search?q=${encodeURIComponent(kw)}`);
      const rows = r.rows || [];
      box.className = "";
      box.innerHTML = rows.length
        ? rows.map((x, i) => `<button class="btn ghost" data-qs="${i}" data-view="${esc(x.view)}" style="display:flex;width:100%;justify-content:space-between;gap:8px;margin-bottom:4px;text-align:left">
            <span><span class="tag">${KIND[x.kind] || esc(x.kind)}</span> ${esc(x.label)}</span>
            <span class="muted" style="font-size:12px;white-space:nowrap">${esc(x.sub || "")}</span></button>`).join("")
        : `<div class="muted">无匹配结果</div>`;
      $all("[data-qs]", mask).forEach((b) => b.onclick = () => { state.view = b.dataset.view; closeModal(); renderMain(); });
    } catch (e) { box.innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  input.oninput = () => { clearTimeout(timer); timer = setTimeout(run, 260); };
  input.onkeydown = (e) => {
    if (e.key === "Enter") { e.preventDefault(); const b = mask.querySelector("[data-qs]"); if (b) b.click(); }
    else if (e.key === "Escape") { closeModal(); }
  };
}

function modal(html, wide) {
  const root = document.getElementById("modal-root");
  const mask = document.createElement("div");
  mask.className = "modal-mask";
  mask.innerHTML = `<div class="modal ${wide ? "wide" : ""}">${html}</div>`;
  // 无障碍：弹窗语义 + 标题标签
  mask.setAttribute("role", "dialog");
  mask.setAttribute("aria-modal", "true");
  const title = mask.querySelector("h3");
  if (title) mask.setAttribute("aria-label", title.textContent.trim());
  root.appendChild(mask);
  mask.addEventListener("click", (e) => { if (e.target === mask) closeModal(); });
  // 焦点陷阱：Tab 在弹窗内循环（不跑到背后页面）
  mask.addEventListener("keydown", (e) => {
    if (e.key !== "Tab") return;
    const f = [...mask.querySelectorAll('a[href],button:not([disabled]),input:not([disabled]),select:not([disabled]),textarea:not([disabled]),[tabindex]:not([tabindex="-1"])')]
      .filter((el) => el.offsetParent !== null);
    if (!f.length) return;
    const first = f[0], last = f[f.length - 1];
    if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
    else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
  });
  modalStack.push(mask);
  // 焦点移入弹窗首个可聚焦元素
  const first = mask.querySelector("input, select, textarea, button");
  if (first) first.focus();
  return mask;
}
function closeModal() {
  // 栈式关闭：只关最上层弹窗，下层（如凭证编辑器）保持不动
  const top = modalStack.pop();
  if (top) {
    top.remove();
    return;
  }
  document.getElementById("modal-root").innerHTML = "";
}
// 全量关闭（登出/切视图等场景显式调用）
function closeAllModals() {
  document.getElementById("modal-root").innerHTML = "";
  modalStack = [];
}
// 确认对话框（Promise 化，替代浏览器原生 confirm）
// 自包含：只关闭自身遮罩，不影响下方已打开的其它弹窗（如凭证编辑器）
function confirmDialog(message, danger) {
  return new Promise((resolve) => {
    const root = document.getElementById("modal-root");
    const mask = document.createElement("div");
    mask.className = "modal-mask";
    mask.setAttribute("role", "dialog");
    mask.setAttribute("aria-modal", "true");
    mask.setAttribute("aria-label", "确认操作");
    mask.innerHTML = `<div class="modal">
      <h3>确认操作</h3>
      <p class="muted" style="margin:0 0 16px;line-height:1.6">${esc(message)}</p>
      <div class="foot">
        <button class="btn ghost" id="cf-cancel">取消</button>
        <button class="btn ${danger ? "danger" : "primary"}" id="cf-ok">确定</button>
      </div>
    </div>`;
    root.appendChild(mask);
    modalStack.push(mask); // 入栈：Ctrl+Enter 提交与 Esc 关闭都作用于最上层
    const done = (val) => {
      document.removeEventListener("keydown", onKey, true);
      const i = modalStack.indexOf(mask);
      if (i >= 0) modalStack.splice(i, 1);
      mask.remove();
      resolve(val);
    };
    const onKey = (e) => {
      // 捕获阶段拦截并阻止冒泡，避免触发全局 Esc 关闭其它弹窗
      if (e.key === "Escape") { e.stopPropagation(); done(false); }
    };
    document.addEventListener("keydown", onKey, true);
    mask.addEventListener("click", (e) => { if (e.target === mask) done(false); });
    $("#cf-ok", mask).onclick = () => done(true);
    $("#cf-cancel", mask).onclick = () => done(false);
    // 危险操作默认聚焦「取消」，防止回车误删；普通操作聚焦「确定」
    (danger ? $("#cf-cancel", mask) : $("#cf-ok", mask)).focus();
  });
}
// Esc 关闭最近弹窗
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape" && modalStack.length) closeModal();
});

// 快捷键帮助面板（顶栏「?」或按 ?）
function openShortcuts() {
  const rows = [
    ["Ctrl+K", "全局快速搜索（凭证 / 采购 / 销售 / 请购 / 报销）"],
    ["Ctrl+Enter", "提交当前弹窗主按钮（保存 / 确定）"],
    ["Ctrl+S", "保存当前录单弹窗"],
    ["Enter", "录单：最后一行金额/摘要回车 → 新增一行（自动带出上一行摘要）"],
    ["F7", "录单：聚焦科目搜索框（输入编码/名称过滤）"],
    ["J / K", "审批实例：上下选择行"],
    ["A / R", "审批实例：通过 / 驳回"],
    ["Esc", "关闭最上层弹窗（下层编辑器保留）"],
    ["?", "打开本帮助"],
  ];
  const mask = modal(`<h3>快捷键与帮助</h3>
    <table class="grid"><tbody>${rows.map(([k, v]) => `<tr><td style="white-space:nowrap"><span class="tag">${k}</span></td><td>${v}</td></tr>`).join("")}</tbody></table>
    <div class="muted" style="font-size:12px;margin-top:8px">所有列表：点列头排序、拖列边调宽、右上「列▾」显隐列（偏好自动记忆）；顶栏「Aa」调字号。</div>
    <div class="foot"><button class="btn ghost" id="kb-close">关闭</button></div>`);
  $("#kb-close", mask).onclick = closeModal;
}

// 录单键盘流 + 全局快捷键：Enter 加行（带出摘要）/ Ctrl+S 保存 / F7 科目搜索 / ? 帮助
document.addEventListener("keydown", (e) => {
  const t = e.target;
  const tag = t && t.tagName;
  // ? 帮助（输入态不触发）
  if (e.key === "?" && !e.ctrlKey && !e.metaKey && !e.altKey && !["INPUT", "SELECT", "TEXTAREA"].includes(tag)) {
    e.preventDefault();
    openShortcuts();
    return;
  }
  // F7：聚焦当前弹窗的科目搜索框
  if (e.key === "F7" && modalStack.length) {
    const q = modalStack[modalStack.length - 1].querySelector(".acct-q");
    if (q) { e.preventDefault(); q.focus(); }
    return;
  }
  // Ctrl+S：保存当前录单弹窗（优先 #v-save，其次栈顶主按钮）
  if ((e.ctrlKey || e.metaKey) && e.key && e.key.toLowerCase() === "s" && modalStack.length) {
    const btn = document.querySelector("#v-save") || modalStack[modalStack.length - 1].querySelector(".foot .btn.primary");
    if (btn) { e.preventDefault(); btn.click(); }
    return;
  }
  // Enter：录单最后一行 → 新增行 + 带出上一行摘要
  if (e.key === "Enter" && !e.ctrlKey && !e.metaKey && t && t.matches && t.matches(".e-sum, .e-d, .e-c")) {
    const tbody = t.closest("tbody");
    const rows = [...tbody.querySelectorAll("tr:has(select.acct-sel)")];
    const cur = t.closest("tr");
    if (rows.length && cur === rows[rows.length - 1]) {
      e.preventDefault();
      const prevSum = (cur.querySelector(".e-sum") || {}).value || "";
      const add = document.querySelector("#v-add");
      if (!add) return;
      add.click();
      const rows2 = [...tbody.querySelectorAll("tr:has(select.acct-sel)")];
      const last = rows2[rows2.length - 1];
      const sum = last && last.querySelector(".e-sum");
      if (sum && !sum.value && prevSum) {
        sum.value = prevSum;
        sum.dispatchEvent(new Event("input", { bubbles: true }));
      }
      const q = last && last.querySelector(".acct-q");
      if (q) q.focus();
    }
  }
});

// ---------- 设备指纹 ----------
function deviceId() {
  let id = localStorage.getItem("finbook_device_id");
  if (!id) { id = (crypto.randomUUID ? crypto.randomUUID() : "d-" + Math.random().toString(36).slice(2) + Date.now()); localStorage.setItem("finbook_device_id", id); }
  return id;
}
function deviceName() { return (navigator.platform || "Web") + " · " + navigator.userAgent.slice(0, 40); }

// ---------- 权限 ----------
function can(p) { return session.user && session.user.perms.indexOf(p) >= 0; }

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
    const me = await api("/me");
    session.user = me;
    shellBuilt = false; // 身份从平台层切换为账套层，顶栏与侧边栏需重建
    state.view = "dashboard";
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
  state.view = "dashboard";
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
      const merged = Object.assign({}, cur, { company, start_period, base_currency: $("#set-currency").value.trim() || "CNY" });
      await api("/options", { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify(merged) });
      closeModal();
      toast("账套创建成功", "ok");
      state.view = "dashboard";
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
  { id: "notes", label: "报表附注", perm: "fin_report", group: "账簿报表" },
  { id: "custom-reports", label: "自定义报表", perm: "fin_report", group: "账簿报表" },
  { id: "period-end", label: "期末处理", perm: "period_close", group: "期末" },
  { id: "assets", label: "固定资产", perm: "account_edit", group: "期末" },
  { id: "bank", label: "银行对账", perm: "voucher_new", group: "期末" },
  { id: "settle", label: "往来核销", perm: "voucher_new", group: "期末" },
  { id: "reconcile", label: "期末对账", perm: "fin_report", group: "期末" },
  { id: "mrp", label: "MRP 运算", perm: "production_ops", group: "生产制造" },
  { id: "mps", label: "MPS 排产", perm: "production_ops", group: "生产制造" },
  { id: "routing", label: "工艺路线", perm: "production_ops", group: "生产制造" },
  { id: "work-report", label: "工序报工", perm: "production_ops", group: "生产制造" },
  { id: "funds", label: "资金管理", perm: "fin_report", group: "资金" },
  { id: "budget-versions", label: "预算版本", perm: "fin_report", group: "管理会计" },
  { id: "budget-alerts", label: "预算预警", perm: "fin_report", group: "管理会计" },
  { id: "budget-analysis", label: "预算分析", perm: "fin_report", group: "管理会计" },
  { id: "cost", label: "成本核算", perm: "fin_report", group: "管理会计" },
  { id: "po-doc", label: "采购单据", perm: "order_ops", group: "采购" },
  { id: "po-reconcile", label: "采购对账", perm: "report", group: "采购" },
  { id: "po-estimate", label: "采购暂估", perm: "order_ops", group: "采购" },
  { id: "procure-quota", label: "供应商配额", perm: "order_ops", group: "采购" },
  { id: "so-doc", label: "销售单据", perm: "order_ops", group: "销售" },
  { id: "so-reconcile", label: "销售对账", perm: "report", group: "销售" },
  { id: "order-change-log", label: "订单变更", perm: "report", group: "销售" },
  { id: "items-master", label: "存货档案", perm: "warehouse", group: "库存" },
  { id: "inv-batch", label: "批次库位", perm: "warehouse", group: "库存" },
  { id: "inv-count", label: "存货盘点", perm: "warehouse", group: "库存" },
  { id: "inv-aging", label: "库存账龄", perm: "report", group: "库存" },
  { id: "inv-abc", label: "库存ABC", perm: "report", group: "库存" },
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

// 视图注册表：id → 渲染函数（函数声明已提升，可在顶层引用）
const VIEWS = {
  "overview": viewOverview,
  "platform-users": viewPlatformUsers,
  "platform-books": viewPlatformBooks,
  "consolidate": viewConsolidate,
  "dashboard": viewDashboard,
  "vouchers": viewVouchers,
  "invoices": viewInvoices,
  "imports": viewImports,
  "ledger": viewLedger,
  "reports": viewReports,
  "balance-sheet": viewBalanceSheet,
  "income-statement": viewIncomeStatement,
  "cash-flow": viewCashFlow,
  "multi-column": viewMultiColumn,
  "summary-table": viewSummaryTable,
  "ratios": viewRatios,
  "equity": viewEquity,
  "compare": viewCompare,
  "daily": viewDaily,
  "period-end": viewPeriodEnd,
  "assets": viewAssets,
  "bank": viewBank,
  "settle": viewSettle,
  "reconcile": viewReconcile,
  "mrp": viewMrp,
  "mps": viewMps,
  "routing": viewRouting,
  "approval": viewApproval,
  "notes": viewNotes,
  "custom-reports": viewCustomReports,
  "archive": viewArchive,
  "budget-versions": viewBudgetVersions,
  "budget-alerts": viewBudgetAlerts,
  "po-reconcile": viewPoReconcile,
  "so-reconcile": viewSoReconcile,
  "inv-aging": viewInvAging,
  "inv-abc": viewInvAbc,
  "inv-serial": viewInvSerial,
  "inv-unit": viewInvUnit,
  "inv-assemble": viewInvAssemble,
  "warehouses": viewWarehouses,
  "inv-warehouse": viewInvWarehouse,
  "inv-transfer": viewInvTransfer,
  "po-estimate": viewPoEstimate,
  "inv-count": viewInvCount,
  "items-master": viewItemsMaster,
  "inv-batch": viewBatch,
  "workflow": viewWorkflow,
  "procure-quota": viewProcureQuota,
  "po-doc": viewPoDoc,
  "so-doc": viewSoDoc,
  "order-change-log": viewOrderChangeLog,
  "work-report": viewWorkReport,
  "funds": viewFunds,
  "budget-analysis": viewBudgetAnalysis,
  "cost": viewCost,
  "security": viewSecurity,
  "accounts": viewAccounts,
  "begin": viewBegin,
  "aux": viewAux,
  "options": viewOptions,
  "logs": viewLogs,
  "backup": viewBackup,
  "templates": viewTemplates,
  "payroll": viewPayroll,
  "claims": viewClaims,
};

/// 异步回调里的整页重绘守卫：请求返回时用户可能已切到别的视图，
/// 直接用旧视图覆盖会打断当前界面（表现为新视图一直停在"加载中…"）。
function rerenderView(id, main) {
  if (state.view === id) VIEWS[id](main);
}

let shellBuilt = false;

// 骨架只渲染一次；切换视图只更新 .main，不再重建 topbar/sidebar
// ---------------- 界面字号调节（四档循环 90/100/115/130%，localStorage 持久化） ----------------
// 字号全为 px 硬编码（rem 改造不现实）→ html.zoom 整体缩放；不支持的浏览器无害回退。
const UI_SCALES = [
  { v: 0.9, label: "小" },
  { v: 1, label: "标准" },
  { v: 1.15, label: "大" },
  { v: 1.3, label: "特大" },
];
function uiScaleIdx() {
  try {
    const s = parseFloat(localStorage.getItem("ui_scale"));
    const i = UI_SCALES.findIndex((x) => x.v === s);
    return i >= 0 ? i : 1;
  } catch (e) { return 1; }
}
function applyUiScale(idx) {
  const d = UI_SCALES[idx] || UI_SCALES[1];
  try { localStorage.setItem("ui_scale", String(d.v)); } catch (e) {}
  document.documentElement.style.zoom = String(d.v);
  const el = document.getElementById("ui-scale-btn");
  if (el) { el.textContent = `Aa ${d.label}`; el.title = `界面字号：${d.label}（点击切换）`; }
}
function cycleUiScale() {
  const next = (uiScaleIdx() + 1) % UI_SCALES.length;
  applyUiScale(next);
  toast(`界面字号：${UI_SCALES[next].label}`, "ok");
}
applyUiScale(uiScaleIdx()); // 脚本加载即应用，防登录前闪烁

// ---------------- 通知中心（铃铛 + 右侧抽屉）：待办实时聚合 + 审计动态 + 水位已读 ----------------
// 动态=审计日志按可见性过滤（AuditLog 权看全量，否则自己的操作）；已读=localStorage 水位
// （存服务端 now，同钟同格式保证字典序=时间序）；60s 轮询仅页面可见时执行。
let _ntTimer = null;
let _ntToastAt = 0;
function ntMark() { try { return localStorage.getItem("nt_mark") || ""; } catch (e) { return ""; } }
function ntSetMark(ts) { try { localStorage.setItem("nt_mark", ts); } catch (e) {} }
function ntBadge(n) {
  const el = document.getElementById("bell-n");
  if (!el) return;
  el.textContent = n > 99 ? "99+" : String(n);
  el.dataset.zero = n > 0 ? "0" : "1";
}
function ntPulse() {
  const b = document.getElementById("bell");
  if (!b) return;
  b.classList.add("pulse");
  setTimeout(() => b.classList.remove("pulse"), 1500);
}
async function ntPoll(first) {
  try {
    const r = await api(`/notices?since=${encodeURIComponent(ntMark())}`);
    const todoN = (r.todos || []).filter((t) => t.count > 0).length;
    const unread = (r.unread_events || 0) + todoN;
    const was = parseInt(document.getElementById("bell-n")?.textContent || "0", 10) || 0;
    ntBadge(unread);
    if (!first && unread > was) {
      ntPulse();
      const now = Date.now();
      if (now - _ntToastAt > 5 * 60 * 1000) {
        _ntToastAt = now;
        toast(`通知：新动态 ${r.unread_events} 条 · 待办 ${todoN} 项`, "ok");
      }
    }
    return r;
  } catch (e) { return null; }
}
function ntStart() {
  if (_ntTimer) return;
  ntPoll(true);
  _ntTimer = setInterval(() => { if (!document.hidden) ntPoll(false); }, 60000);
  document.addEventListener("visibilitychange", () => { if (!document.hidden) ntPoll(false); });
}
async function ntOpenDrawer() {
  const drawer = document.getElementById("nt-drawer");
  const mask = document.getElementById("nt-mask");
  if (!drawer) return;
  const r = await ntPoll(false);
  if (r) ntRender(r);
  drawer.classList.add("open");
  mask.classList.add("open");
}
function ntCloseDrawer() {
  document.getElementById("nt-drawer")?.classList.remove("open");
  document.getElementById("nt-mask")?.classList.remove("open");
}
function ntRender(r) {
  const body = document.getElementById("nt-body");
  if (!body) return;
  const todos = (r.todos || []).filter((t) => t.count > 0);
  let html = todos.length
    ? `<div class="nt-sec">我的待办（点击处理）</div>` + todos.map((t) => `
        <button class="nt-item" data-nt-view="${esc(t.view)}"><div class="nt-t"><span class="nt-dot"></span><b>${esc(t.label)}</b><span class="tag" style="margin-left:auto">${t.count}</span></div><div class="nt-m">${esc(t.domain)} · 直达处理页</div></div>`).join("")
    : "";
  // 动态按服务端时钟分组：今天 / 昨天 / 更早
  const dayOf = (ts) => (ts || "").slice(0, 10);
  const nowD = (r.now || "").slice(0, 10);
  const yd = new Date(nowD + "T00:00:00");
  yd.setDate(yd.getDate() - 1);
  const yD = `${yd.getFullYear()}-${String(yd.getMonth() + 1).padStart(2, "0")}-${String(yd.getDate()).padStart(2, "0")}`;
  const groups = { today: [], yday: [], older: [] };
  (r.events || []).forEach((ev) => {
    const d = dayOf(ev.ts);
    (d === nowD ? groups.today : d === yD ? groups.yday : groups.older).push(ev);
  });
  const G = (list, label) => list.length
    ? `<div class="nt-sec">${label}</div>` + list.map((ev) => `
        <div class="nt-item"><div class="nt-t"><span class="nt-dot"></span><b>${esc(ev.user || "系统")}</b> ${esc(ev.action)}<span class="muted" style="margin-left:auto;font-size:11px">${esc((ev.ts || "").slice(11, 16))}</span></div><div class="nt-m">${esc(ev.module)} · ${esc(ev.detail)}</div></div>`).join("")
    : "";
  html += G(groups.today, "今天") + G(groups.yday, "昨天") + G(groups.older, "更早");
  if (!html) html = `<div class="nt-empty">没有待办，也暂无动态</div>`;
  body.innerHTML = html;
  $all("[data-nt-view]", body).forEach((b) => b.onclick = () => {
    state.view = b.dataset.ntView;
    ntCloseDrawer();
    renderMain();
  });
}
async function ntMarkRead() {
  // 已读 = 水位推到服务端当前时间（notices 返回的 now 与日志同钟同格式）
  try {
    const r = await api(`/notices?since=${encodeURIComponent(ntMark())}`);
    if (r && r.now) {
      ntSetMark(r.now);
      ntBadge((r.todos || []).filter((t) => t.count > 0).length);
      toast("已全部标为已读", "ok");
      const body = document.getElementById("nt-body");
      if (body) $all(".nt-dot", body).forEach((d) => d.style.visibility = "hidden");
    }
  } catch (e) { toast(e.message, "err"); }
}
// ---------------- 通知中心 END ----------------

// ---------------- 列偏好（链7）：表格列显隐，localStorage 持久化 ----------------
// 隐藏走 CSS 类（table.pc-KEY.ch-N 的 nth-child 规则）——**数据异步后填的行自动继承**；
// key = 视图#表序号，重渲染后同 key 自动恢复；列少（<4）的表不挂菜单。
const COL_PREFS = {};
function colCls(key) { return "k" + key.replace(/[^a-zA-Z0-9]/g, "_"); }
function applyColPrefs(key, hidden) {
  COL_PREFS[key] = hidden;
  let st = document.getElementById("colpref-css");
  if (!st) {
    st = document.createElement("style");
    st.id = "colpref-css";
    document.head.appendChild(st);
  }
  st.textContent = Object.entries(COL_PREFS)
    .map(([k, idxs]) => {
      const c = colCls(k);
      return idxs
        .map(
          (i) =>
            `table.grid.pc-${c}.ch-${i} th:nth-child(${i + 1}),table.grid.pc-${c}.ch-${i} td:nth-child(${i + 1}){display:none}`
        )
        .join("");
    })
    .join("");
  const tbl = document.querySelector(`table.grid[data-colkey="${CSS.escape(key)}"]`);
  if (!tbl) return;
  const c = colCls(key);
  [...tbl.classList]
    .filter((cl) => cl.startsWith("pc-") || cl.startsWith("ch-"))
    .forEach((cl) => tbl.classList.remove(cl));
  if (hidden.length) {
    tbl.classList.add(`pc-${c}`);
    hidden.forEach((i) => tbl.classList.add(`ch-${i}`));
  }
}
function autoColPrefs() {
  $all("#main table.grid").forEach((tbl, i) => {
    const key = `${state.view}#${i}`;
    if (tbl.dataset.colkey) return;
    const ths = $all("thead th", tbl);
    if (ths.length < 4) return; // 列少不挂（菜单无价值）
    tbl.dataset.colkey = key;
    let hidden = [];
    try { hidden = JSON.parse(localStorage.getItem("colpref:" + key) || "[]"); } catch (e) {}
    applyColPrefs(key, hidden); // 注册规则并挂类（恢复历史偏好）
    const bar = document.createElement("div");
    bar.className = "col-bar";
    bar.innerHTML = `<button class="btn ghost sm" data-colbtn title="列显隐（持久记忆）">列▾</button><div class="col-menu" hidden></div>`;
    tbl.parentNode.insertBefore(bar, tbl);
    const menu = bar.querySelector(".col-menu");
    menu.innerHTML = ths
      .map(
        (th, ci) =>
          `<label><input type="checkbox" data-col="${ci}" ${hidden.includes(ci) ? "" : "checked"} />${esc(th.textContent.trim().replace(/[▲▼]/g, ""))}</label>`
      )
      .join("");
    menu.addEventListener("change", (e) => {
      const cb = e.target.closest("input[data-col]");
      if (!cb) return;
      const idx = parseInt(cb.dataset.col, 10);
      let cur = [];
      try { cur = JSON.parse(localStorage.getItem("colpref:" + key) || "[]"); } catch (e2) { cur = []; }
      if (cb.checked) cur = cur.filter((x) => x !== idx);
      else if (!cur.includes(idx)) cur.push(idx);
      cur.sort((a, b) => a - b);
      try { localStorage.setItem("colpref:" + key, JSON.stringify(cur)); } catch (e3) {}
      applyColPrefs(key, cur);
    });
    bar.querySelector("[data-colbtn]").addEventListener("click", (e) => {
      e.stopPropagation();
      menu.hidden = !menu.hidden;
    });
  });
}
// 点击列菜单外关闭所有菜单（全局注册一次；菜单内点击不受影响）
document.addEventListener("click", (e) => {
  if (e.target && e.target.closest && e.target.closest(".col-bar")) return;
  $all(".col-menu").forEach((m) => { m.hidden = true; });
});
// ---------------- 通用列头排序 + 列宽拖拽 ----------------
// 排序：table.grid 列头可点（自带 data-sort 的用户列表除外；操作/选择类列跳过）；
// 数字感知（去千分位比较），升/降切换，▲▼ 标记。
function autoColSort() {
  $all("#main table.grid").forEach((tbl) => {
    if (tbl.dataset.colsort) return;
    tbl.dataset.colsort = "1";
    const ths = [...tbl.querySelectorAll("thead th")];
    ths.forEach((th, idx) => {
      if (th.dataset.sort) return;
      const label = (th.textContent || "").trim();
      if (!label || /操作|选择|明细/.test(label)) return;
      th.classList.add("sortable");
      th.title = "点击排序";
      th.addEventListener("click", () => {
        const tb = tbl.tBodies[0];
        if (!tb) return;
        const asc = th.dataset.asc !== "1";
        ths.forEach((h) => { if (h !== th) { h.dataset.asc = ""; h.classList.remove("sort-asc", "sort-desc"); } });
        th.dataset.asc = asc ? "1" : "0";
        th.classList.toggle("sort-asc", asc);
        th.classList.toggle("sort-desc", !asc);
        const cellVal = (tr) => { const td = tr.cells[idx]; return td ? td.textContent.trim() : ""; };
        const num = (s) => { const v = parseFloat(String(s).replace(/[,\s¥%]/g, "")); return Number.isFinite(v) ? v : null; };
        const rows = [...tb.rows];
        rows.sort((a, b) => {
          const av = cellVal(a), bv = cellVal(b);
          const an = num(av), bn = num(bv);
          const r = an !== null && bn !== null ? an - bn : av.localeCompare(bv, "zh-Hans-CN");
          return asc ? r : -r;
        });
        rows.forEach((r) => tb.appendChild(r));
      });
    });
  });
}

// 列宽拖拽：拖 th 右缘调宽；localStorage 按 视图#表序号 持久化
function autoColResize() {
  $all("#main table.grid").forEach((tbl, i) => {
    if (tbl.dataset.colresize) return;
    tbl.dataset.colresize = "1";
    const key = `colw:${state.view}#${i}`;
    let widths = [];
    try { widths = JSON.parse(localStorage.getItem(key) || "[]"); } catch (e) {}
    const ths = [...tbl.querySelectorAll("thead th")];
    ths.forEach((th, ci) => {
      if (widths[ci]) { th.style.width = widths[ci] + "px"; th.style.minWidth = widths[ci] + "px"; }
      th.addEventListener("mousemove", (e) => {
        const r = th.getBoundingClientRect();
        th.style.cursor = e.clientX > r.right - 6 ? "col-resize" : "";
      });
      th.addEventListener("mousedown", (e) => {
        const r = th.getBoundingClientRect();
        if (e.clientX <= r.right - 6) return;
        e.preventDefault();
        const startX = e.clientX, startW = r.width;
        const move = (ev) => {
          const w = Math.max(48, Math.round(startW + ev.clientX - startX));
          th.style.width = w + "px"; th.style.minWidth = w + "px";
        };
        const up = () => {
          document.removeEventListener("mousemove", move);
          document.removeEventListener("mouseup", up);
          const arr = ths.map((h) => Math.round(h.getBoundingClientRect().width));
          try { localStorage.setItem(key, JSON.stringify(arr)); } catch (e2) {}
        };
        document.addEventListener("mousemove", move);
        document.addEventListener("mouseup", up);
      });
    });
  });
}
// 单元格省略号补 title：被 max-width 截断的单元格，悬停显示全文（只补一次）
function autoCellTitles() {
  $all("#main table.grid td").forEach((td) => {
    if (td.title) return;
    if (td.scrollWidth > td.clientWidth + 1) td.title = td.textContent.trim();
  });
}
// ---------------- 列偏好 END ----------------

// 单据行内流程徽标：按业务类型拉取流程实例状态，填充 [data-wftag="类型:id"] 占位
async function fillWfTags(root) {
  const els = $all("[data-wftag]", root || document);
  if (!els.length) return;
  await Promise.all(
    els.map(async (el) => {
      const parts = (el.dataset.wftag || "").split(":");
      if (parts.length !== 2) return;
      try {
        const f = await api(`/workflows/instance-for?biz_type=${encodeURIComponent(parts[0])}&id=${parts[1]}`);
        if (!f.found) { el.innerHTML = ""; return; }
        const label = { running: "审批中", approved: "已通过", rejected: "已驳回" }[f.status] || f.status;
        const ok = f.status === "approved" ? " ok" : "";
        el.innerHTML = `<span class="wf-tag${ok}">流程:${label}${f.status === "running" ? ` · ${esc(f.current_label)}` : ""}</span>`;
      } catch (e) { el.innerHTML = ""; }
    })
  );
}

function renderShell() {
  const u = session.user;
  const app = document.getElementById("app");
  const nav = NAV_ITEMS.filter((n) => !n.perm || can(n.perm));  const periodOpts = state.periods.map((p) => `<option value="${p}" ${p === state.current ? "selected" : ""}>${p}</option>`).join("");
  app.innerHTML = `
    <div class="app">
      <div class="topbar">
        <button class="hamburger" id="menu-btn" aria-label="打开菜单">☰</button>
        <span class="logo" id="logo-home" title="回到工作台">FinBook</span>
        <span class="who">${esc(u.display_name)}（${esc(u.role_label)}）</span>
        <select id="period-sel" title="会计期间">${periodOpts}</select>
        <span class="grow"></span>
        <button class="btn ghost sm" id="ui-scale-btn" title="界面字号（点击切换）" aria-label="界面字号">Aa 标准</button>
        <button class="btn ghost sm bell" id="bell" title="通知（待办与动态）" aria-label="通知">🔔<span class="bell-n" id="bell-n" data-zero="1">0</span></button>
        <button class="btn ghost sm" id="help-btn" title="快捷键与帮助（按 ? 唤起）" aria-label="快捷键与帮助">?</button>
        <button class="btn ghost sm" id="switch-book">切换账套</button>
        <button class="btn ghost sm" id="change-pwd">修改口令</button>
        <button class="btn ghost sm" id="logout">退出登录</button>
      </div>
      <div class="sidebar" id="sidebar">
        ${(() => {
          const pa = !!session.platformAdmin;
          const visible = nav.filter((n) => !(n.admin && !session.user.is_admin) && !(n.platform && !pa));
          let lastGroup = "";
          let html = "";
          for (const n of visible) {
            if (n.group && n.group !== lastGroup) {
              html += `<div class="group">${esc(n.group)}</div>`;
              lastGroup = n.group;
            }
            html += `<button class="nav-item ${n.id === state.view ? "active" : ""}" data-view="${n.id}"${n.id === state.view ? ' aria-current="page"' : ""}>${n.label}</button>`;
          }
          return html;
        })()}
      </div>
      <div class="side-mask" id="side-mask"></div>
      <div class="nt-mask" id="nt-mask"></div>
      <aside class="nt-drawer" id="nt-drawer">
        <div class="nt-head"><b>通知</b><span class="grow"></span>
          <button class="btn ghost sm" id="nt-allread">全部已读</button>
          <button class="btn ghost sm" id="nt-close">✕</button></div>
        <div class="nt-body" id="nt-body"><div class="nt-empty">加载中…</div></div>
      </aside>
      <div class="main" id="main"></div>
    </div>`;

  function closeMenu() { document.body.classList.remove("menu-open"); }
  function toggleMenu() { document.body.classList.toggle("menu-open"); }

  // 点击 logo 回到仪表盘（不再是无行为的死元素）
  $("#logo-home").addEventListener("click", () => { state.view = "dashboard"; closeMenu(); renderMain(); });
  $("#menu-btn").addEventListener("click", toggleMenu);
  $("#help-btn").addEventListener("click", openShortcuts);
  $("#side-mask").addEventListener("click", closeMenu);
  // 事件委托：nav-item 只在 sidebar 容器上绑一次
  $(".sidebar").addEventListener("click", (e) => {
    const btn = e.target.closest(".nav-item");
    if (!btn) return;
    state.view = btn.dataset.view;
    $all(".nav-item").forEach((b) => b.classList.toggle("active", b.dataset.view === state.view));
    closeMenu();
    renderMain();
  });
  $("#period-sel").addEventListener("change", async (e) => {
    state.current = e.target.value;
    try { await api("/period", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ ymm: ymm(e.target.value) }) }); } catch (err) {}
    renderMain();
  });
  $("#logout").addEventListener("click", logout);
  $("#switch-book").addEventListener("click", async () => {
    shellBuilt = false;
    state.bookKey = "";
    await showBookPicker();
  });
  $("#change-pwd").addEventListener("click", () => openChangePwd(false));
  // 界面字号四档循环 + 通知中心：铃铛/抽屉/轮询
  $("#ui-scale-btn").addEventListener("click", cycleUiScale);
  applyUiScale(uiScaleIdx()); // 刷新按钮档位文字
  $("#bell").addEventListener("click", ntOpenDrawer);
  $("#nt-close").addEventListener("click", ntCloseDrawer);
  $("#nt-mask").addEventListener("click", ntCloseDrawer);
  $("#nt-allread").addEventListener("click", ntMarkRead);
  document.addEventListener("keydown", (e) => { if (e.key === "Escape") ntCloseDrawer(); });
  ntStart();
  shellBuilt = true;
}

function render() {
  // session.user 有 perms = 已进入账套（PublicUser）；只有平台身份（PlatformUser）时停在账套选择页
  if (!session.user) { shellBuilt = false; showLogin(); return; }
  if (!session.user.perms) { shellBuilt = false; showBookPicker(); return; }
  if (!shellBuilt) renderShell();
  renderMain();
}

function renderMain() {
  // 换一个新 #main 节点：旧视图（多为 async，先 await 后写 innerHTML）的迟到回调
  // 只会写入已卸载的旧节点，不会把用户刚切到的新视图覆盖掉（修复异步渲染竞态）。
  const old = document.getElementById("main");
  let main = old;
  if (old && old.parentNode) {
    main = document.createElement("div");
    main.id = "main";
    main.className = old.className;
    old.parentNode.replaceChild(main, old);
  }
  closeAllModals(); // 切视图时关闭遗留弹窗（栈式关闭下不再整体清空）
  const fn = VIEWS[state.view] || viewDashboard;
  const r = fn(main);
  // 列偏好：同步表结构先挂一次（async 表由 api() 回调补挂）
  try { autoColPrefs(); } catch (e) {}
  try { autoColSort(); } catch (e) {}
  try { autoColResize(); } catch (e) {}
  try { autoCellTitles(); } catch (e) {}
  return r;
}

async function logout() {
  try { await api("/logout", { method: "POST" }); } catch (e) {}
  closeAllModals();
  session.user = null;
  session.platformAdmin = false;
  render();
}

// ===========================================================================
// 我的工作台：账套状态（凭证数等固定卡）+ 按岗位权限动态拼装的
// 业务卡片 / 我的待办 / 多期趋势（GET /api/workbench，无新权限位）
// ===========================================================================
async function viewDashboard(main) {
  main.innerHTML = `<h2>我的工作台</h2><div class="muted">加载中…</div>`;
  let d;
  try { d = await api("/dashboard"); } catch (e) { main.innerHTML = `<h2>我的工作台</h2><div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; return; }
  let status = { admin_set: false };
  try { status = await api("/setup/status"); } catch (e) {}
  let wb = null;
  try {
    wb = await api(`/workbench?periods=${Number(state.wbPeriods) || 12}`);
  } catch (e) { toast(`工作台数据加载失败：${e.message}`, "err"); }
  const adminBanner = status.admin_set
    ? `<div class="banner set">✅ 管理员账号已设定</div>`
    : `<div class="banner unset">🔧 管理员账号未设定 —— 首次成功登录的账号将自动成为系统管理员。</div>`;
  const nSel = `<label style="font-size:12px;margin-left:auto">趋势期数 <select id="wb-n">${
    [6, 12, 24].map((n) => `<option value="${n}" ${Number(state.wbPeriods || 12) === n ? "selected" : ""}>${n} 期</option>`).join("")
  }</select></label>`;
  const group = (arr) => { const m = {}; (arr || []).forEach((x) => { (m[x.domain] = m[x.domain] || []).push(x); }); return m; };
  let wbHtml = "";
  if (wb) {
    const cg = group(wb.cards);
    const tg = group(wb.trends);
    const cardsHtml = Object.keys(cg).map((dom) => `
      <div class="wb-sec"><h4 style="margin:14px 0 6px">${esc(dom)}</h4>
        <div class="cards">${cg[dom].map((c) => `<div class="card"><div class="k">${esc(c.label)}</div><div class="v">${esc(c.value)}${c.unit === "元" || c.unit === "件" ? `<span style="font-size:12px;font-weight:400;opacity:.65"> ${esc(c.unit)}</span>` : ""}</div></div>`).join("")}</div>
      </div>`).join("");
    const todos = wb.todos || [];
    const todoHtml = todos.length ? `
      <div class="wb-sec"><h4 style="margin:14px 0 6px">我的待办</h4>
        <div style="display:flex;flex-wrap:wrap;gap:8px">${todos.map((t) => `
          <button class="btn ${t.count > 0 ? "primary" : "ghost"}" data-wb-go="${esc(t.view)}" ${t.count > 0 ? "" : "disabled"} style="display:inline-flex;gap:8px;align-items:center">${esc(t.label)}<b>${t.count}</b></button>`).join("")}
        </div>
      </div>` : "";
    const trendHtml = Object.keys(tg).map((dom) => `
      <div class="wb-sec"><h4 style="margin:14px 0 6px">${esc(dom)}</h4>
        <div style="display:grid;grid-template-columns:repeat(auto-fill,minmax(400px,1fr));gap:12px">
          ${tg[dom].map((t) => `<div class="panel" style="margin:0;padding:10px 12px">
            <div style="display:flex;align-items:center;gap:8px"><b style="font-size:13px">${esc(t.title)}</b><span class="muted" style="font-size:12px">单位：${esc(t.unit)}</span></div>
            ${lineChartSvg(t.periods, t.series.map((s) => ({ name: s.name, color: s.color, values: s.points })), 190)}
          </div>`).join("")}
        </div>
      </div>`).join("");
    wbHtml = `<div class="toolbar" style="border:0;padding:6px 0">${nSel}</div>${cardsHtml}${todoHtml}${trendHtml}`;
  }
  const firstRun = Number(d.vouchers) === 0 ? `
    <div class="panel first-run">
      <div class="fr-title">三步开始记账</div>
      <div class="fr-steps">
        <div class="fr-step"><b>1</b> 维护科目与期初（基础资料 → 会计科目 / 期初建账；旧账套可「数据导入」迁移）</div>
        <div class="fr-step"><b>2</b> 录入第一张凭证（凭证 → 记账凭证 → 新增凭证）</div>
        <div class="fr-step"><b>3</b> 期末处理（结转损益 → 记账 → 结账）</div>
      </div>
      <div class="fr-actions">
        <button class="btn primary sm" data-fr-go="vouchers">去录凭证</button>
        <button class="btn ghost sm" data-fr-go="imports">数据导入</button>
        <button class="btn ghost sm" data-fr-go="begin">期初建账</button>
      </div>
    </div>` : "";
  main.innerHTML = `
    <h2>我的工作台</h2>
    ${adminBanner}
    ${firstRun}
    <div class="cards">
      <div class="card"><div class="k">公司名称</div><div class="v" style="font-size:16px">${esc(d.company || "—")}</div></div>
      <div class="card"><div class="k">当前会计期间</div><div class="v">${esc(d.current_period)}</div></div>
      <div class="card"><div class="k">已结账至</div><div class="v">${esc(d.closed_upto || "未结账")}</div></div>
    </div>
    <div class="cards" style="margin-top:14px">
      <div class="card"><div class="k">凭证数</div><div class="v">${esc(d.vouchers)}</div></div>
      <div class="card"><div class="k">分录数</div><div class="v">${esc(d.entries)}</div></div>
      <div class="card"><div class="k">科目数</div><div class="v">${esc(d.accounts)}</div></div>
    </div>
    ${wbHtml}`;
  if ($("#wb-n")) $("#wb-n").onchange = (e) => { state.wbPeriods = Number(e.target.value); viewDashboard(main); };
  $all("[data-wb-go]").forEach((b) => b.onclick = () => { state.view = b.dataset.wbGo; renderMain(); });
  $all("[data-fr-go]").forEach((b) => b.onclick = () => { state.view = b.dataset.frGo; renderMain(); });
}

// ===========================================================================
// 管理员 · 账目总览（只读视角，仅系统管理员可见）
// ===========================================================================

// 金额字符串（"1,234.56" / "-1,234.56"）转数字
function moneyNum(s) {
  const n = parseFloat(String(s == null ? "0" : s).replace(/[^0-9.-]/g, ""));
  return Number.isFinite(n) ? n : 0;
}
function moneyFmt(n) {
  const neg = n < 0;
  const a = Math.abs(n).toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 });
  return neg ? "-" + a : a;
}
function pctFmt(n) { return n.toLocaleString("en-US", { maximumFractionDigits: 1 }) + "%"; }

// 折线图（SVG，纯原生实现）。series: [{name, color, values:number[], anomalies:bool[]}]
function lineChartSvg(labels, series, height) {
  const W = 760, H = height || 250;
  const padL = 62, padR = 14, padT = 16, padB = 30;
  const iw = W - padL - padR, ih = H - padT - padB;
  const n = labels.length;
  if (!n) return "";
  let min = 0, max = 0;
  for (const s of series) for (const v of s.values) { if (v < min) min = v; if (v > max) max = v; }
  if (max === min) max = min + 1;
  const span = max - min || 1;
  const x = (i) => padL + (n === 1 ? iw / 2 : (iw * i) / (n - 1));
  const y = (v) => padT + ih - ((v - min) / span) * ih;
  const y0 = min < 0 && max > 0 ? y(0) : null;

  // 网格与 Y 轴刻度（4 档）
  let grid = "", yticks = "";
  for (let k = 0; k <= 4; k++) {
    const v = min + (span * k) / 4;
    const gy = y(v);
    grid += `<line x1="${padL}" y1="${gy}" x2="${W - padR}" y2="${gy}" stroke="var(--z-200)" stroke-width="1"/>`;
    yticks += `<text x="${padL - 8}" y="${gy + 4}" text-anchor="end" class="ctick">${moneyFmt(v)}</text>`;
  }
  // X 轴标签（最多显示 12 个）
  let xlabels = "";
  const step = Math.ceil(n / 12);
  for (let i = 0; i < n; i += step) {
    xlabels += `<text x="${x(i)}" y="${H - 8}" text-anchor="middle" class="ctick">${esc(labels[i])}</text>`;
  }

  let lines = "", dots = "";
  for (const s of series) {
    const pts = s.values.map((v, i) => `${x(i).toFixed(1)},${y(v).toFixed(1)}`).join(" ");
    lines += `<polyline points="${pts}" fill="none" stroke="${s.color}" stroke-width="2.2" stroke-linejoin="round" stroke-linecap="round"/>`;
    s.values.forEach((v, i) => {
      dots += `<circle cx="${x(i).toFixed(1)}" cy="${y(v).toFixed(1)}" r="${s.anomalies && s.anomalies[i] ? 4.5 : 2.6}" fill="${s.anomalies && s.anomalies[i] ? "#dc2626" : s.color}"/>`;
    });
  }
  const zeroAxis = y0 != null ? `<line x1="${padL}" y1="${y0.toFixed(1)}" x2="${W - padR}" y2="${y0.toFixed(1)}" stroke="var(--z-400)" stroke-width="1" stroke-dasharray="4 3"/>` : "";

  return `<svg viewBox="0 0 ${W} ${H}" preserveAspectRatio="none" class="chart">${grid}${zeroAxis}${lines}${dots}${yticks}${xlabels}</svg>`;
}

// 分组柱状图（当月发生额）。groups: [{name, color, values, anomalies}]
function barChartSvg(labels, groups, height) {
  const W = 760, H = height || 250;
  const padL = 62, padR = 14, padT = 16, padB = 30;
  const iw = W - padL - padR, ih = H - padT - padB;
  const n = labels.length;
  if (!n) return "";
  let min = 0, max = 0;
  for (const g of groups) for (const v of g.values) { if (v < min) min = v; if (v > max) max = v; }
  if (max === min) max = min + 1;
  const span = max - min || 1;
  const y = (v) => padT + ih - ((v - min) / span) * ih;
  const y0 = min < 0 && max > 0 ? y(0) : null;

  let grid = "", yticks = "";
  for (let k = 0; k <= 4; k++) {
    const v = min + (span * k) / 4;
    const gy = y(v);
    grid += `<line x1="${padL}" y1="${gy}" x2="${W - padR}" y2="${gy}" stroke="var(--z-200)" stroke-width="1"/>`;
    yticks += `<text x="${padL - 8}" y="${gy + 4}" text-anchor="end" class="ctick">${moneyFmt(v)}</text>`;
  }
  const g = groups.length;
  const slot = iw / n;
  const bw = Math.min(18, (slot * 0.7) / g);
  let xlabels = "";
  const step = Math.ceil(n / 12);
  for (let i = 0; i < n; i += step) {
    xlabels += `<text x="${barX(i)}" y="${H - 8}" text-anchor="middle" class="ctick">${esc(labels[i])}</text>`;
  }

  let bars = "";
  groups.forEach((grp, gi) => {
    grp.values.forEach((v, i) => {
      const cx = barX(i) + (gi - (g - 1) / 2) * (bw + 2);
      const vy = y(v), base = y0 != null ? y0 : y(0);
      const h = Math.abs(base - vy);
      bars += `<rect x="${(cx - bw / 2).toFixed(1)}" y="${Math.min(vy, base).toFixed(1)}" width="${bw.toFixed(1)}" height="${Math.max(h, 1).toFixed(1)}" fill="${grp.color}" rx="1.5"/>`;
      if (grp.anomalies && grp.anomalies[i]) {
        bars += `<path d="M ${cx.toFixed(1)} ${(vy - 7).toFixed(1)} l 4 7 l -8 0 z" fill="#dc2626"/>`;
      }
    });
  });

  function barX(i) { return padL + slot * (i + 0.5); }
  const zeroAxis = y0 != null ? `<line x1="${padL}" y1="${y0.toFixed(1)}" x2="${W - padR}" y2="${y0.toFixed(1)}" stroke="var(--z-400)" stroke-width="1" stroke-dasharray="4 3"/>` : "";
  return `<svg viewBox="0 0 ${W} ${H}" preserveAspectRatio="none" class="chart">${grid}${zeroAxis}${bars}${yticks}${xlabels}</svg>`;
}

// 指标内因：横向条形列表（带环比变化与占比）
function driverBars(items, signed) {
  const maxAbs = Math.max(1, ...items.map((it) => Math.abs(moneyNum(it.amount))));
  return items.map((it) => {
    const cur = moneyNum(it.amount);
    const prev = moneyNum(it.prev_amount);
    const w = Math.round((Math.abs(cur) / maxAbs) * 100);
    const color = signed ? (cur < 0 ? "var(--err)" : "var(--ok)") : "var(--primary)";
    let change = "";
    if (prev !== 0) {
      const pct = ((cur - prev) / Math.abs(prev)) * 100;
      const dir = pct > 0.5 ? "▲" : pct < -0.5 ? "▼" : "—";
      change = `<span class="drv-chg ${pct > 0.5 ? "up" : pct < -0.5 ? "down" : ""}">${dir} ${pctFmt(Math.abs(pct))}</span>`;
    } else if (cur !== 0) {
      change = `<span class="drv-chg up">▲ 新增</span>`;
    }
    return `<div class="drv-row">
      <div class="drv-head"><span class="drv-name">${esc(it.name)}</span>
        <span class="drv-amt">${moneyFmt(cur)}</span>${change}</div>
      <div class="drv-bar"><i style="width:${w}%;background:${color}"></i></div>
    </div>`;
  }).join("");
}

// 财务指标解读（阀值判断，给出好/中/差）
function ratioVerdict(key, v) {
  switch (key) {
    case "current_ratio": return v >= 2 ? "good" : v >= 1 ? "warn" : "bad";
    case "quick_ratio": return v >= 1 ? "good" : v >= 0.5 ? "warn" : "bad";
    case "debt_ratio": return v <= 0.5 ? "good" : v <= 0.7 ? "warn" : "bad";
    case "gross_margin": return v >= 0.3 ? "good" : v >= 0.1 ? "warn" : "bad";
    case "net_margin": return v >= 0.1 ? "good" : v >= 0 ? "warn" : "bad";
    case "roe": return v >= 0.1 ? "good" : v >= 0 ? "warn" : "bad";
    case "roa": return v >= 0.05 ? "good" : v >= 0 ? "warn" : "bad";
    default: return "warn";
  }
}
const VERDICT_LABEL = { good: "健康", warn: "关注", bad: "预警" };

async function viewOverview(main) {
  main.innerHTML = `<h2>账目总览</h2><div class="muted">加载中…</div>`;
  let d;
  try { d = await api(`/overview?period=${encodeURIComponent(state.current || "")}`); } catch (e) {
    main.innerHTML = `<h2>账目总览</h2><div class="muted" style="color:var(--err)">${esc(e.message)}</div>`;
    return;
  }
  const t = d.totals || {};
  const a = d.analysis || {};
  const stMap = { draft: ["未记账", "warn"], audited: ["已审核", "warn"], posted: ["已记账", "ok"], void: ["已作废", "err"] };
  const card = (k, v, style) => `<div class="card"><div class="k">${k}</div><div class="v" ${style ? `style="${style}"` : ""}>${v}</div></div>`;

  // ---- 走势图数据 ----
  const trend = a.trend || [];
  const labels = trend.map((x) => String(x.period).slice(5) + "月");
  const cumSeries = [
    { name: "累计营业收入", color: "#2563eb", values: trend.map((x) => moneyNum(x.cum_revenue)), anomalies: trend.map((x) => x.anomaly_revenue) },
    { name: "累计营业成本", color: "#f59e0b", values: trend.map((x) => moneyNum(x.cum_cost)), anomalies: trend.map((x) => x.anomaly_cost) },
    { name: "累计净利润", color: "#16a34a", values: trend.map((x) => moneyNum(x.cum_net_profit)), anomalies: trend.map((x) => x.anomaly_net_profit) },
  ];
  const monthGroups = [
    { name: "营业收入", color: "#2563eb", values: trend.map((x) => moneyNum(x.revenue)), anomalies: trend.map((x) => x.anomaly_revenue) },
    { name: "营业成本", color: "#f59e0b", values: trend.map((x) => moneyNum(x.cost)), anomalies: trend.map((x) => x.anomaly_cost) },
    { name: "净利润", color: "#16a34a", values: trend.map((x) => moneyNum(x.net_profit)), anomalies: trend.map((x) => x.anomaly_net_profit) },
  ];
  const legend = (series) => series.map((s) =>
    `<span class="lg-item"><i style="background:${s.color}"></i>${esc(s.name)}</span>`).join("");

  main.innerHTML = `
    <h2>账目总览</h2>
    <p class="muted" style="margin:0 0 12px">
      ${esc(d.company || "")} · 期间 ${esc(d.period || "")} · 已结账至 ${esc(d.closed_upto || "未结账")}
      —— 管理员只读视角：查看账目全貌，不做录入；录入请用「记账凭证」，明细见左侧各查询页。
    </p>
    <div class="cards">
      ${card("资产总额", t.total_asset || "—", "font-size:18px")}
      ${card("负债总额", t.total_liab || "—", "font-size:18px")}
      ${card("所有者权益", t.equity || "—", "font-size:18px")}
      ${card("净利润（年初至今）", t.net_profit || "—", "font-size:18px")}
    </div>

    <div class="panel" style="margin-top:14px">
      <div class="chart-head"><b>财务走势 · 年初至今累计</b><span class="lg">${legend(cumSeries)}</span></div>
      <div class="chart-box">${lineChartSvg(labels, cumSeries, 230)}</div>
      <div class="chart-note">红线圆点 = 异常月份（偏离年内均值 ±2σ 或出现亏损）；折线为 1 月起累计值。</div>
    </div>
    <div class="panel" style="margin-top:14px">
      <div class="chart-head"><b>当月发生额（逐月对比）</b><span class="lg">${legend(monthGroups)}</span></div>
      <div class="chart-box">${barChartSvg(labels, monthGroups, 230)}</div>
      <div class="chart-note">红三角 = 异常月份；柱状为各月发生额，便于发现突增突减与亏损月。</div>
    </div>

    ${(a.anomaly_notes || []).length ? `<div class="panel warn-panel" style="margin-top:14px">
      <b>⚠ 异常提示</b>
      ${(a.anomaly_notes || []).slice(0, 6).map((n) => `<div class="anom">${esc(n)}</div>`).join("")}
      ${(a.anomaly_notes || []).length > 6 ? `<div class="muted">…共 ${a.anomaly_notes.length} 条</div>` : ""}
    </div>` : ""}

    <div class="grid-3" style="margin-top:14px">
      <div class="panel">
        <h4>营业收入构成（本月 vs 上月）</h4>
        ${driverBars(a.revenue_drivers || [], false) || `<div class="muted">暂无数据</div>`}
      </div>
      <div class="panel">
        <h4>营业成本构成（本月 vs 上月）</h4>
        ${driverBars(a.cost_drivers || [], false) || `<div class="muted">暂无数据</div>`}
      </div>
      <div class="panel">
        <h4>净利润构成（利润表口径）</h4>
        ${driverBars(a.profit_drivers || [], true) || `<div class="muted">暂无数据</div>`}
      </div>
    </div>

    <div class="panel" style="margin-top:14px">
      <b>财务状况分析</b>
      <div class="ratio-grid">
        ${(a.ratios || []).map((r) => {
          const verdict = ratioVerdict(r.key, parseFloat(r.value));
          return `<div class="ratio-card ${verdict}">
            <div class="rc-name">${esc(r.name)}</div>
            <div class="rc-val">${esc(r.display)}</div>
            <div class="rc-tag ${verdict}">${VERDICT_LABEL[verdict]}</div>
            <div class="rc-formula">${esc(r.formula)}</div>
          </div>`;
        }).join("") || `<div class="muted">暂无指标</div>`}
      </div>
    </div>

    <div class="cards" style="margin-top:14px">
      ${card("凭证总数（全账套）", esc(d.vouchers))}
      ${card("当期未记账", esc(d.unposted))}
      ${card("当期已记账", esc(d.posted))}
      ${card("科目数（全账套）", esc(d.accounts))}
    </div>
    <div class="cards" style="margin-top:14px">
      ${card("进项发票价税合计", `${(d.invoice_in && d.invoice_in.amount_tax) || "0.00"}（${(d.invoice_in && d.invoice_in.count) || 0} 张）`)}
      ${card("销项发票价税合计", `${(d.invoice_out && d.invoice_out.amount_tax) || "0.00"}（${(d.invoice_out && d.invoice_out.count) || 0} 张）`)}
    </div>
    <div class="toolbar" style="margin-top:14px">
      <span class="muted">科目表维护：</span>
      <button class="btn ghost sm" id="ov-fill">补齐新版科目表</button>
      <span class="muted">内置模板共 199 个科目；旧账套一键补入缺少的科目，不影响已有科目。</span>
    </div>
    <div class="panel" style="margin-top:14px;padding:0;overflow:hidden">
      <table class="grid"><thead><tr>
        <th>期间</th><th>日期</th><th>凭证号</th><th>摘要</th><th class="num">借方</th><th class="num">贷方</th><th>状态</th><th>制单</th>
      </tr></thead><tbody>
        ${(d.recent || []).length ? d.recent.map((v) => {
          const s = stMap[v.status] || [v.status_label, ""];
          return `<tr><td>${esc(v.period)}</td><td>${esc(v.date)}</td><td>${esc(v.voucher_no)}</td><td>${esc(v.summary)}</td><td class="num">${esc(v.debit_total)}</td><td class="num">${esc(v.credit_total)}</td><td><span class="tag ${s[1]}">${esc(s[0])}</span></td><td>${esc(v.prepared_by)}</td></tr>`;
        }).join("") : `<tr><td colspan="8" class="muted" style="text-align:center;padding:18px">暂无凭证</td></tr>`}
      </tbody></table>
    </div>`;
  const fillBtn = $("#ov-fill", main);
  if (fillBtn) fillBtn.addEventListener("click", async () => {
    try {
      const r = await api("/accounts/fill-defaults", { method: "POST" });
      toast(r.inserted > 0 ? `已补入 ${r.inserted} 个科目，当前共 ${r.total} 个` : `科目表已完整（共 ${r.total} 个）`, "ok");
      viewOverview(main);
    } catch (e) { toast(e.message, "err"); }
  });
}

// ===========================================================================
// 凭证
// ===========================================================================
async function ensureAccounts() {
  if (!state.accounts) {
    try { state.accounts = await api("/accounts"); } catch (e) { state.accounts = []; }
  }
  return state.accounts;
}
function accountOptions(sel) {
  const list = accountListSorted();
  return `<span class="acct-pick"><input class="acct-q" placeholder="🔍" title="输入编码/名称快速过滤（Enter 选中首条，↓ 进下拉）" /><select class="acct-sel">${list.map((a) => `<option value="${esc(a.code)}">${esc(a.code)} ${esc(a.name)}</option>`).join("")}</select></span>`;
}
// 科目列表按「最近使用」置顶（localStorage 记最近 12 个编码）
function acctMru() {
  try { return JSON.parse(localStorage.getItem("acct-mru") || "[]"); } catch (e) { return []; }
}
function accountListSorted() {
  const list = state.accounts || [];
  const mru = acctMru();
  if (!mru.length) return list;
  const rank = new Map(mru.map((c, i) => [c, i]));
  return [...list].sort((a, b) => {
    const ra = rank.has(a.code) ? rank.get(a.code) : 999;
    const rb = rank.has(b.code) ? rank.get(b.code) : 999;
    return ra - rb;
  });
}
// 科目选择器：🔍 过滤（编码/名称包含）+ 最近使用记录（原生 select 保留 → 兼容 selectOption 与既有 onchange）
document.addEventListener("input", (e) => {
  const q = e.target.closest && e.target.closest(".acct-q");
  if (!q) return;
  const pick = q.closest(".acct-pick");
  const sel = pick && pick.querySelector("select.acct-sel");
  if (!sel) return;
  const kw = q.value.trim().toLowerCase();
  const list = (state.accounts || []).filter((a) => !kw || a.code.toLowerCase().includes(kw) || (a.name || "").toLowerCase().includes(kw));
  const cur = sel.value;
  sel.innerHTML = list.length
    ? list.map((a) => `<option value="${esc(a.code)}">${esc(a.code)} ${esc(a.name)}</option>`).join("")
    : `<option value="">无匹配</option>`;
  if (list.some((a) => a.code === cur)) sel.value = cur;
  else if (list.length) { sel.value = list[0].code; sel.dispatchEvent(new Event("change", { bubbles: true })); }
});
document.addEventListener("keydown", (e) => {
  const q = e.target.closest && e.target.closest(".acct-q");
  if (!q) return;
  if (e.key === "Enter") {
    e.preventDefault();
    const sel = q.closest(".acct-pick").querySelector("select.acct-sel");
    if (sel && sel.value) sel.dispatchEvent(new Event("change", { bubbles: true }));
  } else if (e.key === "ArrowDown") {
    e.preventDefault();
    const sel = q.closest(".acct-pick").querySelector("select.acct-sel");
    if (sel) sel.focus();
  }
});
// 最近使用：任何 acct-sel 变更都记一笔
document.addEventListener("change", (e) => {
  const sel = e.target.closest && e.target.closest("select.acct-sel");
  if (!sel || !sel.value) return;
  try {
    const mru = acctMru().filter((x) => x !== sel.value);
    mru.unshift(sel.value);
    localStorage.setItem("acct-mru", JSON.stringify(mru.slice(0, 12)));
  } catch (err) {}
});

async function viewVouchers(main) {
  main.innerHTML = `
    <h2>记账凭证</h2>
    <div class="toolbar">
      ${can("voucher_new") ? `<button class="btn sm" id="new-v">新增凭证</button>` : ""}
      <input id="v-q" placeholder="摘要 / 凭证号 / 科目" style="width:200px" />
      <select id="v-status">
        <option value="">全部状态</option>
        <option value="draft">未记账</option>
        <option value="audited">已审核</option>
        <option value="posted">已记账</option><option value="void">已作废</option>
      </select>
      <button class="btn ghost sm" id="v-refresh">查询</button>
      ${can("voucher_edit") ? `<button class="btn ghost sm" id="v-renumber">重排断号</button>` : ""}
      ${can("voucher_post") ? `<button class="btn ghost sm" id="v-batch">批量记账</button>` : ""}
      ${can("report") ? `<button class="btn ghost sm" id="v-printform">凭证套打</button>` : ""}
      ${can("export") ? `<button class="btn ghost sm" id="v-export">导出 CSV</button>` : ""}
      <span class="spacer"></span>
      <span class="muted">期间：${esc(state.current || "")}</span>
    </div>
    <div class="panel"><table class="grid" id="v-table"><thead><tr>
      ${can("voucher_post") ? `<th style="width:26px"><input type="checkbox" id="v-all" title="全选未记账" /></th>` : ""}
      <th>期间</th><th>日期</th><th>凭证号</th><th>摘要</th><th class="num">借方</th><th class="num">贷方</th><th>状态</th><th>制单</th><th></th>
    </tr></thead><tbody><tr><td colspan="${can("voucher_post") ? 10 : 9}" class="muted">加载中…</td></tr></tbody></table></div>`;
  if (can("voucher_new")) $("#new-v").addEventListener("click", () => openVoucherEditor(null));
  $("#v-refresh").addEventListener("click", () => loadVouchers());
  $("#v-q").addEventListener("keydown", (e) => { if (e.key === "Enter") loadVouchers(); });
  if ($("#v-all")) $("#v-all").addEventListener("change", (e) => { $all(".v-sel").forEach((c) => c.checked = e.target.checked); });
  if ($("#v-batch")) $("#v-batch").addEventListener("click", batchPost);
  if ($("#v-printform")) $("#v-printform").addEventListener("click", () => {
    window.open(`/api/vouchers/print-form?period=${encodeURIComponent(state.current || "")}`, "_blank");
  });
  if ($("#v-export")) $("#v-export").addEventListener("click", () => {
    const qs = new URLSearchParams({ period: (state.current || "").replace("-", ""), q: $("#v-q").value, status: $("#v-status").value });
    window.open(`/api/export/vouchers?${qs.toString()}`, "_blank");
  });
  if ($("#v-renumber")) $("#v-renumber").addEventListener("click", async () => {
    if (!(await confirmDialog(`将当前期间「记」字凭证的凭证号重排为连续？`, true))) return;
    try {
      const r = await api("/vouchers/renumber", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ period: ymm(state.current || ""), word: "记" }) });
      toast(`已重排 ${r.renumbered} 张凭证`, "ok"); loadVouchers();
    } catch (e) { toast(e.message, "err"); }
  });
  await ensureAccounts();
  loadVouchers();
}

async function loadVouchers() {
  const tb = $("#v-table tbody");
  const q = $("#v-q").value.trim();
  const st = $("#v-status").value;
  let url = `/vouchers?period=${encodeURIComponent(state.current || "")}`;
  if (q) url += `&q=${encodeURIComponent(q)}`;
  if (st) url += `&status=${st}`;
  let rows;
  try { rows = await api(url); } catch (e) { tb.innerHTML = `<tr><td colspan="${can("voucher_post") ? 10 : 9}" style="color:var(--err)">${esc(e.message)} <button class="btn ghost sm" id="v-retry">重试</button></td></tr>`; const rb = $("#v-retry", tb); if (rb) rb.addEventListener("click", loadVouchers); return; }
  if (!rows.length) {
    tb.innerHTML = `<tr><td colspan="9"><div class="empty-state">
      <div class="es-title">还没有凭证</div>
      <div class="es-hint">点击「新增凭证」录入第一张；已有旧账套数据可到「数据导入」迁移凭证。</div>
      <div class="es-actions"><button class="btn primary sm" id="v-empty-new">新增凭证</button><button class="btn ghost sm" id="v-empty-import">去导入</button></div>
    </div></td></tr>`;
    const b1 = $("#v-empty-new");
    if (b1) b1.onclick = () => openVoucherEditor(null);
    const b2 = $("#v-empty-import");
    if (b2) b2.onclick = () => { state.view = "imports"; renderMain(); };
    return;
  }
  const stMap = { draft: ["未记账", "warn"], audited: ["已审核", "warn"], posted: ["已记账", "ok"], void: ["已作废", "err"] };
  tb.innerHTML = rows.map((v) => {
    const s = stMap[v.status] || [v.status_label, ""];
    const selectable = can("voucher_post") && (v.status === "draft" || v.status === "audited");
    return `<tr>
      ${can("voucher_post") ? `<td>${selectable ? `<input type="checkbox" class="v-sel" data-id="${v.id}" />` : ""}</td>` : ""}
      <td>${esc(v.period)}</td><td>${esc(v.date)}</td><td>${esc(v.voucher_no)}</td>
      <td>${esc(v.summary)}</td><td class="num">${esc(v.debit_total)}</td><td class="num">${esc(v.credit_total)}</td>
      <td><span class="tag ${s[1]}">${esc(s[0])}</span></td><td>${esc(v.prepared_by)}</td>
      <td class="row-actions"><button class="btn ghost sm" data-edit="${v.id}">打开</button></td>
    </tr>`;
  }).join("");
  $all("[data-edit]").forEach((b) => b.addEventListener("click", () => openVoucherEditor(parseInt(b.dataset.edit, 10))));
}

async function batchPost() {
  const ids = $all(".v-sel:checked").map((c) => parseInt(c.dataset.id, 10));
  if (!ids.length) { toast("请先勾选未记账的凭证", "err"); return; }
  try {
    const r = await api("/vouchers/batch-post", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ ids }) });
    const errs = r.errors || [];
    if (r.ok > 0 && errs.length === 0) { toast(`已记账 ${r.ok} 张`, "ok"); }
    else if (r.ok > 0) { toast(`已记账 ${r.ok} 张，失败 ${errs.length} 张`, "warn"); errs.slice(0, 5).forEach((e) => toast(e, "err")); }
    else { toast(`记账失败：${errs[0] || "未知原因"}`, "err"); }
    loadVouchers();
  } catch (e) { toast(e.message, "err"); }
}

// seedEntries：可选，凭证模板生成凭证时预填分录 [{account_code, summary, dir, amount}]
async function openVoucherEditor(id, seedEntries) {
  await ensureAccounts();
  // 辅助核算维度（与后端 AuxKind::bit 对齐）；cashflow 单独用现金流量项目输入
  const AUX_DEFS = [["customer", 1, "客户"], ["supplier", 2, "供应商"], ["dept", 4, "部门"],
    ["employee", 8, "职员"], ["project", 16, "项目"], ["item", 32, "存货"], ["bank", 128, "银行账户"]];
  const blank = { line: 1, account_code: "", summary: "", debit: "0", credit: "0", aux: {}, cf: "", qty: "", price: "" };
  let v = {
    id: 0, period: state.current, date: today(), word: "记", no: 0, attachments: 0, memo: "",
    entries: (seedEntries && seedEntries.length ? seedEntries.map((e, i) => ({
      line: i + 1, account_code: e.account_code || "", summary: e.summary || "",
      debit: e.dir === "credit" ? "0" : (e.amount || "0"),
      credit: e.dir === "credit" ? (e.amount || "0") : "0",
    })) : [Object.assign({}, blank, { line: 1 }), Object.assign({}, blank, { line: 2 })]),
  };
  let status = "draft", voucher_no = "";
  if (id) {
    try { v = await api(`/vouchers/${id}`); status = v.status; voucher_no = v.voucher_no; } catch (e) { toast(e.message, "err"); return; }
  } else {
    try { const n = await api(`/vouchers/next-no?period=${encodeURIComponent(state.current || "")}&word=记`); v.no = n.no; } catch (e) {}
  }
  // 规范化分录：辅助/数量/单价/现金流量统一成表单形态（后端返回 aux.cash_flow）
  v.entries = (v.entries || []).map((e, i) => {
    const aux = Object.assign({}, e.aux || {});
    const cf = aux.cash_flow || "";
    delete aux.cash_flow;
    return {
      line: i + 1, account_code: e.account_code || "", summary: e.summary || "",
      debit: e.debit != null ? String(e.debit) : "0",
      credit: e.credit != null ? String(e.credit) : "0",
      aux, cf,
      qty: e.qty != null ? String(e.qty) : "",
      price: e.price != null ? String(e.price) : "",
      currency: e.currency || "",
      rate: e.rate != null ? String(e.rate) : "",
      amount_for: e.amount_for != null ? String(e.amount_for) : "",
    };
  });
  if (!v.entries.length) v.entries = [Object.assign({}, blank, { line: 1 }), Object.assign({}, blank, { line: 2 })];
  // 出纳日记账「登记收付」预填：置入科目，方向由所填金额列体现
  if (state.pendingCash && !id) {
    const pc = state.pendingCash;
    delete state.pendingCash;
    v.entries[0].account_code = pc.account;
    setTimeout(() => toast(`已预置科目 ${pc.account}（${pc.dir === "debit" ? "收款：填借方金额" : "付款：填贷方金额"}）`, "ok"), 120);
  }
  // 可编辑状态与后端 can_edit() 对齐：未记账（含历史"已审核"）可改；已记账需先反记账
  const editable = (id === 0) || status === "draft" || status === "audited";
  const canPost = status === "draft" || status === "audited";
  const mask = modal(`
    <h3>记账凭证 ${esc(voucher_no)} <span class="muted" style="font-size:13px">${({ draft: "未记账", audited: "已审核", posted: "已记账", void: "已作废" })[status] || esc(status)}${v.cashier ? `　出纳:${esc(v.cashier)}` : ""}</span></h3>
    <div class="toolbar">
      <label>日期 <input id="v-date" type="date" value="${esc(v.date)}" ${editable ? "" : "disabled"} />${editable ? `<button class="btn ghost sm" id="v-today">今天</button>` : ""}</label>
      <span id="v-date-hint" class="muted" style="font-size:12px"></span>
      <label>字 <input id="v-word" value="${esc(v.word)}" style="width:60px" ${editable ? "" : "disabled"} /></label>
      <label>号 <input id="v-no" type="number" value="${v.no}" style="width:70px" ${editable ? "" : "disabled"} /></label>
      <label>附单据 <input id="v-att" type="number" value="${v.attachments}" style="width:60px" ${editable ? "" : "disabled"} /></label>
    </div>
    <table class="grid" id="v-entries">
      <thead><tr><th style="width:40px">行</th><th>科目</th><th>摘要</th><th class="num">借方</th><th class="num">贷方</th><th style="width:64px">辅助</th><th></th></tr></thead>
      <tbody></tbody>
    </table>
    ${editable ? `<button class="btn ghost sm" id="v-add">+ 增加分录</button>` : ""}
    ${editable ? `<button class="btn ghost sm" id="v-paste">粘贴分录…</button>` : ""}
    <div style="margin-top:10px" class="muted">合计：借 <b id="v-dt">0.00</b> 　贷 <b id="v-ct">0.00</b> 　差额 <b id="v-diff">0.00</b></div>
    <div id="v-attach" class="muted" style="margin-top:8px">附件加载中…</div>
    <div class="foot">
      ${editable ? `<button class="btn" id="v-save">保存</button>` : ""}
      ${editable ? `<button class="btn ghost" id="v-save-new">保存并新增</button>` : ""}
      ${can("voucher_audit") && id > 0 && status === "draft" ? `<button class="btn ghost" id="v-audit">审核</button>` : ""}
      ${can("voucher_unaudit") && id > 0 && status === "audited" ? `<button class="btn ghost" id="v-unaudit">反审核</button>` : ""}
      ${can("cashier_sign") && id > 0 && (status === "draft" || status === "audited") ? `<button class="btn ghost" id="v-sign">出纳签字</button>` : ""}
      ${can("cashier_sign") && id > 0 && (status === "draft" || status === "audited") && v.cashier ? `<button class="btn ghost" id="v-unsign">取消签字</button>` : ""}
      ${can("voucher_post") && canPost ? `<button class="btn primary" id="v-post">记账</button>` : ""}
      ${can("voucher_unpost") && status === "posted" ? `<button class="btn ghost" id="v-unpost">反记账</button>` : ""}
      ${can("voucher_new") && id > 0 && status !== "void" ? `<button class="btn ghost" id="v-reverse">红字冲销</button>` : ""}
      ${can("voucher_delete") && id > 0 && (status === "draft" || status === "audited") ? `<button class="btn ghost" id="v-void">作废</button>` : ""}
      ${can("voucher_delete") && id > 0 && status === "void" ? `<button class="btn ghost" id="v-void-back">恢复作废</button>` : ""}
      ${can("voucher_delete") && (status === "draft" || status === "audited") ? `<button class="btn danger" id="v-del">删除</button>` : ""}
      <button class="btn ghost" id="v-close">关闭</button>
    </div>
  `, true);

  const tbody = $("#v-entries tbody", mask);
  let expanded = -1;
  function acctOf(code) { return (state.accounts || []).find((a) => a.code === code); }
  function auxMissing(e) {
    const a = acctOf(e.account_code);
    if (!a) return false;
    for (const [k, bit] of AUX_DEFS) {
      if ((a.aux & bit) && !(e.aux && e.aux[k])) return true;
    }
    if (a.has_qty && !e.qty) return true;
    return false;
  }
  function auxDetailHtml(e, i) {
    const a = acctOf(e.account_code);
    const mask = (a && a.aux) || 0;
    const parts = [];
    for (const [k, bit, label] of AUX_DEFS) {
      if (!(mask & bit)) continue;
      parts.push(`<label>${label}* <input class="aux-in" data-i="${i}" data-k="${k}" value="${esc((e.aux || {})[k] || "")}" style="width:140px" /></label>`);
    }
    if (a && a.has_qty) {
      parts.push(`<label>数量 <input class="aux-in" data-i="${i}" data-k="qty" value="${esc(e.qty)}" style="width:90px" /></label>`);
      parts.push(`<label>单价 <input class="aux-in" data-i="${i}" data-k="price" value="${esc(e.price)}" style="width:90px" /></label>`);
    }
    if (a && (a.is_cash || a.is_bank)) {
      parts.push(`<label>现金流量项目 <input class="aux-in" data-i="${i}" data-k="cf" value="${esc(e.cf)}" placeholder="如 0101" style="width:100px" /></label>`);
    }
    if (a && a.currency) {
      parts.push(`<label>币种 <input class="aux-in" data-i="${i}" data-k="currency" value="${esc(e.currency || a.currency)}" style="width:64px" /></label>`);
      parts.push(`<label>汇率 <input class="aux-in" data-i="${i}" data-k="rate" value="${esc(e.rate)}" placeholder="1 外币=?" style="width:90px" /></label>`);
      parts.push(`<label>原币金额 <input class="aux-in" data-i="${i}" data-k="amount_for" value="${esc(e.amount_for)}" style="width:100px" /></label>`);
    }
    if (!parts.length) parts.push(`<span class="muted">该科目无需辅助核算/数量</span>`);
    return `<div class="muted" style="padding:6px 2px"><span style="font-size:12px">${parts.join(" ")}</span>${editable ? ` <button class="btn ghost sm" id="v-aux-close">收起</button>` : ""}</div>`;
  }
  function renderRows() {
    tbody.innerHTML = v.entries.map((e, i) => {
      const miss = auxMissing(e);
      const hasAux = (e.aux && Object.keys(e.aux).some((k) => e.aux[k])) || e.qty || e.cf;
      const detail = expanded === i ? `<tr><td colspan="7" style="background:rgba(0,0,0,0.03)">${auxDetailHtml(e, i)}</td></tr>` : "";
      return `<tr>
      <td>${e.line}</td>
      <td>${accountOptions()}</td>
      <td><input class="e-sum" value="${esc(e.summary)}" style="width:100%" ${editable ? "" : "disabled"} /></td>
      <td class="num"><input class="e-d num" value="${esc(e.debit)}" style="width:110px;text-align:right" ${editable ? "" : "disabled"} /></td>
      <td class="num"><input class="e-c num" value="${esc(e.credit)}" style="width:110px;text-align:right" ${editable ? "" : "disabled"} /></td>
      <td><button class="btn ghost sm e-aux" title="辅助核算/数量/现金流量" ${editable ? "" : "disabled"} style="${miss ? "color:var(--err)" : ""}">${miss ? "补录!" : (hasAux ? "已填" : "⋯")}</button></td>
      <td>${editable ? `<button class="btn ghost sm e-del">×</button>` : ""}</td>
    </tr>${detail}`;
    }).join("");
    $all("select.acct-sel", tbody).forEach((sel, i) => { sel.value = v.entries[i].account_code; sel.onchange = () => { v.entries[i].account_code = sel.value; renderRows(); }; });
    $all(".e-sum", tbody).forEach((inp, i) => inp.oninput = () => v.entries[i].summary = inp.value);
    $all(".e-d", tbody).forEach((inp, i) => inp.oninput = () => { v.entries[i].debit = inp.value; recalc(); });
    $all(".e-c", tbody).forEach((inp, i) => inp.oninput = () => { v.entries[i].credit = inp.value; recalc(); });
    $all(".e-del", tbody).forEach((b, i) => b.onclick = () => { v.entries.splice(i, 1); v.entries.forEach((e, k) => e.line = k + 1); expanded = -1; renderRows(); recalc(); });
    $all(".e-aux", tbody).forEach((b, i) => b.onclick = () => { expanded = (expanded === i ? -1 : i); renderRows(); });
    $all(".aux-in", tbody).forEach((inp) => {
      inp.oninput = () => {
        const i = parseInt(inp.dataset.i, 10), k = inp.dataset.k;
        const e = v.entries[i];
        if (k === "qty" || k === "price" || k === "cf" || k === "currency" || k === "rate" || k === "amount_for") { e[k] = inp.value; }
        else { e.aux = e.aux || {}; e.aux[k] = inp.value; }
      };
    });
    const closeAux = $("#v-aux-close", tbody);
    if (closeAux) closeAux.onclick = () => { expanded = -1; renderRows(); };
    recalc();
  }
  function recalc() {
    const sum = (arr, k) => arr.reduce((a, e) => a + (parseFloat(e[k]) || 0), 0);
    const dt = sum(v.entries, "debit"), ct = sum(v.entries, "credit");
    $("#v-dt", mask).textContent = dt.toFixed(2);
    $("#v-ct", mask).textContent = ct.toFixed(2);
    $("#v-diff", mask).textContent = (dt - ct).toFixed(2);
  }
  renderRows();
  if (editable) $("#v-add", mask).onclick = () => { v.entries.push(Object.assign({}, blank, { line: v.entries.length + 1 })); renderRows(); };
  // 批量粘贴分录：从 Excel / 旧账套复制一片区域直接进分录表
  //   金蝶 8 列：日期, 凭证字, 凭证号, 摘要, 科目编码, 科目名称, 借, 贷
  //   通用 4 列：摘要, 科目编码, 借, 贷
  //   精简 3 列：科目编码, 借, 贷
  if (editable) {
    const applyPasted = (text) => {
      const res = parseVoucherText(text, state.accounts);
      if (!res.rows.length) { toast("没识别到分录行（首列需为科目编码，或用「摘要/科目/借/贷」四列）", "err"); return; }
      // 空白行（无科目无金额）先剔除，再整体替换当前分录
      const rows = res.rows.map((r, i) => Object.assign({}, blank, r, { line: i + 1 }));
      const keepExisting = !v.entries.some((e) => e.account_code);
      v.entries = keepExisting ? rows : v.entries.concat(rows);
      if (res.date) { try { $("#v-date", mask).value = res.date; } catch (e) {} }
      if (res.word) { try { $("#v-word", mask).value = res.word; } catch (e) {} }
      v.entries.forEach((e, k) => e.line = k + 1);
      expanded = -1;
      renderRows();
      recalc();   // 复用既有合计/差额刷新
      const dt = v.entries.reduce((s, e) => s + (parseFloat(String(e.debit).replace(/,/g, "")) || 0), 0);
      const ct = v.entries.reduce((s, e) => s + (parseFloat(String(e.credit).replace(/,/g, "")) || 0), 0);
      const bal = Math.abs(dt - ct) < 0.005;
      markDirty("凭证未保存");
      if (res.bad.length) {
        toast(`已粘贴 ${rows.length} 行，但 ${res.bad.length} 行有问题：第 ${res.bad[0].line} 行 ${res.bad[0].code} ${res.bad[0].why}${bal ? "" : "；且借贷差额 " + (dt - ct).toFixed(2) + "，请调整"}`, "err");
      } else {
        toast(bal ? `已粘贴 ${rows.length} 行，借贷平衡` : `已粘贴 ${rows.length} 行，差额 ${(dt - ct).toFixed(2)}，请补齐另一方`, bal ? "ok" : "err");
      }
    };
    $("#v-paste", mask).onclick = async () => {
      try {
        const t = await navigator.clipboard.readText();
        if (!t) return toast("剪贴板是空的", "err");
        applyPasted(t);
      } catch (e) {
        // 浏览器不允许读剪贴板时，退回一个大文本框让用户手动粘贴
        const box = modal(`<h3>粘贴分录</h3>
          <div class="muted" style="margin:6px 0 8px">列支持三种：<br>
            · 金蝶 8 列：日期, 凭证字, 凭证号, 摘要, 科目编码, 科目名称, 借方, 贷方<br>
            · 通用 4 列：摘要, 科目编码, 借方, 贷方<br>
            · 精简 3 列：科目编码, 借方, 贷方
          </div>
          <textarea id="v-paste-t" rows="10" style="width:100%;font-family:monospace" placeholder="差旅费&#9;660201&#9;1000.00&#9;0&#10;差旅费&#9;100202&#9;0&#9;1000.00"></textarea>`);
        const t = box.querySelector("#v-paste-t");
        box.querySelector(".foot, .modal").insertAdjacentHTML("beforeend",
          `<div style="margin-top:8px"><button class="btn primary" id="v-paste-ok">粘贴</button></div>`);
        box.querySelector("#v-paste-ok").onclick = () => { applyPasted(t.value); closeModal(); };
      }
    };
    // 表格内直接 Ctrl+V
    $("#v-entries", mask).addEventListener("paste", (e) => {
      const t = (e.clipboardData || window.clipboardData).getData("text");
      if (!t || (!t.includes("\t") && !t.includes(","))) return;
      e.preventDefault();
      applyPasted(t);
    });
  }
  if (editable && $("#v-today", mask)) $("#v-today", mask).onclick = () => { $("#v-date", mask).value = today(); updateDateHint(); };
  // 跨期提示：所选日期与当前期间不一致时提前告知（保存时会按日期归入对应期间）
  function updateDateHint() {
    const el = $("#v-date-hint", mask);
    if (!el) return;
    const dv = $("#v-date", mask).value;
    if (dv && state.current && ymm(dv.slice(0, 7)) !== ymm(state.current)) {
      el.textContent = `该日期属于 ${dv.slice(0, 7)} 期，保存后将归入该期间`;
    } else {
      el.textContent = "";
    }
  }
  if (editable) $("#v-date", mask).addEventListener("change", updateDateHint);

  const save = async (keepOpen) => {
    const dateVal = $("#v-date", mask).value;
    if (!/^\d{4}-\d{2}-\d{2}$/.test(dateVal)) { toast("日期格式应为 YYYY-MM-DD", "err"); return; }
    const payload = {
      id: v.id,
      // 期间取业务日期所属月份（YYYYMM）；编辑已有凭证时后端以原期间为准
      period: ymm(dateVal.slice(0, 7)),
      date: dateVal,
      word: $("#v-word", mask).value,
      no: parseInt($("#v-no", mask).value, 10) || 0,
      attachments: parseInt($("#v-att", mask).value, 10) || 0,
      memo: "",
      entries: v.entries.map((e, i) => {
        const o = { line: i + 1, account_code: e.account_code, summary: e.summary, debit: String(parseFloat(e.debit) || 0), credit: String(parseFloat(e.credit) || 0) };
        const aux = {};
        for (const k of Object.keys(e.aux || {})) { if (e.aux[k]) aux[k] = e.aux[k]; }
        if (Object.keys(aux).length) o.aux = aux;
        if (e.qty) o.qty = String(e.qty);
        if (e.price) o.price = String(e.price);
        if (e.currency) o.currency = String(e.currency);
        if (e.rate) o.rate = String(e.rate);
        if (e.amount_for) o.amount_for = String(e.amount_for);
        if (e.cf) o.cf = e.cf;
        return o;
      }),
    };
    if (!payload.entries.some((e) => e.account_code)) { toast("请至少选择一条科目", "err"); return; }
    try {
      await api("/vouchers", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(payload) });
      if (keepOpen) {
        toast("已保存，继续录入下一张", "ok");
        closeModal();
        loadVouchers();
        openVoucherEditor(null);
      } else {
        toast("已保存", "ok");
        closeModal();
        loadVouchers();
      }
    } catch (e) { toast(e.message, "err"); }
  };
  if (editable) $("#v-save", mask).onclick = () => save(false);
  if ($("#v-save-new", mask)) $("#v-save-new", mask).onclick = () => save(true);
  if ($("#v-post", mask)) $("#v-post", mask).onclick = async () => { try { await api(`/vouchers/${v.id}/post`, { method: "POST" }); toast("已记账", "ok"); closeModal(); loadVouchers(); } catch (e) { toast(e.message, "err"); } };
  if ($("#v-unpost", mask)) $("#v-unpost", mask).onclick = async () => { try { await api(`/vouchers/${v.id}/unpost`, { method: "POST" }); toast("已反记账，凭证可修改", "ok"); closeModal(); loadVouchers(); } catch (e) { toast(e.message, "err"); } };
  if ($("#v-audit", mask)) $("#v-audit", mask).onclick = async () => { try { await api(`/vouchers/${v.id}/audit`, { method: "POST" }); toast("已审核", "ok"); closeModal(); loadVouchers(); } catch (e) { toast(e.message, "err"); } };
  if ($("#v-sign", mask)) $("#v-sign", mask).onclick = async () => { try { await api(`/vouchers/${v.id}/sign`, { method: "POST" }); toast("已出纳签字", "ok"); closeModal(); loadVouchers(); } catch (e) { toast(e.message, "err"); } };
  if ($("#v-unsign", mask)) $("#v-unsign", mask).onclick = async () => { try { await api(`/vouchers/${v.id}/unsign`, { method: "POST" }); toast("已取消签字", "ok"); closeModal(); loadVouchers(); } catch (e) { toast(e.message, "err"); } };
  if ($("#v-void", mask)) $("#v-void", mask).onclick = async () => {
    if (!(await confirmDialog("作废该凭证？作废后不参与账簿汇总，可在同一弹窗恢复。", true))) return;
    try { await api(`/vouchers/${v.id}/void`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ void: true }) }); toast("已作废", "ok"); closeModal(); loadVouchers(); } catch (e) { toast(e.message, "err"); }
  };
  if ($("#v-void-back", mask)) $("#v-void-back", mask).onclick = async () => {
    try { await api(`/vouchers/${v.id}/void`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ void: false }) }); toast("已恢复作废凭证", "ok"); closeModal(); loadVouchers(); } catch (e) { toast(e.message, "err"); }
  };
  if ($("#v-unaudit", mask)) $("#v-unaudit", mask).onclick = async () => { try { await api(`/vouchers/${v.id}/unaudit`, { method: "POST" }); toast("已反审核，凭证可修改", "ok"); closeModal(); loadVouchers(); } catch (e) { toast(e.message, "err"); } };
  if ($("#v-reverse", mask)) $("#v-reverse", mask).onclick = async () => { if (!(await confirmDialog("生成该凭证的红字冲销凭证（借贷互换、摘要加「冲销」前缀），原凭证保留不动？", true))) return; try { await api(`/vouchers/${v.id}/reverse`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ period: ymm(state.current || ""), date: "" }) }); toast("已生成冲销凭证", "ok"); closeModal(); loadVouchers(); } catch (e) { toast(e.message, "err"); } };
  if ($("#v-del", mask)) $("#v-del", mask).onclick = async () => { if (!(await confirmDialog("确定删除该凭证？", true))) return; try { await api(`/vouchers/${v.id}/delete`, { method: "POST" }); toast("已删除", "ok"); closeModal(); loadVouchers(); } catch (e) { toast(e.message, "err"); } };
  async function loadAttachments() {
    const box = $("#v-attach", mask);
    if (!box) return;
    if (!v.id) { box.innerHTML = `<span class="muted">保存凭证后可上传附件（单据影像等）</span>`; return; }
    let list = [];
    try { list = await api(`/vouchers/${v.id}/attachments`); }
    catch (e) { box.innerHTML = `<span class="muted">${esc(e.message)}</span>`; return; }
    const canEdit = can("voucher_edit");
    box.innerHTML = `<b>附件</b> ${list.length
      ? `<ul style="margin:6px 0 0 16px">${list.map((a) => `<li>${esc(a.name)} <span class="muted">(${esc(a.size_text)} ${esc(a.added_by || "")})</span> <a href="/api/attachments/${a.id}" target="_blank">下载</a> ${canEdit ? `<button class="btn sm ghost" data-attdel="${a.id}">删除</button>` : ""}</li>`).join("")}</ul>`
      : `<span class="muted">暂无附件</span>`}
      ${canEdit ? `<div style="margin-top:6px"><input type="file" id="v-attfile" /> <button class="btn sm" id="v-attup">上传</button> <span class="muted" style="font-size:12px">单文件 ≤ 10MB</span></div>` : ""}`;
    if ($("#v-attup", box)) $("#v-attup", box).onclick = async () => {
      const f = $("#v-attfile", box).files[0];
      if (!f) { toast("请选择文件", "err"); return; }
      if (f.size > 10 * 1024 * 1024) { toast("文件超过 10MB 上限", "err"); return; }
      const fd = new FormData();
      fd.append("file", f, f.name);
      try {
        const resp = await fetch(`/api/vouchers/${v.id}/attachments`, { method: "POST", body: fd });
        if (!resp.ok) { let msg = resp.statusText; try { msg = (await resp.json()).error || msg; } catch (e) {} throw new Error(msg); }
        toast("已上传", "ok"); loadAttachments();
      } catch (e) { toast(e.message, "err"); }
    };
    $all("[data-attdel]", box).forEach((b) => b.onclick = async () => {
      if (!(await confirmDialog("删除该附件？", true))) return;
      try { await api(`/attachments/${b.dataset.attdel}`, { method: "DELETE" }); toast("已删除", "ok"); loadAttachments(); } catch (e) { toast(e.message, "err"); }
    });
  }
  loadAttachments();
  $("#v-close", mask).onclick = closeModal;
}
function today() { const d = new Date(); const p = (n) => String(n).padStart(2, "0"); return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`; }

// ===========================================================================
// 发票管理
// ===========================================================================
let invoiceCache = null;

async function loadInvoices(filter) {
  const q = new URLSearchParams();
  if (filter && filter.kind) q.set("kind", filter.kind);
  if (filter && filter.status) q.set("status", filter.status);
  if (filter && filter.keyword) q.set("keyword", filter.keyword);
  const s = q.toString();
  const data = await api(`/invoices${s ? "?" + s : ""}`);
  // 汇总卡片：单独拉取（始终全量）
  let summary = {};
  try { summary = await api("/invoices/summary"); } catch (e) {}
  data.summary = (summary && summary.by_kind) || {};
  invoiceCache = data;
  return data;
}

async function viewInvoices(main) {
  main.innerHTML = `<h2>发票管理</h2><div class="muted">加载中…</div>`;
  let d;
  try { d = await loadInvoices(); } catch (e) { main.innerHTML = `<h2>发票管理</h2><div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; return; }
  renderInvoices(main, d);
}

function renderInvoices(main, d) {
  const rows = (d && d.rows) || [];
  const sum = (d && d.summary) || {};
  const statusBadge = (s) => {
    if (s === "verified") return `<span class="tag ok">已认证</span>`;
    if (s === "rejected") return `<span class="tag err">已作废</span>`;
    return `<span class="tag warn">待认证</span>`;
  };
  const kindLabel = (k) => (k === "out" ? "销项" : "进项");
  main.innerHTML = `
    <h2>发票管理</h2>
    <div class="cards" style="margin-bottom:14px">
      <div class="card"><div class="k">发票总数</div><div class="v">${rows.length}</div></div>
      <div class="card"><div class="k">进项价税合计</div><div class="v" style="font-size:18px">${esc((sum.in && sum.in.amount_tax) || "0.00")}</div></div>
      <div class="card"><div class="k">销项价税合计</div><div class="v" style="font-size:18px">${esc((sum.out && sum.out.amount_tax) || "0.00")}</div></div>
    </div>
    <div class="toolbar">
      <input id="inv-kw" placeholder="号码 / 代码 / 购销方" style="width:180px" />
      <select id="inv-kind">
        <option value="">全部类型</option>
        <option value="in">进项</option>
        <option value="out">销项</option>
      </select>
      <select id="inv-status">
        <option value="">全部状态</option>
        <option value="pending">待认证</option>
        <option value="verified">已认证</option>
        <option value="rejected">已作废</option>
      </select>
      <button class="btn" id="inv-query">查询</button>
      <div class="spacer"></div>
      ${can("voucher_new") ? `<button class="btn primary" id="inv-new">新增发票</button>` : ""}
    </div>
    <div class="panel" style="padding:0;overflow:hidden">
      <table class="grid">
        <thead><tr>
          <th>类型</th><th>发票号码</th><th>开票日期</th><th>购买方</th><th>销售方</th>
          <th class="num">不含税</th><th class="num">税额</th><th class="num">价税合计</th><th>状态</th><th></th>
        </tr></thead>
        <tbody>
          ${rows.length ? rows.map((r) => `
            <tr>
              <td>${kindLabel(r.kind)}</td>
              <td>${esc(r.number)}</td>
              <td>${esc(r.date)}</td>
              <td>${esc(r.buyer)}</td>
              <td>${esc(r.seller)}</td>
              <td class="num">${esc(r.amount)}</td>
              <td class="num">${esc(r.tax)}</td>
              <td class="num">${esc(r.amount_tax)}</td>
              <td>${statusBadge(r.status)}</td>
              <td class="row-actions">
                <button class="btn sm ghost" data-inv-chain="${r.id}">链</button>
                ${can("voucher_edit") ? `<button class="btn sm ghost" data-act="edit" data-id="${r.id}">编辑</button>` : ""}
                ${r.status === "pending" && can("voucher_edit") ? `<button class="btn sm ghost" data-act="verify" data-id="${r.id}">认证</button>` : ""}
                ${r.status !== "rejected" && can("voucher_edit") ? `<button class="btn sm ghost" data-act="reject" data-id="${r.id}">作废</button>` : ""}
                ${can("voucher_delete") ? `<button class="btn sm ghost" data-act="del" data-id="${r.id}">删除</button>` : ""}
              </td>
            </tr>`).join("") : `<tr><td colspan="10" class="muted" style="text-align:center;padding:18px">暂无发票</td></tr>`}
        </tbody>
      </table>
    </div>`;
  $("#inv-query").addEventListener("click", async () => {
    const d2 = await loadInvoices({ kind: $("#inv-kind").value, status: $("#inv-status").value, keyword: $("#inv-kw").value });
    renderInvoices(main, d2);
  });
  if ($("#inv-new")) $("#inv-new").addEventListener("click", () => openInvoiceEditor(main, null));
  $all("[data-inv-chain]", main).forEach((b) => b.addEventListener("click", () => openDocChain("invoice", parseInt(b.dataset.invChain, 10), "发票")));
  $all("[data-act]", main).forEach((b) => b.addEventListener("click", async () => {
    const id = parseInt(b.dataset.id, 10);
    const act = b.dataset.act;
    try {
      if (act === "edit") {
        const list = (await loadInvoices()).rows || [];
        const inv = list.find((x) => x.id === id);
        if (inv) openInvoiceEditor(main, inv);
      } else if (act === "verify") {
        const up = await api(`/invoices/${id}/status`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ status: "verified" }) });
        if (up) { toast("已认证", "ok"); if (state.view === "invoices") renderInvoices(main, await loadInvoices()); }
      } else if (act === "reject") {
        if (!(await confirmDialog("确定作废该发票？", true))) return;
        const up = await api(`/invoices/${id}/status`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ status: "rejected" }) });
        if (up) { toast("已作废", "ok"); if (state.view === "invoices") renderInvoices(main, await loadInvoices()); }
      } else if (act === "del") {
        if (!(await confirmDialog("确定删除该发票？", true))) return;
        await api(`/invoices/${id}`, { method: "DELETE" });
        toast("已删除", "ok");
        if (state.view === "invoices") renderInvoices(main, await loadInvoices());
      }
    } catch (e) { toast(e.message, "err"); }
  }));
}

function openInvoiceEditor(main, inv) {
  const isEdit = !!inv;
  const mask = modal(`
    <h3>${isEdit ? "编辑发票" : "新增发票"}</h3>
    <div class="field"><label>类型</label>
      <select id="inv-kind2">
        <option value="in" ${!isEdit || inv.kind === "in" ? "selected" : ""}>进项</option>
        <option value="out" ${isEdit && inv.kind === "out" ? "selected" : ""}>销项</option>
      </select>
    </div>
    <div class="field"><label>发票代码</label><input id="inv-code" value="${esc(inv ? inv.code : "")}" /></div>
    <div class="field"><label>发票号码 *</label><input id="inv-number" value="${esc(inv ? inv.number : "")}" /></div>
    <div class="field"><label>开票日期</label><input id="inv-date" value="${esc(inv ? inv.date : today())}" /></div>
    <div class="field"><label>购买方</label><input id="inv-buyer" value="${esc(inv ? inv.buyer : "")}" /></div>
    <div class="field"><label>销售方</label><input id="inv-seller" value="${esc(inv ? inv.seller : "")}" /></div>
    <div class="field"><label>价税合计</label><input id="inv-amt" value="${esc(inv ? inv.amount_tax : "0")}" /></div>
    <div class="field"><label>不含税金额</label><input id="inv-amount" value="${esc(inv ? inv.amount : "0")}" /></div>
    <div class="field"><label>税额</label><input id="inv-tax" value="${esc(inv ? inv.tax : "0")}" /></div>
    <div class="field"><label>税率（如 0.13）</label><input id="inv-rate" value="${esc(inv ? inv.tax_rate : "0")}" /></div>
    <div class="field"><label>备注</label><input id="inv-memo" value="${esc(inv ? inv.memo : "")}" /></div>
    <div class="foot">
      <button class="btn ghost" id="inv-close">取消</button>
      <button class="btn primary" id="inv-save">保存</button>
    </div>
  `, true);
  $("#inv-close").addEventListener("click", closeModal);
  $("#inv-save").addEventListener("click", async () => {
    const number = $("#inv-number").value.trim();
    if (!number) { toast("发票号码必填", "err"); return; }
    const body = {
      id: isEdit ? inv.id : 0,
      kind: $("#inv-kind2").value,
      code: $("#inv-code").value.trim(),
      number,
      date: $("#inv-date").value.trim(),
      buyer: $("#inv-buyer").value.trim(),
      seller: $("#inv-seller").value.trim(),
      amount_tax: $("#inv-amt").value.trim() || "0",
      amount: $("#inv-amount").value.trim() || "0",
      tax: $("#inv-tax").value.trim() || "0",
      tax_rate: $("#inv-rate").value.trim() || "0",
      status: isEdit ? inv.status : "pending",
      memo: $("#inv-memo").value.trim(),
    };
    try {
      if (isEdit) {
        await api(`/invoices/${inv.id}`, { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
        toast("已保存", "ok");
      } else {
        await api("/invoices", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
        toast("已新增", "ok");
      }
      closeModal();
      if (state.view === "invoices") renderInvoices(main, await loadInvoices());
    } catch (e) { toast(e.message, "err"); }
  });
}

// ===========================================================================
// 数据导入（其他软件 / CSV / Excel）
// ===========================================================================
async function viewImports(main) {
  main.innerHTML = `
    <h2>数据导入</h2>
    <div class="panel">
      <p class="muted" style="margin:0 0 12px;line-height:1.6">
        从其他财务软件（金蝶 / 用友）或 Excel 导入数据。支持 <b>基础资料</b>（辅助核算档案 / 存货 / 科目）、
        <b>期初余额</b>、<b>期初库存</b> 与 <b>记账凭证</b>；遇到账套里没有的科目可手动映射，
        重复编码自动跳过（幂等可重导）。
      </p>
      <div class="toolbar" style="box-shadow:none;border:none;padding:0;margin:0">
        <label>导入类型</label>
        <select id="imp-kind">
          <optgroup label="基础资料（各岗位迁移）">
            <option value="aux">辅助核算档案（客户/供应商/部门/存货…）</option>
            <option value="item">存货档案</option>
            <option value="account">会计科目</option>
          </optgroup>
          <optgroup label="期初（会计）">
            <option value="begin">期初余额表</option>
            <option value="opening_stock">期初库存（数量/批次）</option>
            <option value="arap_opening">往来期初明细（应收/应付按单据）</option>
          </optgroup>
          <optgroup label="凭证（会计）">
            <option value="voucher">记账凭证</option>
          </optgroup>
        </select>
        <label>来源模板</label>
        <select id="imp-template">
          <option value="generic">通用</option>
          <option value="kingdee">金蝶</option>
          <option value="yonyou">用友</option>
        </select>
        <label>期间（凭证用，YYYYMM）</label>
        <input id="imp-period" placeholder="202601" value="${state.current ? String(state.current).replace('-','') : ""}" style="width:90px" />
        <div class="spacer"></div>
        <button class="btn ghost sm" id="imp-tpl">下载模板</button>
        <button class="btn primary" id="imp-analyze">预检</button>
        <button class="btn" id="imp-run">执行导入</button>
      </div>
      <div style="margin-top:10px">
        <label style="font-weight:600">选择 Excel 文件（.xlsx / .xls / .ods，可选）</label>
        <input type="file" id="imp-file" accept=".xlsx,.xls,.ods" style="display:block;margin:4px 0 8px" />
        <textarea id="imp-text" rows="8"></textarea>
      </div>
      <div id="imp-result" class="muted" style="margin-top:10px;min-height:20px;white-space:pre-wrap;font-size:13px"></div>
    </div>
    <div class="panel" id="imp-mapping-wrap" style="display:none">
      <h3 style="margin:0 0 10px">缺失科目映射</h3>
      <p class="muted" style="margin:0 0 10px">以下科目在当前账套中不存在，请为每个选择目标科目（留空 = 忽略该科目对应行）。</p>
      <div id="imp-mapping"></div>
    </div>
    <div class="panel" style="margin-top:12px">
      <div style="display:flex;align-items:center;gap:8px;flex-wrap:wrap"><h3 style="margin:0">导出计划任务</h3><span class="grow"></span>
        <span class="muted" style="font-size:12px">每日定时写 CSV 到服务器 books/exports/（需「导出」权限）</span>
        ${can("export") ? `<button class="btn primary sm" id="ex-new">新增任务</button>` : ""}
        <button class="btn ghost sm" id="ex-reload">刷新</button>
      </div>
      <div id="ex-list" class="muted" style="margin-top:8px">加载中…</div>
    </div>`;
  let mapping = {};
  let fileB64 = "";
  // 加载科目列表供映射下拉
  let accounts = [];
  try { accounts = await api("/accounts"); } catch (e) {}
  const acctOpts = (sel) => accounts.map((a) => `<option value="${esc(a.code)}" ${sel === a.code ? "selected" : ""}>${esc(a.code)} ${esc(a.name)}</option>`).join("");

  // Excel 文件 → base64
  $("#imp-file").addEventListener("change", (ev) => {
    const f = ev.target.files && ev.target.files[0];
    if (!f) { fileB64 = ""; return; }
    const reader = new FileReader();
    reader.onload = () => {
      const dataUrl = String(reader.result || "");
      fileB64 = dataUrl.split(",").slice(1).join(","); // 去掉 data:...;base64, 前缀
      $("#imp-result").textContent = `已选择文件：${f.name}（${(f.size / 1024).toFixed(1)} KB），点击「预检科目」或「执行导入」。`;
    };
    reader.readAsDataURL(f);
  });

  // 模板下载（当前类型）+ 列头提示随类型切换
  const TPL_COLS = {
    aux: "类型,编码,名称,备注（客户/供应商/部门/职员/项目/银行/存货）",
    item: "编码,名称,保质期天,安全库存",
    account: "编码,名称,类别,方向,备注（类别空按编码首位推）",
    opening_stock: "存货编码,仓库,数量,单价,批次号,生产日期,备注（只入数量，不生成凭证）",
    arap_opening: "类型(应收/应付),客商编码,单据号,单据日期,金额,客商名称,备注（影子挂账进账龄，不生成凭证）",
    begin: "科目编码, 方向(借/贷), 金额（金蝶/用友用完整列）",
    voucher: "日期, 凭证字, 摘要, 科目编码, 借方, 贷方",
  };
  const kindSel = $("#imp-kind");
  const syncKind = () => {
    const k = kindSel.value;
    $("#imp-text").placeholder = `或直接粘贴 CSV 内容…\n列：${TPL_COLS[k] || ""}\n（或点「下载模板」拿标准模板填）`;
    $("#imp-analyze").textContent = k === "begin" || k === "voucher" ? "预检科目" : "预检";
  };
  kindSel.addEventListener("change", syncKind);
  syncKind();
  $("#imp-tpl").addEventListener("click", () => {
    window.location = `/api/import/template?kind=${encodeURIComponent(kindSel.value)}`;
  });

  $("#imp-analyze").addEventListener("click", async () => {
    const text = $("#imp-text").value;
    const kind = $("#imp-kind").value;
    const template = $("#imp-template").value;
    if (!text.trim() && !fileB64) { toast("请粘贴 CSV 内容或选择 Excel 文件", "err"); return; }
    try {
      const r = await api("/import/analyze", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ kind, text, template, file: fileB64 || null }) });
      const missing = (r && r.missing) || [];
      const wrap = $("#imp-mapping-wrap");
      const box = $("#imp-mapping");
      if (missing.length === 0) {
        wrap.style.display = "none";
        $("#imp-result").textContent = "✅ 预检通过：所有科目在账套中均存在，可直接执行导入。";
        return;
      }
      box.innerHTML = missing.map((m) => `
        <div style="display:flex;gap:8px;align-items:center;margin-bottom:6px">
          <span style="min-width:140px;font-family:monospace">${esc(m.code)} <span class="muted">×${m.count}</span></span>
          <select class="imp-map" data-code="${esc(m.code)}" style="flex:1">
            <option value="">— 忽略 —</option>
            ${acctOpts("")}
          </select>
        </div>`).join("");
      wrap.style.display = "";
      $("#imp-result").textContent = `找到 ${missing.length} 个缺失科目，请选择映射或忽略。`;
      $all(".imp-map", box).forEach((s) => s.addEventListener("change", () => {
        mapping[s.dataset.code] = s.value;
      }));
    } catch (e) { $("#imp-result").textContent = "预检失败：" + e.message; }
  });

  $("#imp-run").addEventListener("click", async () => {
    const text = $("#imp-text").value;
    const kind = $("#imp-kind").value;
    const template = $("#imp-template").value;
    const period = parseInt(($("#imp-period").value || "0").replace(/[^0-9]/g, ""), 10) || 0;
    if (!text.trim() && !fileB64) { toast("请粘贴 CSV 内容或选择 Excel 文件", "err"); return; }
    try {
      const r = await api("/import/run", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ kind, text, template, file: fileB64 || null, period, mapping }) });
      const lines = [`✅ 成功导入 ${r.ok} 条`, r.skipped ? `⚠ 跳过 ${r.skipped} 条` : ""].filter(Boolean);
      if (r.warnings && r.warnings.length) {
        lines.push("", "警告：");
        r.warnings.slice(0, 20).forEach((w) => lines.push("  · " + w));
        if (r.warnings.length > 20) lines.push(`  …共 ${r.warnings.length} 条警告`);
      }
      $("#imp-result").textContent = lines.join("\n");
      toast(`已导入 ${r.ok} 条`, "ok");
      state.view = "dashboard";
    } catch (e) { $("#imp-result").textContent = "导入失败：" + e.message; }
  });

  // 导出计划任务：列表 / 新增 / 立即执行 / 删除
  const EX_KINDS = { vouchers: "记账凭证", trial: "科目余额表", payroll: "工资表", claims: "报销单" };
  const loadEx = async () => {
    try {
      const r = await api("/export/schedules");
      const rows = r.rows || [];
      $("#ex-list").innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>类型</th><th>期间</th><th>执行时刻</th><th>状态</th><th>最近执行</th><th>备注</th><th></th></tr></thead><tbody>${rows.map((x) => `<tr>
            <td>${esc(EX_KINDS[x.kind] || x.kind)}</td><td>${x.period_mode === "last" ? "上一期间" : "当前期间"}</td><td>${esc(x.at_time)}</td>
            <td>${x.enabled ? '<span class="tag ok">启用</span>' : '<span class="tag">停用</span>'}</td>
            <td>${esc(x.last_run || "—")}</td><td>${esc(x.memo || "")}</td>
            <td class="row-actions">${can("export") ? `<button class="btn ghost sm" data-ex-run="${x.id}">立即执行</button><button class="btn danger sm" data-ex-del="${x.id}">删</button>` : ""}</td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">暂无导出计划任务</div>`;
      $all("[data-ex-run]").forEach((b) => b.onclick = async () => {
        try { const r = await postJson(`/export/schedules/${b.dataset.exRun}/run`, {}); toast(`已导出：${r.path}`, "ok"); loadEx(); } catch (e) { toast(e.message, "err"); }
      });
      $all("[data-ex-del]").forEach((b) => b.onclick = async () => {
        if (!(await confirmDialog("删除该导出计划任务？", true))) return;
        try { await api(`/export/schedules/${b.dataset.exDel}/delete`, { method: "POST" }); toast("已删除", "ok"); loadEx(); } catch (e) { toast(e.message, "err"); }
      });
    } catch (e) { $("#ex-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  if ($("#ex-new")) $("#ex-new").onclick = () => {
    const m = modal(`<h3>新增导出计划任务</h3>
      <div class="field"><label>导出内容</label><select id="ex-kind"><option value="vouchers">记账凭证</option><option value="trial">科目余额表</option><option value="payroll">工资表</option><option value="claims">报销单</option></select></div>
      <div class="field"><label>期间口径</label><select id="ex-mode"><option value="current">当前期间</option><option value="last">上一期间</option></select></div>
      <div class="field"><label>每日执行时刻（HH:MM）</label><input id="ex-time" value="08:00" style="width:90px" /></div>
      <div class="field"><label style="display:inline-flex;gap:4px;align-items:center"><input type="checkbox" id="ex-enabled" checked /> 启用</label></div>
      <div class="field"><label>备注</label><input id="ex-memo" /></div>
      <div class="foot"><button class="btn primary" id="ex-save">保存</button><button class="btn ghost" id="ex-cancel">取消</button></div>`);
    $("#ex-cancel", m).onclick = closeModal;
    $("#ex-save", m).onclick = async () => {
      try {
        await postJson("/export/schedules", { kind: $("#ex-kind", m).value, period_mode: $("#ex-mode", m).value, at_time: $("#ex-time", m).value.trim(), enabled: $("#ex-enabled", m).checked, memo: $("#ex-memo", m).value.trim() });
        toast("已保存计划任务", "ok"); closeModal(); loadEx();
      } catch (e) { toast(e.message, "err"); }
    };
  };
  $("#ex-reload").onclick = loadEx;
  loadEx();
}

// ===========================================================================
// 明细账
// ===========================================================================
let ledgerTab = "detail";
async function viewLedger(main) {
  await ensureAccounts();
  main.innerHTML = `
    <h2>账簿查询</h2>
    <div class="toolbar">
      <label>科目 <input id="l-code" list="acct-list" placeholder="科目编码，如 1002" style="width:160px" /></label>
      <datalist id="acct-list">${(state.accounts || []).map((a) => `<option value="${esc(a.code)}">${esc(a.name)}</option>`).join("")}</datalist>
      <label>从 <input id="l-from" value="${esc(state.current || "")}" style="width:90px" /></label>
      <label>至 <input id="l-to" value="${esc(state.current || "")}" style="width:90px" /></label>
      <label><input type="checkbox" id="l-children" checked /> 含下级</label>
      <label><input type="checkbox" id="l-posted" checked /> 仅已记账</label>
      <button class="btn sm" id="l-go">查询</button>
      <div class="spacer"></div>
      <button class="btn ghost sm" data-ltab="detail">明细账</button>
      <button class="btn ghost sm" data-ltab="general">总账</button>
      <button class="btn ghost sm" data-ltab="journal">日记账</button>
      <button class="btn ghost sm" id="l-print">打印预览</button>
      <button class="btn ghost sm" id="l-printform">套打</button>
      ${can("export") ? `<button class="btn ghost sm" id="l-export">导出 CSV</button>` : ""}
    </div>
    <div class="panel"><table class="grid" id="l-table"><thead></thead><tbody><tr><td class="muted">请输入科目后查询</td></tr></tbody></table></div>`;
  const setTab = (t) => {
    ledgerTab = t;
    $all("[data-ltab]", main).forEach((b) => b.classList.toggle("primary", b.dataset.ltab === t));
  };
  setTab("detail");
  $all("[data-ltab]", main).forEach((b) => b.onclick = () => { setTab(b.dataset.ltab); loadLedger(); });
  $("#l-go").addEventListener("click", loadLedger);
  $("#l-print").addEventListener("click", () => {
    const el = $("#l-table").querySelector("table");
    printPreview(({ detail: "明细账", general: "总账", journal: "日记账" })[ledgerTab] || "账簿", el);
  });
  $("#l-printform").addEventListener("click", () => {
    const code = $("#l-code").value.trim();
    if (!code) { toast("请先输入科目编码", "err"); return; }
    const q = `code=${encodeURIComponent(code)}&from=${encodeURIComponent($("#l-from").value)}&to=${encodeURIComponent($("#l-to").value)}&include_children=${$("#l-children").checked ? 1 : 0}&posted_only=${$("#l-posted").checked ? 1 : 0}&type=${ledgerTab}`;
    window.open(`/api/ledger/print-form?${q}`, "_blank");
  });
  if ($("#l-export")) $("#l-export").onclick = () => {
    const code = $("#l-code").value.trim();
    if (!code) { toast("请先输入科目编码", "err"); return; }
    const q = `code=${encodeURIComponent(code)}&from=${encodeURIComponent($("#l-from").value)}&to=${encodeURIComponent($("#l-to").value)}&include_children=${$("#l-children").checked ? 1 : 0}&posted_only=${$("#l-posted").checked ? 1 : 0}`;
    window.open(`/api/export/ledger?${q}`, "_blank");
  };
}
async function loadLedger() {
  const code = $("#l-code").value.trim();
  if (!code) { toast("请先输入科目编码", "err"); return; }
  const qs = `code=${encodeURIComponent(code)}&from=${encodeURIComponent($("#l-from").value)}&to=${encodeURIComponent($("#l-to").value)}&include_children=${$("#l-children").checked ? 1 : 0}&posted_only=${$("#l-posted").checked ? 1 : 0}`;
  const url = ledgerTab === "general" ? `/ledger/general?${qs}` : ledgerTab === "journal" ? `/ledger/journal?${qs}` : `/ledger?${qs}`;
  const tbl = $("#l-table");
  const message = (text, isErr) => {
    tbl.innerHTML = `<tbody><tr><td colspan="8" ${isErr ? 'style="color:var(--err)"' : 'class="muted"'}>${esc(text)}</td></tr></tbody>`;
  };
  let rows;
  try { rows = await api(url); } catch (e) { message(e.message, true); return; }
  if (!rows.length) { message("该科目在所选期间无记录", false); return; }
  if (ledgerTab === "general") {
    tbl.innerHTML = `<thead><tr><th>期间</th><th>摘要</th><th class="num">借方</th><th class="num">贷方</th><th>方向</th><th class="num">余额</th></tr></thead><tbody>${rows.map((r) => `<tr><td>${esc(r.period)}</td><td>${esc(r.summary)}</td><td class="num">${esc(r.debit)}</td><td class="num">${esc(r.credit)}</td><td>${esc(r.dir === "debit" ? "借" : "贷")}</td><td class="num">${esc(r.balance)}</td></tr>`).join("")}</tbody>`;
    return;
  }
  if (ledgerTab === "journal") {
    tbl.innerHTML = `<thead><tr><th>日期</th><th>凭证号</th><th>摘要</th><th>对方科目</th><th class="num">借方</th><th class="num">贷方</th><th>方向</th><th>出纳</th><th class="num">余额</th></tr></thead><tbody>${rows.map((r) => `<tr><td>${esc(r.date)}</td><td>${esc(r.voucher_no)}</td><td>${esc(r.summary)}</td><td>${esc(r.opposite_accounts || "")}</td><td class="num">${esc(r.debit)}</td><td class="num">${esc(r.credit)}</td><td>${esc(r.dir === "debit" ? "借" : "贷")}</td><td>${esc(r.cashier || "")}</td><td class="num">${esc(r.balance)}</td></tr>`).join("")}</tbody>`;
    return;
  }
  const hasQty = rows.some((r) => r.qty_balance != null);
  const head = `<tr><th>日期</th><th>凭证号</th><th>摘要</th><th class="num">借方</th><th class="num">贷方</th><th>方向</th><th class="num">余额</th>${hasQty ? '<th class="num">数量余额</th>' : ""}</tr>`;
  const body = rows.map((r) => `<tr>
    <td>${esc(r.date)}</td><td>${esc(r.voucher_no)}</td><td>${esc(r.summary)}</td>
    <td class="num">${esc(r.debit)}</td><td class="num">${esc(r.credit)}</td>
    <td>${esc(r.dir === "debit" ? "借" : "贷")}</td><td class="num">${esc(r.balance)}</td>
    ${hasQty ? `<td class="num" style="color:var(--err,#c62828)">${r.qty_balance != null ? esc(fmt(r.qty_balance)) : ""}</td>` : ""}
  </tr>`).join("");
  tbl.innerHTML = `<thead>${head}</thead><tbody>${body}</tbody>`;
}

// ===========================================================================
// 报表中心（打印 vs 导出 权限区分）
// ===========================================================================
async function viewReports(main) {
  main.innerHTML = `
    <h2>报表中心</h2>
    <div class="toolbar">
      <label>从 <input id="r-from" value="${esc(state.current || "")}" style="width:90px" /></label>
      <label>至 <input id="r-to" value="${esc(state.current || "")}" style="width:90px" /></label>
      <button class="btn sm" id="r-go">生成科目余额表</button>
      <button class="btn ghost sm" id="r-aux">辅助账</button>
      <label>维度 <select id="r-auxkind">
        <option value="customer">客户</option><option value="supplier">供应商</option>
        <option value="dept">部门</option><option value="employee">职员</option>
        <option value="project">项目</option><option value="item">存货</option>
        <option value="bank">银行账户</option>
      </select></label>
      <button class="btn ghost sm" id="r-qty">数量金额账</button>
      <span class="spacer"></span>
      ${can("export") ? `<button class="btn ghost sm" id="r-export">导出 CSV</button>
      <button class="btn ghost sm" id="r-pdf">导出 PDF</button>` : `<span class="tag warn" title="无导出权限">无导出权限，仅可打印</span>`}
      <button class="btn ghost sm" id="r-print">打印预览</button>
    </div>
    <div class="panel"><table class="grid" id="r-table"><thead><tr>
      <th>科目编码</th><th>科目名称</th><th>方向</th><th class="num">期初</th><th class="num">本期借方</th><th class="num">本期贷方</th><th class="num">期末</th><th class="num">本年累计借方</th><th class="num">本年累计贷方</th>
    </tr></thead><tbody><tr><td colspan="9" class="muted">点击「生成科目余额表」</td></tr></tbody></table></div>
    <div id="r-extra" style="margin-top:10px"></div>`;
  $("#r-go").addEventListener("click", loadTrial);
  $("#r-print").addEventListener("click", () => { const f = $("#r-from").value, t = $("#r-to").value; window.open(`/api/reports/trial-balance/print?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`, "_blank"); });
  if (can("export")) {
    $("#r-export").addEventListener("click", () => { const f = $("#r-from").value, t = $("#r-to").value; window.location = `/api/reports/trial-balance/export?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`; });
    $("#r-pdf").addEventListener("click", () => { const f = $("#r-from").value, t = $("#r-to").value; window.location = `/api/reports/trial-balance/pdf?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`; });
  }
  $("#r-aux").addEventListener("click", async () => {
    const kind = $("#r-auxkind").value;
    const qs = new URLSearchParams({ kind, from: $("#r-from").value, to: $("#r-to").value });
    try {
      const d = await api(`/reports/aux-balance?${qs.toString()}`);
      const rows = d.rows || [];
      $("#r-extra").innerHTML = `<div class="panel"><b>${esc(d.kind_label)}辅助账（${esc(d.from)} ~ ${esc(d.to)}）</b>
        <button class="btn ghost sm" style="float:right" id="r-aux-print">打印预览</button>
        <table class="grid" id="r-aux-table" style="margin-top:6px"><thead><tr><th>${esc(d.kind_label)}</th><th class="num">期初</th><th class="num">本期借方</th><th class="num">本期贷方</th><th class="num">期末</th></tr></thead><tbody>${rows.length ? rows.map((r) => `<tr><td>${esc(r.key)}</td><td class="num">${esc(r.begin)}</td><td class="num">${esc(r.debit)}</td><td class="num">${esc(r.credit)}</td><td class="num">${esc(r.end)}</td></tr>`).join("") : `<tr><td colspan="5" class="muted">无数据</td></tr>`}</tbody></table></div>`;
      $("#r-aux-print").onclick = () => printPreview(`${d.kind_label}辅助账`, $("#r-aux-table"));
    } catch (e) { toast(e.message, "err"); }
  });
  $("#r-qty").addEventListener("click", async () => {
    const qs = new URLSearchParams({ from: $("#r-from").value, to: $("#r-to").value });
    try {
      const d = await api(`/reports/qty-balance?${qs.toString()}`);
      const rows = d.rows || [];
      $("#r-extra").innerHTML = `<div class="panel"><b>数量金额账（${esc(d.from)} ~ ${esc(d.to)}）</b>
        <button class="btn ghost sm" style="float:right" id="r-qty-print">打印预览</button>
        <table class="grid" id="r-qty-table" style="margin-top:6px"><thead><tr><th>科目编码</th><th>科目名称</th><th class="num">期初数量</th><th class="num">入库数量</th><th class="num">出库数量</th><th class="num">期末数量</th><th class="num">期初金额</th><th class="num">借方金额</th><th class="num">贷方金额</th><th class="num">期末金额</th></tr></thead><tbody>${rows.length ? rows.map((r) => `<tr><td>${esc(r.account_code)}</td><td>${esc(r.account_name)}</td><td class="num">${esc(r.qty_begin)}</td><td class="num">${esc(r.qty_in)}</td><td class="num">${esc(r.qty_out)}</td><td class="num">${esc(r.qty_end)}</td><td class="num">${esc(r.amount_begin)}</td><td class="num">${esc(r.amount_debit)}</td><td class="num">${esc(r.amount_credit)}</td><td class="num">${esc(r.amount_end)}</td></tr>`).join("") : `<tr><td colspan="10" class="muted">无数量核算科目数据</td></tr>`}</tbody></table></div>`;
      $("#r-qty-print").onclick = () => printPreview("数量金额账", $("#r-qty-table"));
    } catch (e) { toast(e.message, "err"); }
  });
}
async function loadTrial() {
  const f = $("#r-from").value, t = $("#r-to").value;
  const tb = $("#r-table tbody");
  let data;
  try { data = await api(`/reports/trial-balance?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`); } catch (e) { tb.innerHTML = `<tr><td colspan="9" style="color:var(--err)">${esc(e.message)}</td></tr>`; return; }
  const rows = data.rows || [];
  if (!rows.length) { tb.innerHTML = `<tr><td colspan="9" class="muted">无数据</td></tr>`; return; }
  tb.innerHTML = rows.map((r) => {
    // 后端 TrialRow：begin_dir/end_dir/begin/end/debit/credit/ytd_* 均为已格式化字符串
    return `<tr data-drill="${esc(r.account_code)}" title="点击查看明细账" style="cursor:pointer">
      <td>${esc(r.account_code)}</td><td>${esc(r.account_name)}</td>
      <td>${esc(r.end_dir)}</td>
      <td class="num">${esc(r.begin_dir)} ${esc(r.begin)}</td>
      <td class="num">${esc(r.debit)}</td><td class="num">${esc(r.credit)}</td>
      <td class="num">${esc(r.end_dir)} ${esc(r.end)}</td>
      <td class="num">${esc(r.ytd_debit)}</td><td class="num">${esc(r.ytd_credit)}</td>
    </tr>`;
  }).join("");
  // 合计行（借贷平衡校验参考）
  if (data.totals) {
    const t = data.totals;
    tb.innerHTML += `<tr style="background:#fafafa;font-weight:600">
      <td colspan="3">合计</td>
      <td class="num">借 ${esc(t.begin_debit)} / 贷 ${esc(t.begin_credit)}</td>
      <td class="num">${esc(t.debit)}</td><td class="num">${esc(t.credit)}</td>
      <td class="num">借 ${esc(t.end_debit)} / 贷 ${esc(t.end_credit)}</td>
      <td class="num">${esc(t.ytd_debit || "—")}</td><td class="num">${esc(t.ytd_credit || "—")}</td>
    </tr>`;
  }
  // 数字钻取：数据行点击 → 该科目明细账（合计行无 data-drill 不受影响）
  $all("[data-drill]", tb).forEach((tr) => {
    tr.onclick = () => openAccountDetail(tr.dataset.drill, f, t);
  });
}

// 科目明细账弹窗（数字钻取）：期初 + 逐笔 + 合计；行点击打开凭证（二级钻取）
async function openAccountDetail(account, from, to) {
  const mask = modal(`<h3>明细账 · ${esc(account)}</h3><div id="ad-body" class="muted">加载中…</div>
    <div class="foot"><button class="btn ghost" id="ad-print">打印预览</button><button class="btn primary" id="ad-close">关闭</button></div>`, true);
  $("#ad-close", mask).onclick = closeModal;
  let tableEl = null;
  const dir = (v) => Number(v) < 0 ? `贷 ${fmt(-Number(v))}` : Number(v) > 0 ? `借 ${fmt(v)}` : "平";
  try {
    const r = await api(`/reports/account-detail?account=${encodeURIComponent(account)}&from=${encodeURIComponent(from || "")}&to=${encodeURIComponent(to || "")}`);
    const rows = r.rows || [];
    $("#ad-body", mask).innerHTML = `
      <div class="muted" style="font-size:12.5px;margin-bottom:6px">期间 ${esc(r.from)} ~ ${esc(r.to)} · 期初：<b>${dir(r.begin)}</b> · 点击行打开凭证</div>
      ${rows.length ? `<table class="grid" id="ad-tbl"><thead><tr><th>日期</th><th>凭证号</th><th>摘要</th><th>分录摘要</th><th class="num">借方</th><th class="num">贷方</th><th class="num">余额</th></tr></thead><tbody>
        <tr style="background:var(--z-50)"><td>—</td><td>—</td><td colspan="4"><b>期初余额</b></td><td class="num"><b>${dir(r.begin)}</b></td></tr>
        ${rows.map((x) => `<tr data-ad-v="${x.voucher_id}" style="cursor:pointer" title="打开凭证 #${x.voucher_id}"><td>${esc(x.date)}</td><td>${esc(x.no)}</td><td>${esc(x.summary)}</td><td>${esc(x.line_memo || "—")}</td><td class="num">${Number(x.debit) ? fmt(x.debit) : ""}</td><td class="num">${Number(x.credit) ? fmt(x.credit) : ""}</td><td class="num">${dir(x.balance)}</td></tr>`).join("")}
        <tr style="font-weight:600;background:#fafafa"><td colspan="4">合计</td><td class="num">${fmt(r.total_debit)}</td><td class="num">${fmt(r.total_credit)}</td><td class="num"></td></tr>
      </tbody></table>` : `<div class="muted">该科目此期间无发生额</div>`}`;
    tableEl = $("#ad-tbl", mask);
    $all("[data-ad-v]", mask).forEach((tr) => tr.onclick = () => {
      closeModal();
      openVoucherEditor(parseInt(tr.dataset.adV, 10));
    });
  } catch (e) {
    $("#ad-body", mask).innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`;
  }
  $("#ad-print", mask).onclick = () => { if (tableEl) printPreview(`明细账 ${account}`, tableEl); };
}

// ===========================================================================
// 通用打印预览：把页面里的表格渲染成可打印 HTML（新窗口，自动弹打印）
// 数据只走内存，不落地文件；任何有 <table> 结果的报表页都能复用。
// ===========================================================================
function printPreview(title, tableEl) {
  if (!tableEl || !tableEl.outerHTML) { toast("没有可打印的数据", "err"); return; }
  const html = `<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><title>${esc(title)}</title>
    <style>body{font-family:-apple-system,'Microsoft YaHei',sans-serif;color:#222;margin:16px;}
    h2{text-align:center;margin:8px 0;}
    .meta{display:flex;justify-content:space-between;color:#666;font-size:13px;margin-bottom:4px;}
    table{border-collapse:collapse;width:100%;font-size:13px;}
    th,td{border:1px solid #bbb;padding:4px 8px;}
    th{background:#f0f3f7;}td.r,td.num{text-align:right;}
    @media print{body{font-size:12px;margin:0;}}</style></head>
    <body><h2>${esc(title)}</h2>
    <div class="meta"><span>${esc(session.user ? session.user.display_name : "")}</span><span>打印时间：${esc(today())}</span></div>
    ${tableEl.outerHTML}
    <script>window.onload=function(){setTimeout(function(){window.print();},300);};</scr${"ipt"}>
    </body></html>`;
  const w = window.open("", "_blank");
  if (!w) { toast("浏览器拦截了打印窗口，请允许弹出窗口", "err"); return; }
  w.document.write(html);
  w.document.close();
}

// ===========================================================================
// 三大报表：资产负债表 / 利润表 / 现金流量表
// ===========================================================================
function statementTableHtml(t) {
  // t = { title, subtitle, company, columns, rows:[{no,name,indent,style,values,negative}] }
  const head = `<th>行次</th><th>项目</th>${(t.columns || []).map((c) => `<th class="num">${esc(c)}</th>`).join("")}`;
  const body = (t.rows || []).map((r) => {
    const indent = "　".repeat(r.indent || 0);
    const bold = r.style === "total" ? " style='font-weight:700;background:#fafafa'" : r.style === "subtotal" ? " style='font-weight:600'" : r.style === "header" ? " style='font-weight:600;background:#f5f7fa'" : "";
    const cells = (r.values || []).map((v) => `<td class="num"${r.negative && moneyNum(v) < 0 ? " style='color:var(--err)'" : ""}>${esc(v)}</td>`).join("");
    return `<tr${bold}><td class="muted">${esc(r.no)}</td><td>${indent}${esc(r.name)}</td>${cells}</tr>`;
  }).join("");
  return `<table class="grid"><thead><tr>${head}</tr></thead><tbody>${body}</tbody></table>`;
}

async function viewBalanceSheet(main) {
  main.innerHTML = `<h2>资产负债表</h2>
    <div class="toolbar">
      <label>从 <input id="bs-from" value="${esc(state.current)}" style="width:90px" /></label>
      <label>至 <input id="bs-to" value="${esc(state.current)}" style="width:90px" /></label>
      <button class="btn primary" id="bs-run">查询</button>
      <button class="btn ghost sm" id="bs-print">打印预览</button>
    </div>
    <div id="bs-result" class="muted">填写期间后点击查询</div>`;
  const load = async () => {
    const f = $("#bs-from").value.trim(), t = $("#bs-to").value.trim();
    try {
      const r = await api(`/reports/balance-sheet?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`);
      $("#bs-result").innerHTML = `<div class="muted" style="margin-bottom:8px">${esc(r.table.subtitle)}</div>${statementTableHtml(r.table)}`;
    } catch (e) { toast(e.message, "err"); }
  };
  $("#bs-run").addEventListener("click", load);
  $("#bs-print").addEventListener("click", () => { const f = $("#bs-from").value, t = $("#bs-to").value; window.open(`/api/reports/balance-sheet/print?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`, "_blank"); });
  load();
}

async function viewIncomeStatement(main) {
  main.innerHTML = `<h2>利润表</h2>
    <div class="toolbar">
      <label>从 <input id="is-from" value="${esc(state.current)}" style="width:90px" /></label>
      <label>至 <input id="is-to" value="${esc(state.current)}" style="width:90px" /></label>
      <button class="btn primary" id="is-run">查询</button>
      <button class="btn ghost sm" id="is-print">打印预览</button>
    </div>
    <div id="is-result" class="muted">填写期间后点击查询</div>`;
  const load = async () => {
    const f = $("#is-from").value.trim(), t = $("#is-to").value.trim();
    try {
      const r = await api(`/reports/income-statement?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`);
      $("#is-result").innerHTML = `<div class="muted" style="margin-bottom:8px">${esc(r.table.subtitle)}</div>${statementTableHtml(r.table)}`;
    } catch (e) { toast(e.message, "err"); }
  };
  $("#is-run").addEventListener("click", load);
  $("#is-print").addEventListener("click", () => { const f = $("#is-from").value, t = $("#is-to").value; window.open(`/api/reports/income-statement/print?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`, "_blank"); });
  load();
}

async function viewCashFlow(main) {
  main.innerHTML = `<h2>现金流量表</h2>
    <div class="toolbar">
      <label>从 <input id="cf-from" value="${esc(state.current)}" style="width:90px" /></label>
      <label>至 <input id="cf-to" value="${esc(state.current)}" style="width:90px" /></label>
      <button class="btn primary" id="cf-run">查询</button>
      <button class="btn ghost sm" id="cf-print">打印预览</button>
    </div>
    <div id="cf-result" class="muted">填写期间后点击查询</div>`;
  const lineHtml = (l) => `<tr><td class="muted">${esc(l.code)}</td><td>${esc(l.name)}</td><td class="num">${esc(l.net)}</td></tr>`;
  const section = (title, lines, net) => `<tr style="background:#f5f7fa;font-weight:600"><td colspan="3">${esc(title)}</td></tr>
    ${lines.map(lineHtml).join("")}
    <tr style="font-weight:600"><td colspan="2">${esc(title)}小计</td><td class="num">${esc(net)}</td></tr>`;
  const load = async () => {
    const f = $("#cf-from").value.trim(), t = $("#cf-to").value.trim();
    try {
      const r = await api(`/reports/cash-flow?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`);
      const head = `<th>项目编码</th><th>项目</th><th class="num">金额</th>`;
      const tail = `<tr style="font-weight:700;background:#fafafa"><td colspan="2">现金及现金等价物净增加额</td><td class="num">${esc(r.net_increase)}</td></tr>
        <tr><td colspan="2">加：期初现金及现金等价物余额</td><td class="num">${esc(r.begin_cash)}</td></tr>
        <tr style="font-weight:700;background:#fafafa"><td colspan="2">期末现金及现金等价物余额</td><td class="num">${esc(r.end_cash)}</td></tr>
        <tr><td colspan="3" class="muted">${r.ties ? "✔ 净增加额与货币资金变动勾稽一致" : "✖ 勾稽不符"}</td></tr>`;
      $("#cf-result").innerHTML = `<div class="muted" style="margin-bottom:8px">${esc(r.from)} 至 ${esc(r.to)}</div>
        <table class="grid"><thead><tr>${head}</tr></thead><tbody>
        ${section("经营活动产生的现金流量", r.operating, r.operating_net)}
        ${section("投资活动产生的现金流量", r.investing, r.investing_net)}
        ${section("筹资活动产生的现金流量", r.financing, r.financing_net)}
        ${tail}</tbody></table>`;
    } catch (e) { toast(e.message, "err"); }
  };
  $("#cf-run").addEventListener("click", load);
  $("#cf-print").addEventListener("click", () => { const f = $("#cf-from").value, t = $("#cf-to").value; window.open(`/api/reports/cash-flow/print?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`, "_blank"); });
  load();
}

// ===========================================================================
// 安全中心（用户管理）
// ===========================================================================
async function viewSecurity(main) {
  const roles = await loadRoles();
  main.innerHTML = `
    <h2>安全中心</h2>
    <div class="panel">
      <div style="display:flex;gap:14px;align-items:center;flex-wrap:wrap">
        <button class="btn sm" id="me-pwd">修改我的口令</button>
        ${can("user_manage") ? `<button class="btn sm" id="new-user">新建用户</button>` : ""}
        ${can("user_manage") ? `<span class="spacer"></span>
          <label>搜索 <input id="u-kw" style="width:150px" placeholder="账号 / 姓名" /></label>
          <label>角色 <select id="u-role"><option value="">全部</option>${roles.map((r) => `<option value="${r.role}">${esc(r.label)}</option>`).join("")}</select></label>` : ""}
      </div>
    </div>
    ${can("user_manage") ? `<div class="panel"><table class="grid" id="u-table"><thead><tr>
      <th data-sort="username" class="sortable" title="点击排序">账号</th><th>姓名</th><th>角色</th>
      <th data-sort="last_login_at" class="sortable" title="点击排序">最近登录</th><th>锁定</th><th>强制改密</th><th>绑定设备</th><th>状态</th><th></th>
    </tr></thead><tbody><tr><td colspan="9" class="muted">加载中…</td></tr></tbody></table></div>` : `<div class="panel muted">您没有用户管理权限，仅可修改自己的口令。</div>`}
    ${session.platformAdmin ? `
    <div class="panel">
      <h3 style="margin-top:0">口令策略（平台账号）</h3>
      <div class="muted" style="font-size:12px;margin-bottom:8px">对 Web 平台账号生效：登录锁定、改密校验、空闲登出。桌面端账套口令策略在各账套「安全中心」单独设置。</div>
      <div id="sec-pol-form" class="muted">加载中…</div>
    </div>
    <div class="panel">
      <h3 style="margin-top:0">平台登录审计</h3>
      <div style="display:flex;gap:10px;align-items:center;flex-wrap:wrap;margin-bottom:8px">
        <label>账号 <input id="sec-att-user" style="width:140px" placeholder="留空 = 全部" /></label>
        <label>条数 <select id="sec-att-limit"><option>50</option><option>200</option></select></label>
        <button class="btn sm" id="sec-att-load">刷新</button>
        <span class="spacer"></span>
        <label>解锁账号 <input id="sec-unlock-user" style="width:140px" placeholder="账号" /></label>
        <button class="btn sm" id="sec-unlock">解锁</button>
      </div>
      <div id="sec-locked-box"></div>
      <div id="sec-att-table" class="muted">加载中…</div>
    </div>` : ""}`;
  $("#me-pwd").addEventListener("click", () => openChangePwd(false));
  if (session.platformAdmin) {
    loadSecPolicy();
    loadSecAudit();
    $("#sec-att-load").addEventListener("click", loadSecAudit);
    $("#sec-att-user").addEventListener("keydown", (e) => { if (e.key === "Enter") loadSecAudit(); });
    $("#sec-unlock").addEventListener("click", doSecUnlock);
  }
  if (can("user_manage")) {
    $("#new-user").addEventListener("click", openNewUser);
    $("#u-kw").addEventListener("input", loadUsers);
    $("#u-role").addEventListener("change", loadUsers);
    $all(".sortable", main).forEach((th) => th.addEventListener("click", () => {
      const key = th.dataset.sort;
      // 排序字段与方向：再次点击同列翻转；切换列时重置为升序
      window._usersSort = { key, asc: window._usersSort && window._usersSort.key === key ? !window._usersSort.asc : true };
      $all(".sortable", main).forEach((h) => h.classList.remove("sort-asc", "sort-desc"));
      th.classList.add(window._usersSort.asc ? "sort-asc" : "sort-desc");
      loadUsers();
    }));
    loadUsers();
  }
}

// ===========================================================================
// 平台安全中心（口令策略 / 登录审计 / 解锁；仅平台管理员）
// ===========================================================================
async function loadSecPolicy() {
  const box = $("#sec-pol-form");
  if (!box) return;
  let p;
  try { p = await api("/security/policy"); }
  catch (e) { box.innerHTML = `<span style="color:var(--err)">${esc(e.message)}</span>`; return; }
  box.innerHTML = `
    <div style="display:flex;gap:12px;flex-wrap:wrap">
      <div><label>最小口令长度</label><input id="sp-len" type="number" min="1" max="64" value="${p.min_len}" style="width:90px" /></div>
      <div><label>连续失败次数</label><input id="sp-fail" type="number" min="1" max="20" value="${p.max_fail}" style="width:90px" /></div>
      <div><label>锁定时长（分钟）</label><input id="sp-lock" type="number" min="1" max="1440" value="${p.lock_minutes}" style="width:100px" /></div>
      <div><label>空闲登出（分钟，0=不登出）</label><input id="sp-idle" type="number" min="0" max="1440" value="${p.idle_minutes}" style="width:100px" /></div>
      <div><label>口令有效期（天，0=永不过期）</label><input id="sp-age" type="number" min="0" max="3650" value="${p.max_age_days}" style="width:100px" /></div>
    </div>
    <div style="display:flex;gap:14px;align-items:center;margin:10px 0;flex-wrap:wrap">
      <label style="display:inline-flex;gap:4px;align-items:center"><input type="checkbox" id="sp-letter" ${p.need_letter ? "checked" : ""}/> 必须含字母</label>
      <label style="display:inline-flex;gap:4px;align-items:center"><input type="checkbox" id="sp-digit" ${p.need_digit ? "checked" : ""}/> 必须含数字</label>
      <label style="display:inline-flex;gap:4px;align-items:center"><input type="checkbox" id="sp-symbol" ${p.need_symbol ? "checked" : ""}/> 必须含特殊字符</label>
      <span class="spacer"></span>
      <button class="btn primary sm" id="sp-save">保存策略</button>
    </div>
    <div class="muted" style="font-size:12px">锁定语义：连续失败达阈值后账号写入持久锁（跨重启生效），可在下方「解锁账号」解除；策略同时约束改密校验与空闲登出。</div>`;
  $("#sp-save").addEventListener("click", async () => {
    const numv = (id) => Number($(id).value);
    const body = {
      min_len: numv("#sp-len"), need_letter: $("#sp-letter").checked, need_digit: $("#sp-digit").checked,
      need_symbol: $("#sp-symbol").checked, max_age_days: numv("#sp-age"), max_fail: numv("#sp-fail"),
      lock_minutes: numv("#sp-lock"), idle_minutes: numv("#sp-idle"),
    };
    try {
      await api("/security/policy", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
      toast("口令策略已保存", "ok");
      loadSecPolicy();
    } catch (e) { toast(e.message, "err"); }
  });
}

async function loadSecAudit() {
  const box = $("#sec-att-table");
  if (!box) return;
  const u = ($("#sec-att-user").value || "").trim();
  const limit = $("#sec-att-limit").value || "50";
  let r;
  try { r = await api(`/security/login-attempts?limit=${limit}${u ? `&username=${encodeURIComponent(u)}` : ""}`); }
  catch (e) { box.innerHTML = `<span style="color:var(--err)">${esc(e.message)}</span>`; return; }
  // 锁定账号条（与审计分开接口，失败不阻塞审计展示）
  const lockBox = $("#sec-locked-box");
  if (lockBox) {
    try {
      const lk = await api("/security/locked-users");
      lockBox.innerHTML = (lk.items || []).length
        ? `<div style="margin-bottom:8px">🔒 当前锁定：${lk.items.map((x) => `<span class="tag warn">${esc(x.username)}（剩 ${x.remaining_min} 分钟）</span>`).join(" ")}</div>`
        : "";
    } catch (e) { lockBox.innerHTML = ""; }
  }
  const items = r.items || [];
  if (!items.length) { box.innerHTML = `<div class="muted">暂无登录记录</div>`; return; }
  box.innerHTML = `<table class="grid"><thead><tr><th>时间</th><th>账号</th><th>结果</th><th>来源 IP</th></tr></thead><tbody>${
    items.map((x) => `<tr>
      <td>${esc(x.ts)}</td><td>${esc(x.username)}</td>
      <td>${x.ok ? `<span class="tag ok">成功</span>` : `<span class="tag err">失败</span>`}</td>
      <td class="muted">${esc(x.ip || "—")}</td>
    </tr>`).join("")
  }</tbody></table>`;
}

async function doSecUnlock() {
  const u = ($("#sec-unlock-user").value || "").trim();
  if (!u) { toast("请输入要解锁的账号", "err"); return; }
  try {
    await api("/security/unlock", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ username: u }) });
    toast(`已解锁 ${u}`, "ok");
    $("#sec-unlock-user").value = "";
    loadSecAudit();
  } catch (e) { toast(e.message, "err"); }
}

// 角色 → 权限矩阵（/api/roles），缓存一次
let rolesCache = null;
async function loadRoles() {
  if (rolesCache) return rolesCache;
  try { rolesCache = await api("/roles"); } catch (e) { rolesCache = []; }
  return rolesCache;
}
function rolePermsHtml(perms) {
  if (!perms || !perms.length) return `<span class="muted">无权限</span>`;
  return perms.map((p) => `<span class="tag">${esc(p.label)}</span>`).join(" ");
}

async function loadUsers() {
  const tb = $("#u-table tbody");
  const kw = ($("#u-kw") ? $("#u-kw").value : "").trim().toLowerCase();
  const roleF = $("#u-role") ? $("#u-role").value : "";
  let users;
  try { users = await api("/users"); } catch (e) { tb.innerHTML = `<tr><td colspan="9" style="color:var(--err)">${esc(e.message)} <button class="btn ghost sm" id="u-retry">重试</button></td></tr>`; const rb = $("#u-retry", tb); if (rb) rb.addEventListener("click", loadUsers); return; }
  users = users.filter((u) => {
    if (kw && !((u.username || "").toLowerCase().includes(kw) || (u.display_name || "").toLowerCase().includes(kw))) return false;
    if (roleF && u.role !== roleF && !(u.roles || []).includes(roleF)) return false;
    return true;
  });
  // 列排序（账号 / 最近登录）
  const sort = window._usersSort;
  if (sort) {
    const dir = sort.asc ? 1 : -1;
    users.sort((a, b) => {
      const av = sort.key === "username" ? (a.username || "") : (a.last_login_at || "");
      const bv = sort.key === "username" ? (b.username || "") : (b.last_login_at || "");
      return av < bv ? -dir : av > bv ? dir : 0;
    });
  }
  if (!users.length) { tb.innerHTML = `<tr><td colspan="9" class="muted">暂无匹配用户</td></tr>`; return; }
  // 缓存当前页用户，供「编辑」弹窗按用户名取完整对象
  window._usersCache = users;
  tb.innerHTML = users.map((u) => {
    // role_label 由后端提供（Role::label），避免前端硬编码与角色扩展脱节
    const roleLabel = u.role_label || u.role;
    const dev = u.device_name ? `<span class="tag">${esc(u.device_name)}</span>` : `<span class="muted">未绑定</span>`;
    const dis = u.disabled ? `<span class="tag err">已停用</span>` : `<span class="tag ok">启用</span>`;
    const lock = u.locked_until ? `<span class="tag warn">已锁定</span>` : `<span class="muted">—</span>`;
    const must = u.must_change_pwd ? `<span class="tag warn">是</span>` : `<span class="muted">否</span>`;
    const last = u.last_login_at ? esc(u.last_login_at) : `<span class="muted">从未登录</span>`;
    const me = u.username === session.user.username;
    return `<tr>
      <td>${esc(u.username)}${u.memo ? `<div class="muted" style="font-size:11px">${esc(u.memo)}</div>` : ""}</td>
      <td>${esc(u.display_name)}</td><td>${esc(roleLabel)}</td>
      <td>${last}</td><td>${lock}</td><td>${must}</td>
      <td>${dev}</td><td>${dis}</td>
      <td class="row-actions">
        <button class="btn ghost sm" data-edit="${esc(u.username)}">编辑</button>
        <button class="btn ghost sm" data-reset-pwd="${esc(u.username)}">重置口令</button>
        <button class="btn ghost sm" data-reset-dev="${esc(u.username)}">重置设备</button>
        ${u.locked_until ? `<button class="btn ghost sm" data-unlock="${esc(u.username)}">解锁</button>` : ""}
        ${me ? "" : `<button class="btn ghost sm" data-toggle="${esc(u.username)}" data-next="${u.disabled ? "0" : "1"}">${u.disabled ? "启用" : "停用"}</button>
        <button class="btn danger sm" data-del="${esc(u.username)}">删除</button>`}
      </td>
    </tr>`;
  }).join("");
  $all("[data-edit]").forEach((b) => b.onclick = () => { const u = (window._usersCache || []).find((x) => x.username === b.dataset.edit); if (u) openEditUser(u); });
  $all("[data-reset-pwd]").forEach((b) => b.onclick = () => openAdminResetPwd(b.dataset.resetPwd));
  $all("[data-toggle]").forEach((b) => b.onclick = async () => {
    const dis = b.dataset.next === "1";
    if (dis && !(await confirmDialog(`停用 ${b.dataset.toggle}？其全部会话将被立即下线。`, true))) return;
    try {
      await api(`/users/${encodeURIComponent(b.dataset.toggle)}`, { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ disabled: dis }) });
      toast(dis ? "已停用，该账号全部会话已下线" : "已启用", "ok"); loadUsers();
    } catch (e) { toast(e.message, "err"); }
  });
  $all("[data-reset-dev]").forEach((b) => b.onclick = async () => { if (!(await confirmDialog(`重置 ${b.dataset.resetDev} 的设备绑定？该账号可在新设备重新登录。`))) return; try { await api(`/users/${encodeURIComponent(b.dataset.resetDev)}/reset-device`, { method: "POST" }); toast("已重置设备绑定，其会话已下线", "ok"); loadUsers(); } catch (e) { toast(e.message, "err"); } });
  $all("[data-unlock]").forEach((b) => b.onclick = async () => { if (!(await confirmDialog(`解锁 ${b.dataset.unlock}？解锁后可重新登录。`))) return; try { await api(`/users/${encodeURIComponent(b.dataset.unlock)}/unlock`, { method: "POST" }); toast("已解锁", "ok"); loadUsers(); } catch (e) { toast(e.message, "err"); } });
  $all("[data-del]").forEach((b) => b.onclick = async () => { if (!(await confirmDialog(`删除用户 ${b.dataset.del}？删除后该账号无法登录（历史操作日志保留）。`, true))) return; try { await api(`/users/${encodeURIComponent(b.dataset.del)}`, { method: "DELETE" }); toast("已删除", "ok"); loadUsers(); } catch (e) { toast(e.message, "err"); } });
}

async function openNewUser() {
  const roles = await loadRoles();
  const mask = modal(`
    <h3>新建用户（账套内成员）</h3>
    <div class="banner set" style="margin-bottom:12px">此账号用于本账套内的角色分工。对方需已存在同名<b>账号</b>才能登录本账套；没有的请先让管理员在「账号管理」中开通。</div>
    <div class="field"><label>账号（须与已开通账号同名）</label><input id="nu-u" /></div>
    <div class="field"><label>姓名</label><input id="nu-n" /></div>
    <div class="field"><label>角色（主岗位）</label><select id="nu-r">${roles.map((r) => `<option value="${r.role}">${esc(r.label)}</option>`).join("")}</select></div>
    <div class="field"><label>兼任岗位（可多选 = 身兼多职；权限取并集，出纳签字与会计核心仍互斥）</label>
      <span style="display:flex;flex-wrap:wrap;gap:6px 14px">${roles.map((r) => `<label style="display:inline-flex;align-items:center;gap:4px"><input type="checkbox" class="nu-rr" value="${r.role}" />${esc(r.label)}</label>`).join("")}</span>
    </div>
    <div class="field"><label>初始口令（至少 6 位）</label><input id="nu-p" type="password" /></div>
    <div class="field"><label>备注</label><input id="nu-memo" /></div>
    <div class="field"><label><input type="checkbox" id="nu-must" checked /> 首次登录强制改密</label></div>
    <div id="nu-perm-box">${permMatrixHtml(roles, { role: "accountant", extra_perms: [], deny_perms: [] })}</div>
    <div class="foot"><button class="btn" id="nu-save">创建</button><button class="btn ghost" id="nu-cancel">取消</button></div>`);
  // 切换角色时重新生成矩阵（跟随角色的默认勾选随角色变化）
  const refreshMatrix = () => {
    const checkedRoles = $all(".nu-rr", mask).filter((c) => c.checked).map((c) => c.value);
    $("#nu-perm-box", mask).innerHTML = permMatrixHtml(roles, { role: $("#nu-r", mask).value, roles: checkedRoles, extra_perms: [], deny_perms: [] });
  };
  $("#nu-r", mask).addEventListener("change", refreshMatrix);
  $all(".nu-rr", mask).forEach((cb) => cb.addEventListener("change", refreshMatrix));
  $("#nu-cancel", mask).onclick = closeModal;
  $("#nu-save", mask).onclick = async () => {
    const extra = [], deny = [];
    $all("[data-perm]", mask).forEach((sel) => {
      if (sel.value === "on") extra.push(sel.dataset.perm);
      else if (sel.value === "off") deny.push(sel.dataset.perm);
    });
    const body = { username: $("#nu-u", mask).value.trim(), display_name: $("#nu-n", mask).value.trim(), password: $("#nu-p", mask).value, role: $("#nu-r", mask).value, roles: $all(".nu-rr", mask).filter((c) => c.checked).map((c) => c.value), memo: $("#nu-memo", mask).value.trim(), must_change_pwd: $("#nu-must", mask).checked, extra_perms: extra, deny_perms: deny };
    if (!body.username || body.password.length < 6) { toast("账号必填且口令至少 6 位", "err"); return; }
    try { await api("/users", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) }); toast("已创建用户", "ok"); closeModal(); loadUsers(); } catch (e) { toast(e.message, "err"); }
  };
}

// 用户编辑弹窗：角色预设基础上的逐项权限开关矩阵
function permMatrixHtml(roles, u) {
  // 全部权限清单（取所有角色权限的并集）
  const allPerms = [];
  const seen = new Set();
  (roles || []).forEach((r) => (r.perms || []).forEach((p) => { if (!seen.has(p.code)) { seen.add(p.code); allPerms.push(p); } }));
  const extraSet = new Set(u.extra_perms || []);
  const denySet = new Set(u.deny_perms || []);
  // 角色预设权限：按 code 收集（perms 是 {code,label} 对象数组，需映射为 code，
  // 否则 new Set 存的是对象、.has(p.code) 永远为 false，导致"角色默认"全部显示为 ✖）
  // 主岗位 + 兼任岗位的权限并集（身兼多职）
  const roleCodes = new Set();
  [u.role, ...(u.roles || [])].forEach((rc) => {
    ((roles.find((x) => x.role === rc) || {}).perms || []).forEach((p) => roleCodes.add(p.code));
  });
  const stateOf = (code) => (denySet.has(code) ? "off" : extraSet.has(code) ? "on" : "role");
  const rows = allPerms.map((p) => {
    const granters = [u.role, ...(u.roles || [])].filter((rc) =>
      ((roles.find((x) => x.role === rc) || {}).perms || []).some((pp) => pp.code === p.code)
    ).map((rc) => ((roles.find((x) => x.role === rc) || {}).label) || rc);
    const base = granters.length ? `（来自：${granters.join(" + ")}）` : "（无岗位默认）";
    const cur = stateOf(p.code);
    return `<tr><td>${esc(p.label)}</td><td><select data-perm="${esc(p.code)}">
      <option value="role" ${cur === "role" ? "selected" : ""}>跟随角色 ${base}</option>
      <option value="on" ${cur === "on" ? "selected" : ""}>强制开启</option>
      <option value="off" ${cur === "off" ? "selected" : ""}>强制关闭</option>
    </select></td></tr>`;
  }).join("");
  return `<div class="panel">
    <div class="muted" style="margin-bottom:6px">权限明细（在角色预设基础上逐项覆盖）</div>
    <table class="grid"><thead><tr><th>功能权限</th><th style="width:240px">授权方式</th></tr></thead>
    <tbody>${rows}</tbody></table>
  </div>`;
}

async function openEditUser(u) {
  const roles = await loadRoles();
  const ds = u.data_scope || {};
  const depts = (ds.depts || []).join(",");
  const me = u.username === session.user.username;
  const mask = modal(`
    <h3>编辑用户 · ${esc(u.username)}</h3>
    <div class="field"><label>姓名</label><input id="eu-n" value="${esc(u.display_name)}" /></div>
    <div class="field"><label>角色（主岗位）</label><select id="eu-r">${roles.map((r) => `<option value="${r.role}" ${r.role === u.role ? "selected" : ""}>${esc(r.label)}</option>`).join("")}</select></div>
    <div class="field"><label>兼任岗位（可多选 = 身兼多职；权限取并集）</label>
      <span style="display:flex;flex-wrap:wrap;gap:6px 14px">${roles.map((r) => `<label style="display:inline-flex;align-items:center;gap:4px"><input type="checkbox" class="eu-rr" value="${r.role}" ${(u.roles || []).includes(r.role) ? "checked" : ""} />${esc(r.label)}</label>`).join("")}</span>
    </div>
    <div class="field"><label>备注</label><input id="eu-memo" value="${esc(u.memo || "")}" /></div>
    <div class="field"><label><input type="checkbox" id="eu-must" ${u.must_change_pwd ? "checked" : ""} /> 强制下次登录改密</label></div>
    <div class="field"><label><input type="checkbox" id="eu-dis" ${u.disabled ? "checked" : ""} ${me ? "disabled" : ""} /> 停用该账号</label></div>
    <div class="panel">
      <div class="muted" style="margin-bottom:6px">数据范围（留空 / 不勾选 = 不限制）</div>
      <div class="field"><label>可见部门（逗号分隔）</label><input id="eu-depts" value="${esc(depts)}" /></div>
      <div class="field"><label>科目范围 从 <input id="eu-acct-from" value="${esc(ds.account_from || "")}" style="width:90px" /> 至 <input id="eu-acct-to" value="${esc(ds.account_to || "")}" style="width:90px" /></label></div>
      <div class="field"><label><input type="checkbox" id="eu-own-v" ${ds.own_voucher_only ? "checked" : ""} /> 仅看本人填制的凭证</label></div>
      <div class="field"><label><input type="checkbox" id="eu-own-d" ${ds.own_doc_only ? "checked" : ""} /> 仅看本人经手的业务单据</label></div>
    </div>
    ${u.is_admin ? `<div class="panel muted">系统管理员默认拥有全部权限，不受逐项开关限制。</div>` : permMatrixHtml(roles, u)}
    <div class="foot"><button class="btn" id="eu-save">保存</button><button class="btn ghost" id="eu-cancel">取消</button></div>`);
  $("#eu-cancel", mask).onclick = closeModal;
  $("#eu-save", mask).onclick = async () => {
    const deptsVal = $("#eu-depts", mask).value.split(",").map((s) => s.trim()).filter(Boolean);
    const extra = [], deny = [];
    if (!u.is_admin) {
      $all("[data-perm]", mask).forEach((sel) => {
        const code = sel.dataset.perm;
        if (sel.value === "on") extra.push(code);
        else if (sel.value === "off") deny.push(code);
      });
    }
    const body = {
      display_name: $("#eu-n", mask).value.trim(),
      role: $("#eu-r", mask).value,
      roles: $all(".eu-rr", mask).filter((c) => c.checked).map((c) => c.value),
      memo: $("#eu-memo", mask).value.trim(),
      must_change_pwd: $("#eu-must", mask).checked,
      disabled: $("#eu-dis", mask).checked,
      data_scope: { depts: deptsVal, account_from: $("#eu-acct-from", mask).value.trim(), account_to: $("#eu-acct-to", mask).value.trim(), own_voucher_only: $("#eu-own-v", mask).checked, own_doc_only: $("#eu-own-d", mask).checked },
      extra_perms: extra,
      deny_perms: deny,
    };
    try { await api(`/users/${encodeURIComponent(u.username)}`, { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) }); toast("已保存", "ok"); closeModal(); loadUsers(); } catch (e) { toast(e.message, "err"); }
  };
}
function openAdminResetPwd(username) {
  const mask = modal(`
    <h3>重置口令 · ${esc(username)}</h3>
    <div class="field"><label>新口令（至少 6 位）</label><input id="rp-p" type="password" /></div>
    <div class="foot"><button class="btn" id="rp-save">重置</button><button class="btn ghost" id="rp-cancel">取消</button></div>`);
  $("#rp-cancel", mask).onclick = closeModal;
  $("#rp-save", mask).onclick = async () => {
    const np = $("#rp-p", mask).value;
    if (np.length < 6) { toast("口令至少 6 位", "err"); return; }
    try { await api(`/users/${encodeURIComponent(username)}/reset-password`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ new: np }) }); toast("已重置口令", "ok"); closeModal(); } catch (e) { toast(e.message, "err"); }
  };
}
// ===========================================================================
// 账号管理（仅管理员，作用于全局账号库，与账套内子账号无关）
// ===========================================================================
async function viewPlatformUsers(main) {
  main.innerHTML = `<h2>账号管理</h2>
    <div class="toolbar">
      <button class="btn primary" id="pu-add">＋ 开通账号</button>
      <span class="muted">管理员创建与管理账套；普通账号由管理员开通并邀请进入账套工作。</span>
    </div>
    <div id="pu-list" class="muted">加载中…</div>`;
  async function load() {
    try {
      const rows = await api("/platform/users");
      $("#pu-list").innerHTML = rows.length ? `<table class="grid"><thead><tr>
        <th>账号</th><th>展示名</th><th>类型</th><th>状态</th><th>设备</th><th>创建时间</th><th>操作</th></tr></thead>
        <tbody>${rows.map((x) => `<tr>
          <td>${esc(x.username)}</td><td>${esc(x.display_name)}</td>
          <td>${x.is_admin ? "管理员" : "普通账号"}</td>
          <td>${x.disabled ? `<span style="color:var(--err)">已停用</span>` : "正常"}</td>
          <td>${x.device_bound ? "已绑定" : "未绑定"}</td>
          <td class="muted">${esc(x.created_at)}</td>
          <td>
            <button class="btn sm" data-act="edit" data-u="${esc(x.username)}">编辑</button>
            <button class="btn sm" data-act="reset" data-u="${esc(x.username)}">重置口令</button>
            <button class="btn sm" data-act="dev" data-u="${esc(x.username)}">重置设备</button>
            <button class="btn sm ghost" data-act="del" data-u="${esc(x.username)}">删除</button>
          </td></tr>`).join("")}</tbody></table>` : `<div class="muted">暂无账号</div>`;
    } catch (e) { $("#pu-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  }
  $("#pu-add").addEventListener("click", () => {
    const mask = modal(`
      <h3>开通账号</h3>
      <div class="field"><label>账号（登录名）</label><input id="nc-u" autocomplete="off" /></div>
      <div class="field"><label>展示名</label><input id="nc-d" /></div>
      <div class="field"><label>初始口令（至少 6 位，首次登录会要求改密）</label><input id="nc-p" type="text" /></div>
      <div class="field"><label><input type="checkbox" id="nc-a" /> 设为管理员</label></div>
      <div class="foot"><button class="btn" id="nc-cancel">取消</button><button class="btn primary" id="nc-save">创建</button></div>`);
    $("#nc-cancel", mask).onclick = closeModal;
    $("#nc-save", mask).onclick = async () => {
      const username = $("#nc-u", mask).value.trim();
      const password = $("#nc-p", mask).value;
      if (!username) { toast("请输入账号", "err"); return; }
      if (password.length < 6) { toast("口令至少 6 位", "err"); return; }
      try {
        await api("/platform/users", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ username, display_name: $("#nc-d", mask).value.trim(), password, is_admin: $("#nc-a", mask).checked }) });
        toast(`账号「${esc(username)}」已开通`, "ok");
        closeModal(); load();
      } catch (e) { toast(e.message, "err"); }
    };
  });
  $("#pu-list").addEventListener("click", async (e) => {
    const btn = e.target.closest("button[data-act]");
    if (!btn) return;
    const u = btn.dataset.u, act = btn.dataset.act;
    if (act === "reset") {
      const mask = modal(`
        <h3>重置口令：${esc(u)}</h3>
        <div class="field"><label>新口令（至少 6 位）</label><input id="rp-n" type="text" /></div>
        <div class="foot"><button class="btn" id="rp-cancel">取消</button><button class="btn primary" id="rp-save">重置</button></div>`);
      $("#rp-cancel", mask).onclick = closeModal;
      $("#rp-save", mask).onclick = async () => {
        const np = $("#rp-n", mask).value;
        if (np.length < 6) { toast("口令至少 6 位", "err"); return; }
        try {
          await api(`/platform/users/${encodeURIComponent(u)}/reset-password`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ new: np }) });
          toast("口令已重置", "ok"); closeModal();
        } catch (err) { toast(err.message, "err"); }
      };
    } else if (act === "edit") {
      let info = null;
      try { info = (await api("/platform/users")).find((x) => x.username === u); } catch (err) {}
      const mask = modal(`
        <h3>编辑账号：${esc(u)}</h3>
        <div class="field"><label>展示名</label><input id="eu-d" value="${esc(info ? info.display_name : "")}" /></div>
        <div class="field"><label><input type="checkbox" id="eu-a" ${info && info.is_admin ? "checked" : ""} /> 管理员</label></div>
        <div class="field"><label><input type="checkbox" id="eu-x" ${info && info.disabled ? "checked" : ""} /> 停用该账号</label></div>
        <div class="foot"><button class="btn" id="eu-cancel">取消</button><button class="btn primary" id="eu-save">保存</button></div>`);
      $("#eu-cancel", mask).onclick = closeModal;
      $("#eu-save", mask).onclick = async () => {
        try {
          await api(`/platform/users/${encodeURIComponent(u)}`, { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ display_name: $("#eu-d", mask).value.trim(), is_admin: $("#eu-a", mask).checked, disabled: $("#eu-x", mask).checked }) });
          toast("已保存", "ok"); closeModal(); load();
        } catch (err) { toast(err.message, "err"); }
      };
    } else if (act === "dev") {
      if (!(await confirmDialog(`重置「${u}」的设备绑定？该账号将被强制下线，下次登录自动绑定新设备。`, true))) return;
      try { await api(`/platform/users/${encodeURIComponent(u)}/reset-device`, { method: "POST" }); toast("设备绑定已重置", "ok"); load(); } catch (err) { toast(err.message, "err"); }
    } else if (act === "del") {
      if (!(await confirmDialog(`确定删除账号「${u}」？该操作不可恢复（其创建的账套需先删除）。`, true))) return;
      try { await api(`/platform/users/${encodeURIComponent(u)}`, { method: "DELETE" }); toast("已删除", "ok"); load(); } catch (err) { toast(err.message, "err"); }
    }
  });
  await load();
}

// ===========================================================================
// 合并报表（跨账套汇总，仅平台管理员；v1 汇总，不含内部往来抵销）
// ===========================================================================
async function viewConsolidate(main) {
  main.innerHTML = `<h2>合并报表 <span class="muted" style="font-size:12px">跨账套汇总（管理员）</span></h2>
    <div class="toolbar">
      <label>期间 <input id="cs-period" value="${(state.current || "").replace("-", "")}" style="width:90px" placeholder="YYYYMM" /></label>
      <button class="btn primary sm" id="cs-run">合并汇总</button>
      <button class="btn ghost sm" id="cs-print">打印预览</button>
      <span class="muted" style="font-size:12px">口径：各账套仅已记账余额；v1 为汇总，内部往来抵销留待 v2</span>
    </div>
    <div class="panel"><div id="cs-books" class="muted">加载账套…</div></div>
    <div class="panel" style="margin-top:8px"><div id="cs-table" class="muted">选择账套后点「合并汇总」</div></div>`;
  let books = [];
  try {
    const r = await api("/books");
    books = r.books || [];
  } catch (e) { $("#cs-books").textContent = e.message; }
  $("#cs-books").innerHTML = books.length
    ? `<b style="font-size:12.5px">选择账套</b><div style="display:flex;gap:12px;flex-wrap:wrap;margin-top:6px">${books.map((b) => `<label style="display:inline-flex;gap:4px;align-items:center"><input type="checkbox" class="cs-bk" value="${esc(b.key)}" checked /> ${esc(b.company || b.key)} <span class="muted">(${esc(b.key)})</span></label>`).join("")}</div>`
    : `<div class="muted">无可合并账套</div>`;
  $("#cs-run").onclick = async () => {
    const keys = $all(".cs-bk").filter((x) => x.checked).map((x) => x.value);
    if (!keys.length) { toast("请至少选择一个账套", "err"); return; }
    try {
      const r = await api(`/consolidate/preview?period=${encodeURIComponent($("#cs-period").value.trim())}&books=${encodeURIComponent(keys.join(","))}`);
      const bks = r.books || [], rows = r.rows || [];
      $("#cs-table").innerHTML = `<b>合并汇总（${esc(r.period)}）</b><table class="grid" id="cs-grid" style="margin-top:6px"><thead><tr><th>科目</th><th>名称</th>${bks.map((b) => `<th class="num">${esc(b.company || b.key)}</th>`).join("")}<th class="num">合计</th></tr></thead><tbody>${rows.length
        ? rows.map((x) => `<tr><td>${esc(x.account_code)}</td><td>${esc(x.account_name)}</td>${bks.map((b) => `<td class="num">${fmt(x.values[b.key] || "0")}</td>`).join("")}<td class="num"><b>${fmt(x.total)}</b></td></tr>`).join("")
        : `<tr><td colspan="${bks.length + 3}" class="muted">所选账套该期间无余额</td></tr>`}</tbody></table><p class="muted" style="font-size:12px">${esc(r.note || "")}</p>`;
    } catch (e) { $("#cs-table").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  $("#cs-print").onclick = () => {
    const t = $("#cs-grid");
    if (!t) { toast("先执行合并汇总", "err"); return; }
    printPreview("合并汇总", t);
  };
}

async function viewPlatformBooks(main) {
  main.innerHTML = `<h2>全部账套</h2>
    <div class="muted" style="margin-bottom:10px">管理员可进入任意账套查看：以临时管理员身份进入，不在该账套留下账号记录，操作会记入账套审计日志。</div>
    <div id="pb-list" class="muted">加载中…</div>`;
  async function load() {
    try {
      const data = await loadMyBooks();
      const books = data.books || [];
      $("#pb-list").innerHTML = books.length ? `<table class="grid"><thead><tr>
        <th>账套标识</th><th>公司名称</th><th>归属用户</th><th>操作</th></tr></thead>
        <tbody>${books.map((b) => `<tr>
          <td>${esc(b.key)}</td><td>${esc(b.company || "（未命名）")}</td><td>${esc(b.owner)}</td>
          <td><button class="btn sm" data-key="${esc(b.key)}" data-company="${esc(b.company || b.key)}">进入</button>
          <button class="btn sm ghost" data-del="${esc(b.key)}" data-name="${esc(b.company || b.key)}">删除</button></td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">系统中还没有账套。普通用户登录后可自行创建。</div>`;
    } catch (e) { $("#pb-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  }
  $("#pb-list").addEventListener("click", (e) => {
    const btn = e.target.closest("button[data-key]");
    if (btn) { enterBook(btn.dataset.key, btn.dataset.company); return; }
    const del = e.target.closest("button[data-del]");
    if (del) deleteBook(del.dataset.del, del.dataset.name);
  });
  await load();
}

function openChangePwd(forced, onDone) {
  const mask = modal(`
    <h3>${forced ? "首次登录 / 口令已重置：请设置新口令" : "修改口令"}</h3>
    ${forced ? `<div class="banner unset" style="margin-bottom:12px">为安全起见，请先设置一个强度较高的新口令。</div>` : ""}
    <div class="field"><label>原口令</label><input id="cp-o" type="password" ${forced ? "placeholder='首次登录可留空'" : ""} /></div>
    <div class="field"><label>新口令（至少 6 位）</label><input id="cp-n" type="password" /></div>
    <div class="field"><label>确认新口令</label><input id="cp-c" type="password" /></div>
    <div class="foot"><button class="btn" id="cp-save">保存</button>${forced ? "" : `<button class="btn ghost" id="cp-cancel">取消</button>`}</div>`);
  if (!forced) $("#cp-cancel", mask).onclick = closeModal;
  $("#cp-save", mask).onclick = async () => {
    const oldp = $("#cp-o", mask).value, np = $("#cp-n", mask).value, cp = $("#cp-c", mask).value;
    if (np.length < 6) { toast("新口令至少 6 位", "err"); return; }
    if (np !== cp) { toast("两次输入不一致", "err"); return; }
    try {
      await api("/change-password", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ old: oldp, new: np }) });
      toast("口令已更新", "ok");
      closeModal();
      if (typeof onDone === "function") onDone();
    } catch (e) { toast(e.message, "err"); }
  };
}

// ===========================================================================
// 多栏账
// ===========================================================================
async function viewMultiColumn(main) {
  main.innerHTML = `<h2>多栏账</h2>
    <div class="toolbar">
      <label>主科目 <input id="mc-main" value="6602" style="width:90px" /></label>
      <label>栏目(逗号分隔) <input id="mc-cols" value="660201,660202,660203" style="width:220px" /></label>
      <label>期间 <input id="mc-period" value="${esc(state.current)}" style="width:90px" /></label>
      <button class="btn primary" id="mc-run">查询</button>
      <button class="btn ghost sm" id="mc-run-print">打印预览</button>
    </div>
    <div id="mc-result" class="muted">填写条件后点击查询</div>`;
  $("#mc-run").addEventListener("click", async () => {
  $("#mc-run-print").addEventListener("click", () => { const el = $("#mc-result").querySelector("table"); printPreview("多栏账", el); });
    const mainCode = $("#mc-main").value.trim();
    const cols = $("#mc-cols").value.split(",").map((s) => s.trim()).filter(Boolean);
    const p = $("#mc-period").value.trim();
    if (!mainCode || !cols.length) { toast("请填写主科目与栏目", "err"); return; }
    try {
      const r = await api(`/reports/multi-column?main=${encodeURIComponent(mainCode)}&cols=${encodeURIComponent(cols.join(","))}&from=${encodeURIComponent(p)}&to=${encodeURIComponent(p)}`);
      const rows = r.rows || [];
      const head = ["日期", "凭证号", "摘要", "发生额", ...cols, "余额"];
      $("#mc-result").innerHTML = `<table><thead><tr>${head.map((h) => `<th>${esc(h)}</th>`).join("")}</tr></thead>
        <tbody>${rows.map((x) => `<tr><td>${esc(x.date)}</td><td>${esc(x.voucher_no)}</td><td>${esc(x.summary)}</td>
          <td class="r">${fmt(x.amount)}</td>${cols.map((_, i) => `<td class="r">${fmt(x.cols[i])}</td>`).join("")}
          <td class="r">${fmt(x.balance)}</td></tr>`).join("")}</tbody></table>`;
    } catch (e) { toast(e.message, "err"); }
  });
}

// ===========================================================================
// 摘要汇总表
// ===========================================================================
async function viewSummaryTable(main) {
  main.innerHTML = `<h2>摘要汇总表</h2>
    <div class="toolbar">
      <label>期间 <input id="st-period" value="${esc(state.current)}" style="width:90px" /></label>
      <button class="btn primary" id="st-run">查询</button>
      <button class="btn ghost sm" id="st-run-print">打印预览</button>
    </div>
    <div id="st-result" class="muted">填写期间后点击查询</div>`;
  $("#st-run").addEventListener("click", async () => {
  $("#st-run-print").addEventListener("click", () => { const el = $("#st-result").querySelector("table"); printPreview("摘要汇总表", el); });
    const p = $("#st-period").value.trim();
    try {
      const r = await api(`/reports/summary-table?from=${encodeURIComponent(p)}&to=${encodeURIComponent(p)}`);
      const rows = r.rows || [];
      $("#st-result").innerHTML = `<table><thead><tr><th>摘要</th><th>凭证张数</th><th>借方发生额</th><th>贷方发生额</th></tr></thead>
        <tbody>${rows.map((x) => `<tr><td>${esc(x.summary)}</td><td class="r">${x.voucher_count}</td><td class="r">${fmt(x.debit)}</td><td class="r">${fmt(x.credit)}</td></tr>`).join("")}</tbody></table>`;
    } catch (e) { toast(e.message, "err"); }
  });
}

// ===========================================================================
// 财务指标
// ===========================================================================
async function viewRatios(main) {
  main.innerHTML = `<h2>财务指标分析</h2>
    <div class="toolbar">
      <label>期间 <input id="rt-period" value="${esc(state.current)}" style="width:90px" /></label>
      <button class="btn primary" id="rt-run">查询</button>
      <button class="btn ghost sm" id="rt-run-print">打印预览</button>
    </div>
    <div id="rt-result" class="muted">填写期间后点击查询</div>`;
  $("#rt-run").addEventListener("click", async () => {
  $("#rt-run-print").addEventListener("click", () => { const el = $("#rt-result").querySelector("table"); printPreview("财务指标分析", el); });
    const p = $("#rt-period").value.trim();
    try {
      const r = await api(`/reports/ratios?period=${encodeURIComponent(p)}`);
      const rows = r.ratios || [];
      $("#rt-result").innerHTML = `<table><thead><tr><th>指标</th><th>数值</th><th>计算公式</th></tr></thead>
        <tbody>${rows.map((x) => `<tr><td>${esc(x.name)}</td><td class="r">${esc(x.display)}</td><td class="muted">${esc(x.formula)}</td></tr>`).join("")}</tbody></table>`;
    } catch (e) { toast(e.message, "err"); }
  });
}

// ===========================================================================
// MPS 主生产计划 + 粗排 + 细排（链6：清单⑥）
async function viewMps(main) {
  main.innerHTML = `<h2>MPS 主生产计划 · 粗细排产</h2>
    <div class="panel">
      <div style="display:flex;align-items:center;gap:8px;flex-wrap:wrap"><h4 style="margin:0">① MPS 运行（需求 − 现有 − 在制 = 计划）</h4><span class="grow"></span>
        <label style="font-size:12px"><input type="checkbox" id="mps-sales" checked /> 从销售订单收集未发量</label>
        <label style="font-size:12px">手工 <input id="mps-item" placeholder="存货编码" style="width:110px" /> × <input id="mps-qty" placeholder="数量" style="width:70px" /></label>
        <label style="font-size:12px">建议交期 <input type="date" id="mps-due" /></label>
        <button class="btn primary" id="mps-run">运行 MPS</button>
        <button class="btn ghost" id="mps-latest">最近一次</button>
        <button class="btn ghost sm" id="mps-tomrp">计划量转 MRP</button>
      </div>
      <div id="mps-table" class="muted" style="margin-top:8px">运行后显示净算建议；「下达」一键生成已下达生产订单。</div>
    </div>
    <div class="panel" style="margin-top:12px">
      <div style="display:flex;align-items:center;gap:8px;flex-wrap:wrap"><h4 style="margin:0">② 粗排（件/日产能顺排）</h4><span class="grow"></span>
        <label style="font-size:12px">日产能 <input id="rq-daily" value="10" style="width:60px" /> 件/日</label>
        <button class="btn" id="rq-run">计算粗排</button>
        <button class="btn ghost sm" id="rq-apply-all">全部应用到订单</button>
      </div>
      <div id="rq-table" class="muted" style="margin-top:8px">计算后显示排期建议与按日负荷（超载标红）。</div>
    </div>
    <div class="panel" style="margin-top:12px">
      <div style="display:flex;align-items:center;gap:8px"><h4 style="margin:0">③ 细排结果（已写回订单的计划日期）</h4><span class="grow"></span><button class="btn ghost sm" id="sch-reload">刷新</button></div>
      <div id="sch-table" class="muted" style="margin-top:8px">「粗排-应用」写回后在此显示。</div>
    </div>`;
  // 建议交期默认今天 +7（内联计算——shift 是别处的局部函数）
  $("#mps-due").value = (() => {
    const d = new Date();
    d.setDate(d.getDate() + 7);
    return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
  })();
  let mpsRows = [];
  let roughRows = [];

  const renderMps = (rows) => {
    mpsRows = rows || [];
    $("#mps-table").innerHTML = mpsRows.length
      ? `<table class="grid"><thead><tr><th>存货</th><th class="num">需求</th><th class="num">现有</th><th class="num">在制</th><th class="num">计划量</th><th>来源</th><th>建议交期</th><th>状态</th><th></th></tr></thead><tbody>${mpsRows.map((r) => `<tr><td>${esc(r.item_code)}</td><td class="num">${fmt(r.demand)}</td><td class="num">${fmt(r.on_hand)}</td><td class="num">${fmt(r.wip)}</td><td class="num"><b>${fmt(r.planned)}</b></td><td>${esc(r.source)}</td><td>${esc(r.due_date)}</td><td>${r.status === "open" ? '<span class="tag">待下达</span>' : '<span class="tag ok">已下达</span>'}</td>
        <td class="row-actions">${r.status === "open" && Number(r.planned) > 0 ? `<button class="btn primary sm" data-mps-go="${r.id}">下达</button>` : ""}</td></tr>`).join("")}</tbody></table>`
      : `<div class="muted">暂无结果（先运行 MPS）</div>`;
    $all("[data-mps-go]").forEach((b) => b.onclick = async () => {
      try {
        const r = await postJson(`/mps/${b.dataset.mpsGo}/convert`, {});
        toast(`已下达 → 生产订单 ${r.order_no}（到「工序报工」开工/领料）`, "ok");
        loadMps();
      } catch (e) { toast(e.message, "err"); }
    });
  };
  const loadMps = async () => {
    try { const r = await api("/mps/latest"); renderMps(r.rows || []); } catch (e) { $("#mps-table").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  $("#mps-run").onclick = async () => {
    try {
      const demands = [];
      const it = $("#mps-item").value.trim();
      const q = $("#mps-qty").value.trim();
      if (it && q) demands.push({ item_code: it, qty: q, source: "手工" });
      const r = await postJson("/mps/run", { demands, from_sales: $("#mps-sales").checked, due_date: $("#mps-due").value });
      renderMps(r.rows || []);
      $("#mps-item").value = ""; $("#mps-qty").value = "";
      toast(`MPS 运算完成（${(r.rows || []).length} 行）`, "ok");
    } catch (e) { toast(e.message, "err"); }
  };
  $("#mps-latest").onclick = loadMps;
  $("#mps-tomrp").onclick = async () => {
    const demands = mpsRows
      .filter((r) => Number(r.planned) > 0)
      .map((r) => ({ item_code: r.item_code, qty: String(r.planned), source: "MPS" }));
    if (!demands.length) { toast("无正计划量行可转", "err"); return; }
    try {
      await postJson("/mrp/run", { demands });
      toast("已按 MPS 计划量运行 MRP（到「MRP 运算」页点「最近一次结果」）", "ok");
    } catch (e) { toast(e.message, "err"); }
  };

  const applySch = async (list) => {
    const use = list.filter((o) => o && o.sug_start && o.sug_end);
    if (!use.length) { toast("无可用排期", "err"); return; }
    try {
      const r = await postJson("/prod/schedule", { items: use.map((o) => ({ id: o.id, start: o.sug_start, end: o.sug_end })) });
      toast(`已写回 ${r.updated} 单计划日期`, "ok");
      loadSch();
    } catch (e) { toast(e.message, "err"); }
  };
  const renderRough = (orders, load) => {
    roughRows = orders || [];
    $("#rq-table").innerHTML = (roughRows.length
      ? `<table class="grid"><thead><tr><th>订单</th><th>存货</th><th class="num">未完工</th><th class="num">需天数</th><th>建议开工</th><th>建议完工</th><th></th></tr></thead><tbody>${roughRows.map((o) => `<tr><td>${esc(o.no)}</td><td>${esc(o.item_code)}</td><td class="num">${fmt(o.open_qty)}</td><td class="num">${o.need_days}</td><td>${esc(o.sug_start)}</td><td>${esc(o.sug_end)}</td>
        <td class="row-actions"><button class="btn ghost sm" data-sch="${o.id}">应用</button></td></tr>`).join("")}</tbody></table>`
      : `<div class="muted">无未完工自制订单</div>`)
      + ((load || []).length
        ? `<div style="margin-top:10px"><b style="font-size:12.5px">按日负荷</b><table class="grid" style="margin-top:4px"><thead><tr><th>日期</th><th class="num">负荷</th><th class="num">产能</th><th>状态</th></tr></thead><tbody>${load.map((l) => `<tr><td>${esc(l.date)}</td><td class="num">${fmt(l.qty)}</td><td class="num">${fmt(l.capacity)}</td><td>${l.over ? '<span class="tag err">超载</span>' : '<span class="tag ok">正常</span>'}</td></tr>`).join("")}</tbody></table></div>`
        : "");
    $all("[data-sch]").forEach((b) => b.onclick = () => applySch([roughRows.find((o) => String(o.id) === b.dataset.sch)]));
  };
  $("#rq-run").onclick = async () => {
    try {
      const r = await postJson("/mps/rough", { daily_qty: $("#rq-daily").value.trim() || "10" });
      renderRough(r.orders || [], r.load || []);
    } catch (e) { toast(e.message, "err"); }
  };
  $("#rq-apply-all").onclick = () => applySch(roughRows);

  const loadSch = async () => {
    try {
      const r = await api(`/prod?period=${ymm(state.current || "")}`);
      const rows = (r.orders || []).filter((o) => o.plan_start);
      $("#sch-table").innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>订单</th><th>存货</th><th class="num">计划量</th><th>计划开工</th><th>计划完工</th><th>状态</th></tr></thead><tbody>${rows.map((o) => `<tr><td>${esc(o.no)}</td><td>${esc(o.item_code)}</td><td class="num">${fmt(o.planned_qty)}</td><td>${esc(o.plan_start)}</td><td>${esc(o.plan_end)}</td><td>${esc(o.status)}</td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">暂无已排产订单（粗排后点「应用」）</div>`;
    } catch (e) { $("#sch-table").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  $("#sch-reload").onclick = loadSch;
  await Promise.all([loadMps(), loadSch()]);
}

// MRP 运算
// ===========================================================================
async function viewMrp(main) {
  main.innerHTML = `<h2>MRP 运算</h2>
    <div class="toolbar">
      <label>需求产品 <input id="mrp-item" style="width:120px" /></label>
      <label>数量 <input id="mrp-qty" value="10" style="width:80px" /></label>
      <button class="btn primary" id="mrp-run">运行</button>
      <button class="btn ghost" id="mrp-latest">最近一次结果</button>
    </div>
    <div id="mrp-result" class="muted">输入需求产品与数量后运行</div>`;
  const load = (rows) => {
    $("#mrp-result").innerHTML = `<table><thead><tr><th>层级</th><th>物料</th><th>毛需求</th><th>现有库存</th><th>净需求</th><th>计划量</th><th>行动</th><th>来源</th><th></th></tr></thead>
      <tbody>${rows.map((x) => `<tr><td>${x.level}</td><td>${esc(x.item_code)}</td><td class="r">${fmt(x.gross_req)}</td><td class="r">${fmt(x.on_hand)}</td><td class="r">${fmt(x.net_req)}</td><td class="r">${fmt(x.planned_qty)}</td><td>${x.action === "produce" ? "生产" : x.action === "purchase" ? "采购" : "无"}</td><td>${esc(x.source)}</td><td>${x.action === "produce" ? `<button class="btn ghost sm" data-mrp-go-item="${esc(x.item_code)}" data-mrp-go-qty="${esc(String(x.planned_qty))}">下达</button>` : x.action === "purchase" ? `<button class="btn ghost sm" data-mrp-req="${x.id}">下推请购</button>` : ""}</td></tr>`).join("")}</tbody></table>`;
    $all("[data-mrp-go-item]").forEach((b) => b.onclick = async () => {
      try {
        const r = await postJson("/prod", { item_code: b.dataset.mrpGoItem, qty: b.dataset.mrpGoQty });
        toast(`已下达生产订单 ${r.no}（到「工序报工」开工/领料）`, "ok");
      } catch (e) { toast(e.message, "err"); }
    });
    $all("[data-mrp-req]").forEach((b) => b.onclick = async () => {
      try {
        const r = await postJson(`/mrp/${b.dataset.mrpReq}/to-req`, {});
        toast(`已下推请购单 ${r.no}（到「采购单据」审批后下推采购订单）`, "ok");
        b.disabled = true; b.textContent = "已下推";
      } catch (e) { toast(e.message, "err"); }
    });
  };
  $("#mrp-run").addEventListener("click", async () => {
    const item = $("#mrp-item").value.trim();
    const qty = $("#mrp-qty").value.trim();
    if (!item || !qty) { toast("请填写需求产品与数量", "err"); return; }
    try {
      const r = await api("/mrp/run", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ demands: [{ item_code: item, qty, source: "手工" }] }) });
      load(r.rows || []);
      toast("MRP 运算完成", "ok");
    } catch (e) { toast(e.message, "err"); }
  });
  $("#mrp-latest").addEventListener("click", async () => {
    try { const r = await api("/mrp/latest"); load(r.rows || []); } catch (e) { toast(e.message, "err"); }
  });
}

// ===========================================================================
// 工艺路线
// ===========================================================================
async function viewRouting(main) {
  main.innerHTML = `<h2>工艺路线</h2>
    <div class="toolbar">
      <label>产品代码 <input id="rt-item" style="width:120px" /></label>
      <button class="btn primary" id="rt-load">加载</button>
    </div>
    <div id="rt-result" class="muted">输入产品代码后加载</div>`;
  $("#rt-load").addEventListener("click", async () => {
    const item = $("#rt-item").value.trim();
    if (!item) { toast("请输入产品代码", "err"); return; }
    try {
      const r = await api(`/routing/${encodeURIComponent(item)}`);
      const ops = r.ops || [];
      $("#rt-result").innerHTML = `<table><thead><tr><th>序号</th><th>工序编码</th><th>工序名称</th><th>工作中心</th><th>标准工时</th><th>小时费率</th><th>检验点</th></tr></thead>
        <tbody>${ops.map((o) => `<tr><td>${o.seq}</td><td>${esc(o.op_code)}</td><td>${esc(o.op_name)}</td><td>${esc(o.work_center)}</td><td class="r">${fmt(o.std_hours)}</td><td class="r">${fmt(o.rate)}</td><td>${o.qc_required ? '<span class="tag warn">需检验</span>' : '<span class="muted">—</span>'}</td></tr>`).join("")}</tbody></table>`;
      if (!ops.length) $("#rt-result").innerHTML = `<div class="muted">该产品暂无工艺路线。维护请调用 POST /api/routing/:item。</div>`;
    } catch (e) { toast(e.message, "err"); }
  });
}

// ===========================================================================
// 审批中心
// ===========================================================================
async function viewApproval(main) {
  main.innerHTML = `<h2>审批中心</h2><div id="ap-list" class="muted">加载中…</div>`;
  const load = async () => {
    try {
      const r = await api("/approvals/todo");
      const rows = r.rows || [];
      $("#ap-list").innerHTML = rows.length ? rows.map((a) => `
        <div class="card"><div class="row"><b>${esc(a.title)}</b> <span class="tag">${esc(a.biz_kind)}/${a.biz_id}</span></div>
          <div class="muted">申请人 ${esc(a.applicant)} · 当前节点 ${a.current_node}/${a.steps.length}</div>
          <div class="row" style="margin-top:8px">
            <button class="btn sm" data-ap="${a.id}" data-act="1">通过</button>
            <button class="btn danger sm" data-ap="${a.id}" data-act="0">驳回</button>
          </div>
        </div>`).join("") : `<div class="muted">没有待审批的单据</div>`;
      $all("[data-ap]").forEach((b) => b.onclick = async () => {
        const approve = b.dataset.act === "1";
        try { await api(`/approvals/${b.dataset.ap}/act`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ approve, comment: approve ? "同意" : "驳回" }) }); toast(approve ? "已通过" : "已驳回", "ok"); load(); } catch (e) { toast(e.message, "err"); }
      });
    } catch (e) { toast(e.message, "err"); }
  };
  load();
}

// ===========================================================================
// 报表附注
// ===========================================================================
async function viewNotes(main) {
  main.innerHTML = `<h2>报表附注</h2>
    <div class="toolbar">
      <label>报表 <select id="nt-key"><option value="balance_sheet">资产负债表</option><option value="income_statement">利润表</option><option value="cash_flow">现金流量表</option></select></label>
      <label>期间 <input id="nt-period" value="${esc(state.current)}" style="width:90px" /></label>
      <button class="btn primary" id="nt-load">加载</button>
    </div>
    <div id="nt-list"></div>
    <div class="card" style="margin-top:12px"><h3>新增附注</h3>
      <div class="field"><label>标题</label><input id="nt-title" /></div>
      <div class="field"><label>内容</label><textarea id="nt-content" rows="3"></textarea></div>
      <button class="btn primary" id="nt-save">保存</button>
    </div>`;
  const load = async () => {
    const key = $("#nt-key").value, p = $("#nt-period").value.trim();
    try {
      const r = await api(`/reports/notes?report_key=${key}&period=${encodeURIComponent(p)}`);
      const rows = r.rows || [];
      $("#nt-list").innerHTML = rows.map((n) => `<div class="card"><div class="row"><b>${n.seq} · ${esc(n.title)}</b><button class="btn danger sm" data-nt-del="${n.id}">删除</button></div><div>${esc(n.content)}</div><div class="muted">${esc(n.updated_by)} ${esc(n.updated_at)}</div></div>`).join("") || `<div class="muted">暂无附注</div>`;
      $all("[data-nt-del]").forEach((b) => b.onclick = async () => { if (!(await confirmDialog("删除该附注？", true))) return; try { await api(`/reports/notes/${b.dataset.ntDel}/delete`, { method: "POST" }); toast("已删除", "ok"); load(); } catch (e) { toast(e.message, "err"); } });
    } catch (e) { toast(e.message, "err"); }
  };
  $("#nt-load").addEventListener("click", load);
  $("#nt-save").addEventListener("click", async () => {
    const key = $("#nt-key").value, p = $("#nt-period").value.trim();
    const title = $("#nt-title").value.trim(), content = $("#nt-content").value.trim();
    if (!title) { toast("标题不能为空", "err"); return; }
    try {
      await api("/reports/notes", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ report_key: key, period: ymm(p), title, content }) });
      toast("已保存附注", "ok");
      $("#nt-title").value = ""; $("#nt-content").value = "";
      load();
    } catch (e) { toast(e.message, "err"); }
  });
  load();
}

// ===========================================================================
// 电子档案
// ===========================================================================
async function viewArchive(main) {
  main.innerHTML = `<h2>会计电子档案</h2>
    <div class="toolbar">
      <label>期间 <input id="ar-period" value="${esc(state.current)}" style="width:90px" /></label>
      <label>类型(空=全部) <input id="ar-kind" placeholder="voucher/ledger/report/balance" style="width:180px" /></label>
      <button class="btn primary" id="ar-load">加载</button>
    </div>
    <div id="ar-list"></div>
    <div class="card" style="margin-top:12px"><h3>新增归档</h3>
      <div class="field"><label>类型</label><input id="ar-new-kind" value="voucher" style="width:120px" /></div>
      <div class="field"><label>标题</label><input id="ar-new-title" /></div>
      <div class="field"><label>内容(JSON)</label><textarea id="ar-new-payload" rows="3"></textarea></div>
      <button class="btn primary" id="ar-save">归档</button>
    </div>`;
  const load = async () => {
    const p = $("#ar-period").value.trim(), k = $("#ar-kind").value.trim();
    try {
      const r = await api(`/archives?period=${encodeURIComponent(p)}${k ? "&kind=" + encodeURIComponent(k) : ""}`);
      const rows = r.rows || [];
      $("#ar-list").innerHTML = rows.map((a) => `<div class="card"><div class="row"><b>${esc(a.file_no)}</b> <span class="tag">${esc(a.kind)}</span> <span class="tag ok">${a.sealed ? "封存" : "未封存"}</span></div><div>${esc(a.title)}</div><div class="muted">${esc(a.archived_by)} ${esc(a.archived_at)} · ${esc(a.content_hash).slice(0, 16)}</div></div>`).join("") || `<div class="muted">暂无档案</div>`;
    } catch (e) { toast(e.message, "err"); }
  };
  $("#ar-load").addEventListener("click", load);
  $("#ar-save").addEventListener("click", async () => {
    const p = $("#ar-period").value.trim(), kind = $("#ar-new-kind").value.trim();
    const title = $("#ar-new-title").value.trim(), payload = $("#ar-new-payload").value.trim();
    if (!title || !payload) { toast("标题与内容不能为空", "err"); return; }
    try {
      await api("/archives", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ period: ymm(p), kind, title, payload }) });
      toast("已归档", "ok");
      $("#ar-new-title").value = ""; $("#ar-new-payload").value = "";
      load();
    } catch (e) { toast(e.message, "err"); }
  });
  load();
}

// ===========================================================================
// 预算版本
// ===========================================================================
async function viewBudgetVersions(main) {
  main.innerHTML = `<h2>预算版本管理</h2>
    <div id="bv-list" class="muted">加载中…</div>
    <div class="card" style="margin-top:12px"><h3>新建版本</h3>
      <div class="field"><label>版本编码</label><input id="bv-key" placeholder="v2" style="width:140px" /></div>
      <div class="field"><label>版本名称</label><input id="bv-name" placeholder="2026 调整版" /></div>
      <div class="field"><label>备注</label><input id="bv-memo" /></div>
      <label class="muted"><input type="checkbox" id="bv-copy" checked /> 从当前版本复制数据</label>
      <div style="margin-top:8px"><button class="btn primary" id="bv-save">创建版本</button></div>
    </div>
    <div class="card" style="margin-top:12px"><h3>预算编制（当前版本）</h3>
      <div class="toolbar">
        <label>期间 <input id="be-per" value="${(state.current || "").replace("-", "")}" style="width:90px" placeholder="YYYYMM" /></label>
        <button class="btn sm" id="be-load">加载</button>
        <span class="grow"></span>
        <button class="btn primary sm" id="be-new">新增预算行</button>
      </div>
      <div id="be-list" class="muted">选择期间后加载</div>
      <p class="muted" style="font-size:12px;margin-top:6px">口径：执行额 = 会计年度 1 月至该期间已记账发生额；账套参数可设「超预算提醒 / 强控」（凭证保存时生效）。</p>
    </div>`;
  const load = async () => {
    try {
      const r = await api("/budget/versions");
      const versions = r.versions || [], current = r.current || "";
      $("#bv-list").innerHTML = versions.length ? versions.map((v) => `
        <div class="card"><div class="row">
          <b>${v.is_current ? "●" : "○"} ${esc(v.name)}</b> <span class="tag">${esc(v.key)}</span>
          <span class="grow"></span>
          ${v.is_current ? `<span class="tag ok">当前版本</span>` : `<button class="btn ghost sm" data-bv-act="${esc(v.key)}">设为当前</button>`}
          <button class="btn danger sm" data-bv-del="${esc(v.key)}">删除</button>
        </div><div class="muted">${esc(v.created_at)} ${esc(v.memo)}</div></div>`).join("")
        : `<div class="muted">暂无自定义版本，预算存于「默认」版本。当前版本：${current || "默认"}</div>`;
      $all("[data-bv-act]").forEach((b) => b.onclick = async () => { try { await api(`/budget/versions/${encodeURIComponent(b.dataset.bvAct)}/activate`, { method: "POST" }); toast("已设为当前版本", "ok"); load(); } catch (e) { toast(e.message, "err"); } });
      $all("[data-bv-del]").forEach((b) => b.onclick = async () => { if (!(await confirmDialog(`删除版本 ${b.dataset.bvDel} 及其全部预算数据？`, true))) return; try { await api(`/budget/versions/${encodeURIComponent(b.dataset.bvDel)}/delete`, { method: "POST" }); toast("已删除", "ok"); load(); } catch (e) { toast(e.message, "err"); } });
    } catch (e) { toast(e.message, "err"); }
  };
  $("#bv-save").addEventListener("click", async () => {
    const key = $("#bv-key").value.trim(), name = $("#bv-name").value.trim(), memo = $("#bv-memo").value.trim();
    if (!key || !name) { toast("版本编码与名称不能为空", "err"); return; }
    try {
      await api("/budget/versions", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ key, name, memo, is_current: false }) });
      if ($("#bv-copy").checked) {
        const r = await api("/budget/versions");
        const current = r.current || "";
        await api("/budget/versions/copy", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ from: current, to: key }) });
      }
      toast("版本已创建", "ok");
      $("#bv-key").value = ""; $("#bv-name").value = ""; $("#bv-memo").value = "";
      load();
    } catch (e) { toast(e.message, "err"); }
  });
  // 预算编制：当前版本的预算行 CRUD
  const loadRows = async () => {
    try {
      const r = await api(`/budget/rows?period=${encodeURIComponent($("#be-per").value.trim())}`);
      const rows = r.rows || [];
      $("#be-list").innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>科目</th><th>部门</th><th class="num">预算金额</th><th>备注</th><th></th></tr></thead><tbody>${rows.map((x) => `<tr><td>${esc(x.account_code)}</td><td>${esc(x.dept || "—")}</td><td class="num">${fmt(x.amount)}</td><td>${esc(x.memo || "")}</td><td class="row-actions"><button class="btn ghost sm" data-be-del="${x.id}">删</button></td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">该期间（版本：${esc(r.version || "默认")}）暂无预算行</div>`;
      $all("[data-be-del]").forEach((b) => b.onclick = async () => {
        if (!(await confirmDialog("删除该预算行？", true))) return;
        try { await api(`/budget/rows/${b.dataset.beDel}/delete`, { method: "POST" }); toast("已删除", "ok"); loadRows(); } catch (e) { toast(e.message, "err"); }
      });
    } catch (e) { $("#be-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  $("#be-load").onclick = loadRows;
  $("#be-new").onclick = () => {
    const m = modal(`<h3>新增预算行（当前版本）</h3>
      <div class="field"><label>期间 *</label><input id="be2-per" value="${esc($("#be-per").value.trim())}" style="width:100px" /></div>
      <div class="field"><label>科目编码 *</label><input id="be2-acct" placeholder="如 660201" /></div>
      <div class="field"><label>部门（可空 = 全公司口径）</label><input id="be2-dept" /></div>
      <div class="field"><label>预算金额 *</label><input id="be2-amt" /></div>
      <div class="field"><label>备注</label><input id="be2-memo" /></div>
      <div class="foot"><button class="btn primary" id="be2-save">保存</button><button class="btn ghost" id="be2-cancel">取消</button></div>`);
    $("#be2-cancel", m).onclick = closeModal;
    $("#be2-save", m).onclick = async () => {
      try {
        await postJson("/budget/rows", { period: parseInt($("#be2-per", m).value.trim(), 10) || 0, account_code: $("#be2-acct", m).value.trim(), dept: $("#be2-dept", m).value.trim(), amount: $("#be2-amt", m).value.trim(), memo: $("#be2-memo", m).value.trim() });
        toast("已保存预算行", "ok"); closeModal(); loadRows();
      } catch (e) { toast(e.message, "err"); }
    };
  };
  loadRows();
  load();
}

// 新建委外订单（委外=生产的变体：BOM/领料/开工/完工全复用生产链，加工费独立确认）
function openOutsourceEditor(reload) {
  const mask = modal(`<h3>新建委外订单</h3>
    <div class="field"><label>加工产品编码 *</label><input id="os-item" placeholder="如 140501" /></div>
    <div class="field"><label>数量 *</label><input id="os-qty" value="1" /></div>
    <div class="field"><label>供应商编码 *</label><input id="os-sup" placeholder="如 S01" /></div>
    <div class="field"><label>供应商名称</label><input id="os-supn" /></div>
    <div class="field"><label>日期</label><input type="date" id="os-date" value="${today()}" /></div>
    <p class="muted" style="font-size:12px;margin:6px 0 0">委外订单复用生产链：维护 BOM → 领料出库（发材料）→ 开工 → 确认加工费 → 完工入库。</p>
    <div class="foot"><button class="btn primary" id="os-save">创建</button><button class="btn ghost" id="os-cancel">取消</button></div>`);
  $("#os-cancel", mask).onclick = closeModal;
  $("#os-save", mask).onclick = async () => {
    const item = $("#os-item", mask).value.trim();
    const qty = $("#os-qty", mask).value.trim();
    const sup = $("#os-sup", mask).value.trim();
    if (!item || !qty || !sup) { toast("产品编码、数量、供应商必填", "err"); return; }
    try {
      const r = await postJson("/prod", {
        item_code: item, qty, kind: "outsourcing",
        supplier_code: sup, supplier_name: $("#os-supn", mask).value.trim(), date: $("#os-date", mask).value,
      });
      toast(`已创建委外订单 ${r.no}（BOM → 领料 → 开工 → 加工费 → 完工）`, "ok");
      closeModal();
      reload && reload();
    } catch (e) { toast(e.message, "err"); }
  };
}

// 委外加工费确认：借 500102 / 贷应付(供应商辅助)；并入订单人工要素随完工结转
function openFeeEditor(order, reload) {
  if (!order) { toast("请先选择生产订单", "err"); return; }
  if (order.order_kind !== "outsourcing") { toast("仅委外订单可确认加工费", "err"); return; }
  const mask = modal(`<h3>确认加工费 · 委外 ${esc(order.no || "")}</h3>
    <div class="field"><label>加工费金额 *</label><input id="fe-amt" placeholder="不含税金额" /></div>
    <div class="field"><label>日期</label><input type="date" id="fe-date" value="${today()}" /></div>
    <p class="muted" style="font-size:12px;margin:6px 0 0">凭证：借 500102 生产成本-直接人工 / 贷 应付账款（供应商辅助）；金额并入该订单人工要素，完工时随库存商品结转。</p>
    <div class="foot"><button class="btn primary" id="fe-save">确认</button><button class="btn ghost" id="fe-cancel">取消</button></div>`);
  $("#fe-cancel", mask).onclick = closeModal;
  $("#fe-save", mask).onclick = async () => {
    const amt = $("#fe-amt", mask).value.trim();
    if (!amt) { toast("请填写加工费金额", "err"); return; }
    try {
      const r = await postJson(`/prod/${order.id}/outsource-fee`, { amount: amt, date: $("#fe-date", mask).value });
      toast(`已确认加工费，凭证 #${r.voucher_id}`, "ok");
      closeModal();
      reload && reload();
    } catch (e) { toast(e.message, "err"); }
  };
}

// BOM 维护（报工页「维护BOM」）：子件 / 用量 / 损耗率；领料与 MRP 按 BOM 展开
function openBomEditor(item, reload) {
  let rows = [{ child: "", qty: "1", loss: "0" }];
  const mask = modal(`<h3>维护BOM · ${esc(item)}</h3>
    <table class="grid" id="bm-t"><thead><tr><th>子件编码 *</th><th>用量/件</th><th>损耗率</th><th></th></tr></thead><tbody></tbody></table>
    <button class="btn ghost sm" id="bm-add">＋子件</button>
    <p class="muted" style="font-size:12px;margin:6px 0 0">用量 = 每 1 件父件所需子件数量；损耗率如 0.05。保存覆盖当前版本 BOM（领料按 BOM × 计划量展开）。</p>
    <div class="foot"><button class="btn primary" id="bm-save">保存</button><button class="btn ghost" id="bm-cancel">取消</button></div>`);
  const draw = () => {
    $("#bm-t tbody", mask).innerHTML = rows.map((r, i) => `<tr>
      <td><input data-f="child" data-i="${i}" value="${esc(r.child)}" style="width:120px" /></td>
      <td><input data-f="qty" data-i="${i}" value="${esc(r.qty)}" style="width:70px" /></td>
      <td><input data-f="loss" data-i="${i}" value="${esc(r.loss)}" style="width:60px" /></td>
      <td><button class="btn ghost sm" data-del="${i}">×</button></td></tr>`).join("");
    $all("[data-del]", mask).forEach((b) => b.onclick = () => { rows.splice(parseInt(b.dataset.del, 10), 1); if (!rows.length) rows.push({ child: "", qty: "1", loss: "0" }); draw(); });
    $all("[data-f]", mask).forEach((inp) => inp.oninput = () => { rows[parseInt(inp.dataset.i, 10)][inp.dataset.f] = inp.value; });
  };
  draw();
  api(`/bom?parent=${encodeURIComponent(item)}`).then((r) => {
    const list = r.rows || [];
    if (list.length) rows = list.map((x) => ({ child: x.child_code, qty: String(x.qty), loss: String(x.loss_rate) }));
    draw();
  }).catch((e) => toast(e.message, "err"));
  $("#bm-add", mask).onclick = () => { rows.push({ child: "", qty: "1", loss: "0" }); draw(); };
  $("#bm-cancel", mask).onclick = closeModal;
  $("#bm-save", mask).onclick = async () => {
    const clean = rows.filter((r) => r.child.trim());
    if (!clean.length) { toast("至少一个子件", "err"); return; }
    try {
      const r = await postJson("/bom", { parent: item, children: clean.map((c) => ({ child: c.child.trim(), qty: c.qty, loss: c.loss })) });
      toast(`BOM 已保存（${r.children} 个子件）`, "ok");
      closeModal();
      reload && reload();
    } catch (e) { toast(e.message, "err"); }
  };
}

// ===========================================================================
// 工序报工
// ===========================================================================
async function viewWorkReport(main) {
  main.innerHTML = `<h2>工序报工</h2>
    <div class="toolbar">
      <label>生产订单 <select id="wr-po" style="min-width:230px"></select></label>
      <button class="btn primary" id="wr-load">加载工序</button>
      <span class="grow"></span>
      <button class="btn ghost sm" id="wr-bom">维护BOM</button>
      <button class="btn ghost sm" id="wr-edit">变更</button>
      <button class="btn ghost sm" id="wr-cancel">取消</button>
      <button class="btn ghost sm" id="wr-log">变更历史</button>
      <button class="btn ghost sm" id="wr-qc">工序检验</button>
      <button class="btn ghost sm" id="wr-qclog">检验记录</button>
      <button class="btn ghost sm" id="wr-start">开工</button>
      <button class="btn" id="wr-issue">领料出库</button>
      <label>完工数量 <input id="wr-cq" style="width:70px" value="0" /></label>
      <button class="btn" id="wr-complete">完工入库</button>
      <button class="btn ghost sm" id="wr-out">新建委外</button>
      <button class="btn ghost sm" id="wr-fee">确认加工费</button>
    </div>
    <div id="wr-ops" class="muted">选择生产订单后加载工序</div>`;
  let orders = [];
  try {
    const r = await api("/prod");
    orders = r.orders || [];
  } catch (e) { toast(e.message, "err"); }
  $("#wr-po").innerHTML = orders.map((o) => `<option value="${o.id}">${o.order_kind === "outsourcing" ? "【委外】" : ""}${esc(o.no)} · ${esc(o.item_name)}（${o.status}）</option>`).join("") || `<option value="">当前期间无生产订单</option>`;
  // 工厂链动作：下达（MRP 页）→ 开工 → 领料出库 → 完工入库（BOM 在「维护BOM」编辑）
  const selOrder = () => orders.find((o) => String(o.id) === $("#wr-po").value);
  const syncCq = () => {
    const o = selOrder();
    $("#wr-cq").value = o ? String(Math.max(0, Number(o.planned_qty) - Number(o.completed_qty))) : "0";
  };
  $("#wr-po").onchange = syncCq;
  syncCq();
  const reloadView = () => rerenderView("work-report", main);
  $("#wr-start").onclick = async () => {
    const o = selOrder();
    if (!o) { toast("请先选择生产订单", "err"); return; }
    try { await postJson(`/prod/${o.id}/start`, {}); toast("已开工（订单进入生产中，可完工）", "ok"); reloadView(); } catch (e) { toast(e.message, "err"); }
  };
  $("#wr-issue").onclick = async () => {
    const o = selOrder();
    if (!o) { toast("请先选择生产订单", "err"); return; }
    if (!(await confirmDialog(`对 ${o.no} 领料出库？按 BOM × 计划量展开并生成领料凭证。`, true))) return;
    try {
      const r = await postJson(`/prod/${o.id}/issue`, { date: today() });
      toast(`已领料 ${r.items} 项物料，成本合计 ${r.total}`, "ok");
    } catch (e) { toast(e.message, "err"); }
  };
  $("#wr-complete").onclick = async () => {
    const o = selOrder();
    if (!o) { toast("请先选择生产订单", "err"); return; }
    const qty = $("#wr-cq").value.trim();
    if (!(await confirmDialog(`完工入库 ${qty} 件？将推进订单状态并生成完工结转凭证。`, true))) return;
    try { await postJson(`/prod/${o.id}/complete`, { qty, date: today() }); toast("已完工入库并结转成本", "ok"); reloadView(); } catch (e) { toast(e.message, "err"); }
  };
  $("#wr-bom").onclick = () => {
    const o = selOrder();
    if (!o) { toast("请先选择生产订单", "err"); return; }
    openBomEditor(o.item_code, reloadView);
  };
  $("#wr-out").onclick = () => openOutsourceEditor(reloadView);
  $("#wr-fee").onclick = () => openFeeEditor(selOrder(), reloadView);
  // 生产订单变更 / 取消 / 变更历史（草稿/已下达可变更；数量不得低于已完工）
  $("#wr-edit").onclick = () => {
    const o = selOrder();
    if (!o) { toast("请先选择生产订单", "err"); return; }
    if (!["Draft", "Released"].includes(String(o.status))) { toast("仅草稿/已下达的订单可变更", "err"); return; }
    const m = modal(`<h3>生产订单变更 — ${esc(o.no)}</h3>
      <div class="field"><label>计划数量（现 ${esc(String(o.planned_qty))}，已完工 ${esc(String(o.completed_qty))}）</label><input id="pe-qty" value="${esc(String(o.planned_qty))}" /></div>
      <div class="field"><label>计划开工（YYYY-MM-DD，留空清除）</label><input id="pe-start" value="${esc(o.plan_start || "")}" /></div>
      <div class="field"><label>计划完工（YYYY-MM-DD，留空清除）</label><input id="pe-end" value="${esc(o.plan_end || "")}" /></div>
      <div class="field"><label>备注</label><input id="pe-memo" value="${esc(o.memo || "")}" /></div>
      <div class="foot"><button class="btn primary" id="pe-save">保存</button><button class="btn ghost" id="pe-cancel">取消</button></div>`);
    $("#pe-cancel", m).onclick = closeModal;
    $("#pe-save", m).onclick = async () => {
      try {
        await api(`/prod/${o.id}`, {
          method: "PUT", headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ qty: $("#pe-qty", m).value.trim(), plan_start: $("#pe-start", m).value.trim(), plan_end: $("#pe-end", m).value.trim(), memo: $("#pe-memo", m).value.trim() }),
        });
        toast("已保存变更（留痕可在「变更历史」查看）", "ok"); closeModal(); reloadView();
      } catch (e) { toast(e.message, "err"); }
    };
  };
  $("#wr-cancel").onclick = async () => {
    const o = selOrder();
    if (!o) { toast("请先选择生产订单", "err"); return; }
    if (!(await confirmDialog(`取消生产订单 ${o.no}？仅草稿/已下达可取消（已开工需先完工/退料）。`, true))) return;
    try { await postJson(`/prod/${o.id}/cancel`, {}); toast("已取消", "ok"); reloadView(); } catch (e) { toast(e.message, "err"); }
  };
  $("#wr-log").onclick = () => {
    const o = selOrder();
    if (!o) { toast("请先选择生产订单", "err"); return; }
    const m = modal(`<h3>变更历史 — ${esc(o.no)}</h3><div id="pl-body" class="muted">加载中…</div>
      <div class="foot"><button class="btn ghost" id="pl-close">关闭</button></div>`);
    $("#pl-close", m).onclick = closeModal;
    api(`/prod/${o.id}/changes`).then((r) => {
      const rows = r.rows || [];
      $("#pl-body", m).innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>时间</th><th>字段</th><th>改前</th><th>改后</th><th>操作人</th></tr></thead><tbody>${rows.map((x) => `<tr><td>${esc(x.changed_at)}</td><td>${esc(x.field)}</td><td class="muted">${esc(x.old_value || "—")}</td><td>${esc(x.new_value || "—")}</td><td>${esc(x.changed_by)}</td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">暂无变更记录</div>`;
    }).catch((e) => { $("#pl-body", m).textContent = e.message; });
  };
  // 工序检验（生产中订单；不合格必选处置；报废扣减计划量）
  $("#wr-qc").onclick = () => {
    const o = selOrder();
    if (!o) { toast("请先选择生产订单", "err"); return; }
    if (String(o.status) !== "InProgress") { toast("仅「生产中」的订单可录工序检验", "err"); return; }
    const m = modal(`<h3>工序检验 — ${esc(o.no)}</h3>
      <div class="field"><label>检验数量 *</label><input id="qc-insp" value="${esc(String($("#wr-cq").value || "0"))}" /></div>
      <div class="field"><label>不合格数（0 = 全合格）</label><input id="qc-fail" value="0" /></div>
      <div class="field"><label>处置（不合格 &gt; 0 必选）</label><select id="qc-disp"><option value="">—</option><option value="rework">返修</option><option value="scrap">报废（扣减计划量）</option><option value="concession">让步接收</option></select></div>
      <div class="field"><label>日期</label><input id="qc-date" value="${today()}" /></div>
      <div class="field"><label>备注</label><input id="qc-memo" /></div>
      <div class="foot"><button class="btn primary" id="qc-save">保存检验单</button><button class="btn ghost" id="qc-cancel">取消</button></div>`);
    $("#qc-cancel", m).onclick = closeModal;
    $("#qc-save", m).onclick = async () => {
      try {
        const r = await postJson(`/prod/${o.id}/qc`, { qty_insp: $("#qc-insp", m).value.trim(), qty_fail: $("#qc-fail", m).value.trim(), disposition: $("#qc-disp", m).value, date: $("#qc-date", m).value.trim(), memo: $("#qc-memo", m).value.trim() });
        toast(`检验单 ${r.no}（${r.result === "pass" ? "合格" : r.result === "fail" ? "全不合格" : "部分合格"}）`, "ok");
        closeModal(); reloadView();
      } catch (e) { toast(e.message, "err"); }
    };
  };
  $("#wr-qclog").onclick = () => {
    const o = selOrder();
    if (!o) { toast("请先选择生产订单", "err"); return; }
    const m = modal(`<h3>检验记录 — ${esc(o.no)}</h3><div id="ql-body" class="muted">加载中…</div>
      <div class="foot"><button class="btn ghost" id="ql-close">关闭</button></div>`);
    $("#ql-close", m).onclick = closeModal;
    api(`/prod/${o.id}/qc`).then((r) => {
      const rows = r.rows || [];
      const disp = { rework: "返修", scrap: "报废", concession: "让步接收", "": "—" };
      $("#ql-body", m).innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>单号</th><th>日期</th><th class="num">检验</th><th class="num">合格</th><th class="num">不合格</th><th>处置</th><th>结论</th><th>检验员</th></tr></thead><tbody>${rows.map((x) => `<tr><td>${esc(x.no)}</td><td>${esc(x.date)}</td><td class="num">${esc(x.qty_insp)}</td><td class="num">${esc(x.qty_pass)}</td><td class="num">${esc(x.qty_fail)}</td><td>${esc(disp[x.disposition] || x.disposition || "—")}</td><td>${x.result === "pass" ? '<span class="tag ok">合格</span>' : x.result === "fail" ? '<span class="tag err">全不合格</span>' : '<span class="tag warn">部分合格</span>'}</td><td>${esc(x.inspector)}</td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">暂无检验记录</div>`;
    }).catch((e) => { $("#ql-body", m).textContent = e.message; });
  };
  if (!orders.length) { $("#wr-ops").innerHTML = `<div class="muted">当前期间没有生产订单。先到「MRP」页对「生产」行点「下达」。</div>`; return; }
  const loadOps = async () => {
    const poId = $("#wr-po").value;
    if (!poId) return;
    try {
      const r = await api(`/prod/${poId}/ops`);
      const ops = r.ops || [];
      $("#wr-ops").innerHTML = ops.length ? ops.map((op) => `
        <div class="card"><div class="row">
          <b>${esc(op.op_name)}</b> <span class="tag">${esc(op.work_center)}</span>
          <span class="tag ${op.status === "done" ? "ok" : op.status === "in_progress" ? "" : ""}">${op.status === "done" ? "完工" : op.status === "in_progress" ? "进行中" : "待开工"}</span>
        </div>
        <div class="row" style="margin-top:6px">
          <label>完工数量 <input id="wq-${op.id}" value="0" style="width:80px" /></label>
          <label>实际工时 <input id="wh-${op.id}" value="0" style="width:80px" /></label>
          <button class="btn sm" data-wr-report="${op.id}">报工</button>
          <button class="btn sm" data-wr-finish="${op.id}">完工</button>
          <span class="muted">已累计 数量 ${fmt(op.qty_done)} · 工时 ${fmt(op.hours)}</span>
        </div></div>`).join("") : `<div class="muted">该订单暂无工序，请先维护产品工艺路线</div>`;
      $all("[data-wr-report]").forEach((b) => b.onclick = async () => {
        const qty = $(`#wq-${b.dataset.wrReport}`).value.trim();
        const hours = $(`#wh-${b.dataset.wrReport}`).value.trim();
        if (!qty && !hours) { toast("数量与工时不能同时为空", "err"); return; }
        try { await api("/prod/op/report", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ op_id: parseInt(b.dataset.wrReport, 10), qty, hours }) }); toast("已报工", "ok"); loadOps(); } catch (e) { toast(e.message, "err"); }
      });
      $all("[data-wr-finish]").forEach((b) => b.onclick = async () => { if (!(await confirmDialog("将该工序标记为完工？"))) return; try { await api("/prod/op/finish", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ op_id: parseInt(b.dataset.wrFinish, 10) }) }); toast("已完工", "ok"); loadOps(); } catch (e) { toast(e.message, "err"); } });
    } catch (e) { toast(e.message, "err"); }
  };
  $("#wr-load").addEventListener("click", loadOps);
}

// ===========================================================================
// 权益变动表
// ===========================================================================
async function viewEquity(main) {
  main.innerHTML = `<h2>所有者权益变动表</h2>
    <div class="toolbar"><label>期间 <input id="eq-period" value="${esc(state.current)}" style="width:90px" /></label>
    <button class="btn primary" id="eq-run">查询</button>
      <button class="btn ghost sm" id="eq-run-print">打印预览</button></div>
    <div id="eq-result" class="muted">填写期间后点击查询</div>`;
  $("#eq-run").addEventListener("click", async () => {
  $("#eq-run-print").addEventListener("click", () => { const el = $("#eq-result").querySelector("table"); printPreview("所有者权益变动表", el); });
    const p = $("#eq-period").value.trim();
    try {
      const r = await api(`/reports/equity?period=${encodeURIComponent(p)}`);
      const s = r.statement;
      const rows = [...s.lines, s.total];
      $("#eq-result").innerHTML = `<table><thead><tr><th>项目</th><th>本年年初余额</th><th>本年增减变动</th><th>本年年末余额</th></tr></thead>
        <tbody>${rows.map((l) => `<tr><td>${esc(l.name)}</td><td class="r">${fmt(l.begin)}</td><td class="r">${fmt(l.change)}</td><td class="r">${fmt(l.end)}</td></tr>`).join("")}</tbody></table>
        <div class="muted">${s.ties ? "✔ 年初 + 增减 = 年末，勾稽通过" : "✖ 勾稽不符"}</div>`;
    } catch (e) { toast(e.message, "err"); }
  });
}

// ===========================================================================
// 报表对比
// ===========================================================================
async function viewCompare(main) {
  main.innerHTML = `<h2>报表对比分析</h2>
    <div class="toolbar">
      <label>报表 <select id="cp-key"><option value="balance_sheet">资产负债表</option><option value="income_statement">利润表</option></select></label>
      <label>当前期 <input id="cp-cur" value="${esc(state.current)}" style="width:90px" /></label>
      <label>对比期 <input id="cp-prev" style="width:90px" /></label>
      <button class="btn primary" id="cp-run">对比</button>
      <button class="btn ghost sm" id="cp-run-print">打印预览</button></div>
    <div id="cp-result" class="muted">填写期间后点击对比</div>`;
  const p = $("#cp-cur").value.trim();
  try { $("#cp-prev").value = prevPeriod(p); } catch (e) {}
  $("#cp-run").addEventListener("click", async () => {
  $("#cp-run-print").addEventListener("click", () => { const el = $("#cp-result").querySelector("table"); printPreview("报表对比分析", el); });
    const key = $("#cp-key").value, cur = $("#cp-cur").value.trim(), prev = $("#cp-prev").value.trim();
    try {
      const r = await api(`/reports/compare?key=${encodeURIComponent(key)}&period=${encodeURIComponent(cur)}&prev=${encodeURIComponent(prev)}`);
      const rows = r.rows || [];
      $("#cp-result").innerHTML = `<table><thead><tr><th>项目</th><th>当前期</th><th>对比期</th><th>差额</th><th>变动率</th></tr></thead>
        <tbody>${rows.map((x) => `<tr><td>${esc(x.name)}</td><td class="r">${fmt(x.current)}</td><td class="r">${fmt(x.previous)}</td><td class="r">${fmt(x.diff)}</td><td class="r">${x.rate == null ? "—" : x.rate + "%"}</td></tr>`).join("")}</tbody></table>`;
    } catch (e) { toast(e.message, "err"); }
  });
}
function prevPeriod(p) {
  const [y, m] = String(p).split("-").map(Number);
  const pm = m - 1 < 1 ? 12 : m - 1, py = m - 1 < 1 ? y - 1 : y;
  return `${py}-${String(pm).padStart(2, "0")}`;
}

// 某期间的末日（YYYY-MM-DD）：新建业务单据时默认日期用当前浏览期间，
// 用 today() 会在跨期浏览时与所属期间不一致，生成凭证被引擎拒绝。
function periodLastDay(p) {
  const [y, m] = String(p).split("-").map(Number);
  const last = new Date(y, m, 0).getDate();
  return `${y}-${String(m).padStart(2, "0")}-${String(last).padStart(2, "0")}`;
}

// ===========================================================================
// 科目日报表
// ===========================================================================
async function viewDaily(main) {
  main.innerHTML = `<h2>科目日报表</h2>
    <div class="toolbar">
      <label>科目 <input id="dl-code" placeholder="1001" style="width:90px" /></label>
      <label>期间 <input id="dl-from" value="${esc(state.current)}" style="width:90px" /></label>
      <button class="btn primary" id="dl-run">查询</button>
      <button class="btn ghost sm" id="dl-run-print">打印预览</button></div>
    <div id="dl-result" class="muted">填写科目与期间后点击查询</div>`;
  $("#dl-run").addEventListener("click", async () => {
  $("#dl-run-print").addEventListener("click", () => { const el = $("#dl-result").querySelector("table"); printPreview("科目日报表", el); });
    const code = $("#dl-code").value.trim();
    if (!code) { toast("请输入科目编码", "err"); return; }
    const from = $("#dl-from").value.trim();
    try {
      const r = await api(`/reports/daily?code=${encodeURIComponent(code)}&from=${encodeURIComponent(from)}&to=${encodeURIComponent(from)}`);
      const rows = r.rows || [];
      $("#dl-result").innerHTML = `<table><thead><tr><th>日期</th><th>借方</th><th>贷方</th><th>日末余额</th></tr></thead>
        <tbody>${rows.map((x) => `<tr><td>${esc(x.date)}</td><td class="r">${fmt(x.debit)}</td><td class="r">${fmt(x.credit)}</td><td class="r">${fmt(x.balance)}</td></tr>`).join("")}</tbody></table>`;
    } catch (e) { toast(e.message, "err"); }
  });
}

// ===========================================================================
// 期末对账
// ===========================================================================
async function viewReconcile(main) {
  main.innerHTML = `<h2>期末对账</h2>
    <div class="toolbar"><label>期间 <input id="rc-period" value="${esc(state.current)}" style="width:90px" /></label>
    <button class="btn primary" id="rc-run">对账</button>
      <button class="btn ghost sm" id="rc-run-print">打印预览</button></div>
    <div id="rc-result" class="muted">填写期间后点击对账</div>`;
  $("#rc-run").addEventListener("click", async () => {
  $("#rc-run-print").addEventListener("click", () => { const el = $("#rc-result").querySelector("table"); printPreview("期末对账", el); });
    const p = $("#rc-period").value.trim();
    try {
      const r = await api(`/reports/reconcile?period=${encodeURIComponent(p)}`);
      const items = r.items || [];
      $("#rc-result").innerHTML = items.map((i) => `<div class="card"><div class="row"><b>${esc(i.name)}</b><span class="tag ${i.ok ? "ok" : "err"}">${i.ok ? "通过" : "异常"}</span></div><div class="muted">${esc(i.detail)}</div></div>`).join("") || `<div class="muted">暂无对账数据</div>`;
    } catch (e) { toast(e.message, "err"); }
  });
}

// ===========================================================================
// 预算预警
// ===========================================================================
async function viewBudgetAlerts(main) {
  main.innerHTML = `<h2>预算预警</h2>
    <div class="toolbar">
      <label>期间 <input id="ba-period" value="${esc(state.current)}" style="width:90px" /></label>
      <label>阈值(%) <input id="ba-thr" value="90" style="width:60px" /></label>
      <button class="btn primary" id="ba-run">查询</button>
      <button class="btn ghost sm" id="ba-run-print">打印预览</button></div>
    <div id="ba-result" class="muted">填写期间后点击查询</div>`;
  $("#ba-run").addEventListener("click", async () => {
  $("#ba-run-print").addEventListener("click", () => { const el = $("#ba-result").querySelector("table"); printPreview("预算预警", el); });
    const p = $("#ba-period").value.trim(), thr = $("#ba-thr").value.trim() || "90";
    try {
      const r = await api(`/budget/alerts?period=${encodeURIComponent(p)}&threshold=${encodeURIComponent(thr)}`);
      const rows = r.rows || [];
      $("#ba-result").innerHTML = rows.length
        ? `<table><thead><tr><th>科目</th><th>部门</th><th>预算数</th><th>实际数</th><th>执行率</th><th>超支额</th></tr></thead>
          <tbody>${rows.map((x) => `<tr><td>${esc(x.account_code)} ${esc(x.account_name)}</td><td>${esc(x.dept || "—")}</td><td class="r">${fmt(x.budget)}</td><td class="r">${fmt(x.actual)}</td><td class="r">${x.rate}%</td><td class="r" style="color:var(--err)">${fmt(x.over_amount)}</td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">无预警科目</div>`;
    } catch (e) { toast(e.message, "err"); }
  });
}

// ===========================================================================
// 期末处理（与桌面端对齐：结转损益 / 年末结转 / 结账 / 反结账）
// ===========================================================================
async function viewPeriodEnd(main) {
  const cur = (state.current || "").replace("-", "");
  main.innerHTML = `<h2>期末处理</h2>
    <div class="toolbar">
      <label>期间 <input id="pe-period" value="${esc(cur)}" style="width:90px" placeholder="YYYYMM" /></label>
      <label><input type="checkbox" id="pe-reqcarry" checked /> 要求先结转损益</label>
      <button class="btn" id="pe-check">重新检查</button>
      <div class="spacer"></div>
      ${can("carry_forward") ? `<button class="btn ghost" id="pe-carry">结转损益</button>` : ""}
      ${can("carry_forward") ? `<button class="btn ghost" id="pe-yearend">年末结转</button>` : ""}
      ${can("period_close") ? `<button class="btn primary" id="pe-close">结账</button>` : ""}
      ${can("period_close") ? `<button class="btn danger" id="pe-unclose">反结账</button>` : ""}
    </div>
    <div class="muted" id="pe-status" style="margin-bottom:8px">加载中…</div>
    <div id="pe-issues"></div>`;
  const periodOf = () => $("#pe-period", main).value.trim();
  const post = (path, body) => api(path, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body || {}) });
  async function refresh() {
    const p = periodOf();
    if (!/^\d{6}$/.test(p)) { $("#pe-status", main).textContent = "期间格式应为 YYYYMM"; return; }
    try {
      const chk = await api(`/periods/${encodeURIComponent(p)}/precheck`);
      const issues = chk.issues || [];
      $("#pe-status", main).innerHTML = `期间 <b>${esc(chk.period)}</b>　已结账至：<b>${esc(chk.closed_upto || "—")}</b>　损益科目：<b>${chk.pl_count}</b> 个　本年利润余额：<b>${esc(chk.profit_balance)}</b>`;
      $("#pe-issues", main).innerHTML = issues.length
        ? `<div class="panel"><b>待处理问题：</b><ul style="margin:6px 0 0 18px">${issues.map((s) => `<li>${esc(s)}</li>`).join("")}</ul></div>`
        : `<div class="panel" style="color:var(--ok,#2e7d32)">✔ 未发现阻断项（是否要求先结转以左侧选项为准）</div>`;
    } catch (e) { $("#pe-status", main).textContent = e.message; $("#pe-issues", main).innerHTML = ""; }
  }
  $("#pe-check", main).onclick = refresh;
  if ($("#pe-carry", main)) $("#pe-carry", main).onclick = async () => {
    const p = periodOf();
    if (!(await confirmDialog(`为 ${p} 生成结转损益凭证（转入本年利润）？生成的凭证为未记账状态，需到凭证列表记账。`, false))) return;
    try { const r = await post(`/periods/${encodeURIComponent(p)}/carry-forward`); toast(`已生成结转凭证 ${r.voucher_no || r.id}（未记账）`, "ok"); refresh(); } catch (e) { toast(e.message, "err"); }
  };
  if ($("#pe-yearend", main)) $("#pe-yearend", main).onclick = async () => {
    const p = periodOf();
    if (!(await confirmDialog(`将 ${p} 的「本年利润」余额转入未分配利润？一般在 12 期结账后执行。`, false))) return;
    try { const r = await post(`/periods/${encodeURIComponent(p)}/year-end`); toast(`已生成年末结转凭证 ${r.voucher_no || r.id}（未记账）`, "ok"); refresh(); } catch (e) { toast(e.message, "err"); }
  };
  if ($("#pe-close", main)) $("#pe-close", main).onclick = async () => {
    const p = periodOf();
    const req = $("#pe-reqcarry", main).checked;
    if (!(await confirmDialog(`确定对 ${p} 结账？结账后该期间不能再录入/修改凭证。`, true))) return;
    try { await post(`/periods/${encodeURIComponent(p)}/close`, { require_carry: req }); toast(`${p} 已结账`, "ok"); refresh(); } catch (e) { toast(e.message, "err"); refresh(); }
  };
  if ($("#pe-unclose", main)) $("#pe-unclose", main).onclick = async () => {
    const p = periodOf();
    if (!(await confirmDialog(`确定对 ${p} 反结账？反结账后该期间可以重新录入凭证。`, true))) return;
    try { await post(`/periods/${encodeURIComponent(p)}/unclose`); toast(`${p} 已反结账`, "ok"); refresh(); } catch (e) { toast(e.message, "err"); }
  };
  refresh();
}

// ===========================================================================
// 固定资产（与桌面端对齐：卡片 / 折旧计划 / 计提 / 清理）
// ===========================================================================
async function viewAssets(main) {
  const cur = (state.current || "").replace("-", "");
  const METHODS = [["straight", "直线法"], ["ddb", "双倍余额递减法"], ["sum_of_years", "年数总和法"], ["one_time", "一次性摊销法"], ["fifty_fifty", "五五摊销法"]];
  main.innerHTML = `<h2>固定资产</h2>
    <div class="toolbar">
      <label>期间 <input id="as-period" value="${esc(cur)}" style="width:90px" placeholder="YYYYMM" /></label>
      <button class="btn" id="as-load">查询</button>
      <div class="spacer"></div>
      <button class="btn ghost" id="as-print">打印预览</button>
      <button class="btn ghost" id="as-recon">总账对账</button>
      ${can("account_edit") ? `<button class="btn ghost" id="as-count">资产盘点</button>` : ""}
      ${can("voucher_new") ? `<button class="btn ghost" id="as-accrue">计提本期折旧</button>` : ""}
      ${can("account_edit") ? `<button class="btn ghost" id="as-deldep">删除本期折旧</button>` : ""}
      ${can("account_edit") ? `<button class="btn primary" id="as-new">新增卡片</button>` : ""}
    </div>
    <div class="muted" id="as-summary" style="margin-bottom:8px">加载中…</div>
    <div class="panel"><table class="grid" id="as-table"><thead><tr>
      <th>编码</th><th>名称</th><th>部门</th><th class="num">原值</th><th class="num">累计折旧</th><th class="num">净值</th><th>状态</th><th>操作</th>
    </tr></thead><tbody><tr><td colspan="8" class="muted">加载中…</td></tr></tbody></table></div>
    <div id="as-plan" style="margin-top:10px"></div>`;
  const periodOf = () => $("#as-period", main).value.trim();
  const post = (path, body) => api(path, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body || {}) });
  $("#as-recon", main).onclick = openAssetRecon;
  if ($("#as-count", main)) $("#as-count").onclick = () => {
    api(`/assets?period=${encodeURIComponent(periodOf())}`).then((d) => {
      const cards = (d.ledger || []).filter((r) => r.asset.status !== "disposed");
      if (!cards.length) { toast("无可盘点卡片", "err"); return; }
      const m = modal(`<h3>资产盘点</h3>
        <div class="muted" style="font-size:12px;margin-bottom:6px">默认全部盘实；取消勾选 = 盘亏（过账后卡片置停用）</div>
        <table class="grid"><thead><tr><th>编码</th><th>名称</th><th>盘实</th></tr></thead><tbody>${cards.map((r) => `<tr><td>${esc(r.asset.code)}</td><td>${esc(r.asset.name)}</td><td><input type="checkbox" class="ac-f" data-id="${r.asset.id}" checked /></td></tr>`).join("")}</tbody></table>
        <div class="field"><label>备注</label><input id="ac-memo" /></div>
        <div class="foot"><button class="btn primary" id="ac-save">保存盘点单</button><button class="btn ghost" id="ac-cancel">取消</button></div>`);
      $("#ac-cancel", m).onclick = closeModal;
      $("#ac-save", m).onclick = async () => {
        const lines = $all(".ac-f", m).map((x) => ({ asset_id: parseInt(x.dataset.id, 10), found: x.checked }));
        try {
          const r = await post("/assets/counts", { period: parseInt(periodOf(), 10), date: today(), memo: $("#ac-memo", m).value.trim(), lines });
          if (await confirmDialog(`盘点单 ${r.no} 已保存，立即过账（盘亏置停用）？`, false)) {
            const pr = await post(`/assets/counts/${r.id}/post`, {});
            toast(`已过账，盘亏 ${pr.lost} 张`, "ok");
          } else {
            toast("已保存草稿", "ok");
          }
          closeModal();
          load();
        } catch (e) { toast(e.message, "err"); }
      };
    }).catch((e) => toast(e.message, "err"));
  };

  async function load() {
    const p = periodOf();
    if (!/^\d{6}$/.test(p)) { $("#as-summary", main).textContent = "期间格式应为 YYYYMM"; return; }
    let d;
    try { d = await api(`/assets?period=${encodeURIComponent(p)}`); }
    catch (e) { $("#as-summary", main).textContent = e.message; return; }
    const planTotal = (d.plan || []).reduce((a, x) => a + (parseFloat(String(x.amount).replace(/,/g, "")) || 0), 0);
    $("#as-summary", main).innerHTML = `共 <b>${(d.cards || []).length}</b> 张卡片　本期应计提 <b>${planTotal.toFixed(2)}</b>　本期已计提记录 <b>${(d.deps || []).length}</b> 条`;
    const rows = d.ledger || [];
    $("#as-table tbody", main).innerHTML = rows.length ? rows.map((r) => {
      const a = r.asset;
      const disposed = a.status === "disposed";
      return `<tr>
        <td>${esc(a.code)}</td><td>${esc(a.name)}</td><td>${esc(a.dept || "—")}</td>
        <td class="num">${esc(a.original_value)}</td><td class="num">${esc(r.accum)}</td><td class="num">${esc(r.net)}</td>
        <td>${disposed ? `<span class="tag err">已清理</span>` : `<span class="tag ok">在用</span>`} ${esc(a.method_label || "")}</td>
        <td class="row-actions">
          ${can("account_edit") && !disposed ? `<button class="btn sm ghost" data-as="edit" data-id="${a.id}">改</button>` : ""}
          ${can("account_edit") ? `<button class="btn sm ghost" data-as="deps" data-id="${a.id}">折旧</button>` : ""}
          ${can("account_edit") ? `<button class="btn sm ghost" data-as="changes" data-id="${a.id}">变更</button>` : ""}
          ${can("account_edit") && !disposed ? `<button class="btn sm ghost" data-as="impair" data-id="${a.id}">减值</button>` : ""}
          ${can("account_edit") && !disposed ? `<button class="btn sm ghost" data-as="dispose" data-id="${a.id}">清理</button>` : ""}
          ${can("account_edit") ? `<button class="btn sm ghost" data-as="del" data-id="${a.id}">删</button>` : ""}
        </td></tr>`;
    }).join("") : `<tr><td colspan="8" class="muted" style="text-align:center;padding:16px">暂无固定资产卡片</td></tr>`;
    const plan = d.plan || [];
    $("#as-plan", main).innerHTML = plan.length ? `<div class="panel"><b>本期折旧计划（未生成凭证前可核对）：</b><table class="grid" style="margin-top:6px"><thead><tr><th>编码</th><th>名称</th><th>部门</th><th class="num">本期应提</th><th class="num">提后累计</th><th class="num">提后净值</th></tr></thead><tbody>${plan.map((x) => `<tr><td>${esc(x.code)}</td><td>${esc(x.name)}</td><td>${esc(x.dept || "—")}</td><td class="num">${esc(x.amount)}</td><td class="num">${esc(x.accum)}</td><td class="num">${esc(x.net)}</td></tr>`).join("")}</tbody></table></div>` : "";
    $all("[data-as]", main).forEach((b) => b.onclick = async () => {
      const id = parseInt(b.dataset.id, 10);
      const card = (d.cards || []).find((x) => x.id === id);
      if (b.dataset.as === "edit" && card) openAssetEditor(card);
      else if (b.dataset.as === "deps") showAssetDeps(card, id);
      else if (b.dataset.as === "changes") showAssetChanges(card, id);
      else if (b.dataset.as === "impair") {
        const amt = prompt(`对「${card ? card.name : id}」计提减值金额`, "");
        if (amt === null) return;
        if (!amt.trim()) { toast("减值金额不能为空", "err"); return; }
        try {
          await post(`/assets/${id}/impair`, { amount: amt.trim(), period: parseInt(periodOf(), 10), memo: "" });
          toast("已计提减值", "ok");
          load();
        } catch (e) { toast(e.message, "err"); }
      }
      else if (b.dataset.as === "dispose") {
        if (!(await confirmDialog(`确定对「${card ? card.name : id}」做资产清理？清理当月仍计提，次月停提；将生成清理转销凭证草稿。`, true))) return;
        const amt = prompt("清理金额（可留空）", "");
        if (amt === null) return;
        try {
          const r = await post(`/assets/${id}/dispose`, { ymm: parseInt(periodOf(), 10), amount: amt || "" });
          toast(`已清理，转销凭证 #${r.voucher_id}（变卖收款与净损益结转请另行制单）`, "ok");
          load();
        } catch (e) { toast(e.message, "err"); }
      } else if (b.dataset.as === "del") {
        if (!(await confirmDialog("确定删除该资产卡片？（已计提过折旧的卡片不能删除）", true))) return;
        try { await api(`/assets/${id}`, { method: "DELETE" }); toast("已删除", "ok"); load(); } catch (e) { toast(e.message, "err"); }
      }
    });
  }

  function showAssetDeps(card, id) {
    const m = modal(`<h3>折旧明细 — ${esc(card ? card.name : id)}</h3><div id="ad-body" class="muted">加载中…</div>
      <div class="foot"><button class="btn ghost" id="ad-close">关闭</button></div>`);
    $("#ad-close", m).onclick = closeModal;
    api(`/assets/${id}/depreciations`).then((r) => {
      const rows = r.rows || [];
      $("#ad-body", m).innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>期间</th><th class="num">本期折旧</th><th class="num">累计折旧</th><th class="num">净值</th><th>凭证</th></tr></thead><tbody>${rows.map((x) => `<tr><td>${esc(x.period)}</td><td class="num">${esc(x.amount)}</td><td class="num">${esc(x.accum)}</td><td class="num">${esc(x.net_value)}</td><td>${x.voucher_id ? "#" + x.voucher_id : "—"}</td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">暂无折旧记录</div>`;
    }).catch((e) => { $("#ad-body", m).textContent = e.message; });
  }

  function showAssetChanges(card, id) {
    const m = modal(`<h3>变更历史 — ${esc(card ? card.name : id)}</h3><div id="ach-body" class="muted">加载中…</div>
      <div class="foot"><button class="btn ghost" id="ach-close">关闭</button></div>`);
    $("#ach-close", m).onclick = closeModal;
    api(`/assets/${id}/changes`).then((r) => {
      const rows = r.rows || [];
      $("#ach-body", m).innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>时间</th><th>操作人</th><th>字段</th><th>改前</th><th>改后</th><th>说明</th></tr></thead><tbody>${rows.map((x) => `<tr><td>${esc(x.ts)}</td><td>${esc(x.who)}</td><td>${esc(x.field)}</td><td class="muted">${esc(x.old_value || "—")}</td><td>${esc(x.new_value || "—")}</td><td class="muted">${esc(x.memo || "")}</td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">暂无变更记录</div>`;
    }).catch((e) => { $("#ach-body", m).textContent = e.message; });
  }

  function openAssetRecon() {
    const m = modal(`<h3>固定资产 ↔ 总账对账 <span class="muted" style="font-size:12px;font-weight:400">资产侧=未清理卡片原值/累计折旧；总账侧=科目余额（仅已记账）</span></h3>
      <div class="toolbar"><label>期间 <input id="ar-period" value="${esc(periodOf())}" style="width:90px" /></label><button class="btn sm" id="ar-load">对账</button></div>
      <div id="ar-body" class="muted">加载中…</div>
      <div class="foot"><button class="btn ghost" id="ar-close">关闭</button></div>`);
    $("#ar-close", m).onclick = closeModal;
    const loadRecon = async () => {
      try {
        const r = await api(`/assets/gl-reconcile?period=${encodeURIComponent($("#ar-period", m).value.trim())}`);
        const rows = r.rows || [];
        const badge = (v) => Math.abs(parseFloat(String(v).replace(/,/g, "")) || 0) < 0.005 ? `<span class="tag ok">平</span>` : `<span class="tag err">差 ${esc(v)}</span>`;
        $("#ar-body", m).innerHTML = `
          <div class="cards" style="margin-bottom:8px">
            <div class="card"><div class="k">原值 资产/总账</div><div class="v" style="font-size:15px">${esc(r.cost_asset)} / ${esc(r.cost_gl)}</div><div class="muted">${badge(r.cost_diff)}</div></div>
            <div class="card"><div class="k">累计折旧 资产/总账</div><div class="v" style="font-size:15px">${esc(r.dep_asset)} / ${esc(r.dep_gl)}</div><div class="muted">${badge(r.dep_diff)}</div></div>
          </div>
          ${rows.length ? `<table class="grid"><thead><tr><th>科目</th><th>名称</th><th>口径</th><th class="num">资产侧</th><th class="num">总账侧</th><th class="num">差异</th></tr></thead><tbody>${rows.map((x) => `<tr style="${Math.abs(parseFloat(String(x.diff).replace(/,/g, "")) || 0) >= 0.005 ? "background:rgba(220,53,69,.08)" : ""}"><td>${esc(x.account_code)}</td><td>${esc(x.account_name || "")}</td><td>${esc(x.kind)}</td><td class="num">${esc(x.asset_value)}</td><td class="num">${esc(x.gl_value)}</td><td class="num">${esc(x.diff)}</td></tr>`).join("")}</tbody></table>` : `<div class="muted">无资产卡片/相关科目余额</div>`}`;
      } catch (e) { $("#ar-body", m).textContent = e.message; }
    };
    $("#ar-load", m).onclick = loadRecon;
    loadRecon();
  }

  function openAssetEditor(card) {
    const isNew = !card;
    const c = card || { code: "", name: "", category: "", spec: "", dept: "", asset_account: "160101", dep_account: "1602", expense_account: "660201", original_value: "", residual_rate: "5", life_months: 36, method: "straight", start_period: cur, memo: "" };
    const m = modal(`<h3>${isNew ? "新增" : "修改"}资产卡片</h3>
      <div style="display:grid;grid-template-columns:1fr 1fr;gap:8px">
        <label>编码 <input id="ae-code" value="${esc(c.code)}" ${isNew ? "" : "disabled"} /></label>
        <label>名称 <input id="ae-name" value="${esc(c.name)}" /></label>
        <label>类别 <input id="ae-cat" value="${esc(c.category || "")}" placeholder="如 电子设备" /></label>
        <label>规格 <input id="ae-spec" value="${esc(c.spec || "")}" /></label>
        <label>使用部门 <input id="ae-dept" value="${esc(c.dept || "")}" /></label>
        <label>启用期间 <input id="ae-start" value="${esc(c.start_period)}" placeholder="YYYYMM" /></label>
        <label>原值 <input id="ae-orig" value="${esc(c.original_value)}" /></label>
        <label>残值率% <input id="ae-residual" value="${esc(c.residual_rate)}" /></label>
        <label>使用月数 <input id="ae-life" type="number" value="${c.life_months}" /></label>
        <label>折旧方法 <select id="ae-method">${METHODS.map(([k, v]) => `<option value="${k}" ${c.method === k ? "selected" : ""}>${v}</option>`).join("")}</select></label>
        <label>资产科目 <input id="ae-acct" value="${esc(c.asset_account)}" /></label>
        <label>累计折旧科目 <input id="ae-depacct" value="${esc(c.dep_account)}" /></label>
        <label>费用科目 <input id="ae-expacct" value="${esc(c.expense_account)}" /></label>
        <label>备注 <input id="ae-memo" value="${esc(c.memo || "")}" /></label>
      </div>
      <div class="foot"><button class="btn primary" id="ae-save">保存</button><button class="btn ghost" id="ae-cancel">取消</button></div>`);
    $("#ae-cancel", m).onclick = closeModal;
    $("#ae-save", m).onclick = async () => {
      const body = {
        id: c.id || 0,
        code: $("#ae-code", m).value.trim(),
        name: $("#ae-name", m).value.trim(),
        category: $("#ae-cat", m).value.trim(),
        spec: $("#ae-spec", m).value.trim(),
        dept: $("#ae-dept", m).value.trim(),
        asset_account: $("#ae-acct", m).value.trim(),
        dep_account: $("#ae-depacct", m).value.trim(),
        expense_account: $("#ae-expacct", m).value.trim(),
        original_value: $("#ae-orig", m).value.trim(),
        residual_rate: $("#ae-residual", m).value.trim(),
        life_months: parseInt($("#ae-life", m).value, 10) || 0,
        method: $("#ae-method", m).value,
        start_period: parseInt($("#ae-start", m).value.trim(), 10) || 0,
        memo: $("#ae-memo", m).value.trim(),
      };
      try {
        if (isNew) await post("/assets", body);
        else await api(`/assets/${c.id}`, { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
        toast("已保存", "ok"); closeModal(); load();
      } catch (e) { toast(e.message, "err"); }
    };
  }

  $("#as-load", main).onclick = load;
  $("#as-print", main).onclick = () => printPreview("固定资产台账", $("#as-table", main).querySelector("table"));
  if ($("#as-new", main)) $("#as-new", main).onclick = () => openAssetEditor(null);
  if ($("#as-accrue", main)) $("#as-accrue", main).onclick = async () => {
    const p = periodOf();
    if (!(await confirmDialog(`按部门/费用科目汇总生成 ${p} 折旧凭证？同一期间重复点击不会重复生成。`, false))) return;
    try {
      const r = await post("/assets/depreciate", { ymm: parseInt(p, 10) });
      toast(r.already ? `本期已计提过（凭证 ${r.voucher_no || "—"}）` : `已生成折旧凭证 ${r.voucher_no}，共 ${r.count} 项 ${r.total}`, "ok");
      load();
    } catch (e) { toast(e.message, "err"); }
  };
  if ($("#as-deldep", main)) $("#as-deldep", main).onclick = async () => {
    const p = periodOf();
    if (!(await confirmDialog(`删除 ${p} 的全部折旧明细（不改凭证）？删除后可重新计提。`, true))) return;
    try { const r = await post("/assets/depreciations/delete-period", { ymm: parseInt(p, 10) }); toast(`已删除 ${r.removed} 条`, "ok"); load(); } catch (e) { toast(e.message, "err"); }
  };
  load();
}

// ===========================================================================
// 银行对账（与桌面端对齐：导入 → 自动勾对 → 手工补勾 → 余额调节表）
// ===========================================================================
async function viewBank(main) {
  const cur = (state.current || "").replace("-", "");
  main.innerHTML = `<h2>银行对账</h2>
    <div class="toolbar">
      <label>期间 <input id="bk-period" value="${esc(cur)}" style="width:90px" /></label>
      <label>银行科目 <input id="bk-acct" value="1002" style="width:100px" /></label>
      <button class="btn" id="bk-load">查询</button>
      <div class="spacer"></div>
      <label>日期容差 <input id="bk-tol" type="number" value="3" style="width:56px" /> 天</label>
      ${can("voucher_new") ? `<button class="btn ghost" id="bk-auto">自动勾对</button>` : ""}
      ${can("voucher_new") ? `<button class="btn ghost" id="bk-import">导入对账单</button>` : ""}
      ${can("voucher_new") ? `<button class="btn ghost" id="bk-link">手工勾对</button>` : ""}
      ${can("voucher_new") ? `<button class="btn danger" id="bk-clear">清空对账单</button>` : ""}
    </div>
    <div class="muted" id="bk-sum" style="margin-bottom:8px">加载中…</div>
    <div style="display:grid;grid-template-columns:1fr 1fr;gap:10px">
      <div class="panel"><b>银行对账单</b><div id="bk-stmts"></div></div>
      <div class="panel"><b>账面银行分录（已记账）</b><div id="bk-book"></div></div>
    </div>
    <div class="panel" id="bk-recon" style="margin-top:10px"></div>`;
  let data = null, selStmt = null, selBook = null;
  const periodOf = () => $("#bk-period", main).value.trim();
  const accountOf = () => $("#bk-acct", main).value.trim();
  const post = (path, body) => api(path, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body || {}) });

  function render() {
    if (!data) return;
    const stmts = data.statements || [], book = data.book || [];
    $("#bk-sum", main).innerHTML = `银行流水 <b>${stmts.length}</b> 条（已勾 <b>${stmts.filter((s) => s.entry_id).length}</b>）　账面分录 <b>${book.length}</b> 条`;
    $("#bk-stmts", main).innerHTML = `<table class="grid"><thead><tr><th>日期</th><th>摘要</th><th>结算号</th><th class="num">进账</th><th class="num">支出</th><th class="num">余额</th><th></th></tr></thead><tbody>${stmts.length ? stmts.map((s) => `<tr class="${selStmt === s.id ? "row-sel" : ""}" data-stmt="${s.id}"><td>${esc(s.date)}</td><td>${esc(s.summary)}</td><td>${esc(s.settle_no || "")}</td><td class="num">${s.debit === "0.00" ? "" : esc(s.debit)}</td><td class="num">${s.credit === "0.00" ? "" : esc(s.credit)}</td><td class="num">${esc(s.balance)}</td><td>${s.entry_id ? `<button class="btn sm ghost" data-unlink="${s.id}">取消</button>` : `<span class="muted">未勾</span>`}</td></tr>`).join("") : `<tr><td colspan="7" class="muted">暂无对账单，请先导入</td></tr>`}</tbody></table>`;
    $("#bk-book", main).innerHTML = `<table class="grid"><thead><tr><th>日期</th><th>凭证号</th><th>摘要</th><th class="num">借</th><th class="num">贷</th><th></th></tr></thead><tbody>${book.length ? book.map((b) => `<tr class="${selBook === b.entry_id ? "row-sel" : ""}" data-book="${b.entry_id}"><td>${esc(b.date)}</td><td>${esc(b.voucher_no)}</td><td>${esc(b.summary)}</td><td class="num">${b.debit === "0.00" ? "" : esc(b.debit)}</td><td class="num">${b.credit === "0.00" ? "" : esc(b.credit)}</td><td></td></tr>`).join("") : `<tr><td colspan="6" class="muted">该科目本期无已记账分录</td></tr>`}</tbody></table>`;
    const r = data.reconcile || {};
    const list = (arr, f) => (arr || []).length ? `<ul style="margin:4px 0 0 16px">${arr.map((x) => `<li>${f(x)}</li>`).join("")}</ul>` : `<span class="muted">无</span>`;
    $("#bk-recon", main).innerHTML = `<b>余额调节表</b>　${r.balanced ? '<span class="tag ok">调节后一致</span>' : `<span class="tag err">差额 ${esc(r.diff)}</span>`}
      <table class="grid" style="margin-top:6px"><thead><tr><th>口径</th><th class="num">余额</th><th class="num">调节后</th></tr></thead><tbody>
      <tr><td>银行对账单</td><td class="num">${esc(r.bank_balance || "0.00")}</td><td class="num">${esc(r.bank_adjusted || "0.00")}</td></tr>
      <tr><td>企业账面</td><td class="num">${esc(r.book_balance || "0.00")}</td><td class="num">${esc(r.book_adjusted || "0.00")}</td></tr>
      </tbody></table>
      <div style="display:grid;grid-template-columns:1fr 1fr;gap:10px;margin-top:8px;font-size:13px">
        <div><b>企业已收、银行未收</b>${list(r.book_only_in, (x) => `${x.voucher_no} ${x.summary} ${x.debit}`)}<b>企业已付、银行未付</b>${list(r.book_only_out, (x) => `${x.voucher_no} ${x.summary} ${x.credit}`)}</div>
        <div><b>银行已收、企业未记</b>${list(r.bank_only_in, (x) => `${x.date} ${x.summary} ${x.debit}`)}<b>银行已付、企业未记</b>${list(r.bank_only_out, (x) => `${x.date} ${x.summary} ${x.credit}`)}</div>
      </div>`;
    $all("[data-stmt]", main).forEach((tr) => tr.onclick = (ev) => { if (ev.target.dataset.unlink) return; selStmt = parseInt(tr.dataset.stmt, 10); render(); });
    $all("[data-book]", main).forEach((tr) => tr.onclick = () => { selBook = parseInt(tr.dataset.book, 10); render(); });
    $all("[data-unlink]", main).forEach((b) => b.onclick = async (ev) => {
      ev.stopPropagation();
      try { await post("/bank/unlink", { stmt_id: parseInt(b.dataset.unlink, 10) }); toast("已取消勾对", "ok"); load(); } catch (e) { toast(e.message, "err"); }
    });
  }

  async function load() {
    const p = periodOf(), acct = accountOf();
    if (!/^\d{6}$/.test(p) || !acct) { $("#bk-sum", main).textContent = "请填写期间与银行科目"; return; }
    try { data = await api(`/bank?period=${encodeURIComponent(p)}&account=${encodeURIComponent(acct)}`); }
    catch (e) { $("#bk-sum", main).textContent = e.message; return; }
    selStmt = selBook = null;
    render();
  }

  $("#bk-load", main).onclick = load;
  if ($("#bk-auto", main)) $("#bk-auto", main).onclick = async () => {
    const p = periodOf(), acct = accountOf(), tol = parseInt($("#bk-tol", main).value, 10) || 0;
    try {
      const r = await post("/bank/auto-match", { ymm: parseInt(p, 10), account: acct, tolerance: tol });
      toast(`自动勾对 ${r.matched} 对（结算号 ${r.by_no}、金额+日期 ${r.by_amount_date}、金额 ${r.by_amount}；存疑 ${r.ambiguous}）`, "ok");
      load();
    } catch (e) { toast(e.message, "err"); }
  };
  if ($("#bk-link", main)) $("#bk-link", main).onclick = async () => {
    if (!selStmt || !selBook) { toast("请分别在左右两表各选一行", "err"); return; }
    try { await post("/bank/link", { stmt_id: selStmt, entry_id: selBook }); toast("已勾对", "ok"); load(); } catch (e) { toast(e.message, "err"); }
  };
  if ($("#bk-import", main)) $("#bk-import", main).onclick = () => {
    const m = modal(`<h3>导入银行对账单</h3>
      <div class="muted" style="font-size:12px;margin-bottom:6px">每行一条，逗号/制表符/分号分隔。支持：日期,摘要,结算号,借方,贷方,余额 或 日期,摘要,结算号,金额,余额（负数支出）。首行表头自动跳过。</div>
      <textarea id="bi-text" style="width:100%;height:220px;font-family:monospace" placeholder="2026-01-06,收到货款,SN001,1000.00,0.00,101000.00"></textarea>
      <div class="foot"><button class="btn primary" id="bi-ok">导入</button><button class="btn ghost" id="bi-cancel">取消</button></div>`);
    $("#bi-cancel", m).onclick = closeModal;
    $("#bi-ok", m).onclick = async () => {
      try {
        const r = await post("/bank/import", { ymm: parseInt(periodOf(), 10), account: accountOf(), text: $("#bi-text", m).value });
        toast(`已导入 ${r.imported} 条`, "ok");
        (r.warnings || []).slice(0, 3).forEach((w) => toast(w, "err"));
        closeModal(); load();
      } catch (e) { toast(e.message, "err"); }
    };
  };
  if ($("#bk-clear", main)) $("#bk-clear", main).onclick = async () => {
    if (!(await confirmDialog(`清空 ${periodOf()} 期 ${accountOf()} 的全部对账单流水？`, true))) return;
    try { const r = await post("/bank/clear", { ymm: parseInt(periodOf(), 10), account: accountOf(), tolerance: 0 }); toast(`已清空 ${r.removed} 条`, "ok"); load(); } catch (e) { toast(e.message, "err"); }
  };
  load();
}

// ===========================================================================
// 往来核销（与桌面端对齐：未核销清单 / 自动核销 / 手工核销 / 账龄）
// ===========================================================================
async function viewSettle(main) {
  const cur = (state.current || "").replace("-", "");
  main.innerHTML = `<h2>往来核销</h2>
    <div class="toolbar">
      <label>往来科目 <input id="st-acct" value="1122" style="width:100px" /></label>
      <label>截止期间 <input id="st-upto" value="${esc(cur)}" style="width:90px" /></label>
      <button class="btn" id="st-load">查询未核销</button>
      <div class="spacer"></div>
      <label>尾差 <input id="st-tol" value="0.01" style="width:60px" /></label>
      ${can("voucher_new") ? `<button class="btn ghost" id="st-auto">自动核销</button>` : ""}
      ${can("voucher_new") ? `<button class="btn ghost" id="st-manual">手工核销</button>` : ""}
      <button class="btn ghost" id="st-aging">账龄分析</button>
      <button class="btn ghost" id="st-records">核销记录</button>
    </div>
    <div class="muted" id="st-sum" style="margin-bottom:8px">加载中…</div>
    <div class="panel"><div id="st-table"></div></div>
    <div id="st-extra" style="margin-top:10px"></div>
    <div class="panel" style="margin-top:12px">
      <div style="display:flex;align-items:center;gap:8px;flex-wrap:wrap"><h4 style="margin:0">往来期初明细（按单据 · 迁移）</h4><span class="grow"></span>
        <span class="muted" style="font-size:12px" id="arap-total"></span>
        <button class="btn ghost sm" id="arap-reload">刷新</button>
        ${can("voucher_new") ? `<button class="btn ghost sm" id="arap-goimport">去导入</button>` : ""}
      </div>
      <div id="arap-list" class="muted" style="margin-top:8px">加载中…</div>
    </div>
    <div class="panel" style="margin-top:12px">
      <div style="display:flex;align-items:center;gap:8px;flex-wrap:wrap"><h4 style="margin:0">催款单 / 对账函</h4><span class="grow"></span>
        <span class="muted" style="font-size:12px" id="dn-total"></span>
        <button class="btn ghost sm" id="dn-reload">刷新</button>
        ${can("voucher_new") ? `<button class="btn primary sm" id="dn-new">新建催款单</button>` : ""}
      </div>
      <div id="dn-list" class="muted" style="margin-top:8px">加载中…</div>
    </div>`;
  let selFrom = null, selTo = null, rows = [];
  const post = (path, body) => api(path, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body || {}) });
  const acct = () => $("#st-acct", main).value.trim();
  const upto = () => $("#st-upto", main).value.trim();

  function render() {
    const totalOpen = rows.reduce((a, r) => a + (parseFloat(String(r.open).replace(/,/g, "")) || 0), 0);
    $("#st-sum", main).innerHTML = `未核销 <b>${rows.length}</b> 笔，合计 <b>${totalOpen.toFixed(2)}</b>`;
    $("#st-table", main).innerHTML = `<table class="grid"><thead><tr><th>日期</th><th>凭证号</th><th>往来对象</th><th>摘要</th><th>方向</th><th class="num">发生额</th><th class="num">已核销</th><th class="num">未核销</th><th>选择</th></tr></thead><tbody>${rows.length ? rows.map((r) => `<tr><td>${esc(r.date)}</td><td>${esc(r.voucher_no)}</td><td>${esc(r.aux_key || "—")}</td><td>${esc(r.summary)}</td><td>${esc(r.dir)}</td><td class="num">${esc(r.debit === "0.00" ? r.credit : r.debit)}</td><td class="num">${esc(r.settled)}</td><td class="num">${esc(r.open)}</td><td>
      <button class="btn sm ${selFrom === r.entry_id ? "primary" : "ghost"}" data-from="${r.entry_id}">原单</button>
      <button class="btn sm ${selTo === r.entry_id ? "primary" : "ghost"}" data-to="${r.entry_id}">收/付</button></td></tr>`).join("") : `<tr><td colspan="9" class="muted" style="text-align:center;padding:16px">没有未核销分录</td></tr>`}</tbody></table>`;
    $all("[data-from]", main).forEach((b) => b.onclick = () => { selFrom = parseInt(b.dataset.from, 10); render(); });
    $all("[data-to]", main).forEach((b) => b.onclick = () => { selTo = parseInt(b.dataset.to, 10); render(); });
  }

  async function load() {
    const a = acct(), u = upto();
    try {
      const d = await api(`/settle/open?account=${encodeURIComponent(a)}&upto=${encodeURIComponent(u)}`);
      rows = d.rows || [];
      selFrom = selTo = null;
      $("#st-extra", main).innerHTML = "";
      render();
    } catch (e) { $("#st-sum", main).textContent = e.message; }
  }

  // 往来期初明细（迁移数据）：列表 + 合计 + 删除；「账龄分析」自动包含期初行（影子挂账）
  const loadArap = async () => {
    try {
      const r = await api("/arap-opening");
      const arows = r.rows || [];
      $("#arap-total").textContent = arows.length ? `应收合计 ${r.total_ar} · 应付合计 ${r.total_ap}` : "";
      $("#arap-list").innerHTML = arows.length
        ? `<table class="grid"><thead><tr><th>类型</th><th>客商</th><th>名称</th><th>单据号</th><th>单据日期</th><th class="num">金额</th><th>备注</th><th>录入</th><th></th></tr></thead><tbody>${arows.map((o) => `<tr><td>${o.kind === "ar" ? '<span class="tag">应收</span>' : '<span class="tag warn">应付</span>'}</td><td>${esc(o.party_code)}</td><td>${esc(o.party_name || "—")}</td><td>${esc(o.doc_no)}</td><td>${esc(o.doc_date)}</td><td class="num">${fmt(o.amount)}</td><td>${esc(o.memo || "")}</td><td>${esc(o.created_by)}</td>
            <td class="row-actions">${can("voucher_new") ? `<button class="btn ghost sm" data-arap-del="${o.id}">删</button>` : ""}</td></tr>`).join("")}</tbody></table>
          <p class="muted" style="font-size:12px;margin-top:6px">期初明细为影子挂账：进入「账龄分析」（单据号标「期初」），暂不参与自动核销（核销 v2）；金额总额仍以科目期初为准。</p>`
        : `<div class="muted">暂无往来期初（点「去导入」按单据批量迁移）</div>`;
      $all("[data-arap-del]", main).forEach((b) => b.onclick = async () => {
        if (!(await confirmDialog("删除该期初明细行？", true))) return;
        try { await api(`/arap-opening/${b.dataset.arapDel}`, { method: "DELETE" }); toast("已删除", "ok"); loadArap(); } catch (e) { toast(e.message, "err"); }
      });
    } catch (e) { $("#arap-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  loadArap();
  if ($("#arap-reload", main)) $("#arap-reload", main).onclick = loadArap;
  if ($("#arap-goimport", main)) $("#arap-goimport", main).onclick = () => { state.view = "imports"; renderMain(); };

  // 催款单 / 对账函：按客商快照未核销 + 期初；状态流转 + 打印预览
  const loadDunnings = async () => {
    try {
      const r = await api("/settle/dunnings");
      const drows = r.rows || [];
      const stMap = { draft: ["草稿", ""], sent: ["已发出", "warn"], settled: ["已结清", "ok"], cancelled: ["已作废", ""] };
      $("#dn-total").textContent = drows.length ? `共 ${drows.length} 张` : "";
      $("#dn-list").innerHTML = drows.length
        ? `<table class="grid"><thead><tr><th>单号</th><th>日期</th><th>类型</th><th>客商</th><th class="num">金额</th><th class="num">笔数</th><th>状态</th><th>明细</th><th></th></tr></thead><tbody>${drows.map((d) => {
            const st = stMap[d.status] || [d.status, ""];
            const acts = can("voucher_new") ? [
              d.status === "draft" ? `<button class="btn ghost sm" data-dn-st="${d.id}" data-to="sent">发出</button>` : "",
              (d.status === "draft" || d.status === "sent") ? `<button class="btn ghost sm" data-dn-st="${d.id}" data-to="settled">结清</button>` : "",
              (d.status === "draft" || d.status === "sent") ? `<button class="btn ghost sm" data-dn-st="${d.id}" data-to="cancelled">作废</button>` : "",
            ].filter(Boolean).join(" ") : "";
            return `<tr><td>${esc(d.no)}</td><td>${esc(d.date)}</td><td>${d.kind === "ar" ? '<span class="tag">催款</span>' : '<span class="tag warn">对账函</span>'}</td>
              <td>${esc(d.party_code)} ${esc(d.party_name || "")}</td><td class="num">${fmt(d.amount)}</td><td class="num">${d.item_count}</td>
              <td><span class="tag ${st[1]}">${st[0]}</span></td>
              <td><button class="btn ghost sm" data-dn-detail="${d.id}">查看</button></td><td class="row-actions">${acts}</td></tr>`;
          }).join("")}</tbody></table>`
        : `<div class="muted">暂无催款单（点「新建催款单」按客商生成）</div>`;
      window._dnCache = drows;
      $all("[data-dn-st]", main).forEach((b) => b.onclick = async () => {
        try { await post(`/settle/dunnings/${b.dataset.dnSt}/status`, { status: b.dataset.to }); toast("状态已更新", "ok"); loadDunnings(); }
        catch (e) { toast(e.message, "err"); }
      });
      $all("[data-dn-detail]", main).forEach((b) => b.onclick = () => {
        const d = (window._dnCache || []).find((x) => x.id === parseInt(b.dataset.dnDetail, 10));
        if (d) openDunningDetail(d);
      });
    } catch (e) { $("#dn-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  function openDunningDetail(d) {
    const m = modal(`<h3>${esc(d.no)} · ${d.kind === "ar" ? "催款单" : "对账函"} <span class="muted" style="font-size:12px">${esc(d.party_code)} ${esc(d.party_name || "")}</span></h3>
      <table class="grid" id="dn-table"><thead><tr><th>单据</th><th>日期</th><th>摘要</th><th class="num">金额</th></tr></thead><tbody>${(d.detail || []).map((x) => `<tr><td>${esc(x.doc_no)}</td><td>${esc(x.date)}</td><td>${esc(x.summary)}</td><td class="num">${fmt(x.amount)}</td></tr>`).join("")}
      <tr><td colspan="3"><b>合计</b></td><td class="num"><b>${fmt(d.amount)}</b></td></tr></tbody></table>
      <div class="muted" style="font-size:12px;margin-top:6px">${esc(d.memo || "")}</div>
      <div class="foot"><button class="btn ghost" id="dn-print">打印预览</button><button class="btn ghost" id="dn-close">关闭</button></div>`);
    $("#dn-close", m).onclick = closeModal;
    $("#dn-print", m).onclick = () => printPreview(`${d.kind === "ar" ? "催款单" : "对账函"} ${d.no}`, $("#dn-table", m));
  }
  if ($("#dn-new", main)) $("#dn-new").onclick = async () => {
    const kind = prompt("类型：ar=催款单 / ap=对账函", "ar");
    if (kind === null) return;
    if (!["ar", "ap"].includes(kind.trim())) { toast("类型只能是 ar 或 ap", "err"); return; }
    const party = prompt("客商编码（如 C01）", "");
    if (party === null) return;
    if (!party.trim()) { toast("客商编码不能为空", "err"); return; }
    const pname = prompt("客商名称（可留空）", "");
    if (pname === null) return;
    try {
      const r = await post("/settle/dunnings", { kind: kind.trim(), party_code: party.trim(), party_name: pname.trim(), account: acct(), date: "" });
      toast(`已生成 ${r.dunning.no}，金额 ${fmt(r.dunning.amount)}（${r.dunning.item_count} 笔）`, "ok");
      loadDunnings();
    } catch (e) { toast(e.message, "err"); }
  };
  loadDunnings();
  if ($("#dn-reload", main)) $("#dn-reload", main).onclick = loadDunnings;

  $("#st-load", main).onclick = load;
  if ($("#st-auto", main)) $("#st-auto", main).onclick = async () => {
    const p = /^\d{6}$/.test(upto()) ? parseInt(upto(), 10) : 0;
    if (!(await confirmDialog(`对 ${acct()} 自动核销（等额优先，保守不勾错）？`, false))) return;
    try { const r = await post("/settle/auto", { account: acct(), ymm: p, tolerance: $("#st-tol", main).value.trim() }); toast(`核销 ${r.pairs} 对，金额 ${r.amount}（精确 ${r.exact}、尾差抹平 ${r.written_off}）`, "ok"); load(); } catch (e) { toast(e.message, "err"); }
  };
  if ($("#st-manual", main)) $("#st-manual", main).onclick = async () => {
    if (!selFrom || !selTo) { toast("请分别选择「原单」与「收/付」两行", "err"); return; }
    const amt = prompt("核销金额", "");
    if (amt === null) return;
    try { await post("/settle/run", { from_entry: selFrom, to_entry: selTo, amount: amt }); toast("已核销", "ok"); load(); } catch (e) { toast(e.message, "err"); }
  };
  if ($("#st-aging", main)) $("#st-aging", main).onclick = async () => {
    try {
      const d = await api(`/settle/aging?account=${encodeURIComponent(acct())}&upto=${encodeURIComponent(upto())}`);
      $("#st-extra", main).innerHTML = `<div class="panel"><b>账龄分析（${esc(d.as_of)}）</b><table class="grid" style="margin-top:6px"><thead><tr><th>往来对象</th>${d.buckets.map((b) => `<th class="num">${esc(b)}</th>`).join("")}<th class="num">借方合计</th><th class="num">贷方合计</th><th class="num">净额</th><th class="num">最老天数</th></tr></thead><tbody>${(d.rows || []).length ? d.rows.map((r) => `<tr><td>${esc(r.key || "—")}</td>${r.amounts.map((x) => `<td class="num">${esc(x)}</td>`).join("")}<td class="num">${esc(r.total)}</td><td class="num">${esc(r.credit_total)}</td><td class="num">${esc(r.net)}</td><td class="num">${r.max_days}</td></tr>`).join("") : `<tr><td colspan="${d.buckets.length + 5}" class="muted">无数据</td></tr>`}</tbody></table></div>`;
    } catch (e) { toast(e.message, "err"); }
  };
  if ($("#st-records", main)) $("#st-records", main).onclick = async () => {
    try {
      const d = await api(`/settle/records?account=${encodeURIComponent(acct())}`);
      const rows2 = d.rows || [];
      $("#st-extra", main).innerHTML = `<div class="panel"><b>核销记录</b><table class="grid" style="margin-top:6px"><thead><tr><th>期间</th><th>往来对象</th><th class="num">金额</th><th>操作人</th><th>时间</th><th></th></tr></thead><tbody>${rows2.length ? rows2.map((r) => `<tr><td>${esc(r.period)}</td><td>${esc(r.aux_key || "—")}</td><td class="num">${esc(r.amount)}</td><td>${esc(r.settled_by)}</td><td>${esc(r.settled_at)}</td><td>${can("voucher_new") ? `<button class="btn sm ghost" data-unsettle="${r.id}">取消核销</button>` : ""}</td></tr>`).join("") : `<tr><td colspan="6" class="muted">暂无记录</td></tr>`}</tbody></table></div>`;
      $all("[data-unsettle]", main).forEach((b) => b.onclick = async () => {
        try { await post("/settle/unsettle", { id: parseInt(b.dataset.unsettle, 10) }); toast("已取消", "ok"); $("#st-records", main).click(); } catch (e) { toast(e.message, "err"); }
      });
    } catch (e) { toast(e.message, "err"); }
  };
  load();
}

// ===========================================================================
// 自定义报表（UFO 公式，与桌面端对齐）
// ===========================================================================
async function viewCustomReports(main) {
  const cur = (state.current || "").replace("-", "");
  main.innerHTML = `<h2>自定义报表</h2>
    <div class="toolbar">
      <label>报表 <select id="cr-list" style="min-width:160px"></select></label>
      <button class="btn sm" id="cr-load">打开</button>
      ${can("account_edit") ? `<button class="btn ghost sm" id="cr-new">新建</button>
      <button class="btn danger sm" id="cr-del">删除</button>` : ""}
      <div class="spacer"></div>
      <label>期间 <input id="cr-period" value="${esc(cur)}" style="width:90px" /></label>
      <button class="btn ghost sm" id="cr-preview">生成</button>
      <button class="btn ghost sm" id="cr-print">打印预览</button>
    </div>
    <div class="panel">
      <div class="field"><label>名称</label><input id="cr-name" /></div>
      <div class="field"><label>列标题（逗号分隔）</label><input id="cr-cols" placeholder="本期,本年累计" /></div>
      <div style="margin-top:6px">
        ${can("account_edit") ? `<button class="btn ghost sm" id="cr-addrow">+ 行</button>
        <button class="btn ghost sm" id="cr-addcol">+ 列</button>
        <button class="btn sm" id="cr-save">保存</button>` : `<span class="muted">无修改权限（需要 account_edit）</span>`}
        <span id="cr-err" class="muted" style="color:var(--err)"></span>
      </div>
      <div id="cr-grid" style="margin-top:8px" class="muted">新建或打开一张报表</div>
      <div class="muted" style="font-size:12px;margin-top:6px">公式示例：QM("1001") 期末余额、QC("1001") 期初余额、FS("6001",-1,"贷") 上期贷方发生额、LFS("6001") 本年累计；支持 + - * / 与括号。</div>
    </div>
    <div id="cr-preview-box" style="margin-top:10px"></div>`;
  let report = null;
  async function refreshList() {
    try {
      const list = await api("/custom-reports");
      $("#cr-list", main).innerHTML = list.length
        ? list.map((r) => `<option value="${esc(r.key)}">${esc(r.key)} ${esc(r.name)}</option>`).join("")
        : `<option value="">（暂无）</option>`;
    } catch (e) { toast(e.message, "err"); }
  }
  function renderGrid() {
    const box = $("#cr-grid", main);
    if (!report) { box.className = "muted"; box.innerHTML = "新建或打开一张报表"; return; }
    box.className = "";
    const cols = report.columns || [];
    const head = `<tr><th>行名称</th>${cols.map((c, i) => `<th>${esc(c)} <button class="btn sm ghost" data-delcol="${i}">×</button></th>`).join("")}<th></th></tr>`;
    const body = (report.lines || []).map((l, li) => `<tr>
      <td><input class="cr-lname" data-li="${li}" value="${esc(l.name)}" style="width:130px" /></td>
      ${cols.map((_, ci) => `<td><input class="cr-f" data-li="${li}" data-ci="${ci}" value="${esc((l.formulas || [])[ci] || "")}" style="width:150px" placeholder='如 QM("1001")' /></td>`).join("")}
      <td><button class="btn sm ghost" data-delrow="${li}">×</button></td></tr>`).join("");
    box.innerHTML = `<table class="grid"><thead>${head}</thead><tbody>${body || `<tr><td colspan="${cols.length + 2}" class="muted">暂无行，点「+ 行」新增</td></tr>`}</tbody></table>`;
    $all(".cr-lname", box).forEach((inp) => inp.oninput = () => { report.lines[+inp.dataset.li].name = inp.value; });
    $all(".cr-f", box).forEach((inp) => inp.oninput = () => {
      const l = report.lines[+inp.dataset.li];
      l.formulas = l.formulas || [];
      while (l.formulas.length <= +inp.dataset.ci) l.formulas.push("");
      l.formulas[+inp.dataset.ci] = inp.value;
    });
    $all("[data-delrow]", box).forEach((b) => b.onclick = () => { report.lines.splice(+b.dataset.delrow, 1); renderGrid(); });
    $all("[data-delcol]", box).forEach((b) => b.onclick = () => {
      const i = +b.dataset.delcol;
      report.columns.splice(i, 1);
      report.lines.forEach((l) => (l.formulas || []).splice(i, 1));
      renderGrid();
    });
  }
  function fillForm() {
    $("#cr-name", main).value = report ? report.name : "";
    $("#cr-cols", main).value = report ? (report.columns || []).join(",") : "";
    renderGrid();
  }
  async function open(key) {
    try { const d = await api(`/custom-reports/${encodeURIComponent(key)}`); report = d.report; fillForm(); }
    catch (e) { toast(e.message, "err"); }
  }
  await refreshList();
  const first = $("#cr-list", main).value;
  if (first) await open(first);
  $("#cr-load", main).onclick = () => { const k = $("#cr-list", main).value; if (k) open(k); };
  if ($("#cr-new", main)) $("#cr-new", main).onclick = () => {
    report = { key: "", name: "新报表", columns: ["本期", "本年累计"], lines: [{ name: "", indent: 0, formulas: ["", ""], bold: false }] };
    fillForm();
  };
  if ($("#cr-del", main)) $("#cr-del", main).onclick = async () => {
    if (!report || !report.key) { toast("请先打开一张已保存的报表", "err"); return; }
    if (!(await confirmDialog(`删除自定义报表「${report.name}」？`, true))) return;
    try { await api(`/custom-reports/${encodeURIComponent(report.key)}/delete`, { method: "POST" }); toast("已删除", "ok"); report = null; fillForm(); refreshList(); } catch (e) { toast(e.message, "err"); }
  };
  if ($("#cr-addrow", main)) $("#cr-addrow", main).onclick = () => {
    if (!report) return;
    report.lines.push({ name: "", indent: 0, formulas: (report.columns || []).map(() => ""), bold: false });
    renderGrid();
  };
  if ($("#cr-addcol", main)) $("#cr-addcol", main).onclick = () => {
    if (!report) return;
    report.columns.push(`列${report.columns.length + 1}`);
    report.lines.forEach((l) => { l.formulas = l.formulas || []; l.formulas.push(""); });
    fillForm();
  };
  if ($("#cr-save", main)) $("#cr-save", main).onclick = async () => {
    if (!report) return;
    report.name = $("#cr-name", main).value.trim();
    report.columns = $("#cr-cols", main).value.split(",").map((s) => s.trim()).filter(Boolean);
    try {
      const r = await api("/custom-reports", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(report) });
      report.key = r.key;
      const errs = r.errors || [];
      $("#cr-err", main).textContent = errs.length ? `公式有误：第 ${errs[0].line + 1} 行第 ${errs[0].column + 1} 列 ${errs[0].error}` : "";
      toast("已保存", "ok");
      refreshList();
    } catch (e) { toast(e.message, "err"); }
  };
  $("#cr-preview", main).onclick = async () => {
    if (!report || !report.key) { toast("请先保存报表", "err"); return; }
    try {
      const d = await api(`/custom-reports/${encodeURIComponent(report.key)}?period=${encodeURIComponent($("#cr-period", main).value.trim())}`);
      const cols = d.report.columns || [];
      const vals = d.values || [];
      $("#cr-preview-box", main).innerHTML = `<div class="panel"><b>${esc(d.report.name)}（${esc(d.period)}）</b><table class="grid" id="cr-preview-table" style="margin-top:6px"><thead><tr><th>项目</th>${cols.map((c) => `<th class="num">${esc(c)}</th>`).join("")}</tr></thead><tbody>${vals.length ? vals.map((row, i) => `<tr><td>${esc((d.report.lines[i] || {}).name || "")}</td>${row.map((v) => `<td class="num">${esc(v)}</td>`).join("")}</tr>`).join("") : `<tr><td colspan="${cols.length + 1}" class="muted">无数据</td></tr>`}</tbody></table></div>`;
    } catch (e) { toast(e.message, "err"); }
  };
  $("#cr-print", main).onclick = () => {
    const t = $("#cr-preview-table", main);
    if (!t) { toast("请先生成", "err"); return; }
    printPreview("自定义报表", t);
  };
}

// ===========================================================================
// 采购对账
// ===========================================================================
async function viewPoReconcile(main) {
    main.innerHTML = `<h2>采购对账</h2>
    <div class="toolbar"><button class="btn ghost sm" id="pr-result-print">打印预览</button></div>
    <div id="pr-result" class="muted">加载中…</div>`;
  $("#pr-result-print").addEventListener("click", () => { const el = $("#pr-result").querySelector("table"); printPreview("采购对账", el); });
  try {
    const r = await api("/procure/reconcile");
    const rows = r.rows || [];
    $("#pr-result").innerHTML = `<table><thead><tr><th>订单号</th><th>供应商</th><th>订单金额</th><th>已付款</th><th>未付款</th><th>暂估</th></tr></thead>
      <tbody>${rows.map((x) => `<tr><td>${esc(x.no)}</td><td>${esc(x.supplier)}</td><td class="r">${fmt(x.order_amount)}</td><td class="r">${fmt(x.paid)}</td><td class="r">${fmt(x.unpaid)}</td><td class="r">${fmt(x.open_estimate)}</td></tr>`).join("")}</tbody></table>`;
  } catch (e) { $("#pr-result").innerHTML = `<div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; }
}

// ===========================================================================
// 销售对账
// ===========================================================================
async function viewSoReconcile(main) {
    main.innerHTML = `<h2>销售对账</h2>
    <div class="toolbar"><button class="btn ghost sm" id="sr-result-print">打印预览</button></div>
    <div id="sr-result" class="muted">加载中…</div>`;
  $("#sr-result-print").addEventListener("click", () => { const el = $("#sr-result").querySelector("table"); printPreview("销售对账", el); });
  try {
    const r = await api("/sales/reconcile");
    const rows = r.rows || [];
    $("#sr-result").innerHTML = `<table><thead><tr><th>订单号</th><th>客户</th><th>订单金额</th><th>已收款</th><th>未收款</th></tr></thead>
      <tbody>${rows.map((x) => `<tr><td>${esc(x.no)}</td><td>${esc(x.customer)}</td><td class="r">${fmt(x.order_amount)}</td><td class="r">${fmt(x.received)}</td><td class="r">${fmt(x.unreceived)}</td></tr>`).join("")}</tbody></table>`;
  } catch (e) { $("#sr-result").innerHTML = `<div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; }
}

// ===========================================================================
// 库存账龄
// ===========================================================================
async function viewInvAging(main) {
    main.innerHTML = `<h2>库存账龄分析</h2>
    <div class="toolbar"><button class="btn ghost sm" id="ia-result-print">打印预览</button></div>
    <div id="ia-result" class="muted">加载中…</div>`;
  $("#ia-result-print").addEventListener("click", () => { const el = $("#ia-result").querySelector("table"); printPreview("库存账龄分析", el); });
  try {
    const r = await api("/inventory/aging");
    const rows = r.rows || [];
    $("#ia-result").innerHTML = `<table><thead><tr><th>存货</th><th>最近入库</th><th>账龄(天)</th><th>结存数量</th><th>结存金额</th></tr></thead>
      <tbody>${rows.map((x) => `<tr><td>${esc(x.item)}</td><td>${esc(x.last_in || "—")}</td><td class="r">${x.days}</td><td class="r">${fmt(x.qty)}</td><td class="r">${fmt(x.amount)}</td></tr>`).join("")}</tbody></table>`;
  } catch (e) { $("#ia-result").innerHTML = `<div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; }
}

// ===========================================================================
// 库存 ABC
// ===========================================================================
async function viewInvAbc(main) {
  main.innerHTML = `<h2>库存 ABC 分析</h2>
    <div class="toolbar"><button class="btn ghost sm" id="ib-result-print">打印预览</button></div>
    <div id="ib-result" class="muted">加载中…</div>`;
  $("#ib-result-print").addEventListener("click", () => { const el = $("#ib-result").querySelector("table"); printPreview("库存ABC分析", el); });
  try {
    const r = await api("/inventory/abc");
    const rows = r.rows || [];
    $("#ib-result").innerHTML = `<table><thead><tr><th>存货</th><th>结存金额</th><th>累计占比</th><th>分类</th></tr></thead>
      <tbody>${rows.map((x) => `<tr><td>${esc(x.item)}</td><td class="r">${fmt(x.amount)}</td><td class="r">${x.cum_pct}%</td><td>${esc(x.class)}</td></tr>`).join("")}</tbody></table>`;
  } catch (e) { $("#ib-result").innerHTML = `<div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; }
}

// ===========================================================================
// 库存深度：序列号 / 多单位 / 组装拆卸 / 分仓库 / 调拨
// ===========================================================================
function postJson(path, body) {
  return api(path, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
}

async function viewInvSerial(main) {
  main.innerHTML = `<h2>序列号管理</h2>
    <div class="toolbar">
      <label>存货 <input id="is-item" style="width:140px" /></label>
      <button class="btn primary" id="is-load">查询在库</button>
      <span class="grow"></span>
    </div>
    <div class="toolbar">
      <label>日期 <input id="is-date" style="width:110px" /></label>
      <label>批次 <input id="is-batch" style="width:110px" /></label>
      <label>序列号(逗号分隔) <input id="is-serials" style="width:240px" placeholder="S001,S002" /></label>
      <button class="btn" id="is-in">入库登记</button>
      <button class="btn" id="is-out">出库登记</button>
    </div>
    <div id="is-result" class="muted">填写存货后点击查询</div>`;
  $("#is-date").value = new Date().toISOString().slice(0, 10);
  $("#is-load").addEventListener("click", async () => {
    const item = $("#is-item").value.trim();
    if (!item) { toast("请填写存货", "err"); return; }
    try {
      const r = await api(`/inventory/serial?item=${encodeURIComponent(item)}`);
      const rows = r.rows || [];
      $("#is-result").innerHTML = rows.length
        ? `<table><thead><tr><th>序列号</th><th>存货</th><th>批次</th><th>状态</th><th>入库日期</th><th>出库日期</th></tr></thead>
          <tbody>${rows.map((x) => `<tr><td>${esc(x.serial)}</td><td>${esc(x.item)}</td><td>${esc(x.batch_no)}</td><td>${esc(x.status === "in" ? "在库" : x.status === "out" ? "已出库" : "报废")}</td><td>${esc(x.in_date)}</td><td>${esc(x.out_date || "—")}</td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">无序列号记录</div>`;
    } catch (e) { toast(e.message, "err"); }
  });
  $("#is-in").addEventListener("click", async () => {
    const item = $("#is-item").value.trim();
    const serials = ($("#is-serials").value || "").split(/[,，\s]+/).map((s) => s.trim()).filter(Boolean);
    if (!item || !serials.length) { toast("请填写存货与序列号", "err"); return; }
    try {
      const r = await postJson("/inventory/serial", { item, serials, batch_no: $("#is-batch").value.trim(), date: $("#is-date").value.trim() });
      toast(`已入库 ${r.count} 个序列号`, "ok");
      $("#is-serials").value = "";
      $("#is-load").click();
    } catch (e) { toast(e.message, "err"); }
  });
  $("#is-out").addEventListener("click", async () => {
    const serials = ($("#is-serials").value || "").split(/[,，\s]+/).map((s) => s.trim()).filter(Boolean);
    if (!serials.length) { toast("请填写序列号", "err"); return; }
    try {
      const r = await postJson("/inventory/serial/out", { serials, date: $("#is-date").value.trim() });
      toast(`已出库 ${r.count} 个序列号`, "ok");
      $("#is-serials").value = "";
      $("#is-load").click();
    } catch (e) { toast(e.message, "err"); }
  });
}

async function viewInvUnit(main) {
  main.innerHTML = `<h2>多单位换算</h2>
    <div class="toolbar">
      <label>存货 <input id="iu-item" style="width:140px" /></label>
      <button class="btn primary" id="iu-load">载入</button>
      <span class="grow"></span>
    </div>
    <div class="toolbar">
      <label>主单位 <input id="iu-base" style="width:90px" /></label>
      <label>辅助单位 <input id="iu-alt" style="width:90px" /></label>
      <label>系数(1主=系数辅) <input id="iu-factor" style="width:90px" /></label>
      <button class="btn" id="iu-save">保存换算</button>
    </div>
    <div id="iu-info" class="muted"></div>`;
  $("#iu-load").addEventListener("click", async () => {
    const item = $("#iu-item").value.trim();
    if (!item) { toast("请填写存货", "err"); return; }
    try {
      const r = await api(`/inventory/unit?item=${encodeURIComponent(item)}`);
      if (r.unit) { $("#iu-base").value = r.unit.base_unit || ""; $("#iu-alt").value = r.unit.alt_unit || ""; $("#iu-factor").value = r.unit.factor || ""; $("#iu-info").textContent = "已载入现有换算"; }
      else { $("#iu-base").value = ""; $("#iu-alt").value = ""; $("#iu-factor").value = ""; $("#iu-info").textContent = "该存货尚未设置换算"; }
    } catch (e) { toast(e.message, "err"); }
  });
  $("#iu-save").addEventListener("click", async () => {
    const item = $("#iu-item").value.trim();
    if (!item) { toast("请填写存货", "err"); return; }
    try {
      await postJson("/inventory/unit", { item, base_unit: $("#iu-base").value.trim(), alt_unit: $("#iu-alt").value.trim(), factor: $("#iu-factor").value.trim() });
      toast("已保存换算", "ok");
      $("#iu-load").click();
    } catch (e) { toast(e.message, "err"); }
  });
}

async function viewInvAssemble(main) {
  main.innerHTML = `<h2>组装 / 拆卸</h2>
    <div class="toolbar">
      <label>日期 <input id="ia-date" style="width:110px" /></label>
      <label>成品/母件 <input id="ia-parent" style="width:140px" /></label>
      <label>备注 <input id="ia-memo" style="width:160px" /></label>
      <span class="grow"></span>
    </div>
    <div class="toolbar">
      <label>子件(名称:数量, 每行一个)</label>
      <textarea id="ia-children" style="width:420px;height:70px" placeholder="RM1:2&#10;RM2:1"></textarea>
      <button class="btn" id="ia-do">组装</button>
      <button class="btn" id="ia-undo">拆卸</button>
      <span class="muted" style="font-size:12px">组装=子件按移动加权成本等值转入成品；拆卸=成品成本按子件数量比例分摊；无成本价将被拒绝</span>
    </div>
    <div class="toolbar">
      <label style="font-weight:600">形态转换</label>
      <label>源物料 <input id="fc-from" style="width:120px" /></label>
      <label>→ 目标物料 <input id="fc-to" style="width:120px" /></label>
      <label>数量 <input id="fc-qty" style="width:80px" /></label>
      <label>备注 <input id="fc-memo" style="width:140px" /></label>
      <button class="btn" id="fc-do">转换</button>
      <span class="muted" style="font-size:12px">同数量一减一增，金额按源移动加权成本平移</span>
    </div>`;
  $("#ia-date").value = new Date().toISOString().slice(0, 10);
  const doOp = async (disassemble) => {
    const parent = $("#ia-parent").value.trim();
    const children = ($("#ia-children").value || "").split("\n").map((l) => l.trim()).filter(Boolean).map((l) => {
      const i = l.search(/[:：]/);
      return i < 0 ? [l, "0"] : [l.slice(0, i).trim(), l.slice(i + 1).trim()];
    });
    if (!parent || !children.length) { toast("请填写母件与子件", "err"); return; }
    try {
      await postJson(disassemble ? "/inventory/disassemble" : "/inventory/assemble", { parent, children, memo: $("#ia-memo").value.trim(), date: $("#ia-date").value.trim() });
      toast(disassemble ? "已拆卸" : "已组装", "ok");
    } catch (e) { toast(e.message, "err"); }
  };
  $("#ia-do").addEventListener("click", () => doOp(false));
  $("#fc-do").onclick = async () => {
    const from = $("#fc-from").value.trim(), to = $("#fc-to").value.trim(), qty = $("#fc-qty").value.trim();
    if (!from || !to || !qty) { toast("请填写源物料、目标物料与数量", "err"); return; }
    if (from === to) { toast("源与目标不能相同", "err"); return; }
    try {
      await postJson("/inventory/form-convert", { from_item: from, to_item: to, qty, memo: $("#fc-memo").value.trim(), date: $("#ia-date").value.trim() });
      toast(`已形态转换 ${from} → ${to} ×${qty}`, "ok");
      $("#fc-qty").value = ""; $("#fc-memo").value = "";
    } catch (e) { toast(e.message, "err"); }
  };
  $("#ia-undo").addEventListener("click", () => doOp(true));
}

// ===========================================================================
// 仓库档案（主数据，v30）：默认仓/停用/删除守卫
// ===========================================================================
async function viewWarehouses(main) {
  main.innerHTML = `<h2>仓库档案</h2>
    <div class="toolbar">
      <button class="btn ghost sm" id="wh-reload">刷新</button>
      ${can("warehouse") ? `<button class="btn primary sm" id="wh-new">新增仓库</button>` : ""}
      <span class="muted" style="font-size:12px">出入库未填仓库时按默认仓入账；默认仓不可删除，被流水引用的仓不可删除（可停用）</span>
    </div>
    <div class="panel"><div id="wh-list" class="muted">加载中…</div></div>`;
  const load = async () => {
    try {
      const r = await api("/warehouses");
      const rows = r.rows || [];
      $("#wh-list").innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>编码</th><th>名称</th><th>默认</th><th>状态</th><th>备注</th><th></th></tr></thead><tbody>${rows.map((w) => `<tr>
            <td>${esc(w.code)}</td><td>${esc(w.name)}</td>
            <td>${w.is_default ? '<span class="tag ok">默认</span>' : ""}</td>
            <td>${w.disabled ? '<span class="tag err">停用</span>' : '<span class="tag">启用</span>'}</td>
            <td>${esc(w.memo || "")}</td>
            <td class="row-actions">${can("warehouse") ? `${w.is_default ? "" : `<button class="btn ghost sm" data-wh-def="${esc(w.code)}">设默认</button>`}<button class="btn ghost sm" data-wh-edit="${esc(w.code)}">编辑</button>${w.is_default ? "" : `<button class="btn danger sm" data-wh-del="${esc(w.code)}">删</button>`}` : ""}</td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">暂无仓库（升级后应自动有默认仓「01 主仓」）</div>`;
      window._whCache = rows;
      $all("[data-wh-def]", main).forEach((b) => b.onclick = async () => {
        const w = (window._whCache || []).find((x) => x.code === b.dataset.whDef);
        if (!w) return;
        try {
          await api("/warehouses", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(Object.assign({}, w, { is_default: true })) });
          toast("已设为默认仓", "ok");
          load();
        } catch (e) { toast(e.message, "err"); }
      });
      $all("[data-wh-edit]", main).forEach((b) => b.onclick = () => {
        const w = (window._whCache || []).find((x) => x.code === b.dataset.whEdit);
        if (w) openWhEditor(w);
      });
      $all("[data-wh-del]", main).forEach((b) => b.onclick = async () => {
        if (!(await confirmDialog(`删除仓库 ${b.dataset.whDel}？被流水引用过将无法删除。`, true))) return;
        try { await api(`/warehouses/${encodeURIComponent(b.dataset.whDel)}`, { method: "DELETE" }); toast("已删除", "ok"); load(); }
        catch (e) { toast(e.message, "err"); }
      });
    } catch (e) { $("#wh-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  function openWhEditor(w) {
    const isNew = !w;
    const c = w || { code: "", name: "", is_default: false, disabled: false, memo: "" };
    const m = modal(`<h3>${isNew ? "新增" : "编辑"}仓库</h3>
      <div class="field"><label>编码 *</label><input id="wh-code" value="${esc(c.code)}" ${isNew ? "" : "disabled"} /></div>
      <div class="field"><label>名称 *</label><input id="wh-name" value="${esc(c.name)}" /></div>
      <div class="field"><label style="display:inline-flex;gap:4px;align-items:center"><input type="checkbox" id="wh-def" ${c.is_default ? "checked" : ""}/> 设为默认仓（出入库未填仓库时使用）</label></div>
      <div class="field"><label style="display:inline-flex;gap:4px;align-items:center"><input type="checkbox" id="wh-dis" ${c.disabled ? "checked" : ""}/> 停用（不可再入账）</label></div>
      <div class="field"><label>备注</label><input id="wh-memo" value="${esc(c.memo || "")}" /></div>
      <div class="foot"><button class="btn primary" id="wh-save">保存</button><button class="btn ghost" id="wh-cancel">取消</button></div>`);
    $("#wh-cancel", m).onclick = closeModal;
    $("#wh-save", m).onclick = async () => {
      const body = {
        code: $("#wh-code", m).value.trim(), name: $("#wh-name", m).value.trim(),
        is_default: $("#wh-def", m).checked, disabled: $("#wh-dis", m).checked, memo: $("#wh-memo", m).value.trim(),
      };
      if (!body.code || !body.name) { toast("编码与名称必填", "err"); return; }
      try {
        await api("/warehouses", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
        toast("已保存", "ok"); closeModal(); load();
      } catch (e) { toast(e.message, "err"); }
    };
  }
  if ($("#wh-new", main)) $("#wh-new").onclick = () => openWhEditor(null);
  $("#wh-reload", main).onclick = load;
  load();
}

async function viewInvWarehouse(main) {
  main.innerHTML = `<h2>分仓库库存</h2>
    <div class="toolbar">
      <label>存货 <input id="iw-item" style="width:160px" /></label>
      <button class="btn primary" id="iw-load">查询</button>
      <button class="btn ghost sm" id="iw-load-print">打印预览</button>
    </div>
    <div id="iw-result" class="muted">填写存货后点击查询</div>`;
  $("#iw-load").addEventListener("click", async () => {
  $("#iw-load-print").addEventListener("click", () => { const el = $("#iw-result").querySelector("table"); printPreview("分仓库库存", el); });
    const item = $("#iw-item").value.trim();
    if (!item) { toast("请填写存货", "err"); return; }
    try {
      const r = await api(`/inventory/warehouse-stock?item=${encodeURIComponent(item)}`);
      const rows = r.rows || [];
      $("#iw-result").innerHTML = rows.length
        ? `<table><thead><tr><th>仓库</th><th>存货</th><th>结存数量</th><th>可用</th><th>待检</th><th>隔离</th></tr></thead>
          <tbody>${rows.map((x) => `<tr><td>${esc(x.warehouse)}</td><td>${esc(x.item)}</td><td class="r">${fmt(x.qty)}</td><td class="r">${fmt(x.available || "0")}</td><td class="r">${x.pending && x.pending !== "0" ? fmt(x.pending) : "—"}</td><td class="r">${x.quarantine && x.quarantine !== "0" ? fmt(x.quarantine) : "—"}</td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">无库存</div>`;
    } catch (e) { toast(e.message, "err"); }
  });
}

async function viewInvTransfer(main) {
    main.innerHTML = `<h2>调拨报表</h2>
    <div class="toolbar"><button class="btn ghost sm" id="it-result-print">打印预览</button></div>
    <div id="it-result" class="muted">加载中…</div>`;
  $("#it-result-print").addEventListener("click", () => { const el = $("#it-result").querySelector("table"); printPreview("调拨报表", el); });
  try {
    const r = await api("/inventory/transfer");
    const rows = r.rows || [];
    $("#it-result").innerHTML = rows.length
      ? `<table><thead><tr><th>日期</th><th>存货</th><th>批号</th><th>仓库</th><th>数量</th><th>备注</th></tr></thead>
        <tbody>${rows.map((x) => `<tr><td>${esc(x.date)}</td><td>${esc(x.item)}</td><td>${esc(x.batch_no || "—")}</td><td>${esc(x.warehouse)}</td><td class="r">${fmt(x.qty)}</td><td>${esc(x.memo || "")}</td></tr>`).join("")}</tbody></table>`
      : `<div class="muted">本期间无调拨流水</div>`;
  } catch (e) { $("#it-result").innerHTML = `<div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; }
}

// ===========================================================================
// 采购/销售深度：采购暂估 / 供应商配额 / 订单变更
// ===========================================================================
// 存货盘点：账面按仓库快照（服务端）→ 录实盘 → 应用出其他入库/出库流水 + 盘盈盘亏凭证
async function viewInvCount(main) {
  main.innerHTML = `<h2>存货盘点</h2>
    <div class="toolbar">
      <label>仓库 <input id="ic-wh" style="width:110px" placeholder="空 = 全部仓库" /></label>
      <button class="btn primary" id="ic-new">新建盘点单</button>
      <span class="grow"></span>
      <span class="muted" style="font-size:12px">草稿可删；应用后生成盘盈盘亏流水与凭证（金额 = 差异 × 标准价，未配标准价只调流水），不可再删——冲回请做反向盘点</span>
    </div>
    <div id="ic-list" class="muted">加载中…</div>`;
  const load = async () => {
    try {
      const r = await api("/inventory/counts");
      const rows = r.rows || [];
      $("#ic-list").innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>单号</th><th>日期</th><th>仓库</th><th>明细（账面 → 实盘）</th><th>状态</th><th>凭证</th><th></th></tr></thead><tbody>${rows.map((c) => {
            const lines = (c.lines || []).map((l) => `${esc(l.item)}${l.batch_no ? `#${esc(l.batch_no)}` : ""} ${fmt(l.book_qty)} → <b>${fmt(l.count_qty)}</b>${Number(l.count_qty) - Number(l.book_qty) !== 0 ? `（${(Number(l.count_qty) - Number(l.book_qty)) > 0 ? "+" : ""}${Number(l.count_qty) - Number(l.book_qty)}）` : ""}`).join("；");
            return `<tr><td>${esc(c.no)}</td><td>${esc(c.date)}</td><td>${esc(c.warehouse || "全部")}</td><td>${lines || "—"}</td>
              <td>${c.status === "applied" ? '<span class="tag ok">已应用</span>' : '<span class="tag warn">草稿</span>'}</td>
              <td>${c.voucher_id ? `<a href="#" data-ic-v="${c.voucher_id}">凭证 #${c.voucher_id}</a>` : "—"}</td>
              <td class="row-actions">${c.status === "draft" ? `<button class="btn ghost sm" data-ic-apply="${c.id}">应用</button><button class="btn ghost sm" data-ic-del="${c.id}">删除</button>` : ""}</td></tr>`;
          }).join("")}</tbody></table>`
        : `<div class="muted">暂无盘点单，点「新建盘点单」开始</div>`;
      $all("[data-ic-v]").forEach((a) => a.onclick = (e) => { e.preventDefault(); openVoucherEditor(parseInt(a.dataset.icV, 10)); });
      $all("[data-ic-del]").forEach((b) => b.onclick = async () => {
        if (!(await confirmDialog("删除该盘点单？", true))) return;
        try { await api(`/inventory/count/${b.dataset.icDel}/delete`, { method: "POST" }); toast("已删除", "ok"); load(); } catch (e) { toast(e.message, "err"); }
      });
      $all("[data-ic-apply]").forEach((b) => b.onclick = async () => {
        try {
          const r2 = await postJson(`/inventory/count/${b.dataset.icApply}/apply`, {});
          toast(r2.voucher_id ? `已应用，盘盈盘亏凭证 #${r2.voucher_id}` : `已应用（${r2.message || "仅调库存流水"}）`, "ok");
          load();
        } catch (e) { toast(e.message, "err"); }
      });
    } catch (e) { $("#ic-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  $("#ic-new").onclick = () => openCountEditor($("#ic-wh").value.trim(), load);
  await load();
}

function openCountEditor(warehouse, reload) {
  let lines = [{ item: "", batch_no: "", count_qty: "", memo: "" }];
  const mask = modal(`<h3>新建盘点单</h3>
    <div class="toolbar">
      <label>日期 <input type="date" id="cn-date" value="${today()}" /></label>
      <label>仓库 <input id="cn-wh" value="${esc(warehouse)}" style="width:110px" placeholder="空 = 全部仓库" /></label>
      <label>备注 <input id="cn-memo" style="width:150px" /></label>
    </div>
    <table class="grid" id="cn-tbl"><thead><tr><th>存货编码 *</th><th>批次号</th><th>实盘数量 *</th><th>备注</th><th></th></tr></thead><tbody></tbody></table>
    <button class="btn ghost sm" id="cn-add">+ 增加行</button>
    <p class="muted" style="font-size:12px;margin:6px 0 0">保存时服务端按仓库快照账面数量（快照后可看到 账面→实盘 差异）；应用后不可修改。</p>
    <div class="foot"><button class="btn primary" id="cn-save">保存盘点单</button><button class="btn ghost" id="cn-cancel">取消</button></div>`);
  const tbody = $("#cn-tbl tbody", mask);
  const render = () => {
    tbody.innerHTML = lines.map((l, i) => `<tr>
      <td><input data-f="item" data-i="${i}" value="${esc(l.item)}" style="width:130px" /></td>
      <td><input data-f="batch_no" data-i="${i}" value="${esc(l.batch_no)}" placeholder="留空=整仓" style="width:110px" /></td>
      <td><input data-f="count_qty" data-i="${i}" value="${esc(l.count_qty)}" style="width:95px" /></td>
      <td><input data-f="memo" data-i="${i}" value="${esc(l.memo)}" style="width:140px" /></td>
      <td><button class="btn ghost sm" data-del="${i}">删</button></td></tr>`).join("");
    $all("input[data-f]", tbody).forEach((inp) => { inp.onchange = () => { lines[parseInt(inp.dataset.i, 10)][inp.dataset.f] = inp.value; }; });
    $all("[data-del]", tbody).forEach((b) => b.onclick = () => {
      lines.splice(parseInt(b.dataset.del, 10), 1);
      if (!lines.length) lines.push({ item: "", batch_no: "", count_qty: "", memo: "" });
      render();
    });
  };
  render();
  $("#cn-add", mask).onclick = () => { lines.push({ item: "", batch_no: "", count_qty: "", memo: "" }); render(); };
  $("#cn-cancel", mask).onclick = closeModal;
  $("#cn-save", mask).onclick = async () => {
    const clean = lines.filter((l) => l.item.trim());
    if (!clean.length) { toast("至少填写一行存货编码", "err"); return; }
    try {
      const r = await postJson("/inventory/count", {
        period: ymm(state.current || ""),
        date: $("#cn-date", mask).value,
        warehouse: $("#cn-wh", mask).value.trim(),
        memo: $("#cn-memo", mask).value.trim(),
        lines: clean.map((l) => ({ item: l.item.trim(), batch_no: (l.batch_no || "").trim(), count_qty: l.count_qty, memo: l.memo })),
      });
      toast(`已保存盘点单 ${r.no}`, "ok");
      closeModal();
      reload();
    } catch (e) { toast(e.message, "err"); }
  };
}

// 批次库存与库位（对标金蝶批号/保质期/货位）：登记写库存流水带批号（与普通库存同一本账）、
// 批次余额=流水按批汇总、FEFO 近效期先出推荐、临期预警、库位主数据
// 存货档案（独立页，C 选项）：一站式 编码/名称/单位/现量/计划参数/保质期/质检/停用
async function viewItemsMaster(main) {
  main.innerHTML = `<h2>存货档案</h2>
    <div class="toolbar">
      <label>搜索 <input id="im-q" placeholder="编码/名称" style="width:150px" /></label>
      <span class="grow"></span>
      <label style="font-size:12px"><input type="checkbox" id="im-low" /> 只看低库存</label>
      <button class="btn ghost sm" id="im-reload">刷新</button>
      <button class="btn primary" id="im-new">新增存货</button>
    </div>
    <div class="panel"><div id="im-list" class="muted">加载中…</div></div>
    <p class="muted" style="font-size:12px">档案字段（保质期/质检/安全库存等）在行内「编辑」统一维护；单位换算到「多单位换算」页；批量迁移用「数据导入 → 存货档案」。低库存 = 现量 &lt; 安全库存（红显）。</p>`;
  let rows = [];
  const render = () => {
    const q = ($("#im-q").value || "").trim().toLowerCase();
    const lowOnly = $("#im-low").checked;
    const list = rows.filter((r) => {
      if (q && !(r.code.toLowerCase().includes(q) || (r.name || "").toLowerCase().includes(q))) return false;
      if (lowOnly && !(Number(r.safety) > 0 && Number(r.qty) < Number(r.safety))) return false;
      return true;
    });
    $("#im-list").innerHTML = list.length
      ? `<table class="grid"><thead><tr><th>编码</th><th>名称</th><th>单位</th><th class="num">现量</th><th class="num">安全库存</th><th class="num">前置期</th><th>保质期</th><th>质检</th><th>状态</th><th>备注</th><th></th></tr></thead><tbody>${list.map((r) => {
          const low = Number(r.safety) > 0 && Number(r.qty) < Number(r.safety);
          return `<tr${low ? ' style="background:var(--err-bg)"' : ""}>
            <td>${esc(r.code)}</td><td>${esc(r.name)}</td><td>${esc(r.unit || "—")}</td>
            <td class="num"${low ? ' style="color:var(--err);font-weight:600"' : ""}>${fmt(r.qty)}</td>
            <td class="num">${Number(r.safety) ? fmt(r.safety) : "—"}</td>
            <td class="num">${r.lead_days ? r.lead_days + "天" : "—"}</td>
            <td>${Number(r.shelf_life) ? r.shelf_life + "天" : "—"}</td>
            <td>${r.qc ? '<span class="tag">质检</span>' : ""}</td>
            <td>${r.disabled ? '<span class="tag warn">停用</span>' : low ? '<span class="tag err">低库存</span>' : '<span class="tag ok">在用</span>'}</td>
            <td>${esc(r.memo || "")}</td>
            <td class="row-actions"><button class="btn ghost sm" data-im-edit="${r.id}">编辑</button></td>
          </tr>`;
        }).join("")}</tbody></table>`
      : `<div class="muted">暂无存货档案（右上「新增存货」，或到「数据导入」批量迁移）</div>`;
    $all("[data-im-edit]").forEach((b) => b.onclick = () => {
      const r = rows.find((x) => String(x.id) === b.dataset.imEdit);
      if (!r) return;
      // 构造 AuxEntity 形状复用通用档案编辑器（字段与辅助档案一致）
      openAuxEditor(main, {
        id: r.id, kind: "item", code: r.code, name: r.name,
        parent_code: r.parent || null, disabled: !!r.disabled,
        props: { shelf_life_days: String(r.shelf_life || "0"), qc_required: r.qc ? "1" : "0" },
        memo: r.memo || "",
      }, "item");
    });
  };
  const load = async () => {
    try {
      const r = await api("/items/master");
      rows = r.rows || [];
      render();
    } catch (e) { $("#im-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  $("#im-q").addEventListener("input", render);
  $("#im-low").addEventListener("change", render);
  $("#im-reload").addEventListener("click", load);
  $("#im-new").addEventListener("click", () => openAuxEditor(main, null, "item"));
  await load();
}

async function viewBatch(main) {
  main.innerHTML = `<h2>批次库存与库位</h2>
    <div class="toolbar">
      <label>存货 * <input id="bt-item" style="width:110px" /></label>
      <label>方向 <select id="bt-dir"><option value="in">入库</option><option value="out">出库</option></select></label>
      <label>批号 <input id="bt-no" style="width:150px" placeholder="留空自动 BT+日期+序号" /></label>
      <label>生产日期 <input type="date" id="bt-prod" value="${today()}" /></label>
      <label>仓库 <input id="bt-wh" style="width:90px" placeholder="空=默认仓" /></label>
      <label>库位 <select id="bt-loc" style="width:120px"><option value="">（无）</option></select></label>
      <label>数量 <input id="bt-qty" style="width:80px" /></label>
      <button class="btn primary" id="bt-reg">登记</button>
      <span class="grow"></span>
      <label>FEFO需求 <input id="bt-fq" style="width:70px" value="1" /></label>
      <button class="btn" id="bt-fefo">近效期推荐</button>
      <button class="btn ghost sm" id="bt-loc-new">新增库位</button>
    </div>
    <div id="bt-fefo-out" class="muted" style="margin-bottom:6px"></div>
    <div class="panel"><div style="display:flex;align-items:center;gap:8px"><h4 style="margin:0">批次台账</h4><span class="grow"></span><label style="font-size:12px">临期窗口 <input id="bt-days" value="30" style="width:56px" /> 天 <button class="btn ghost sm" id="bt-exp">刷新</button></label></div><div id="bt-list" class="muted" style="margin-top:6px">加载中…</div></div>
    <div class="panel"><div style="display:flex;align-items:center;gap:8px"><h4 style="margin:0">批次成本勾稽</h4><span class="grow"></span><span class="muted" style="font-size:12px">Σ批次价值 vs 存货辅助账期末</span></div><div id="bt-cost" class="muted" style="margin-top:8px">加载中…</div></div>
    <div class="panel"><h4 style="margin-top:12px">库位主数据（存储 / 拣货 / 隔离）</h4><div id="bt-locs" class="muted">加载中…</div></div>`;
  const loadLocs = async () => {
    try {
      const r = await api("/inventory/locations");
      const rows = r.rows || [];
      const sel = $("#bt-loc");
      if (sel) sel.innerHTML = `<option value="">（无）</option>` + rows.map((l) => `<option value="${esc(l.code)}">${esc(l.code)} ${esc(l.name)}</option>`).join("");
      $("#bt-locs").innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>编码</th><th>名称</th><th>类型</th><th>备注</th><th></th></tr></thead><tbody>${rows.map((l) => `<tr><td>${esc(l.code)}</td><td>${esc(l.name)}</td><td>${{ storage: "存储", pick: "拣货", quarantine: "隔离" }[l.kind] || esc(l.kind)}</td><td>${esc(l.memo || "")}</td><td class="row-actions"><button class="btn ghost sm" data-lo-del="${l.id}">删</button></td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">暂无库位，点「新增库位」</div>`;
      $all("[data-lo-del]").forEach((b) => b.onclick = async () => {
        if (!(await confirmDialog("删除该库位？", true))) return;
        try { await api(`/inventory/locations/${b.dataset.loDel}/delete`, { method: "POST" }); toast("已删除", "ok"); loadLocs(); } catch (e) { toast(e.message, "err"); }
      });
    } catch (e) { $("#bt-locs").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  const load = async () => {
    try {
      const [r1, r2] = await Promise.all([
        api("/inventory/batches"),
        api(`/inventory/batches/expiring?days=${parseInt($("#bt-days").value, 10) || 30}`),
      ]);
      const rows = r1.rows || [];
      const exp = new Set((r2.rows || []).map((b) => `${b.item}|${b.batch_no}`));
      $("#bt-list").innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>存货</th><th>批号</th><th>生产日期</th><th>失效日期</th><th>仓库</th><th>库位</th><th class="num">余额</th><th>状态</th><th></th></tr></thead><tbody>${rows.map((b) => {
            const isExp = exp.has(`${b.item}|${b.batch_no}`);
            return `<tr><td>${esc(b.item)}</td><td>${esc(b.batch_no)}</td><td>${esc(b.production_date || "—")}</td><td>${esc(b.expiry_date || "—")}</td><td>${esc(b.warehouse || "默认仓")}</td><td>${esc(b.location || "—")}</td><td class="num">${fmt(b.balance)}</td><td>${isExp ? '<span class="tag warn">临期</span>' : Number(b.balance) > 0 ? '<span class="tag ok">在库</span>' : '<span class="muted">已清</span>'}</td><td class="row-actions">${Number(b.balance) > 0 ? `<button class="btn ghost sm" data-tr="${esc(b.item)}|${esc(b.batch_no)}|${esc(b.warehouse || "")}">调拨</button>` : ""}</td></tr>`;
          }).join("")}</tbody></table>`
        : `<div class="muted">暂无批次，左上「登记」入库（批号留空自动生成）</div>`;
      $all("[data-tr]", $("#bt-list")).forEach((btn) => btn.onclick = () => {
        const p = btn.dataset.tr.split("|");
        openTransferEditor(p[0], p[1], p[2], load);
      });
    } catch (e) { $("#bt-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  // 批次成本勾稽：Σ批次价值 vs 存货辅助账期末，差异行高亮；明细折叠
  const loadBc = async () => {
    const box = $("#bt-cost");
    if (!box) return;
    try {
      const r = await api("/inventory/batch-cost");
      const tot = r.totals || [];
      const detail = r.detail || [];
      box.innerHTML = (tot.length
        ? `<table class="grid"><thead><tr><th>存货</th><th class="num">Σ批次数量</th><th class="num">Σ批次价值</th><th class="num">账面（辅助期末）</th><th class="num">差异</th></tr></thead><tbody>${tot.map((t) => `<tr><td>${esc(t.item)}</td><td class="num">${fmt(t.qty)}</td><td class="num">${fmt(t.amount)}</td><td class="num">${fmt(t.book)}</td><td class="num" style="${Number(t.diff) !== 0 ? "color:var(--err);font-weight:600" : ""}">${fmt(t.diff)}</td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">暂无批次流水</div>`)
        + (detail.length
          ? `<details style="margin-top:6px"><summary class="muted" style="cursor:pointer;font-size:12px">批次成本明细（${detail.length} 条）</summary><table class="grid" style="margin-top:4px"><thead><tr><th>存货</th><th>批号</th><th class="num">数量</th><th class="num">金额</th></tr></thead><tbody>${detail.map((d) => `<tr><td>${esc(d.item)}</td><td>${esc(d.batch_no)}</td><td class="num">${fmt(d.qty)}</td><td class="num">${fmt(d.amount)}</td></tr>`).join("")}</tbody></table></details>`
          : "");
    } catch (e) { box.innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  $("#bt-reg").onclick = async () => {
    const item = $("#bt-item").value.trim();
    if (!item) { toast("请填写存货编码", "err"); return; }
    if (!$("#bt-qty").value.trim()) { toast("请填写数量", "err"); return; }
    try {
      const dir = $("#bt-dir").value;
      const r = await postJson("/inventory/batch", {
        item, batch_no: $("#bt-no").value.trim(), production_date: $("#bt-prod").value,
        warehouse: $("#bt-wh").value.trim(), location: $("#bt-loc").value,
        qty: $("#bt-qty").value.trim(), direction: dir, memo: "",
      });
      toast(`已${dir === "in" ? "入库" : "出库"} 批次 ${r.batch_no}，余额 ${r.balance}`, "ok");
      $("#bt-no").value = ""; $("#bt-qty").value = "";
      load();
    } catch (e) { toast(e.message, "err"); }
  };
  $("#bt-fefo").onclick = async () => {
    const item = $("#bt-item").value.trim();
    if (!item) { toast("请填写存货编码", "err"); return; }
    try {
      const need = Number($("#bt-fq").value || 0);
      const r = await postJson("/inventory/batches/fefo", { item, qty: $("#bt-fq").value.trim() || "1" });
      const rows = r.rows || [];
      const got = rows.reduce((s, x) => s + Number(x.take || 0), 0);
      $("#bt-fefo-out").innerHTML = rows.length
        ? `<span class="tag ok">FEFO 近效期先出</span> ` + rows.map((x) => `${esc(x.batch_no)}${x.expiry_date ? `（失效 ${esc(x.expiry_date)}）` : ""} → <b>${esc(x.take)}</b>`).join("；") + (got < need ? `　<span class="tag warn">余额不足（需 ${need}）</span>` : "")
        : `<span class="tag warn">无可用批次余额</span>`;
    } catch (e) { toast(e.message, "err"); }
  };
  $("#bt-exp").onclick = () => load();
  $("#bt-loc-new").onclick = () => openLocEditor(loadLocs);
  await Promise.all([loadLocs(), load(), loadBc()]);
}

// 批次调拨（对标金蝶调拨单 v1）：源仓默认=批次主仓（可改，服务端按分仓余额校验）；
// 批号留空 = FEFO 近效期自动选批（近效期批次不足量时 400 提示分批）。
function openTransferEditor(item, batchNo, warehouse, reload) {
  const mask = modal(`<h3>批次调拨 · ${esc(item)}</h3>
    <div class="field"><label>批号</label><input id="tf-no" value="${esc(batchNo || "")}" placeholder="留空 = FEFO 近效期自动选批" style="width:220px" /></div>
    <div class="field" style="display:flex;gap:12px;flex-wrap:wrap">
      <div><label>源仓 *</label><input id="tf-from" value="${esc(warehouse || "")}" style="width:110px" /></div>
      <div><label>目标仓 *</label><input id="tf-to" style="width:110px" /></div>
      <div><label>数量 *</label><input id="tf-qty" style="width:100px" /></div>
      <div><label>日期</label><input type="date" id="tf-date" value="${today()}" /></div>
    </div>
    <div class="field"><label>备注</label><input id="tf-memo" style="width:100%" /></div>
    <p class="muted" style="font-size:12px;margin:6px 0 0">调出/调入同事务生成两条调拨流水（标准价，金额由计价引擎结算）；分仓数量按流水分账，批次主仓标签改写为目标仓。</p>
    <div class="foot"><button class="btn primary" id="tf-ok">执行调拨</button><button class="btn ghost" id="tf-cancel">取消</button></div>`);
  $("#tf-cancel", mask).onclick = closeModal;
  $("#tf-ok", mask).onclick = async () => {
    const bn = $("#tf-no", mask).value.trim();
    const fw = $("#tf-from", mask).value.trim();
    const tw = $("#tf-to", mask).value.trim();
    const q = $("#tf-qty", mask).value.trim();
    if (!fw || !tw) { toast("源仓与目标仓必填", "err"); return; }
    if (fw === tw) { toast("源仓与目标仓不能相同", "err"); return; }
    if (!q) { toast("请填写调拨数量", "err"); return; }
    try {
      const r = await postJson("/inventory/transfer", {
        period: ymm(state.current || ""), date: $("#tf-date", mask).value,
        item, batch_no: bn, from_warehouse: fw, to_warehouse: tw, qty: q,
        memo: $("#tf-memo", mask).value.trim(),
      });
      toast(`已调拨 ${item} ${r.batch_no} ${fw}→${tw} ×${q}（流水 ${r.out_id}/${r.in_id}）`, "ok");
      closeModal();
      reload && reload();
    } catch (e) { toast(e.message, "err"); }
  };
}

function openLocEditor(reload) {
  const mask = modal(`<h3>新增库位</h3>
    <div class="field"><label>编码 *</label><input id="lo-code" /></div>
    <div class="field"><label>名称 *</label><input id="lo-name" /></div>
    <div class="field"><label>类型</label><select id="lo-kind"><option value="storage">存储</option><option value="pick">拣货</option><option value="quarantine">隔离</option></select></div>
    <div class="field"><label>备注</label><input id="lo-memo" /></div>
    <div class="foot"><button class="btn primary" id="lo-save">保存</button><button class="btn ghost" id="lo-cancel">取消</button></div>`);
  $("#lo-cancel", mask).onclick = closeModal;
  $("#lo-save", mask).onclick = async () => {
    const code = $("#lo-code", mask).value.trim();
    const name = $("#lo-name", mask).value.trim();
    if (!code || !name) { toast("编码与名称必填", "err"); return; }
    try {
      await postJson("/inventory/locations", { code, name, kind: $("#lo-kind", mask).value, memo: $("#lo-memo", mask).value.trim() });
      toast("已保存库位", "ok"); closeModal(); reload();
    } catch (e) { toast(e.message, "err"); }
  };
}

async function viewPoEstimate(main) {
  main.innerHTML = `<h2>采购暂估</h2>
    <div class="toolbar">
      <label>采购订单ID <input id="pe-poid" style="width:90px" /></label>
      <button class="btn primary" id="pe-load">查询暂估</button>
      <span class="grow"></span>
    </div>
    <div class="toolbar">
      <label>存货(科目) <input id="pe-item" style="width:140px" placeholder="如 140301" /></label>
      <label>暂估金额 <input id="pe-amount" style="width:110px" /></label>
      <button class="btn" id="pe-add">登记暂估</button>
    </div>
    <div id="pe-result" class="muted">填写订单ID后查询</div>`;
  const load = async () => {
    const poId = $("#pe-poid").value.trim();
    if (!poId) { toast("请填写采购订单ID", "err"); return; }
    try {
      const r = await api(`/procure/estimate?po_id=${encodeURIComponent(poId)}`);
      const rows = r.rows || [];
      const open = rows.filter((x) => !x.settled).reduce((s, x) => s + Number(x.est_amount || 0), 0);
      $("#pe-result").innerHTML = `<div class="muted">未冲回暂估合计：<b>${fmtMoney(String(open))}</b></div>` + (rows.length
        ? `<table><thead><tr><th>#</th><th>存货</th><th>暂估金额</th><th>状态</th><th>操作</th></tr></thead>
          <tbody>${rows.map((x) => `<tr><td>${x.id}</td><td>${esc(x.item)}</td><td class="r">${fmt(x.est_amount)}</td><td>${x.settled ? "已冲回" : "未冲回"}</td><td>${x.settled ? "" : `<button class="btn sm" data-settle="${x.id}">冲回</button>`}</td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">无暂估记录</div>`);
      $all("[data-settle]", $("#pe-result")).forEach((b) => b.onclick = async () => {
        try { const x = await postJson(`/procure/estimate/${b.dataset.settle}/settle`, {}); toast(x.voucher_id ? `已冲回，冲回凭证 #${x.voucher_id}` : "已冲回", "ok"); load(); } catch (e) { toast(e.message, "err"); }
      });
    } catch (e) { toast(e.message, "err"); }
  };
  $("#pe-load").addEventListener("click", load);
  $("#pe-add").addEventListener("click", async () => {
    const po_id = parseInt($("#pe-poid").value.trim(), 10);
    if (!po_id) { toast("请填写采购订单ID", "err"); return; }
    try {
      const r = await postJson("/procure/estimate", { po_id, item: $("#pe-item").value.trim(), est_amount: $("#pe-amount").value.trim() });
      toast(r.voucher_id ? `已登记暂估，凭证 #${r.voucher_id}` : "已登记暂估", "ok");
      $("#pe-amount").value = "";
      load();
    } catch (e) { toast(e.message, "err"); }
  });
}

async function viewProcureQuota(main) {
  main.innerHTML = `<h2>供应商配额</h2>
    <div class="toolbar">
      <label>供应商 <input id="pq-sup" style="width:140px" /></label>
      <label>物料 <input id="pq-item" style="width:140px" /></label>
      <label>配额数量 <input id="pq-qty" style="width:110px" /></label>
      <button class="btn" id="pq-save">保存配额</button>
      <button class="btn primary" id="pq-query">查询剩余</button>
    </div>
    <div id="pq-result" class="muted"></div>`;
  $("#pq-save").addEventListener("click", async () => {
    const supplier = $("#pq-sup").value.trim(), item = $("#pq-item").value.trim();
    if (!supplier || !item) { toast("请填写供应商与物料", "err"); return; }
    try {
      await postJson("/procure/quota", { supplier, item, quota_qty: $("#pq-qty").value.trim() });
      toast("已保存配额", "ok");
      $("#pq-result").textContent = `剩余配额：${fmt($("#pq-qty").value.trim())}`;
    } catch (e) { toast(e.message, "err"); }
  });
  $("#pq-query").addEventListener("click", async () => {
    const supplier = $("#pq-sup").value.trim(), item = $("#pq-item").value.trim();
    if (!supplier || !item) { toast("请填写供应商与物料", "err"); return; }
    try {
      const r = await api(`/procure/quota?supplier=${encodeURIComponent(supplier)}&item=${encodeURIComponent(item)}`);
      $("#pq-result").textContent = r.remaining == null ? "该供应商/物料未设置配额" : `剩余配额：${fmt(String(r.remaining))}`;
    } catch (e) { toast(e.message, "err"); }
  });
}

// 来料质检（对标金蝶检验单）：合格留库；不合格自动按订单单价退货冲减库存
function openQcEditor(poId, reload) {
  const mask = modal(`<h3>来料质检 · 采购订单 #${esc(String(poId))}</h3>
    <div class="field"><label>检验数量 *</label><input id="qc-insp" placeholder="本批送检数量" /></div>
    <div class="field"><label>不合格数量（0 = 全部合格）</label><input id="qc-fail" value="0" /></div>
    <div class="field"><label>检验人</label><input id="qc-who" placeholder="留空 = 当前账号" /></div>
    <div class="field"><label>日期</label><input type="date" id="qc-date" value="${today()}" /></div>
    <div class="field"><label>备注</label><input id="qc-memo" /></div>
    <p class="muted" style="font-size:12px;margin:6px 0 0">待检物料（档案勾了「来料检验」）：按待检量逐笔检验，合格转正可用、不合格转隔离（仍可退货）；事后质检（未勾检验）：不合格自动按订单单价退货（单价为 0 请先补价）。</p>
    <div class="foot"><button class="btn primary" id="qc-save">保存质检单</button><button class="btn ghost" id="qc-cancel">取消</button></div>`);
  $("#qc-cancel", mask).onclick = closeModal;
  $("#qc-save", mask).onclick = async () => {
    const insp = $("#qc-insp", mask).value.trim();
    if (!insp) { toast("请填写检验数量", "err"); return; }
    try {
      const r = await postJson("/inventory/qc", {
        po_id: Number(poId), qty_insp: insp, qty_fail: $("#qc-fail", mask).value.trim() || "0",
        inspector: $("#qc-who", mask).value.trim(), date: $("#qc-date", mask).value, memo: $("#qc-memo", mask).value.trim(),
      });
      toast(`质检完成：合格 ${r.qty_pass}${Number(r.qty_fail) > 0 ? `，不合格 ${r.qty_fail} 已自动退货` : ""}`, "ok");
      closeModal();
      reload && reload();
    } catch (e) { toast(e.message, "err"); }
  };
}

// 发货通知（对标金蝶发货通知单）：订单确认后备货指令；出库后自动完成
function openNoticeEditor(soId, unshipped, reload) {
  const qtyDefault = unshipped > 0 ? String(unshipped) : "";
  const mask = modal(`<h3>发货通知 · 订单 #${esc(String(soId))}</h3>
    <div class="field"><label>通知数量 *</label><input id="sn-qty" value="${esc(qtyDefault)}" placeholder="未发量 ${unshipped}" /></div>
    <div class="field"><label>通知日期</label><input type="date" id="sn-date" value="${today()}" /></div>
    <div class="field"><label>备注</label><input id="sn-memo" placeholder="如：备货送仓 A 区" /></div>
    <p class="muted" style="font-size:12px;margin:6px 0 0">通知数量 ≤ 未发量；仓库在「待发通知」面板点「去出库」预填执行坞，发货成功后通知自动完成。</p>
    <div class="foot"><button class="btn primary" id="sn-save">发出通知</button><button class="btn ghost" id="sn-cancel">取消</button></div>`);
  $("#sn-cancel", mask).onclick = closeModal;
  $("#sn-save", mask).onclick = async () => {
    const q = $("#sn-qty", mask).value.trim();
    if (!q) { toast("请填写通知数量", "err"); return; }
    try {
      await postJson(`/sales/so/${Number(soId)}/notice`, { qty: q, date: $("#sn-date", mask).value, memo: $("#sn-memo", mask).value.trim() });
      toast("已发出发货通知（见「待发通知」面板）", "ok");
      closeModal();
      reload && reload();
      snLoad();
    } catch (e) { toast(e.message, "err"); }
  };
}

// 待发通知面板：pending 列表 → 「去出库」预填执行坞（发货成功后通知自动完成）
async function snLoad() {
  const box = document.getElementById("sn-list");
  if (!box) return;
  try {
    const r = await api("/sales/notices");
    const rows = (r.rows || []).filter((n) => n.status === "pending");
    box.innerHTML = rows.length
      ? `<table class="grid"><thead><tr><th>订单</th><th class="num">数量</th><th>通知日期</th><th>备注</th><th>通知人</th><th></th></tr></thead><tbody>${rows.map((n) => `<tr><td>#${n.so_id}</td><td class="num">${esc(String(n.qty))}</td><td>${esc(n.date)}</td><td>${esc(n.memo || "—")}</td><td>${esc(n.created_by)}</td>
          <td class="row-actions"><button class="btn ghost sm" data-sn-go="${esc(JSON.stringify({ so: n.so_id, qty: n.qty }))}">去出库</button></td></tr>`).join("")}</tbody></table>`
      : `<div class="muted">暂无待发通知（销售订单行「通知」按钮创建）</div>`;
    $all("[data-sn-go]", box).forEach((b) => b.onclick = () => {
      const d = JSON.parse(b.dataset.snGo);
      const soid = document.getElementById("sd-soid");
      const amtv = document.getElementById("sd-amt");
      const memoEl = document.getElementById("sd-memo");
      if (soid) soid.value = String(d.so);
      if (amtv) amtv.value = String(d.qty);
      if (memoEl) memoEl.value = "发货通知";
      toast(`已填入执行坞（订单 ${d.so} × ${d.qty}），点「发货」完成出库`, "ok");
      if (soid && soid.scrollIntoView) soid.scrollIntoView({ block: "center" });
    });
  } catch (e) { box.innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
}

// 单据链追溯面板（对标金蝶 源单/目标单 追溯）：上游源单 + 下游执行流水合并展示
async function openDocChain(kind, id, title) {
  const mask = modal(`<h3>单据链 · ${esc(title || kind)} #${esc(String(id))}</h3>
    <div id="dc-body" class="muted">加载中…</div>
    <div class="foot"><button class="btn ghost" id="dc-close">关闭</button></div>`);
  $("#dc-close", mask).onclick = closeModal;
  try {
    const r = await api(`/doc-links?kind=${encodeURIComponent(kind)}&id=${id}`);
    const rows = r.rows || [];
    const KIND = { req: "请购单", po: "采购订单", so: "销售订单", receipt: "到货/退货", payment: "采购付款", shipment: "发货/退货", so_payment: "销售收款" };
    const dirTag = (dd) => (dd === "up" ? `<span class="tag">上游</span>` : `<span class="tag ok">下游</span>`);
    $("#dc-body", mask).innerHTML = rows.length
      ? `<table class="grid"><thead><tr><th></th><th>类型</th><th>单号</th><th>日期</th><th>摘要</th><th class="num">数量</th><th class="num">金额</th><th>备注</th></tr></thead><tbody>${rows.map((n) => `<tr>
          <td>${dirTag(n.dir)}</td><td>${KIND[n.kind] || esc(n.kind)}</td><td>${n.no ? esc(n.no) : "#" + n.id}</td>
          <td>${esc(n.date)}</td><td>${esc(n.title || "—")}</td>
          <td class="num">${n.qty !== "0" ? esc(n.qty) : ""}</td>
          <td class="num">${n.amount !== "0" ? fmt(n.amount) : ""}</td>
          <td>${esc(n.memo || "")}</td></tr>`).join("")}</tbody></table>`
      : `<div class="muted">暂无关联单据（下推后此处显示源单；执行后显示到货/发货/收付款流水）</div>`;
  } catch (e) { $("#dc-body", mask).innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
}

async function viewPoDoc(main) {
  const period = encodeURIComponent(state.current || "");
  let lastReq = null; // 最近一条请购（「复制上一条」数据源）
  const table = (rows) => rows.length
    ? `<table class="grid"><thead><tr><th>单号</th><th>存货</th><th>数量</th><th>状态</th><th>请购人</th><th>备注</th><th></th></tr></thead>
      <tbody>${rows.map((r) => `<tr><td>${esc(r.no)}</td><td>${esc(r.item_name)}</td><td class="num">${esc(r.qty)}</td><td>${r.status === "ordered" ? `<span class="tag ok">已下推</span>` : esc(r.status)}<span data-wftag="purchase_req:${r.id}"></span></td><td>${esc(r.requester)}</td><td>${esc(r.memo)}</td><td class="row-actions">${r.status === "draft" ? `<button class="btn ghost sm" data-req-approve="${r.id}">审批</button>` : ""}${r.status === "approved" || r.status === "ordered" ? `<button class="btn primary sm" data-req-push="${r.id}">下推采购订单</button>` : ""}</td></tr>`).join("")}</tbody></table>`
    : `<div class="muted">暂无请购单</div>`;
  const poTrack = (rows) => rows.length
    ? `<table class="grid"><thead><tr><th>订单号</th><th>供应商</th><th>订单数量</th><th>到货数量</th><th>执行率</th></tr></thead>
      <tbody>${rows.map((t) => `<tr><td>${esc(t.no)}</td><td>${esc(t.supplier_name)}</td><td class="num">${esc(t.ordered_qty)}</td><td class="num">${esc(t.received_qty)}</td><td class="num">${esc(t.rate)}%</td></tr>`).join("")}</tbody></table>`
    : `<div class="muted">暂无采购订单</div>`;

  main.innerHTML = `<h2>采购单据</h2>
    <div class="toolbar">
      <label>存货 <input id="pd-item" style="width:130px" /></label>
      <label>数量 <input id="pd-qty" style="width:80px" /></label>
      <label>备注 <input id="pd-memo" style="width:140px" /></label>
      <button class="btn primary" id="pd-save">保存请购单</button>
      <button class="btn ghost sm" id="pd-copy">复制上一条</button>
    </div>
    <div class="toolbar">
      <label>采购订单ID <input id="pd-poid" style="width:80px" /></label>
      <label>数量/金额 <input id="pd-amt" style="width:100px" /></label>
      <label>仓库 <input id="pd-wh" style="width:90px" placeholder="默认仓" /></label>
      <label>备注 <input id="pd-memo2" style="width:120px" /></label>
      <button class="btn" id="pd-receipt">到货</button>
      <button class="btn" id="pd-return">退货</button>
      <button class="btn" id="pd-pay">付款</button>
    </div>
    <div class="panel" style="margin-top:12px"><div style="display:flex;align-items:center;gap:8px"><h4 style="margin:0">采购订单</h4><span class="grow"></span>${sizeSel("po-size")}<button class="btn ghost sm" id="po-print">打印所选</button><button class="btn ghost sm" id="po-deli">送货单(跟车)</button><button class="btn ghost sm" id="po-printcfg">打印设置</button><button class="btn primary sm" id="po-new">新建采购订单</button></div><div id="po-list" class="muted" style="margin-top:8px">加载中…</div></div>
    <div class="panel" style="margin-top:12px"><h4>请购单</h4><div id="pd-list">加载中…</div></div>
    <div class="panel" style="margin-top:12px"><h4>采购订单执行跟踪</h4><div id="pd-track">加载中…</div></div>`;

  const load = async () => {
    try {
      const s = await api(`/procure/po?period=${period}`);
      const orows = s.rows || [];
      $("#po-list").innerHTML = orows.length
        ? `<table class="grid"><thead><tr><th style="width:26px"><input type="checkbox" id="po-chkall" title="全选" /></th><th>单号</th><th>供应商</th><th class="num">不含税</th><th class="num">税额</th><th class="num">价税合计</th><th>状态</th><th></th></tr></thead><tbody>${orows.map((o) => `<tr><td><input type="checkbox" class="po-chk" value="${o.id}" /></td><td>${esc(o.no)}</td><td>${esc(o.supplier_name)}</td><td class="num">${fmt(o.total_amount)}</td><td class="num">${fmt(o.total_tax)}</td><td class="num"><b>${fmt((Number(o.total_amount) || 0) + (Number(o.total_tax) || 0))}</b></td><td><span class="tag ${o.status === "Cancelled" ? "warn" : o.status === "Draft" ? "" : "ok"}">${esc(ORDER_STATUS_LABEL[o.status] || o.status)}</span></td><td class="row-actions"><button class="btn ghost sm" data-po-edit="${o.id}">编辑</button>${o.status === "Draft" ? `<button class="btn ghost sm" data-po-confirm="${o.id}">确认</button><button class="btn ghost sm" data-po-del="${o.id}">删除</button>` : ""}${o.status !== "Cancelled" ? `<button class="btn ghost sm" data-po-cancel="${o.id}">作废</button>` : ""}<button class="btn ghost sm" data-po-chain="${o.id}">链</button>${can("warehouse") ? `<button class="btn ghost sm" data-po-qc="${o.id}">质检</button>${can("voucher_new") && can("order_ops") ? `<button class="btn ghost sm" data-po-inv="${o.id}">票</button>` : ""}` : ""}<button class="btn ghost sm" data-po-exec="${o.id}">执行</button></td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">暂无采购订单，点右上「新建采购订单」</div>`;
      $all("[data-po-edit]").forEach((b) => b.onclick = () => openOrderEditor("po", main, parseInt(b.dataset.poEdit, 10)));
      $all("[data-po-confirm]").forEach((b) => b.onclick = () => poTransition(b.dataset.poConfirm, "Confirmed"));
      $all("[data-po-cancel]").forEach((b) => b.onclick = () => poTransition(b.dataset.poCancel, "Cancelled"));
      $all("[data-po-chain]").forEach((b) => b.onclick = () => openDocChain("po", b.dataset.poChain, "采购订单"));
      $all("[data-po-inv]").forEach((b) => b.onclick = async () => {
        try {
          const r = await postJson("/invoices/from-po", { po_id: Number(b.dataset.poInv) });
          toast(`已下推采购发票 #${r.invoice_id}（待认证，到发票管理认证）`, "ok");
        } catch (e) { toast(e.message, "err"); }
      });
      $all("[data-po-qc]").forEach((b) => b.onclick = () => openQcEditor(b.dataset.poQc, load));
      $all("[data-po-exec]").forEach((b) => b.onclick = () => { $("#pd-poid").value = b.dataset.poExec; toast(`已填入订单ID ${b.dataset.poExec}，可到货/付款`, "ok"); });
      $all("[data-po-del]").forEach((b) => b.onclick = async () => { if (!(await confirmDialog("删除该采购订单？", true))) return; try { await api(`/procure/po/${b.dataset.poDel}/delete`, { method: "POST" }); toast("已删除", "ok"); load(); } catch (e) { toast(e.message, "err"); } });
      if ($("#po-chkall")) $("#po-chkall").onclick = (e) => { $all(".po-chk").forEach((c) => { c.checked = e.target.checked; }); };
      $("#po-print").onclick = () => runPrint("/api/procure/po/print-form", ".po-chk", "order", PRINT_ORDER_FIELDS);
      $("#po-deli").onclick = () => runPrint("/api/procure/po/print-form", ".po-chk", "order", PRINT_ORDER_FIELDS, DELIVERY_FIELDS);
      $("#po-printcfg").onclick = () => openPrintConfig("order", PRINT_ORDER_FIELDS);
      bindSize($("#po-size"), "order", PRINT_ORDER_FIELDS);
    } catch (e) { $("#po-list").innerHTML = `<div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; }
    try {
      const r = await api(`/procure/req?period=${period}`);
      $("#pd-list").innerHTML = table(r.rows || []);
      lastReq = (r.rows || [])[0] || null;
      fillWfTags($("#pd-list"));
      $all("[data-req-approve]").forEach((b) => b.onclick = async () => {
        try { const r = await api(`/procure/req/${b.dataset.reqApprove}/approve`, { method: "POST" }); toast(r && r.pending ? `已审批 → 下一节点：${r.pending}` : "已审批", "ok"); load(); } catch (e) { toast(e.message, "err"); }
      });
      $all("[data-req-push]").forEach((b) => b.onclick = async () => {
        try {
          const r = await postJson(`/procure/req/${b.dataset.reqPush}/push-po`, {});
          toast(`已下推采购订单 ${r.po_no}（草稿：请补供应商与单价）`, "ok");
          load();
        } catch (e) { toast(e.message, "err"); }
      });
    } catch (e) { $("#pd-list").innerHTML = `<div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; }
    try {
      const t = await api("/procure/track");
      $("#pd-track").innerHTML = poTrack(t.rows || []);
    } catch (e) { $("#pd-track").innerHTML = `<div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  const poTransition = async (id, status) => {
    try { await api(`/procure/po/${id}/transition`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ status }) }); toast("已更新订单状态", "ok"); load(); } catch (e) { toast(e.message, "err"); }
  };
  $("#po-new").onclick = () => openOrderEditor("po", main, null);
  $("#pd-save").addEventListener("click", async () => {
    try {
      await postJson("/procure/req", { id: 0, period: ymm(state.current || ""), date: today(), item_code: $("#pd-item").value.trim(), item_name: $("#pd-item").value.trim(), qty: $("#pd-qty").value.trim() || "0", status: "draft", requester: "", memo: $("#pd-memo").value.trim() });
      toast("已保存请购单", "ok"); $("#pd-qty").value = ""; $("#pd-memo").value = ""; load();
    } catch (e) { toast(e.message, "err"); }
  });
  $("#pd-copy").onclick = () => {
    if (!lastReq) { toast("本期暂无请购单可复制", "err"); return; }
    $("#pd-item").value = lastReq.item_name || lastReq.item_code || "";
    $("#pd-qty").value = String(lastReq.qty || "");
    $("#pd-memo").value = lastReq.memo || "";
    toast("已复制上一条（请核对后保存）", "ok");
    $("#pd-item").focus();
  };
  const poid = () => parseInt($("#pd-poid").value.trim(), 10) || 0;
  const amt = () => $("#pd-amt").value.trim();
  const memo = () => $("#pd-memo2").value.trim();
  $("#pd-receipt").addEventListener("click", async () => { if (!poid()) { toast("请填写采购订单ID", "err"); return; } try { const rr = await postJson("/procure/receipt", { po_id: poid(), period: ymm(state.current || ""), date: today(), qty: amt(), warehouse: $("#pd-wh").value.trim(), memo: memo() }); toast(rr && rr.qc_pending ? "已到货（待检入库，待质检转正后可用）" : "已到货", "ok"); $("#pd-amt").value=""; $("#pd-memo2").value=""; load(); } catch (e) { toast(e.message, "err"); } });
  $("#pd-return").addEventListener("click", async () => { if (!poid()) { toast("请填写采购订单ID", "err"); return; } try { await postJson("/procure/return", { po_id: poid(), period: ymm(state.current || ""), date: today(), qty: amt(), warehouse: $("#pd-wh").value.trim(), memo: memo() }); toast("已退货", "ok"); $("#pd-amt").value=""; $("#pd-memo2").value=""; load(); } catch (e) { toast(e.message, "err"); } });
  $("#pd-pay").addEventListener("click", async () => { if (!poid()) { toast("请填写采购订单ID", "err"); return; } try { const r = await postJson("/procure/payment", { po_id: poid(), period: ymm(state.current || ""), date: today(), amount: amt(), memo: memo() }); toast(r && r.doc_id ? `已付款，付款单 #${r.doc_id}（待审核，审核后出凭证并自动核销）` : "已付款", "ok"); $("#pd-amt").value=""; $("#pd-memo2").value=""; load(); } catch (e) { toast(e.message, "err"); } });
  load();
}

async function viewSoDoc(main) {
  const period = encodeURIComponent(state.current || "");
  const table = (rows) => rows.length
    ? `<table class="grid"><thead><tr><th>单号</th><th>客户</th><th>存货</th><th>数量</th><th>单价</th><th>状态</th><th></th></tr></thead>
      <tbody>${rows.map((r) => `<tr><td>${esc(r.no)}</td><td>${esc(r.customer_name)}</td><td>${esc(r.item_name)}</td><td class="num">${esc(r.qty)}</td><td class="num">${esc(r.unit_price)}</td><td>${esc(({ draft: "草稿", approved: "已审批", converted: "已转订单", cancelled: "已作废" })[r.status] || r.status)}<span data-wftag="quotation:${r.id}"></span></td><td>${r.status === "draft" ? `<button class="btn ghost sm" data-quo-approve="${r.id}">审批</button>` : ""}${r.status === "approved" ? `<button class="btn ghost sm" data-quo-toorder="${r.id}">转订单</button>` : ""}${r.status === "converted" ? `<span class="tag ok">已转订单</span>` : ""}</td></tr>`).join("")}</tbody></table>`
    : `<div class="muted">暂无报价单</div>`;
  const soTrack = (rows) => rows.length
    ? `<table class="grid"><thead><tr><th>订单号</th><th>客户</th><th>订单数量</th><th>发货数量</th><th>执行率</th></tr></thead>
      <tbody>${rows.map((t) => `<tr><td>${esc(t.no)}</td><td>${esc(t.customer_name)}</td><td class="num">${esc(t.ordered_qty)}</td><td class="num">${esc(t.shipped_qty)}</td><td class="num">${esc(t.rate)}%</td></tr>`).join("")}</tbody></table>`
    : `<div class="muted">暂无销售订单</div>`;

  main.innerHTML = `<h2>销售单据</h2>
    <div class="toolbar">
      <label>客户 <input id="sd-cust" style="width:110px" /></label>
      <label>存货 <input id="sd-item" style="width:130px" /></label>
      <label>数量 <input id="sd-qty" style="width:70px" /></label>
      <label>单价 <input id="sd-price" style="width:80px" /></label>
      <button class="btn primary" id="sd-save">保存报价单</button>
    </div>
    <div class="toolbar">
      <label>销售订单ID <input id="sd-soid" style="width:80px" /></label>
      <label>数量/金额 <input id="sd-amt" style="width:100px" /></label>
      <label>仓库 <input id="sd-wh" style="width:90px" placeholder="默认仓" /></label>
      <label>备注 <input id="sd-memo" style="width:120px" /></label>
      <button class="btn" id="sd-ship">发货</button>
      <button class="btn" id="sd-return">退货</button>
      <button class="btn" id="sd-pay">收款</button>
      <span class="spacer"></span>
      <label>客户 <input id="sd-credit-cust" style="width:110px" /></label>
      <button class="btn" id="sd-credit">信用检查</button>
    </div>
    <div id="sd-credit-result" class="muted" style="margin-top:6px"></div>
    <div class="panel" style="margin-top:12px"><div style="display:flex;align-items:center;gap:8px"><h4 style="margin:0">发货通知（待发）</h4><span class="grow"></span><button class="btn ghost sm" id="sn-reload">刷新</button></div><div id="sn-list" class="muted" style="margin-top:8px">加载中…</div></div>
    <div class="panel" style="margin-top:12px"><div style="display:flex;align-items:center;gap:8px"><h4 style="margin:0">销售订单</h4><span class="grow"></span>${sizeSel("so-size")}<button class="btn ghost sm" id="so-print">打印所选</button><button class="btn ghost sm" id="so-deli">送货单(跟车)</button><button class="btn ghost sm" id="so-printcfg">打印设置</button><button class="btn primary sm" id="so-new">新建销售订单</button></div><div id="so-list" class="muted" style="margin-top:8px">加载中…</div></div>
    <div class="panel" style="margin-top:12px"><h4>报价单</h4><div id="sd-list">加载中…</div></div>
    <div class="panel" style="margin-top:12px"><h4>销售订单执行跟踪</h4><div id="sd-track">加载中…</div></div>`;

  const load = async () => {
    try {
      const s = await api(`/sales/so?period=${period}`);
      const orows = s.rows || [];
      $("#so-list").innerHTML = orows.length
        ? `<table class="grid"><thead><tr><th style="width:26px"><input type="checkbox" id="so-chkall" title="全选" /></th><th>单号</th><th>客户</th><th class="num">不含税</th><th class="num">税额</th><th class="num">价税合计</th><th>状态</th><th></th></tr></thead><tbody>${orows.map((o) => `<tr><td><input type="checkbox" class="so-chk" value="${o.id}" /></td><td>${esc(o.no)}</td><td>${esc(o.customer_name)}</td><td class="num">${fmt(o.total_amount)}</td><td class="num">${fmt(o.total_tax)}</td><td class="num"><b>${fmt((Number(o.total_amount) || 0) + (Number(o.total_tax) || 0))}</b></td><td><span class="tag ${o.status === "Cancelled" ? "warn" : o.status === "Draft" ? "" : "ok"}">${esc(ORDER_STATUS_LABEL[o.status] || o.status)}</span></td><td class="row-actions"><button class="btn ghost sm" data-so-edit="${o.id}">编辑</button>${o.status === "Draft" ? `<button class="btn ghost sm" data-so-confirm="${o.id}">确认</button><button class="btn ghost sm" data-so-del="${o.id}">删除</button>` : ""}${o.status !== "Cancelled" ? `<button class="btn ghost sm" data-so-cancel="${o.id}">作废</button>` : ""}<button class="btn ghost sm" data-so-chain="${o.id}">链</button>${can("voucher_new") && can("order_ops") ? `<button class="btn ghost sm" data-so-inv="${o.id}">票</button>` : ""}${o.status !== "Draft" && o.status !== "Cancelled" ? `<button class="btn ghost sm" data-so-notice="${o.id}">通知</button>` : ""}<button class="btn ghost sm" data-so-exec="${o.id}">执行</button></td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">暂无销售订单，点右上「新建销售订单」</div>`;
      $all("[data-so-edit]").forEach((b) => b.onclick = () => openOrderEditor("so", main, parseInt(b.dataset.soEdit, 10)));
      $all("[data-so-confirm]").forEach((b) => b.onclick = () => soTransition(b.dataset.soConfirm, "Confirmed"));
      $all("[data-so-cancel]").forEach((b) => b.onclick = () => soTransition(b.dataset.soCancel, "Cancelled"));
      $all("[data-so-chain]").forEach((b) => b.onclick = () => openDocChain("so", b.dataset.soChain, "销售订单"));
      $all("[data-so-inv]").forEach((b) => b.onclick = async () => {
        try {
          const r = await postJson("/invoices/from-so", { so_id: Number(b.dataset.soInv) });
          toast(`已下推销售发票 #${r.invoice_id}（待认证，到发票管理认证）`, "ok");
        } catch (e) { toast(e.message, "err"); }
      });
      $all("[data-so-notice]").forEach((b) => b.onclick = () => {
        const row = orows.find((x) => String(x.id) === b.dataset.soNotice);
        const un = ((row && row.lines) || []).reduce(
          (s, l) => s + (Number(l.qty_ordered) || 0) - (Number(l.qty_shipped) || 0), 0
        );
        openNoticeEditor(b.dataset.soNotice, un, load);
      });
      $all("[data-so-exec]").forEach((b) => b.onclick = () => { $("#sd-soid").value = b.dataset.soExec; toast(`已填入订单ID ${b.dataset.soExec}，可执行发货/收款`, "ok"); });
      $all("[data-so-del]").forEach((b) => b.onclick = async () => { if (!(await confirmDialog("删除该销售订单？", true))) return; try { await api(`/sales/so/${b.dataset.soDel}/delete`, { method: "POST" }); toast("已删除", "ok"); load(); } catch (e) { toast(e.message, "err"); } });
      if ($("#so-chkall")) $("#so-chkall").onclick = (e) => { $all(".so-chk").forEach((c) => { c.checked = e.target.checked; }); };
      $("#so-print").onclick = () => runPrint("/api/sales/so/print-form", ".so-chk", "order", PRINT_ORDER_FIELDS);
      $("#so-deli").onclick = () => runPrint("/api/sales/so/print-form", ".so-chk", "order", PRINT_ORDER_FIELDS, DELIVERY_FIELDS);
      $("#so-printcfg").onclick = () => openPrintConfig("order", PRINT_ORDER_FIELDS);
      bindSize($("#so-size"), "order", PRINT_ORDER_FIELDS);
    } catch (e) { $("#so-list").innerHTML = `<div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; }
    try {
      const r = await api(`/sales/quote?period=${period}`);
      $("#sd-list").innerHTML = table(r.rows || []);
      $all("[data-quo-approve]").forEach((b) => b.onclick = async () => {
        try { const r = await api(`/sales/quote/${b.dataset.quoApprove}/approve`, { method: "POST" }); toast(r && r.pending ? `已审批 → 下一节点：${r.pending}` : "已审批", "ok"); load(); } catch (e) { toast(e.message, "err"); }
      });
      fillWfTags($("#sd-list"));
      $all("[data-quo-toorder]").forEach((b) => b.onclick = async () => {
        try {
          const r = await postJson(`/sales/quote/${b.dataset.quoToorder}/to-order`, {});
          toast(`已生成销售订单 #${r.so_id}（草稿，可在上方订单列表确认执行）`, "ok");
          load();
        } catch (e) { toast(e.message, "err"); }
      });
    } catch (e) { $("#sd-list").innerHTML = `<div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; }
    try {
      const t = await api("/sales/track");
      $("#sd-track").innerHTML = soTrack(t.rows || []);
    } catch (e) { $("#sd-track").innerHTML = `<div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  const soTransition = async (id, status) => {
    try { await api(`/sales/so/${id}/transition`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ status }) }); toast("已更新订单状态", "ok"); load(); } catch (e) { toast(e.message, "err"); }
  };
  $("#so-new").onclick = () => openOrderEditor("so", main, null);
  $("#sn-reload").addEventListener("click", snLoad);
  snLoad();
  $("#sd-save").addEventListener("click", async () => {
    try {
      await postJson("/sales/quote", { id: 0, period: ymm(state.current || ""), date: today(), customer_code: $("#sd-cust").value.trim(), customer_name: $("#sd-cust").value.trim(), item_code: $("#sd-item").value.trim(), item_name: $("#sd-item").value.trim(), qty: $("#sd-qty").value.trim() || "0", unit_price: $("#sd-price").value.trim() || "0", status: "draft", prepared_by: "", memo: "" });
      toast("已保存报价单", "ok"); $("#sd-qty").value=""; $("#sd-price").value=""; load();
    } catch (e) { toast(e.message, "err"); }
  });
  const soid = () => parseInt($("#sd-soid").value.trim(), 10) || 0;
  const amt = () => $("#sd-amt").value.trim();
  const memo = () => $("#sd-memo").value.trim();
  $("#sd-ship").addEventListener("click", async () => { if (!soid()) { toast("请填写销售订单ID", "err"); return; } try { const r = await postJson("/sales/shipment", { so_id: soid(), period: ymm(state.current || ""), date: today(), qty: amt(), warehouse: $("#sd-wh").value.trim(), memo: memo() }); toast(r && r.voucher_id ? `已发货，确认收入凭证 #${r.voucher_id}` : "已发货", "ok"); $("#sd-amt").value=""; $("#sd-memo").value=""; load(); snLoad(); } catch (e) { toast(e.message, "err"); } });
  $("#sd-return").addEventListener("click", async () => { if (!soid()) { toast("请填写销售订单ID", "err"); return; } try { const r = await postJson("/sales/return", { so_id: soid(), period: ymm(state.current || ""), date: today(), qty: amt(), memo: memo() }); toast(r && r.voucher_id ? `已退货，冲回凭证 #${r.voucher_id}` : "已退货", "ok"); $("#sd-amt").value=""; $("#sd-memo").value=""; load(); } catch (e) { toast(e.message, "err"); } });
  $("#sd-pay").addEventListener("click", async () => { if (!soid()) { toast("请填写销售订单ID", "err"); return; } try { const r = await postJson("/sales/payment", { so_id: soid(), period: ymm(state.current || ""), date: today(), amount: amt(), memo: memo() }); toast(r && r.doc_id ? `已收款，收款单 #${r.doc_id}（待审核，审核后出凭证并自动核销）` : "已收款", "ok"); $("#sd-amt").value=""; $("#sd-memo").value=""; load(); } catch (e) { toast(e.message, "err"); } });
  $("#sd-credit").addEventListener("click", async () => {
    const c = $("#sd-credit-cust").value.trim();
    if (!c) { toast("请填写客户", "err"); return; }
    try {
      const r = await api(`/sales/credit?customer=${encodeURIComponent(c)}`);
      $("#sd-credit-result").innerHTML = r.over
        ? `<span class="tag err">超额度</span> 占用 ${esc(r.receivable)} / 额度 ${esc(r.limit)}`
        : `<span class="tag ok">未超额度</span> 占用 ${esc(r.receivable)} / 额度 ${esc(r.limit || "未设")}`;
    } catch (e) { toast(e.message, "err"); }
  });
  load();
}

// ---------------- 工作流可视化设计器 + 运行实例（对标金蝶审批流设计器） ----------------
// 节点-动作模型：开始/审批/条件/消息节点 + 普通/驳回连线；**无出边的审批节点 = 流程终点**；
// 无已发布流程的业务走默认审批（向后兼容）。设计保存需 sys_option（账套管理员）。
async function viewWorkflow(main) {
  const mode = state.wfMode || "design";
  main.innerHTML = `<h2>工作流</h2>
    <div class="toolbar">
      <button class="btn sm ${mode === "design" ? "primary" : "ghost"}" id="wf-d">流程设计</button>
      <button class="btn sm ${mode === "instances" ? "primary" : "ghost"}" id="wf-i">运行实例</button>
      <span class="grow"></span>
      ${can("sys_option") ? `<button class="btn primary sm" id="wf-new">新建流程</button>` : `<span class="muted" style="font-size:12px">仅账套管理员可新建/发布流程；审批人按节点参与人在实例页操作</span>`}
    </div>
    <div id="wf-body">${mode === "design" ? "加载中…" : ""}</div>`;
  $("#wf-d").onclick = () => { state.wfMode = "design"; viewWorkflow(main); };
  $("#wf-i").onclick = () => { state.wfMode = "instances"; viewWorkflow(main); };
  if (mode === "instances") return renderWfInstances($("#wf-body"));
  await renderWfDesign($("#wf-body"));
  if ($("#wf-new")) $("#wf-new").onclick = () => openWfEditor(null, () => renderWfDesign($("#wf-body")));
}

async function renderWfInstances(body) {
  body.className = "";
  try {
    const r = await api("/workflows/instances");
    const rows = r.rows || [];
    body.className = "";
    body.innerHTML = rows.length
      ? `<div class="muted" style="font-size:12px;margin-bottom:6px">键盘：J / K 上下选择 · A 通过 · R 驳回（焦点不在输入框时）</div><table class="grid"><thead><tr><th>#</th><th>类型</th><th>单据</th><th>流程</th><th>当前节点</th><th>状态</th><th>轨迹</th><th></th></tr></thead><tbody>${rows.map((it) => {
          const st = it.status === "running" ? '<span class="tag">进行中</span>' : it.status === "approved" ? '<span class="tag ok">已通过</span>' : '<span class="tag warn">已驳回</span>';
          const track = (it.log || []).map((l) => `<div>${esc(l.at)} · ${esc(l.who)} · ${l.action === "approve" ? "通过" : "驳回"}</div>`).join("") || "<div class='muted'>无</div>";
          return `<tr data-kb="${it.id}" style="cursor:pointer"><td>${it.id}</td><td>${esc(it.biz_label)}</td><td>#${it.biz_id}</td><td>${esc(it.flow_name)}</td><td>${esc(it.current_label)}</td><td>${st}</td>
            <td><details><summary class="muted">查看</summary><div style="font-size:12px">${track}</div></details></td>
            <td class="row-actions">${it.status === "running" ? `<button class="btn ghost sm" data-wf-ok='{"bt":"${it.biz_type}","bi":${it.biz_id}}'>通过</button><button class="btn ghost sm" data-wf-no='{"bt":"${it.biz_type}","bi":${it.biz_id}}'>驳回</button>` : ""}</td></tr>`;
        }).join("")}</tbody></table>`
      : `<div class="muted">暂无运行实例（对已发布流程的业务单据执行「审批/审核」时自动创建并推进）</div>`;
    const act = async (bt, bi, approve) => {
      try {
        let resp;
        if (bt === "quotation") resp = await postJson(`/sales/quote/${bi}/approve`, {});
        else if (bt === "purchase_req") resp = await postJson(`/procure/req/${bi}/approve`, {});
        else if (bt === "claim") resp = await postJson(`/claims/${bi}/transition`, { status: approve ? "approved" : "rejected" });
        else if (bt === "receipt") resp = approve ? await postJson(`/funds/receipts/${bi}/audit`, {}) : await Promise.reject(new Error("收付款单请在资金管理页处理"));
        else throw new Error("该类型暂不支持从实例页操作");
        if (resp && resp.pending) toast(`已审批 → 下一节点：${resp.pending}`, "ok");
        else if (approve) toast("已批准，业务单据同步生效", "ok");
        else toast("已驳回", "ok");
        renderWfInstances(body);
      } catch (e) { toast(e.message, "err"); }
    };
    $all("[data-wf-ok]").forEach((b) => b.onclick = () => { const d = JSON.parse(b.dataset.wfOk); act(d.bt, d.bi, true); });
    $all("[data-wf-no]").forEach((b) => b.onclick = async () => { if (await confirmDialog("确认驳回该单据？", true)) { const d = JSON.parse(b.dataset.wfNo); act(d.bt, d.bi, false); } });
    // 行点击选中（键盘 J/K/A/R 的作用对象）
    $all("[data-kb]").forEach((tr) => tr.onclick = (e) => {
      if (e.target.closest("button") || e.target.closest("a") || e.target.closest("summary")) return;
      $all("[data-kb]").forEach((x) => x.classList.remove("kb-sel"));
      tr.classList.add("kb-sel");
    });
    if (rows.some((x) => x.status === "running")) {
      const first = body.querySelector("[data-kb]");
      if (first) first.classList.add("kb-sel");
    }
  } catch (e) { body.innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
}

// 实例页快捷键：J/K 上下选择 · A 通过 · R 驳回（焦点在输入框或弹窗打开时不生效）
document.addEventListener("keydown", (e) => {
  const t = (e.target.tagName || "").toLowerCase();
  if (t === "input" || t === "select" || t === "textarea") return;
  if (document.querySelector(".modal-mask")) return;
  if (e.ctrlKey || e.metaKey || e.altKey) return;
  if (state.view !== "workflow" || (state.wfMode || "design") !== "instances") return;
  const rowsKb = $all("[data-kb]");
  if (!rowsKb.length) return;
  let idx = rowsKb.findIndex((r) => r.classList.contains("kb-sel"));
  const key = e.key.toLowerCase();
  if (key === "j") { e.preventDefault(); idx = idx < 0 ? 0 : Math.min(rowsKb.length - 1, idx + 1); }
  else if (key === "k") { e.preventDefault(); idx = idx <= 0 ? 0 : idx - 1; }
  else if (key === "a" && idx >= 0) {
    e.preventDefault();
    rowsKb[idx].querySelector("[data-wf-ok]")?.click();
    return;
  } else if (key === "r" && idx >= 0) {
    e.preventDefault();
    if (rowsKb[idx].querySelector("[data-wf-no]")) rowsKb[idx].querySelector("[data-wf-no]").click();
    return;
  } else return;
  rowsKb.forEach((r) => r.classList.remove("kb-sel"));
  rowsKb[idx].classList.add("kb-sel");
  rowsKb[idx].scrollIntoView({ block: "nearest" });
});

async function renderWfDesign(body) {
  body.className = "";
  try {
    const r = await api("/workflows");
    const rows = r.rows || [];
    const canEdit = can("sys_option");
    body.className = "";
    body.innerHTML = rows.length
      ? `<table class="grid"><thead><tr><th>流程名</th><th>业务类型</th><th>状态</th><th>节点 / 连线</th><th>更新时间</th><th></th></tr></thead><tbody>${rows.map((f) => `<tr>
          <td>${esc(f.name)}</td><td>${esc(f.biz_type)}</td>
          <td>${f.status === "published" ? '<span class="tag ok">已发布</span>' : '<span class="tag warn">草稿</span>'}</td>
          <td>${(f.nodes || []).length} / ${(f.edges || []).length}</td><td>${esc(f.updated_at || "")}</td>
          <td class="row-actions">${canEdit ? `<button class="btn ghost sm" data-wf-edit="${f.id}">编辑</button>${f.status === "published" ? `<button class="btn ghost sm" data-wf-unpub="${f.id}">撤回</button>` : `<button class="btn ghost sm" data-wf-pub="${f.id}">发布</button>`}<button class="btn ghost sm" data-wf-del="${f.id}">删除</button>` : ""}</td></tr>`).join("")}</tbody></table>`
      : `<div class="muted">暂无流程，点右上「新建流程」开始（未配置流程的业务保持默认审批，行为不变）</div>`;
    const reload = () => renderWfDesign(body);
    const byId = (id) => rows.find((f) => f.id === id);
    $all("[data-wf-edit]").forEach((b) => b.onclick = () => openWfEditor(byId(parseInt(b.dataset.wfEdit, 10)), reload));
    $all("[data-wf-pub]").forEach((b) => b.onclick = async () => { try { await postJson(`/workflows/${b.dataset.wfPub}/publish`, {}); toast("已发布：该业务单据的审批将走此流程", "ok"); reload(); } catch (e) { toast(e.message, "err"); } });
    $all("[data-wf-unpub]").forEach((b) => b.onclick = async () => { try { await postJson(`/workflows/${b.dataset.wfUnpub}/unpublish`, {}); toast("已撤回，恢复默认审批", "ok"); reload(); } catch (e) { toast(e.message, "err"); } });
    $all("[data-wf-del]").forEach((b) => b.onclick = async () => { if (!(await confirmDialog("删除该流程？运行中实例将一并清理。", true))) return; try { await postJson(`/workflows/${b.dataset.wfDel}/delete`, {}); toast("已删除", "ok"); reload(); } catch (e) { toast(e.message, "err"); } });
  } catch (e) { body.innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
}

// 画布编辑器：拖拽移动（20px 钉吸）、圆点拖出连线（Shift=驳回线）、滚轮缩放、空白拖拽平移、
// Delete 删除、Ctrl+Z/Y 撤销重做、小地图、属性侧抽屉（参与人=角色多选）
function openWfEditor(flow, reload) {
  let roles = [];
  let fid = flow ? flow.id : 0;
  let name = flow ? flow.name : "";
  let biz = flow ? flow.biz_type : "quotation";
  let nodes = flow && flow.nodes ? JSON.parse(JSON.stringify(flow.nodes)) : [
    { id: "n1", type: "start", name: "开始", participants: [], strategy: "all", reject_to: "", x: 40, y: 140 },
  ];
  let edges = flow && flow.edges ? JSON.parse(JSON.stringify(flow.edges)) : [];
  let sel = null;           // {kind:"node"|"edge", id}
  let connFrom = null;      // 连线起点节点 id
  let tempTo = null;        // 鼠标当前位置（svg 坐标）
  let drag = null;          // {id, dx, dy}
  let pan = null;           // {sx, sy, vx, vy}
  const view = { x: 0, y: 0, w: 720, h: 430 };
  const undo = [], redo = [];
  const snap = () => { undo.push(JSON.stringify({ nodes, edges })); if (undo.length > 60) undo.shift(); redo.length = 0; };
  const T = { start: "开始", approve: "审批", condition: "条件", message: "消息" };
  const nodeById = (id) => nodes.find((n) => n.id === id);
  const mask = modal(`
    <h3>${flow ? "编辑流程" : "新建流程"}</h3>
    <div class="toolbar" style="margin-bottom:6px">
      <label>流程名 <input id="wf-name" value="${esc(name)}" style="width:150px" /></label>
      <label>业务类型 <select id="wf-biz">
        <option value="quotation" ${biz === "quotation" ? "selected" : ""}>报价单</option>
        <option value="purchase_req" ${biz === "purchase_req" ? "selected" : ""}>请购单</option>
        <option value="claim" ${biz === "claim" ? "selected" : ""}>报销单</option>
        <option value="receipt" ${biz === "receipt" ? "selected" : ""}>收付款单</option>
      </select></label>
      <span class="grow"></span>
      <button class="btn ghost sm" data-add="start">＋开始</button>
      <button class="btn ghost sm" data-add="approve">＋审批</button>
      <button class="btn ghost sm" data-add="condition">＋条件</button>
      <button class="btn ghost sm" data-add="message">＋消息</button>
      <button class="btn ghost sm" id="wf-fit">适配</button>
      ${fid && can("sys_option") ? `<button class="btn ghost sm" id="wf-pub">发布/撤回</button>` : ""}
      <button class="btn primary sm" id="wf-save">保存</button>
      <button class="btn ghost sm" id="wf-close">关闭</button>
    </div>
    <div style="display:flex;gap:10px;align-items:stretch">
      <div style="flex:1;position:relative;border:1px solid #ccc;border-radius:6px;overflow:hidden">
        <svg id="wf-svg" style="display:block;width:100%;height:430px;touch-action:none;background:#fafbfc"></svg>
        <svg id="wf-map" width="150" height="92" style="position:absolute;right:8px;bottom:8px;background:rgba(255,255,255,.92);border:1px solid #bbb;border-radius:4px"></svg>
      </div>
      <div id="wf-props" style="width:250px;border:1px solid #ccc;border-radius:6px;padding:8px;font-size:12px;overflow:auto"></div>
    </div>
    <p class="muted" style="font-size:12px;margin:6px 0 0">拖动节点移动（20px 钉吸）；从节点右侧蓝点拖到另一节点连线，按住 Shift 拖 = 驳回线；滚轮缩放、空白处拖拽平移、Delete 删除、Ctrl+Z/Y 撤销重做。<b>无出边的审批节点即流程终点</b>；参与人留空 = 需「审核权限」者可批。</p>
  `, true);
  const svg = $("#wf-svg", mask);
  const map = $("#wf-map", mask);
  const props = $("#wf-props", mask);

  const toSvg = (e) => {
    const rc = svg.getBoundingClientRect();
    return { x: view.x + (e.clientX - rc.left) * view.w / rc.width, y: view.y + (e.clientY - rc.top) * view.h / rc.height };
  };
  const render = () => {
    svg.setAttribute("viewBox", `${view.x} ${view.y} ${view.w} ${view.h}`);
    const paths = edges.map((e) => {
      const a = nodeById(e.from), b = nodeById(e.to);
      if (!a || !b) return "";
      const isRej = e.kind === "reject";
      const x1 = a.x + 120, y1 = a.y + 20, x2 = b.x, y2 = b.y + 20;
      const mid = (x1 + x2) / 2;
      const d = `M ${x1} ${y1} C ${mid} ${y1}, ${mid} ${y2}, ${x2} ${y2}`;
      const color = isRej ? "#c62828" : "#455a64";
      const dash = isRej ? ' stroke-dasharray="6 4"' : "";
      const selA = sel && sel.kind === "edge" && sel.id === e.id;
      const label = (e.condition || (isRej ? "驳回" : "")) ? `<text x="${mid}" y="${(y1 + y2) / 2 - 5}" font-size="11" text-anchor="middle" fill="${color}">${esc(e.condition || "驳回")}</text>` : "";
      return `<path d="${d}" fill="none" stroke="${selA ? "#1976d2" : color}" stroke-width="${selA ? 3 : 2}"${dash} marker-end="url(#wf-arw)" data-edge="${e.id}" style="cursor:pointer" />${label}`;
    }).join("");
    const temp = connFrom && tempTo ? (() => {
      const a = nodeById(connFrom);
      if (!a) return "";
      return `<line x1="${a.x + 120}" y1="${a.y + 20}" x2="${tempTo.x}" y2="${tempTo.y}" stroke="#1976d2" stroke-width="2" stroke-dasharray="4 3" />`;
    })() : "";
    const gs = nodes.map((n) => {
      const fill = { start: "#e8f5e9", approve: "#e3f2fd", condition: "#fff8e1", message: "#f3e5f5" }[n.type] || "#eee";
      const on = sel && sel.kind === "node" && sel.id === n.id;
      const label = n.name && n.name.trim() ? n.name : T[n.type] || n.type;
      return `<g data-id="${n.id}" transform="translate(${n.x},${n.y})" style="cursor:move">
        <rect width="120" height="40" rx="8" fill="${fill}" stroke="${on ? "#1976d2" : "#546e7a"}" stroke-width="${on ? 2.5 : 1.2}"/>
        <text x="60" y="25" font-size="13" text-anchor="middle" fill="#263238">${esc(label)}</text>
        <circle class="wf-h" cx="120" cy="20" r="7" fill="#1976d2" stroke="#fff" stroke-width="1.5" style="cursor:crosshair"/>
      </g>`;
    }).join("");
    svg.innerHTML = `<defs><marker id="wf-arw" markerWidth="9" markerHeight="9" refX="8" refY="4.5" orient="auto"><path d="M0,0 L9,4.5 L0,9 z" fill="#455a64"/></marker></defs>${paths}${temp}${gs}`;
    // 小地图
    const maxX = Math.max(400, ...nodes.map((n) => n.x + 140));
    const maxY = Math.max(240, ...nodes.map((n) => n.y + 60));
    const sc = Math.min(150 / maxX, 92 / maxY);
    map.innerHTML = nodes.map((n) => `<rect x="${n.x * sc}" y="${n.y * sc}" width="${120 * sc}" height="${40 * sc}" fill="#546e7a" rx="2"/>`).join("")
      + `<rect x="${view.x * sc}" y="${view.y * sc}" width="${view.w * sc}" height="${view.h * sc}" fill="none" stroke="#1976d2" stroke-width="1.5"/>`;
    renderProps();
  };
  const renderProps = () => {
    if (!sel) {
      props.innerHTML = `<div class="muted">未选中。<br>单击节点/连线选中后在此编辑属性。<br><br>· 审批节点设置参与人（留空 = 需审核权限）<br>· 驳回：连线用 Shift+拖拽 或节点「驳回至」<br>· 保存后由管理员发布</div>`;
      return;
    }
    if (sel.kind === "node") {
      const n = nodeById(sel.id);
      if (!n) { sel = null; return renderProps(); }
      props.innerHTML = `
        <div class="field"><label>名称</label><input id="pr-name" value="${esc(n.name)}" /></div>
        <div class="field"><label>类型</label><input value="${T[n.type] || n.type}" readonly /></div>
        ${n.type === "approve" ? `
        <div class="field"><label>参与人（角色，留空=需审核权限）</label><div id="pr-parts" style="display:flex;flex-direction:column;gap:4px"></div></div>
        <div class="field"><label>会签策略（v1 单人通过即过）</label><select id="pr-strat">
          <option value="all" ${n.strategy === "all" ? "selected" : ""}>全部通过（会签：每个参与角色各需一票）</option>
          <option value="any" ${n.strategy === "any" ? "selected" : ""}>任一通过</option>
        </select></div>
        <div class="field"><label>驳回至（可选）</label><select id="pr-rej"><option value="">（用驳回连线）</option>${nodes.filter((x) => x.id !== n.id).map((x) => `<option value="${x.id}" ${n.reject_to === x.id ? "selected" : ""}>${esc(x.name || T[x.type] || x.id)}</option>`).join("")}</select></div>` : ""}
        <div style="display:flex;gap:6px;margin-top:6px"><button class="btn danger sm" id="pr-del">删除节点</button></div>`;
      $("#pr-name", mask).onchange = (ev) => { snap(); n.name = ev.target.value; render(); };
      if (n.type === "approve") {
        const box = $("#pr-parts", mask);
        const draw = () => {
          if (!roles.length) { box.innerHTML = `<span class="muted">角色加载中…</span>`; return; }
          box.innerHTML = roles.map((r) => `<label style="display:flex;gap:4px;align-items:center"><input type="checkbox" class="pr-p" value="${r.role}" ${(n.participants || []).includes(r.role) ? "checked" : ""} />${esc(r.label)}</label>`).join("");
          $all(".pr-p", mask).forEach((cb) => cb.onchange = () => {
            snap();
            n.participants = $all(".pr-p", mask).filter((c) => c.checked).map((c) => c.value);
          });
        };
        draw();
        if (!roles.length) loadRoles().then((rs) => { roles = rs; draw(); });
        $("#pr-strat", mask).onchange = (ev) => { snap(); n.strategy = ev.target.value; };
        $("#pr-rej", mask).onchange = (ev) => { snap(); n.reject_to = ev.target.value; render(); };
      }
      $("#pr-del", mask).onclick = () => { snap(); nodes = nodes.filter((x) => x.id !== n.id); edges = edges.filter((e) => e.from !== n.id && e.to !== n.id); sel = null; render(); };
    } else {
      const e2 = edges.find((x) => x.id === sel.id);
      if (!e2) { sel = null; return renderProps(); }
      props.innerHTML = `
        <div class="field"><label>连线类型</label><select id="pr-kind">
          <option value="normal" ${e2.kind !== "reject" ? "selected" : ""}>普通</option>
          <option value="reject" ${e2.kind === "reject" ? "selected" : ""}>驳回</option>
        </select></div>
        <div class="field"><label>连线条件（审批时求值）</label><input id="pr-cond" value="${esc(e2.condition || "")}" placeholder="如：amount &gt; 5000" /><p class="muted" style="font-size:11px;margin:4px 0 0">字段：amount/qty/customer_code 等；运算符 &gt; &gt;= &lt; &lt;= == !=；多条出线按序匹配，空条件为兜底；字符串值加引号</p></div>
        <div style="margin-top:6px"><button class="btn danger sm" id="pr-del2">删除连线</button></div>`;
      $("#pr-kind", mask).onchange = (ev) => { snap(); e2.kind = ev.target.value; render(); };
      $("#pr-cond", mask).onchange = (ev) => { snap(); e2.condition = ev.target.value; render(); };
      $("#pr-del2", mask).onclick = () => { snap(); edges = edges.filter((x) => x.id !== e2.id); sel = null; render(); };
    }
  };

  // 交互：按下
  svg.addEventListener("pointerdown", (e) => {
    const pt = toSvg(e);
    const g = e.target.closest && e.target.closest("g[data-id]");
    const path = e.target.closest && e.target.closest("[data-edge]");
    if (e.target.classList && e.target.classList.contains("wf-h") && g) {
      snap();
      connFrom = g.dataset.id;
      tempTo = pt;
      svg.setPointerCapture(e.pointerId);
      render();
      return;
    }
    if (g) {
      const n = nodeById(g.dataset.id);
      if (!n) return;
      sel = { kind: "node", id: n.id };
      snap();
      drag = { id: n.id, dx: pt.x - n.x, dy: pt.y - n.y };
      svg.setPointerCapture(e.pointerId);
      render();
      return;
    }
    if (path) {
      sel = { kind: "edge", id: path.dataset.edge };
      render();
      return;
    }
    sel = null;
    pan = { sx: e.clientX, sy: e.clientY, vx: view.x, vy: view.y };
    svg.setPointerCapture(e.pointerId);
    render();
  });
  svg.addEventListener("pointermove", (e) => {
    const pt = toSvg(e);
    if (drag) {
      const n = nodeById(drag.id);
      if (!n) return;
      n.x = Math.round((pt.x - drag.dx) / 20) * 20;
      n.y = Math.round((pt.y - drag.dy) / 20) * 20;
      render();
    } else if (connFrom) {
      tempTo = pt;
      render();
    } else if (pan) {
      const rc = svg.getBoundingClientRect();
      view.x = pan.vx - (e.clientX - pan.sx) * view.w / rc.width;
      view.y = pan.vy - (e.clientY - pan.sy) * view.h / rc.height;
      render();
    }
  });
  svg.addEventListener("pointerup", (e) => {
    if (connFrom) {
      const g = e.target.closest && e.target.closest("g[data-id]");
      const to = g && g.dataset.id;
      if (to && to !== connFrom) {
        edges.push({ id: `e${Date.now()}`, from: connFrom, to, kind: e.shiftKey ? "reject" : "normal", condition: "" });
      }
      connFrom = null;
      tempTo = null;
      render();
    }
    drag = null;
    pan = null;
  });
  svg.addEventListener("wheel", (e) => {
    e.preventDefault();
    const f = e.deltaY > 0 ? 1.15 : 1 / 1.15;
    const cx = view.x + view.w / 2, cy = view.y + view.h / 2;
    view.w = Math.min(4000, Math.max(240, view.w * f));
    view.h = view.w * (430 / 720);
    view.x = cx - view.w / 2;
    view.y = cy - view.h / 2;
    render();
  }, { passive: false });

  const onKey = (e) => {
    const tag = (e.target.tagName || "").toLowerCase();
    if (tag === "input" || tag === "select" || tag === "textarea") return;
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "z") { e.preventDefault(); doUndo(); }
    else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "y") { e.preventDefault(); doRedo(); }
    else if ((e.key === "Delete" || e.key === "Backspace") && sel) {
      e.preventDefault();
      if (sel.kind === "node") {
        snap();
        nodes = nodes.filter((x) => x.id !== sel.id);
        edges = edges.filter((x) => x.from !== sel.id && x.to !== sel.id);
      } else {
        snap();
        edges = edges.filter((x) => x.id !== sel.id);
      }
      sel = null;
      render();
    }
  };
  const doUndo = () => {
    if (!undo.length) return;
    redo.push(JSON.stringify({ nodes, edges }));
    const s = JSON.parse(undo.pop());
    nodes = s.nodes; edges = s.edges; sel = null; render();
  };
  const doRedo = () => {
    if (!redo.length) return;
    undo.push(JSON.stringify({ nodes, edges }));
    const s = JSON.parse(redo.pop());
    nodes = s.nodes; edges = s.edges; sel = null; render();
  };
  document.addEventListener("keydown", onKey);

  // 工具条
  $all("[data-add]", mask).forEach((b) => b.onclick = () => {
    snap();
    const type = b.dataset.add;
    const off = (nodes.length % 6) * 24;
    nodes.push({
      id: `n${Date.now()}`, type,
      name: type === "approve" ? "审批" : (T[type] || type),
      participants: [], strategy: "all", reject_to: "",
      x: Math.round((view.x + view.w / 2 - 60 + off) / 20) * 20,
      y: Math.round((view.y + 60 + off) / 20) * 20,
    });
    render();
  });
  $("#wf-fit", mask).onclick = () => {
    if (!nodes.length) return;
    view.x = -20;
    view.y = -20;
    view.w = Math.max(...nodes.map((n) => n.x + 160)) + 40;
    view.h = Math.max(...nodes.map((n) => n.y + 80)) + 40;
    render();
  };
  $("#wf-save", mask).onclick = async () => {
    name = $("#wf-name", mask).value.trim();
    biz = $("#wf-biz", mask).value;
    if (!name) { toast("请填写流程名", "err"); return; }
    if (!nodes.some((n) => n.type === "start")) { toast("流程必须包含一个开始节点", "err"); return; }
    try {
      const r = await postJson("/workflows", { id: fid, name, biz_type: biz, nodes, edges });
      fid = r.id;
      toast(`已保存（流程 #${fid}）`, "ok");
      document.removeEventListener("keydown", onKey);
      closeModal();
      reload && reload();
    } catch (e) { toast(e.message, "err"); }
  };
  if ($("#wf-pub", mask)) $("#wf-pub", mask).onclick = async () => {
    try { await postJson(`/workflows/${fid}/publish`, {}); toast("已发布/切换发布状态", "ok"); document.removeEventListener("keydown", onKey); closeModal(); reload && reload(); } catch (e) { toast(e.message, "err"); }
  };
  $("#wf-close", mask).onclick = () => { document.removeEventListener("keydown", onKey); closeModal(); };
  loadRoles().then((rs) => { roles = rs; renderProps(); });
  render();
}

// ---------------- 单据套打（订单/收付款单）：字段白名单 + 批量打印 ----------------
const PRINT_ORDER_FIELDS = ["no", "date", "status", "party", "memo", "prepared", "code", "name", "qty", "price", "rate", "amount", "tax", "linememo", "totals", "sign", "pack"];
const PRINT_RECEIPT_FIELDS = ["no", "date", "status", "party", "fund", "voucher", "memo", "amount", "sign", "pack"];
const PRINT_LABELS = {
  no: "单号", date: "日期", status: "状态 / 类型", party: "客户·供应商 / 往来单位",
  memo: "备注", prepared: "制单人", code: "存货编码", name: "名称", qty: "数量",
  price: "单价", rate: "税率", amount: "金额", tax: "税额", linememo: "行备注",
  fund: "资金账户", voucher: "关联凭证号", totals: "合计行", sign: "签章栏",
  pack: "紧凑分页（多单挤一页，充分利用 A4）",
};
function loadPrintCfg(scope, tokens) {
  // 返回 {set: 已勾选字段, size: 纸张}；兼容旧存储（纯字段数组 → A4）
  try {
    const raw = localStorage.getItem("fb.print." + scope);
    if (raw) {
      const parsed = JSON.parse(raw);
      const fields = Array.isArray(parsed) ? parsed : (parsed && Array.isArray(parsed.f) ? parsed.f : null);
      if (fields) {
        const saved = new Set(fields);
        const size = (!Array.isArray(parsed) && typeof parsed.s === "string") ? parsed.s : "a4";
        return { set: new Set(tokens.filter((t) => saved.has(t))), size };
      }
    }
  } catch (e) { /* 坏数据回默认 */ }
  return { set: new Set(tokens), size: "a4" };
}
function savePrintCfg(scope, cfg) {
  try { localStorage.setItem("fb.print." + scope, JSON.stringify({ v: 1, f: [...cfg.set], s: cfg.size })); } catch (e) { /* 存储不可用则本次会话仍生效 */ }
}
function printFieldsParam(cfg) {
  return [...cfg.set].filter((t) => t !== "pack").join(",");
}
function packParam(cfg) { return cfg.set.has("pack") ? "1" : "0"; }
const PRINT_SIZES = [
  ["a4", "A4（210×297）"],
  ["a5", "A5 二等分（210×148）"],
  ["third", "三等分（99×210）"],
  ["custom", "自定义 宽x高 mm"],
];
// 一键预设（仅订单域）：送货单 = 跟车联——保留数量供清点，隐藏单价/税率/金额/税额/合计/状态（价格不外流）
const PRINT_PRESETS = {
  full: null,
  delivery: ["no", "date", "party", "memo", "prepared", "code", "name", "qty", "linememo", "sign", "pack"],
};
// 送货单固定字段集（跟车联）：不读用户保存的字段配置，杜绝「上次送货单设置污染普通打印」
const DELIVERY_FIELDS = PRINT_PRESETS.delivery.filter((t) => t !== "pack").join(",");
// 统一打印入口：勾选优先、未勾选 = 全部；fieldsForced（送货单）强制覆盖保存的字段配置
function runPrint(base, cls, scope, tokens, fieldsForced) {
  let ids = $all(cls).filter((c) => c.checked).map((c) => c.value);
  if (!ids.length) ids = $all(cls).map((c) => c.value);
  if (!ids.length) { toast("当前没有可打印的单据", "err"); return; }
  const cfg = loadPrintCfg(scope, tokens);
  const fields = fieldsForced || printFieldsParam(cfg);
  window.open(`${base}?ids=${ids.join(",")}&fields=${fields}&pack=${packParam(cfg)}&size=${encodeURIComponent(cfg.size)}`, "_blank");
}
// 纸张直选下拉（高频切换不用进设置弹窗）：change 即写回本机设置，与设置弹窗同一数据源
function sizeSel(id) {
  return `<label style="font-size:12px;display:inline-flex;align-items:center;gap:4px">纸张<select id="${id}" style="width:92px"><option value="">自定义</option><option value="a4">A4</option><option value="a5">A5二等分</option><option value="third">三等分</option></select></label>`;
}
function bindSize(sel, scope, tokens) {
  if (!sel) return;
  const cfg = loadPrintCfg(scope, tokens);
  sel.value = ["a4", "a5", "third"].includes(cfg.size) ? cfg.size : "";
  sel.onchange = () => {
    if (!sel.value) return;
    const c = loadPrintCfg(scope, tokens);
    c.size = sel.value;
    savePrintCfg(scope, c);
    toast(`纸张已切换为 ${sel.options[sel.selectedIndex].text}`, "ok");
  };
}
function openPrintConfig(scope, tokens) {
  const cfg = loadPrintCfg(scope, tokens);
  const isCustom = /^\d{2,3}x\d{2,3}$/.test(cfg.size);
  const showPresets = scope === "order";
  const deliveryMode = !cfg.set.has("price") && !cfg.set.has("amount") && !cfg.set.has("totals");
  const mask = modal(`
    <h3>打印设置 · 模式：<span class="tag ${deliveryMode ? "warn" : "ok"}">${deliveryMode ? "送货单（无价格）" : "默认全单"}</span></h3>
    <div class="field" style="display:flex;gap:16px;align-items:flex-end;flex-wrap:wrap">
      <div><label>纸张</label>
        <select id="pc-size">
          ${PRINT_SIZES.map(([v, l]) => `<option value="${v}" ${(isCustom ? v === "custom" : v === cfg.size) ? "selected" : ""}>${l}</option>`).join("")}
        </select>
      </div>
      <div id="pc-custom-wrap" style="display:${isCustom ? "inline-flex" : "none"};gap:6px;align-items:center">
        <label>宽x高(mm) <input id="pc-custom" value="${isCustom ? esc(cfg.size) : "140x210"}" style="width:100px" placeholder="如 140x210" /></label>
      </div>
      ${showPresets ? `<div><label>预设</label>
        <button class="btn ghost sm" id="pc-p-full">默认全单</button>
        <button class="btn ghost sm" id="pc-p-del">送货单（随车·留数量藏价格）</button>
      </div>` : ""}
    </div>
    <div class="field">
      <label style="display:flex;flex-wrap:wrap;gap:8px 16px;align-items:center">
        ${tokens.map((t) => `<span style="display:inline-flex;align-items:center;gap:4px"><input type="checkbox" class="pc-f" value="${t}" ${cfg.set.has(t) ? "checked" : ""} />${PRINT_LABELS[t] || t}</span>`).join("")}
      </label>
    </div>
    <p class="muted" style="font-size:12px;margin:0">纸张决定 @page 尺寸与分页预算（A4≈42 行 / A5≈20 / 三等分≈28，窄纸自动压缩字号）；「紧凑分页」把多张单据排进同页，关掉则一单一页。设置保存在本机浏览器。</p>
    <div class="foot" style="gap:8px;justify-content:space-between">
      <span><button class="btn ghost sm" id="pc-all">全选</button> <button class="btn ghost sm" id="pc-none">清空</button></span>
      <span><button class="btn ghost" id="pc-cancel">取消</button> <button class="btn primary" id="pc-save">保存</button></span>
    </div>
  `);
  const boxes = $all(".pc-f", mask);
  const applyPreset = (keys) => {
    const on = keys === null ? new Set(tokens) : new Set(keys);
    boxes.forEach((b) => { b.checked = on.has(b.value); });
  };
  if (showPresets) {
    $("#pc-p-full", mask).onclick = () => applyPreset(PRINT_PRESETS.full);
    $("#pc-p-del", mask).onclick = () => applyPreset(PRINT_PRESETS.delivery);
  }
  $("#pc-all", mask).onclick = () => applyPreset(PRINT_PRESETS.full);
  $("#pc-none", mask).onclick = () => boxes.forEach((b) => { b.checked = false; });
  const sizeSel = $("#pc-size", mask);
  sizeSel.onchange = () => { $("#pc-custom-wrap", mask).style.display = sizeSel.value === "custom" ? "inline-flex" : "none"; };
  $("#pc-cancel", mask).onclick = closeModal;
  $("#pc-save", mask).onclick = () => {
    let size = sizeSel.value;
    if (size === "custom") {
      const v = $("#pc-custom", mask).value.trim().toLowerCase();
      if (!/^\d{2,3}x\d{2,3}$/.test(v)) { toast("自定义尺寸格式：宽x高 mm，如 140x210", "err"); return; }
      size = v;
    }
    savePrintCfg(scope, { set: new Set(boxes.filter((b) => b.checked).map((b) => b.value)), size });
    closeModal();
    toast("打印设置已保存", "ok");
  };
}

// ---------------- 订单（销售/采购）CRUD：对标金蝶订单流程 ----------------
const ORDER_STATUS_LABEL = {
  Draft: "草稿", Confirmed: "已确认", PartialShip: "部分发货", PartialIn: "部分入库",
  Completed: "已完成", Cancelled: "已作废",
};
const ORDER_CFG = {
  so: { base: "/sales/so", view: "so-doc", party: "客户编码", partyName: "客户名称", pkey: "customer_code", nkey: "customer_name" },
  po: { base: "/procure/po", view: "po-doc", party: "供应商编码", partyName: "供应商名称", pkey: "supplier_code", nkey: "supplier_name" },
};

async function openOrderEditor(kind, main, id) {
  const cfg = ORDER_CFG[kind];
  let o = null;
  if (id) {
    try {
      const r = await api(`${cfg.base}?period=${encodeURIComponent(state.current || "")}`);
      o = (r.rows || []).find((x) => x.id === id);
    } catch (e) { toast(e.message, "err"); return; }
    if (!o) { toast("未找到该订单，请刷新列表", "err"); return; }
  }
  const ord = o || { id: 0, status: "Draft", date: today(), memo: "", lines: [] };
  ord[cfg.pkey] = ord[cfg.pkey] || "";
  ord[cfg.nkey] = ord[cfg.nkey] || "";
  let lines = (ord.lines || []).map((l) => ({
    item_code: l.item_code || "", item_name: l.item_name || "",
    qty_ordered: String(l.qty_ordered != null ? l.qty_ordered : "1"),
    unit_price: String(l.unit_price != null ? l.unit_price : "0"),
    tax_rate: String(l.tax_rate != null ? l.tax_rate : "0.13"),
    qty_shipped: String(l.qty_shipped != null ? l.qty_shipped : "0"),
    qty_received: String(l.qty_received != null ? l.qty_received : "0"),
    memo: l.memo || "",
  }));
  const blank = { item_code: "", item_name: "", qty_ordered: "1", unit_price: "0", tax_rate: "0.13", qty_shipped: "0", qty_received: "0", memo: "" };
  if (!lines.length) lines = [{ ...blank }];
  const mask = modal(`
    <h3>${id ? `编辑订单 ${esc(ord.no || "")}` : (kind === "so" ? "新建销售订单" : "新建采购订单")}</h3>
    <div class="toolbar">
      <label>${cfg.party} * <input id="oe-party" value="${esc(ord[cfg.pkey])}" style="width:100px" /></label>
      <label>${cfg.partyName} <input id="oe-party-name" value="${esc(ord[cfg.nkey])}" style="width:110px" /></label>
      <label>日期 <input id="oe-date" type="date" value="${esc(ord.date)}" /></label>
      <label>备注 <input id="oe-memo" value="${esc(ord.memo)}" style="width:120px" /></label>
    </div>
    <table class="grid" id="oe-tbl">
      <thead><tr><th>存货编码 *</th><th>名称</th><th class="num">数量</th><th class="num">单价(不含税)</th><th class="num">税率</th><th class="num">行金额</th><th></th></tr></thead>
      <tbody></tbody>
    </table>
    <button class="btn ghost sm" id="oe-add">+ 增加明细行</button>
    <div style="margin-top:8px" class="muted">合计：不含税 <b id="oe-t-amt">0.00</b>　税额 <b id="oe-t-tax">0.00</b>　价税合计 <b id="oe-t-total">0.00</b></div>
    <div class="foot"><button class="btn primary" id="oe-save">保存</button><button class="btn ghost" id="oe-close">取消</button></div>
  `, true);
  const tbody = $("#oe-tbl tbody", mask);
  const recalc = () => {
    let a = 0, t = 0;
    lines.forEach((l) => {
      a += (parseFloat(l.qty_ordered) || 0) * (parseFloat(l.unit_price) || 0);
      t += (parseFloat(l.qty_ordered) || 0) * (parseFloat(l.unit_price) || 0) * (parseFloat(l.tax_rate) || 0);
    });
    $("#oe-t-amt", mask).textContent = a.toFixed(2);
    $("#oe-t-tax", mask).textContent = t.toFixed(2);
    $("#oe-t-total", mask).textContent = (a + t).toFixed(2);
  };
  const render = () => {
    tbody.innerHTML = lines.map((l, i) => `<tr>
      <td><input data-f="item_code" data-i="${i}" value="${esc(l.item_code)}" style="width:90px" /></td>
      <td><input data-f="item_name" data-i="${i}" value="${esc(l.item_name)}" style="width:110px" /></td>
      <td><input data-f="qty_ordered" data-i="${i}" value="${esc(l.qty_ordered)}" style="width:64px" /></td>
      <td><input data-f="unit_price" data-i="${i}" value="${esc(l.unit_price)}" style="width:80px" /></td>
      <td><input data-f="tax_rate" data-i="${i}" value="${esc(l.tax_rate)}" style="width:56px" /></td>
      <td class="num">${((parseFloat(l.qty_ordered) || 0) * (parseFloat(l.unit_price) || 0)).toFixed(2)}</td>
      <td><button class="btn ghost sm" data-del="${i}">删</button></td>
    </tr>`).join("");
    $all("input[data-f]", tbody).forEach((inp) => {
      inp.onchange = () => {
        const i = parseInt(inp.dataset.i, 10);
        lines[i][inp.dataset.f] = inp.value;
        recalc();
        render();
        // 采购行：单价为空/0 时自动带出最近采购价（price_history 由保存订单自动沉淀）
        if (kind === "po" && inp.dataset.f === "item_code" && inp.value.trim() &&
            (!lines[i].unit_price || lines[i].unit_price === "0")) {
          const item = inp.value.trim();
          const target = i;
          api(`/procure/price-history?item=${encodeURIComponent(item)}`).then((r2) => {
            const h = (r2.rows || [])[0];
            if (h && lines[target] && (!lines[target].unit_price || lines[target].unit_price === "0")) {
              lines[target].unit_price = String(h.price);
              recalc();
              render();
              toast(`已带出最近价 ${h.price}${h.supplier ? `（${h.supplier}）` : ""} ${h.date || ""}`.trim(), "ok");
            }
          }).catch(() => {});
        }
      };
    });
    $all("[data-del]", tbody).forEach((b) => b.onclick = () => {
      lines.splice(parseInt(b.dataset.del, 10), 1);
      if (!lines.length) lines.push({ ...blank });
      recalc(); render();
    });
  };
  render(); recalc();
  $("#oe-add", mask).onclick = () => { lines.push({ ...blank }); render(); recalc(); };
  $("#oe-close", mask).onclick = closeModal;
  $("#oe-save", mask).onclick = async () => {
    const party = $("#oe-party", mask).value.trim();
    if (!party) { toast(`${cfg.party}必填`, "err"); return; }
    const clean = lines.filter((l) => l.item_code.trim());
    if (!clean.length) { toast("至少一行明细（填写存货编码）", "err"); return; }
    const body = {
      id: ord.id || 0,
      period: ymm(state.current || ""),
      date: $("#oe-date", mask).value,
      [cfg.pkey]: party,
      [cfg.nkey]: $("#oe-party-name", mask).value.trim() || party,
      status: ord.status || "Draft",
      memo: $("#oe-memo", mask).value.trim(),
      lines: clean.map((l) => ({
        item_code: l.item_code.trim(), item_name: l.item_name.trim(),
        qty_ordered: l.qty_ordered, unit_price: l.unit_price, tax_rate: l.tax_rate,
        qty_shipped: l.qty_shipped, qty_received: l.qty_received, memo: l.memo,
      })),
    };
    try {
      await api(cfg.base, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
      toast("已保存订单", "ok"); closeModal(); rerenderView(cfg.view, main);
    } catch (e) { toast(e.message, "err"); }
  };
}

async function viewOrderChangeLog(main) {
  main.innerHTML = `<h2>订单变更历史</h2>
    <div class="toolbar">
      <label>类型 <select id="ocl-type"><option value="po">采购订单</option><option value="so">销售订单</option></select></label>
      <label>订单ID <input id="ocl-id" style="width:90px" /></label>
      <button class="btn primary" id="ocl-load">查询</button>
    </div>
    <div id="ocl-result" class="muted">填写订单ID后查询</div>`;
  $("#ocl-load").addEventListener("click", async () => {
    const type = $("#ocl-type").value, id = $("#ocl-id").value.trim();
    if (!id) { toast("请填写订单ID", "err"); return; }
    try {
      const r = await api(`/order/change-log?type=${encodeURIComponent(type)}&id=${encodeURIComponent(id)}`);
      const rows = r.rows || [];
      $("#ocl-result").innerHTML = rows.length
        ? `<table><thead><tr><th>字段</th><th>旧值</th><th>新值</th><th>操作人</th><th>时间</th></tr></thead>
          <tbody>${rows.map((x) => `<tr><td>${esc(x[0])}</td><td>${esc(x[1])}</td><td>${esc(x[2])}</td><td>${esc(x[3])}</td><td>${esc(x[4])}</td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">无变更记录</div>`;
    } catch (e) { toast(e.message, "err"); }
  });
}

// ===========================================================================
// 资金管理：资金日报 / 票据 / 融资 / 资金预测
// ===========================================================================
async function viewFunds(main) {
  main.innerHTML = `<h2>资金管理</h2>
    <div class="toolbar">
      <button class="btn sm ${state.fundsTab === "daily" ? "primary" : "ghost"}" id="ft-daily">资金日报</button>
      <button class="btn sm ${state.fundsTab === "bill" ? "primary" : "ghost"}" id="ft-bill">票据</button>
      <button class="btn sm ${state.fundsTab === "loan" ? "primary" : "ghost"}" id="ft-loan">融资</button>
      <button class="btn sm ${state.fundsTab === "count" ? "primary" : "ghost"}" id="ft-count">现金盘点</button>
      <button class="btn sm ${state.fundsTab === "check" ? "primary" : "ghost"}" id="ft-check">支票簿</button>
      <button class="btn sm ${state.fundsTab === "journal" ? "primary" : "ghost"}" id="ft-journal">日记账</button>
      <button class="btn sm ${state.fundsTab === "advance" ? "primary" : "ghost"}" id="ft-advance">借支</button>
      <button class="btn sm ${state.fundsTab === "shift" ? "primary" : "ghost"}" id="ft-shift">交接班</button>
      <button class="btn sm ${state.fundsTab === "budget" ? "primary" : "ghost"}" id="ft-budget">资金预算</button>
      <button class="btn sm ${state.fundsTab === "receipt" ? "primary" : "ghost"}" id="ft-receipt">收付款</button>
      <button class="btn sm ${state.fundsTab === "forecast" ? "primary" : "ghost"}" id="ft-forecast">资金预测</button>
    </div>
    <div id="funds-body" class="muted">加载中…</div>`;
  const tab = state.fundsTab || "daily";
  // 资金管理页写按钮按 voucher_new 显隐：导航放行的是 fin_report（只读/成本会计可进页），
  // 而页内写操作服务端要求 voucher_new——显隐与权限一致，避免"看得见点不了"
  const FUNDS_WRITE_IDS = ["#rc-new", "#cc-new", "#ck-new", "#ad-new", "#bill-new", "#loan-new",
    "#jn-clear", "#jn-unclear", "#jn-receipt", "#jn-payment"];
  const gateFundsWrites = () => {
    if (can("voucher_new")) return;
    FUNDS_WRITE_IDS.forEach((s) => { const el = $(s); if (el) el.remove(); });
  };
  const switchTab = (t) => { state.fundsTab = t; viewFunds(main); gateFundsWrites(); };
  $("#ft-daily").onclick = () => switchTab("daily");
  $("#ft-bill").onclick = () => switchTab("bill");
  $("#ft-loan").onclick = () => switchTab("loan");
  $("#ft-count").onclick = () => switchTab("count");
  $("#ft-check").onclick = () => switchTab("check");
  $("#ft-journal").onclick = () => switchTab("journal");
  $("#ft-advance").onclick = () => switchTab("advance");
  $("#ft-shift").onclick = () => switchTab("shift");
  $("#ft-budget").onclick = () => switchTab("budget");
  $("#ft-receipt").onclick = () => switchTab("receipt");
  $("#ft-forecast").onclick = () => switchTab("forecast");

  const body = $("#funds-body");
  if (tab === "daily") {
    body.className = "";
    const shift = (ds, n) => { const d = new Date(ds + "T00:00:00"); d.setDate(d.getDate() + n); return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`; };
    if (!state.fundsDate) state.fundsDate = today();
    body.innerHTML = `<div class="toolbar">
        <label>日期 <input type="date" id="fd-date" value="${esc(state.fundsDate)}" /></label>
        <button class="btn sm ghost" id="fd-prev">« 前一日</button>
        <button class="btn sm ghost" id="fd-next">后一日 »</button>
        <button class="btn sm ghost" id="fd-today">今天</button>
      </div><div id="fd-table" class="muted">加载中…</div>`;
    const load = async () => {
      try {
        const r = await api(`/funds/daily-by-date?date=${encodeURIComponent(state.fundsDate)}`);
        const rows = r.rows || [];
        $("#fd-table").innerHTML = rows.length
          ? `<table class="grid"><thead><tr><th>科目</th><th>科目名称</th><th class="num">上日结余</th><th class="num">本日收入</th><th class="num">本日支出</th><th class="num">日末结存</th></tr></thead><tbody>${rows.map((x) => `<tr><td>${esc(x.account_code)}</td><td>${esc(x.account_name)}</td><td class="num">${fmt(x.begin)}</td><td class="num">${fmt(x.income)}</td><td class="num">${fmt(x.expense)}</td><td class="num"><b>${fmt(x.end)}</b></td></tr>`).join("")}</tbody></table>`
          : `<div class="muted">无现金/银行科目</div>`;
      } catch (e) { $("#fd-table").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
    };
    const gotoDate = (ds) => { state.fundsDate = ds; $("#fd-date").value = ds; load(); };
    $("#fd-date").onchange = (e) => { state.fundsDate = e.target.value; load(); };
    $("#fd-prev").onclick = () => gotoDate(shift(state.fundsDate, -1));
    $("#fd-next").onclick = () => gotoDate(shift(state.fundsDate, 1));
    $("#fd-today").onclick = () => gotoDate(today());
    load();
  } else if (tab === "bill") {
    renderBills(body);
  } else if (tab === "loan") {
    renderLoans(body);
  } else if (tab === "count") {
    renderCashCount(body);
  } else if (tab === "check") {
    renderChecks(body);
  } else if (tab === "journal") {
    renderJournal(body);
  } else if (tab === "advance") {
    renderAdvances(body);
  } else if (tab === "shift") {
    renderCashShifts(body);
  } else if (tab === "budget") {
    renderBudget(body);
  } else if (tab === "receipt") {
    renderReceipts(body);
  } else {
    body.className = "";
    try {
      const r = await api("/funds/forecast");
      const f = r.forecast || {};
      body.innerHTML = `<div class="cards">
        ${["现金/银行结存", "在库应收票据", "应付票据", "放款可收回", "借款需偿还", "预计资金头寸"].map((t, i) => {
          const k = ["cash_balance", "receivable_bills", "payable_bills", "lend", "borrow", "position"][i];
          const v = f[k] || "0";
          return `<div class="card"><div class="k">${t}</div><div class="v">${fmt(v)}</div></div>`;
        }).join("")}
      </div>`;
    } catch (e) { body.innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
    body.innerHTML += `<div class="panel" style="margin-top:12px">
      <div style="display:flex;align-items:center;gap:8px;flex-wrap:wrap"><b>滚动预测（票据到期 + 融资起止按期间展开）</b><span class="grow"></span>
        <label style="font-size:12px">起 <input id="fr-from" value="${esc((state.current || "").replace("-", ""))}" style="width:90px" /></label>
        <label style="font-size:12px">期数 <input id="fr-n" value="6" style="width:50px" /></label>
        <button class="btn ghost sm" id="fr-run">滚动预测</button>
      </div>
      <div id="fr-out" class="muted" style="margin-top:6px">口径：期初结存=该期现金/银行期末（已记账）；不含未到期未核销往来</div>
    </div>`;
    $("#fr-run").onclick = async () => {
      try {
        const r = await api(`/funds/forecast-rolling?from=${encodeURIComponent($("#fr-from").value.trim())}&periods=${encodeURIComponent($("#fr-n").value.trim())}`);
        const rows = r.rows || [];
        $("#fr-out").innerHTML = rows.length
          ? `<table class="grid"><thead><tr><th>期间</th><th class="num">票据到期(收)</th><th class="num">票据到期(付)</th><th class="num">融资到账</th><th class="num">融资偿还</th><th class="num">净流</th><th class="num">期末结存</th></tr></thead><tbody>${rows.map((x) => `<tr><td>${esc(x.period)}</td><td class="num">${fmt(x.bill_in)}</td><td class="num">${fmt(x.bill_out)}</td><td class="num">${fmt(x.loan_in)}</td><td class="num">${fmt(x.loan_out)}</td><td class="num"><b>${fmt(x.net)}</b></td><td class="num"><b>${fmt(x.balance)}</b></td></tr>`).join("")}</tbody></table>`
          : `<div class="muted">无数据</div>`;
      } catch (e) { $("#fr-out").innerHTML = `<span style="color:var(--err)">${esc(e.message)}</span>`; }
    };
  }
  // 页内写按钮按 voucher_new 显隐（初始进入与切页签都会经过）
  gateFundsWrites();
}

// 资金预算 vs 执行（现金/银行科目；实际=当期已记账净额 H-3）
async function renderBudget(body) {
  body.className = "";
  body.innerHTML = `<div id="fb-table" class="muted">加载中…</div>`;
  try {
    const r = await api(`/funds/budget?period=${ymm(state.current || "")}`);
    const rows = r.rows || [];
    $("#fb-table").innerHTML = rows.length
      ? `<table class="grid"><thead><tr><th>科目</th><th>科目名称</th><th class="num">预算</th><th class="num">实际(净额)</th><th class="num">差异</th><th class="num">执行率</th><th>状态</th></tr></thead><tbody>${rows.map((x) => `<tr>
          <td>${esc(x.account_code)}</td><td>${esc(x.account_name)}</td>
          <td class="num">${fmt(x.budget)}</td><td class="num">${fmt(x.actual)}</td>
          <td class="num">${fmt(x.diff)}</td><td class="num">${esc(String(x.rate))}%</td>
          <td>${x.over ? '<span class="tag err">超预算</span>' : '<span class="tag ok">正常</span>'}</td></tr>`).join("")}</tbody></table>
        <div class="muted" style="font-size:12px;margin-top:6px">口径：预算取当前版本的现金/银行科目预算行；实际为当期已记账发生净额（H-3）。预算金额在预算管理维护。</div>`
      : `<div class="muted">本期未编制现金/银行科目预算：到【预算】为 1001/1002 等科目新增预算行后，此处显示执行情况。</div>`;
  } catch (e) { $("#fb-table").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
}

// 员工借支（出纳）：建单 → 支付（出凭证）→ 核销冲账（出凭证）
async function renderAdvances(body) {
  body.className = "";
  body.innerHTML = `<div class="toolbar">
      <label>日期 <input type="date" id="ad-date" value="${today()}" /></label>
      <label>借支人 <input id="ad-emp" style="width:90px" /></label>
      <label>事由 <input id="ad-purpose" style="width:150px" /></label>
      <label>金额 <input id="ad-amt" style="width:100px" /></label>
      <label>支付账户 <input id="ad-acct" value="1001" style="width:90px" /></label>
      <label>备注 <input id="ad-memo" style="width:110px" /></label>
      <button class="btn sm" id="ad-new">新增借支</button>
    </div>
    <div class="toolbar">
      <span class="muted" style="font-size:12px">核销参数（支付/核销按今天记账）：</span>
      <label>冲账费用科目 <input id="ad-exp-acct" value="660201" style="width:90px" /></label>
      <label>冲账金额 <input id="ad-exp-amt" style="width:100px" placeholder="全额退回填0" /></label>
    </div><div id="ad-list" class="muted">加载中…</div>`;
  $("#ad-new").onclick = async () => {
    if (!$("#ad-emp").value.trim()) { toast("借支人必填", "err"); return; }
    try {
      await api("/funds/advances", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({
        date: $("#ad-date").value, employee: $("#ad-emp").value.trim(), purpose: $("#ad-purpose").value.trim(),
        amount: $("#ad-amt").value.trim(), pay_account: $("#ad-acct").value.trim(),
        expense_account: $("#ad-exp-acct").value.trim(), memo: $("#ad-memo").value.trim(),
      }) });
      toast("已新增借支单", "ok");
      for (const k of ["ad-emp", "ad-purpose", "ad-amt", "ad-memo"]) $("#" + k).value = "";
      load();
    } catch (e) { toast(e.message, "err"); }
  };
  await load();
  async function load() {
    try {
      const r = await api("/funds/advances");
      const rows = r.rows || [];
      const stMap = { approved: ["待支付", "warn"], paid: ["已支付", "tag"], settled: ["已核销", "ok"] };
      $("#ad-list").innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>编号</th><th>日期</th><th>借支人</th><th>事由</th><th class="num">金额</th><th>支付账户</th><th>状态</th><th></th></tr></thead><tbody>${rows.map((a) => {
            const s = stMap[a.status] || [a.status, ""];
            return `<tr><td>${esc(a.no)}</td><td>${esc(a.date)}</td><td>${esc(a.employee)}</td><td>${esc(a.purpose || "—")}</td>
              <td class="num">${fmt(a.amount)}</td><td>${esc(a.pay_account)}</td>
              <td><span class="tag ${s[1]}">${esc(s[0])}</span></td>
              <td class="row-actions">
                ${a.status === "approved" ? `<button class="btn ghost sm" data-ad-p="${a.id}">支付</button><button class="btn ghost sm" data-ad-d="${a.id}">删除</button>` : ""}
                ${a.status === "paid" ? `<button class="btn ghost sm" data-ad-s="${a.id}">核销</button>` : ""}
              </td></tr>`;
          }).join("")}</tbody></table>`
        : `<div class="muted">暂无借支单</div>`;
      $all("[data-ad-p]").forEach((b) => b.onclick = async () => {
        try { const x = await api(`/funds/advances/${b.dataset.adP}/pay`, { method: "POST" }); toast(x.voucher_id ? `已支付，生成凭证 #${x.voucher_id}` : "已支付", "ok"); load(); } catch (e) { toast(e.message, "err"); }
      });
      $all("[data-ad-s]").forEach((b) => b.onclick = async () => {
        const expAmt = $("#ad-exp-amt").value.trim();
        if (expAmt === "") { toast("请先填写冲账金额（全额退回填0）", "err"); return; }
        try {
          const x = await api(`/funds/advances/${b.dataset.adS}/settle`, { method: "POST", headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ expense_account: $("#ad-exp-acct").value.trim() || "660201", expense_amount: expAmt }) });
          toast(x.voucher_id ? `已核销，生成凭证 #${x.voucher_id}` : "已核销", "ok");
          $("#ad-exp-amt").value = "";
          load();
        } catch (e) { toast(e.message, "err"); }
      });
      $all("[data-ad-d]").forEach((b) => b.onclick = async () => { if (!(await confirmDialog("删除该借支单？", true))) return; try { await api(`/funds/advances/${b.dataset.adD}/delete`, { method: "POST" }); toast("已删除", "ok"); load(); } catch (e) { toast(e.message, "err"); } });
    } catch (e) { $("#ad-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  }
}

// 收付款单（对标金蝶收款单/付款单）：保存即出凭证并按往来单位 FIFO 自动核销
async function renderReceipts(body) {
  body.className = "";
  const kindSel = () => $("#rc-kind").value;
  const loadParties = async () => {
    let parties = [];
    try { parties = await api(`/aux?kind=${kindSel() === "receipt" ? "customer" : "supplier"}`); } catch (e) { parties = []; }
    const sel = $("#rc-party");
    if (!sel) return;
    sel.innerHTML = parties.length
      ? parties.map((p) => `<option value="${esc(p.code)}">${esc(p.code)} ${esc(p.name)}</option>`).join("")
      : `<option value="">（先到辅助档案新增${kindSel() === "receipt" ? "客户" : "供应商"}）</option>`;
    sel.disabled = !parties.length;
  };
  body.innerHTML = `<div class="toolbar">
      <label>日期 <input type="date" id="rc-date" value="${today()}" /></label>
      <label>类型 <select id="rc-kind"><option value="receipt">收款</option><option value="payment">付款</option></select></label>
      <label>资金账户 <input id="rc-fund" value="" placeholder="默认100201" style="width:100px" /></label>
      <label>往来单位 <select id="rc-party" style="width:170px"></select></label>
      <label>金额 <input id="rc-amt" style="width:110px" /></label>
      <label>备注 <input id="rc-memo" style="width:130px" /></label>
      <button class="btn primary" id="rc-new">新增收付款</button>
    </div>
    <div class="muted" style="font-size:12px;margin-bottom:6px">保存即生成记账凭证草稿（会计记账后入账，H-3 草稿不入余额），并按往来单位对未清挂账 FIFO 自动核销；往来单位选凭证辅助编码（C01/S01…）。</div>
    <div class="panel"><div style="display:flex;align-items:center;gap:8px"><h4 style="margin:0">收付款单</h4><span class="grow"></span>${sizeSel("rc-size")}<button class="btn ghost sm" id="rc-print">批量打印</button><button class="btn ghost sm" id="rc-printcfg">打印设置</button></div><div id="rc-list" class="muted" style="margin-top:8px">加载中…</div></div>`;
  $("#rc-kind").onchange = () => loadParties();
  $("#rc-print").onclick = () => runPrint("/api/funds/receipts/print-form", ".rc-chk", "receipt", PRINT_RECEIPT_FIELDS);
  $("#rc-printcfg").onclick = () => openPrintConfig("receipt", PRINT_RECEIPT_FIELDS);
  bindSize($("#rc-size"), "receipt", PRINT_RECEIPT_FIELDS);
  $("#rc-new").onclick = async () => {
    if (!$("#rc-party").value) { toast("请选择往来单位", "err"); return; }
    try {
      const r = await postJson("/funds/receipts", {
        date: $("#rc-date").value, kind: kindSel(), fund_account: $("#rc-fund").value.trim(),
        party: $("#rc-party").value, amount: $("#rc-amt").value.trim(), memo: $("#rc-memo").value.trim(),
      });
      toast(`已保存为待审核单（审核后生成凭证并自动核销）`, "ok");
      $("#rc-amt").value = ""; $("#rc-memo").value = "";
      load();
    } catch (e) { toast(e.message, "err"); }
  };
  await loadParties();
  await load();
  async function load() {
    try {
      const r = await api("/funds/receipts");
      const rows = r.rows || [];
      $("#rc-list").innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th style="width:26px"><input type="checkbox" id="rc-chkall" title="全选" /></th><th>单号</th><th>日期</th><th>类型</th><th>资金账户</th><th>往来单位</th><th class="num">金额</th><th>凭证</th><th>状态</th><th>备注</th><th></th></tr></thead><tbody>${rows.map((d) => `<tr>
            <td><input type="checkbox" class="rc-chk" value="${d.id}" /></td><td>${esc(d.no)}</td><td>${esc(d.date)}</td><td>${d.kind === "receipt" ? "收款" : "付款"}</td>
            <td>${esc(d.fund_account)}</td><td>${esc(d.party)}</td><td class="num">${fmt(d.amount)}</td>
            <td>${d.voucher_id ? `<a href="#" data-rc-v="${d.voucher_id}">凭证 #${d.voucher_id}</a>` : "—"}</td>
            <td>${d.status === "audited" ? '<span class="tag ok">已审核</span>' : '<span class="tag warn">待审核</span>'}<span data-wftag="receipt:${d.id}"></span></td>
            <td>${esc(d.memo || "")}</td>
            <td class="row-actions">${can("voucher_audit") ? (d.status === "draft" ? `<button class="btn ghost sm" data-rc-audit="${d.id}">审核</button>` : `<button class="btn ghost sm" data-rc-unaudit="${d.id}">撤审</button>`) : ""}<button class="btn ghost sm" data-rc-d="${d.id}">删除</button></td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">暂无收付款单</div>`;
      fillWfTags($("#rc-list"));
      $all("[data-rc-v]").forEach((a) => a.onclick = (e) => { e.preventDefault(); openVoucherEditor(parseInt(a.dataset.rcV, 10)); });
      $all("[data-rc-d]").forEach((b) => b.onclick = async () => { if (!(await confirmDialog("删除该收付款单？其凭证需先作废或删除", true))) return; try { await api(`/funds/receipts/${b.dataset.rcD}/delete`, { method: "POST" }); toast("已删除", "ok"); load(); } catch (e) { toast(e.message, "err"); } });
      if ($("#rc-chkall")) $("#rc-chkall").onclick = (e) => { $all(".rc-chk").forEach((c) => { c.checked = e.target.checked; }); };
      $all("[data-rc-audit]").forEach((b) => b.onclick = async () => {
        try {
          const r = await postJson(`/funds/receipts/${b.dataset.rcAudit}/audit`, {});
          if (r.pending) { toast(`已审批 → 下一节点：${r.pending}`, "ok"); load(); return; }
          toast(`已审核，凭证 #${r.voucher_id}${r.settled ? `（自动核销 ${r.settled} 笔）` : ""}`, "ok");
          load();
        } catch (e) { toast(e.message, "err"); }
      });
      $all("[data-rc-unaudit]").forEach((b) => b.onclick = async () => {
        if (!(await confirmDialog("撤销审核？将删除其未记账凭证并清理核销配对，单据回到草稿。", true))) return;
        try { await postJson(`/funds/receipts/${b.dataset.rcUnaudit}/unaudit`, {}); toast("已撤销审核", "ok"); load(); } catch (e) { toast(e.message, "err"); }
      });
    } catch (e) { $("#rc-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  }
}

// 支票登记簿（出纳备查簿，不入账；账务由凭证体现）
async function renderChecks(body) {
  body.className = "";
  body.innerHTML = `<div class="toolbar">
      <label>票号 <input id="ck-no" style="width:110px" /></label>
      <label>类型 <select id="ck-kind"><option value="transfer">转账支票</option><option value="cash">现金支票</option></select></label>
      <label>付款科目 <input id="ck-bank" value="100201" style="width:90px" /></label>
      <label>收款人 <input id="ck-payee" style="width:120px" /></label>
      <label>金额 <input id="ck-amt" style="width:100px" /></label>
      <label>开出日 <input type="date" id="ck-date" value="${today()}" /></label>
      <label>备注 <input id="ck-memo" style="width:110px" /></label>
      <button class="btn sm" id="ck-new">新增支票</button>
    </div><div id="ck-list" class="muted">加载中…</div>`;
  $("#ck-new").onclick = async () => {
    if (!$("#ck-no").value.trim()) { toast("支票号必填", "err"); return; }
    try {
      await api("/funds/checks", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({
        no: $("#ck-no").value.trim(), kind: $("#ck-kind").value, bank_account: $("#ck-bank").value.trim(),
        payee: $("#ck-payee").value.trim(), amount: $("#ck-amt").value.trim(),
        issued_date: $("#ck-date").value, memo: $("#ck-memo").value.trim(),
      }) });
      toast("已登记支票", "ok");
      for (const k of ["ck-no", "ck-payee", "ck-amt", "ck-memo"]) $("#" + k).value = "";
      load();
    } catch (e) { toast(e.message, "err"); }
  };
  await load();
  async function load() {
    try {
      const r = await api("/funds/checks");
      const rows = r.rows || [];
      $("#ck-list").innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>票号</th><th>类型</th><th>付款科目</th><th>收款人</th><th class="num">金额</th><th>开出日</th><th>状态</th><th></th></tr></thead><tbody>${rows.map((c) => `<tr>
            <td>${esc(c.no)}</td><td>${c.kind === "cash" ? "现金支票" : "转账支票"}</td>
            <td>${esc(c.bank_account)}</td><td>${esc(c.payee || "—")}</td>
            <td class="num">${fmt(c.amount)}</td><td>${esc(c.issued_date)}</td>
            <td><span class="tag ${c.status === "void" ? "warn" : "ok"}">${c.status === "void" ? "已作废" : "已开出"}</span></td>
            <td class="row-actions">
              <button class="btn ghost sm" data-ck-s="${c.id}" data-to="${c.status === "void" ? "issued" : "void"}">${c.status === "void" ? "恢复" : "作废"}</button>
              <button class="btn ghost sm" data-ck-d="${c.id}">删除</button>
            </td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">暂无支票记录</div>`;
      $all("[data-ck-s]").forEach((b) => b.onclick = async () => { try { await api(`/funds/checks/${b.dataset.ckS}/status`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ status: b.dataset.to }) }); toast("已更新", "ok"); load(); } catch (e) { toast(e.message, "err"); } });
      $all("[data-ck-d]").forEach((b) => b.onclick = async () => { if (!(await confirmDialog("删除该支票记录？", true))) return; try { await api(`/funds/checks/${b.dataset.ckD}/delete`, { method: "POST" }); toast("已删除", "ok"); load(); } catch (e) { toast(e.message, "err"); } });
    } catch (e) { $("#ck-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  }
}

// 出纳交接班：交班快照（现金/银行结存、在库票据、未日清）+ 接班人确认
async function renderCashShifts(body) {
  body.className = "";
  const canWrite = can("cashier_sign");
  body.innerHTML = `<div class="toolbar">
      <label>日期 <input type="date" id="cs-date" value="${today()}" /></label>
      <label>接班人 <input id="cs-to" style="width:120px" placeholder="留空 = 待定" /></label>
      <label>备注 <input id="cs-memo" style="width:180px" /></label>
      <button class="btn sm primary" id="cs-new">新建交班单</button>
    </div>
    <div class="muted" style="font-size:12px;margin-bottom:8px">交班单快照当日现金/银行结存（仅已记账口径）、在库票据与未日清账户数；由接班人确认，交班人不能自确认。</div>
    <div id="cs-table" class="muted">加载中…</div>`;
  const load = async () => {
    try {
      const r = await api("/funds/shifts");
      const rows = r.rows || [];
      const stMap = { open: ["待确认", "warn"], confirmed: ["已确认", "ok"], cancelled: ["已取消", ""] };
      $("#cs-table").innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>日期</th><th>交班人</th><th>接班人</th><th class="num">现金结存</th><th class="num">银行结存</th><th class="num">在库票据</th><th class="num">未日清</th><th>状态</th><th>确认</th><th>备注</th><th></th></tr></thead><tbody>${rows.map((s) => {
            const st = stMap[s.status] || [s.status, ""];
            const acts = s.status === "open" && canWrite
              ? `<button class="btn ghost sm" data-cs-confirm="${s.id}">确认接班</button> <button class="btn ghost sm" data-cs-cancel="${s.id}">取消</button>` : "";
            return `<tr>
              <td>${esc(s.date)}</td><td>${esc(s.from_user)}</td><td>${esc(s.to_user || "—")}</td>
              <td class="num">${fmt(s.cash_balance)}</td><td class="num">${fmt(s.bank_balance)}</td>
              <td class="num">${s.bill_count} 张 / ${fmt(s.bill_amount)}</td>
              <td class="num">${s.uncleared}</td>
              <td><span class="tag ${st[1]}">${st[0]}</span></td>
              <td>${s.confirmed_by ? `${esc(s.confirmed_by)}<div class="muted" style="font-size:11px">${esc(s.confirmed_at || "")}</div>` : `<span class="muted">—</span>`}</td>
              <td>${esc(s.memo)}</td><td class="row-actions">${acts}</td></tr>`;
          }).join("")}</tbody></table>`
        : `<div class="muted">暂无交班记录</div>`;
      $all("[data-cs-confirm]").forEach((b) => b.onclick = async () => {
        if (!(await confirmDialog("确认接班？确认后该交班单不可再修改。"))) return;
        try {
          await api(`/funds/shifts/${b.dataset.csConfirm}/confirm`, { method: "POST", headers: { "Content-Type": "application/json" }, body: "{}" });
          toast("已确认接班", "ok");
          load();
        } catch (e) { toast(e.message, "err"); }
      });
      $all("[data-cs-cancel]").forEach((b) => b.onclick = async () => {
        if (!(await confirmDialog("取消该交班单？"))) return;
        try {
          await api(`/funds/shifts/${b.dataset.csCancel}/cancel`, { method: "POST", headers: { "Content-Type": "application/json" }, body: "{}" });
          toast("已取消", "ok");
          load();
        } catch (e) { toast(e.message, "err"); }
      });
    } catch (e) { $("#cs-table").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  if (canWrite) {
    $("#cs-new").onclick = async () => {
      try {
        await api("/funds/shifts", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ date: $("#cs-date").value, to_user: $("#cs-to").value.trim(), memo: $("#cs-memo").value.trim() }) });
        toast("交班单已创建，等待接班人确认", "ok");
        $("#cs-to").value = ""; $("#cs-memo").value = "";
        load();
      } catch (e) { toast(e.message, "err"); }
    };
  } else {
    const b = $("#cs-new"); if (b) b.remove();
  }
  load();
}

// 出纳日记账：本期已记账逐笔滚动 + 日清标记 + 收付登记（跳凭证录入预填科目）
async function renderJournal(body) {
  body.className = "";
  if (!state.journalAcct) state.journalAcct = "1001";
  body.innerHTML = `<div class="toolbar">
      <label>科目 <input id="jn-acct" value="${esc(state.journalAcct)}" style="width:90px" /></label>
      <label>日清日期 <input type="date" id="jn-date" value="${today()}" /></label>
      <button class="btn sm" id="jn-clear">标记日清</button>
      <button class="btn sm ghost" id="jn-unclear">取消日清</button>
      <button class="btn sm" id="jn-receipt">收款登记</button>
      <button class="btn sm" id="jn-payment">付款登记</button>
      <button class="btn sm ghost" id="jn-print">打印日记账</button>
    </div><div id="jn-table" class="muted">加载中…</div>`;
  const period = ymm(state.current || "");
  const y = parseInt(period.slice(0, 4), 10), m = parseInt(period.slice(4, 6), 10);
  const lastDay = new Date(y, m, 0).getDate();
  const pFrom = `${period.slice(0, 4)}-${period.slice(4, 6)}-01`;
  const pTo = `${period.slice(0, 4)}-${period.slice(4, 6)}-${String(lastDay).padStart(2, "0")}`;
  const acct = () => $("#jn-acct").value.trim() || "1001";
  const load = async () => {
    const a = acct();
    state.journalAcct = a;
    try {
      const [rows, cleared] = await Promise.all([
        api(`/ledger/journal?code=${encodeURIComponent(a)}&from=${period}&to=${period}`),
        api(`/funds/day-clear?account=${encodeURIComponent(a)}&from=${pFrom}&to=${pTo}`),
      ]);
      const cset = new Set(cleared.dates || []);
      $("#jn-table").innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>日期</th><th>凭证号</th><th>摘要</th><th>对方科目</th><th class="num">借方</th><th class="num">贷方</th><th class="num">余额</th><th>日清</th></tr></thead><tbody>${rows.map((r) => {
            const ds = String(r.date).slice(0, 10);
            return `<tr><td>${esc(ds)}</td><td>${esc(r.voucher_no)}</td><td>${esc(r.summary)}</td><td>${esc(r.opposite_accounts || "")}</td><td class="num">${fmt(r.debit)}</td><td class="num">${fmt(r.credit)}</td><td class="num">${fmt(r.balance)}</td><td>${cset.has(ds) ? '<span class="tag ok">已日清</span>' : ""}</td></tr>`;
          }).join("")}</tbody></table>`
        : `<div class="muted">该科目本期无已记账记录</div>`;
    } catch (e) { $("#jn-table").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  $("#jn-acct").onchange = () => load();
  const setClear = async (clear) => {
    try {
      await api("/funds/day-clear", { method: "POST", headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ account_code: acct(), date: $("#jn-date").value, clear }) });
      toast(clear ? "已标记日清" : "已取消日清", "ok");
      load();
    } catch (e) { toast(e.message, "err"); }
  };
  $("#jn-clear").onclick = () => setClear(true);
  $("#jn-unclear").onclick = () => setClear(false);
  $("#jn-print").onclick = () => printPreview(`出纳日记账 ${acct()} ${period}`, $("#jn-table").querySelector("table"));
  const jump = (dir) => {
    state.pendingCash = { account: acct(), dir };
    document.querySelector('.nav-item[data-view="vouchers"]').click();
    setTimeout(() => { const b = $("#new-v"); if (b) b.click(); }, 150);
  };
  $("#jn-receipt").onclick = () => jump("debit");
  $("#jn-payment").onclick = () => jump("credit");
  await load();
}

// 现金盘点（出纳）：账面按资金日报（按日）口径快照，差异一键出盘盈盘亏凭证
async function renderCashCount(body) {
  body.className = "";
  body.innerHTML = `<div class="toolbar">
      <label>日期 <input type="date" id="cc-date" value="${today()}" /></label>
      <label>科目 <input id="cc-acct" value="1001" style="width:90px" /></label>
      <label>实盘金额 <input id="cc-counted" style="width:110px" /></label>
      <label>备注 <input id="cc-memo" style="width:150px" /></label>
      <button class="btn sm" id="cc-new">新增盘点</button>
    </div><div id="cc-list" class="muted">加载中…</div>`;
  $("#cc-new").onclick = async () => {
    try {
      const r = await api("/funds/cash-counts", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({
        date: $("#cc-date").value, account_code: $("#cc-acct").value.trim(),
        counted: $("#cc-counted").value.trim(), memo: $("#cc-memo").value.trim(),
      }) });
      toast(`账面 ${fmt(r.book_amount)}，差异 ${fmt(r.diff)}`, "ok");
      $("#cc-counted").value = ""; $("#cc-memo").value = "";
      load();
    } catch (e) { toast(e.message, "err"); }
  };
  await load();
  async function load() {
    try {
      const r = await api("/funds/cash-counts");
      const rows = r.rows || [];
      $("#cc-list").innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>日期</th><th>科目</th><th class="num">账面余额</th><th class="num">实盘金额</th><th class="num">差异</th><th>备注</th><th></th></tr></thead><tbody>${rows.map((c) => `<tr>
            <td>${esc(c.date)}</td><td>${esc(c.account_code)}</td>
            <td class="num">${fmt(c.book_amount)}</td><td class="num">${fmt(c.counted)}</td>
            <td class="num">${Number(c.diff) === 0 ? fmt(c.diff) : `<b style="color:${Number(c.diff) > 0 ? "var(--ok)" : "var(--err)"}">${Number(c.diff) > 0 ? "+" : ""}${fmt(c.diff)}</b>`}</td>
            <td>${esc(c.memo || "")}</td>
            <td class="row-actions">
              ${!c.voucher_id && Number(c.diff) !== 0 ? `<button class="btn ghost sm" data-cc-v="${c.id}">生成凭证</button>` : ""}
              ${c.voucher_id ? `<span class="tag ok">已出凭证</span>` : ""}
              <button class="btn ghost sm" data-cc-d="${c.id}">删除</button>
            </td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">暂无盘点记录</div>`;
      $all("[data-cc-v]").forEach((b) => b.onclick = async () => { try { const x = await api(`/funds/cash-counts/${b.dataset.ccV}/voucher`, { method: "POST" }); toast(`已生成盘盈盘亏凭证 #${x.id}`, "ok"); load(); } catch (e) { toast(e.message, "err"); } });
      $all("[data-cc-d]").forEach((b) => b.onclick = async () => { if (!(await confirmDialog("删除该盘点记录？", true))) return; try { await api(`/funds/cash-counts/${b.dataset.ccD}/delete`, { method: "POST" }); toast("已删除", "ok"); load(); } catch (e) { toast(e.message, "err"); } });
    } catch (e) { $("#cc-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  }
}

async function renderBills(body) {
  body.className = "";
  const toolbar = `<div class="toolbar">
      <button class="btn sm" id="bill-new">新增票据</button>
      <label>类型 <select id="bill-kind"><option value="">全部</option><option value="receivable">应收</option><option value="payable">应付</option></select></label>
    </div><div id="bill-list" class="muted">加载中…</div>`;
  body.innerHTML = toolbar;
  $("#bill-new").onclick = () => openBillEditor(null);
  $("#bill-kind").onchange = loadBills;
  await loadBills();
  async function loadBills() {
    const kind = $("#bill-kind").value;
    try {
      const r = await api(`/funds/bills?kind=${encodeURIComponent(kind)}`);
      const rows = r.rows || [];
      const stMap = { in_hand: ["在库", "ok"], endorsed: ["已背书", "warn"], discounted: ["已贴现", "warn"], matured: ["已到期", "err"], settled: ["已兑付", "ok"] };
      $("#bill-list").innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>类型</th><th>票据号</th><th>出票日</th><th>到期日</th><th>对方单位</th><th>承兑银行</th><th class="num">金额</th><th>状态</th><th></th></tr></thead>
          <tbody>${rows.map((b) => {
            const s = stMap[b.status] || [b.status, ""];
            return `<tr><td>${b.kind === "receivable" ? "应收" : "应付"}</td><td>${esc(b.no)}</td>
              <td>${esc(b.issue_date)}</td><td>${esc(b.due_date)}</td><td>${esc(b.counterpart || "—")}</td>
              <td>${esc(b.bank || "—")}</td><td class="num">${fmt(b.amount)}</td>
              <td><span class="tag ${s[1]}">${esc(s[0])}</span></td>
              <td class="row-actions">
                <button class="btn ghost sm" data-bill="${b.id}">打开</button>
                ${b.status === "in_hand" ? `<button class="btn ghost sm" data-bill-act="${b.id}" data-to="endorsed">背书</button>
                <button class="btn ghost sm" data-bill-act="${b.id}" data-to="discounted">贴现</button>
                <button class="btn ghost sm" data-bill-act="${b.id}" data-to="settled">兑付</button>` : ""}
                ${["discounted", "endorsed", "settled"].includes(b.status) && !b.voucher_id ? `<button class="btn ghost sm" data-bill-v="${b.id}">生成凭证</button>` : ""}
              </td></tr>`;
          }).join("")}</tbody></table>`
        : `<div class="muted">暂无票据</div>`;
      $all("[data-bill]").forEach((b) => b.onclick = () => openBillEditor(parseInt(b.dataset.bill, 10)));
      $all("[data-bill-act]").forEach((b) => b.onclick = async () => {
        try {
          const r = await api(`/funds/bills/${b.dataset.billAct}/status`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ status: b.dataset.to, date: today() }) });
          toast(r.voucher_id ? `已更新，生成凭证 #${r.voucher_id}` : "已更新", "ok"); loadBills();
        } catch (e) { toast(e.message, "err"); }
      });
      $all("[data-bill-v]").forEach((b) => b.onclick = async () => {
        try { const r = await api(`/funds/bills/${b.dataset.billV}/voucher`, { method: "POST" }); toast(`已生成凭证 #${r.id}`, "ok"); loadBills(); } catch (e) { toast(e.message, "err"); }
      });
    } catch (e) { $("#bill-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  }
}

async function openBillEditor(id) {
  let b = { id: 0, kind: "receivable", no: "", issue_date: today(), due_date: today(), counterpart: "", bank: "", amount: "0", status: "in_hand", memo: "" };
  if (id) { try { const r = await api("/funds/bills"); b = (r.rows || []).find((x) => x.id === id) || b; } catch (e) { toast(e.message, "err"); return; } }
  const mask = modal(`
    <h3>${id ? "编辑票据" : "新增票据"}</h3>
    <div class="field"><label>类型</label><select id="b-kind">
      <option value="receivable" ${b.kind === "receivable" ? "selected" : ""}>应收票据</option>
      <option value="payable" ${b.kind === "payable" ? "selected" : ""}>应付票据</option></select></div>
    <div class="field"><label>票据号</label><input id="b-no" value="${esc(b.no)}" /></div>
    <div class="field"><label>出票日</label><input id="b-issue" type="date" value="${esc(b.issue_date)}" /></div>
    <div class="field"><label>到期日</label><input id="b-due" type="date" value="${esc(b.due_date)}" /></div>
    <div class="field"><label>对方单位</label><input id="b-cp" value="${esc(b.counterpart)}" /></div>
    <div class="field"><label>承兑银行</label><input id="b-bank" value="${esc(b.bank)}" /></div>
    <div class="field"><label>金额</label><input id="b-amt" value="${esc(b.amount)}" /></div>
    <div class="field"><label>备注</label><input id="b-memo" value="${esc(b.memo)}" /></div>
    <div class="foot"><button class="btn" id="b-save">保存</button>${id ? `<button class="btn danger ghost" id="b-del">删除</button>` : ""}<button class="btn ghost" id="b-cancel">取消</button></div>`);
  $("#b-cancel", mask).onclick = closeModal;
  $("#b-save", mask).onclick = async () => {
    const body2 = {
      id: b.id, kind: $("#b-kind", mask).value, no: $("#b-no", mask).value.trim(),
      period: ymm(state.current || ""), issue_date: $("#b-issue", mask).value,
      due_date: $("#b-due", mask).value, counterpart: $("#b-cp", mask).value.trim(),
      bank: $("#b-bank", mask).value.trim(), amount: $("#b-amt", mask).value.trim(),
      status: "in_hand", memo: $("#b-memo", mask).value.trim(),
    };
    if (!body2.no || !body2.issue_date || !body2.due_date) { toast("票据号与日期必填", "err"); return; }
    try { await api("/funds/bills", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body2) }); toast("已保存", "ok"); closeModal(); renderBills($("#funds-body")); } catch (e) { toast(e.message, "err"); }
  };
  if (id) $("#b-del", mask).onclick = async () => { if (!(await confirmDialog("删除该票据？", true))) return; try { await api(`/funds/bills/${id}/delete`, { method: "POST" }); toast("已删除", "ok"); closeModal(); renderBills($("#funds-body")); } catch (e) { toast(e.message, "err"); } };
}

async function renderLoans(body) {
  body.className = "";
  body.innerHTML = `<div class="toolbar"><button class="btn sm" id="loan-new">新增融资</button>
      <label>类型 <select id="loan-kind"><option value="">全部</option><option value="borrow">借款</option><option value="lend">放款</option></select></label>
    </div><div id="loan-list" class="muted">加载中…</div>`;
  $("#loan-new").onclick = () => openLoanEditor(null);
  $("#loan-kind").onchange = loadLoans;
  await loadLoans();
  async function loadLoans() {
    const kind = $("#loan-kind").value;
    try {
      const r = await api(`/funds/loans?kind=${encodeURIComponent(kind)}`);
      const rows = r.rows || [];
      $("#loan-list").innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>类型</th><th>编号</th><th>机构</th><th class="num">本金</th><th class="num">年利率%</th><th>起息日</th><th>到期日</th><th>状态</th><th></th></tr></thead>
          <tbody>${rows.map((l) => `<tr>
            <td>${l.kind === "borrow" ? "借款" : "放款"}</td><td>${esc(l.no)}</td><td>${esc(l.bank || "—")}</td>
            <td class="num">${fmt(l.principal)}</td><td class="num">${fmt(l.rate_pct)}</td>
            <td>${esc(l.start_date)}</td><td>${esc(l.end_date)}</td>
            <td><span class="tag ${l.status === "active" ? "warn" : "ok"}">${l.status === "active" ? "存续" : "已结清"}</span></td>
            <td class="row-actions">
              <button class="btn ghost sm" data-loan="${l.id}">打开</button>
              ${l.status === "active" ? `<button class="btn ghost sm" data-loan-settle="${l.id}">结清</button>` : ""}
              ${l.status === "active" && !l.voucher_id ? `<button class="btn ghost sm" data-loan-v="${l.id}">到账凭证</button>` : ""}
              ${l.status !== "active" && !l.settle_voucher_id ? `<button class="btn ghost sm" data-loan-v="${l.id}">还本凭证</button>` : ""}
            </td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">暂无融资记录</div>`;
      $all("[data-loan]").forEach((b) => b.onclick = () => openLoanEditor(parseInt(b.dataset.loan, 10)));
      $all("[data-loan-settle]").forEach((b) => b.onclick = async () => {
        if (!(await confirmDialog("结清该笔融资？", true))) return;
        try {
          const r = await api(`/funds/loans/${b.dataset.loanSettle}/settle`, { method: "POST" });
          toast(r.voucher_id ? `已结清，生成还本凭证 #${r.voucher_id}` : "已结清", "ok"); loadLoans();
        } catch (e) { toast(e.message, "err"); }
      });
      $all("[data-loan-v]").forEach((b) => b.onclick = async () => {
        try { const r = await api(`/funds/loans/${b.dataset.loanV}/voucher`, { method: "POST" }); toast(`已生成凭证 #${r.id}`, "ok"); loadLoans(); } catch (e) { toast(e.message, "err"); }
      });
    } catch (e) { $("#loan-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  }
}

async function openLoanEditor(id) {
  let l = { id: 0, kind: "borrow", no: "", bank: "", principal: "0", rate_pct: "0", start_date: today(), end_date: today(), status: "active", memo: "" };
  if (id) { try { const r = await api("/funds/loans"); l = (r.rows || []).find((x) => x.id === id) || l; } catch (e) { toast(e.message, "err"); return; } }
  const mask = modal(`
    <h3>${id ? "编辑融资" : "新增融资"}</h3>
    <div class="field"><label>类型</label><select id="l-kind">
      <option value="borrow" ${l.kind === "borrow" ? "selected" : ""}>借款</option>
      <option value="lend" ${l.kind === "lend" ? "selected" : ""}>放款</option></select></div>
    <div class="field"><label>编号</label><input id="l-no" value="${esc(l.no)}" /></div>
    <div class="field"><label>机构</label><input id="l-bank" value="${esc(l.bank)}" /></div>
    <div class="field"><label>本金</label><input id="l-pr" value="${esc(l.principal)}" /></div>
    <div class="field"><label>年利率(%)</label><input id="l-rate" value="${esc(l.rate_pct)}" /></div>
    <div class="field"><label>起息日</label><input id="l-start" type="date" value="${esc(l.start_date)}" /></div>
    <div class="field"><label>到期日</label><input id="l-end" type="date" value="${esc(l.end_date)}" /></div>
    <div class="field"><label>备注</label><input id="l-memo" value="${esc(l.memo)}" /></div>
    <div class="foot"><button class="btn" id="l-save">保存</button>${id ? `<button class="btn danger ghost" id="l-del">删除</button>` : ""}<button class="btn ghost" id="l-cancel">取消</button></div>`);
  $("#l-cancel", mask).onclick = closeModal;
  $("#l-save", mask).onclick = async () => {
    const body2 = {
      id: l.id, kind: $("#l-kind", mask).value, no: $("#l-no", mask).value.trim(),
      bank: $("#l-bank", mask).value.trim(), principal: $("#l-pr", mask).value.trim(),
      rate_pct: $("#l-rate", mask).value.trim(), start_date: $("#l-start", mask).value,
      end_date: $("#l-end", mask).value, status: "active", memo: $("#l-memo", mask).value.trim(),
    };
    if (!body2.no || !body2.start_date || !body2.end_date) { toast("编号与日期必填", "err"); return; }
    try { await api("/funds/loans", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body2) }); toast("已保存", "ok"); closeModal(); renderLoans($("#funds-body")); } catch (e) { toast(e.message, "err"); }
  };
  if (id) $("#l-del", mask).onclick = async () => { if (!(await confirmDialog("删除该笔融资？", true))) return; try { await api(`/funds/loans/${id}/delete`, { method: "POST" }); toast("已删除", "ok"); closeModal(); renderLoans($("#funds-body")); } catch (e) { toast(e.message, "err"); } };
}

// ===========================================================================
// 预算分析：年度逐月预算 vs 实际（含部门维度）
// ===========================================================================
async function viewBudgetAnalysis(main) {
  main.innerHTML = `<h2>预算分析</h2>
    <div class="toolbar">
      <label>年度 <input id="ana-year" value="${new Date().getFullYear()}" style="width:70px" /></label>
      <label>版本 <input id="ana-ver" value="" placeholder="留空=当前" style="width:110px" /></label>
      <button class="btn primary" id="ana-run">查询</button>
      <button class="btn ghost sm" id="ana-run-print">打印预览</button>
      <button class="btn ghost sm" id="ana-sum">仅汇总</button>
    </div>
    <div id="ana-body" class="muted">选择年度后查询</div>`;
  const run = async (summaryOnly) => {
    const year = $("#ana-year").value.trim() || String(new Date().getFullYear());
    const ver = $("#ana-ver").value.trim();
    try {
      const r = await api(`/budget/analysis?year=${encodeURIComponent(year)}&version=${encodeURIComponent(ver)}`);
      const rows = r.rows || [], sums = r.summary || [];
      if (summaryOnly || !rows.length) {
        $("#ana-body").innerHTML = sums.length
          ? `<div class="muted" style="margin-bottom:8px">年度汇总（科目 × 部门）</div><table class="grid"><thead><tr>
              <th>科目</th><th>部门</th><th class="num">预算</th><th class="num">实际</th><th class="num">执行率</th></tr></thead>
            <tbody>${sums.map((x) => `<tr><td>${esc(x.account_code)} ${esc(x.account_name)}</td><td>${esc(x.dept || "—")}</td>
              <td class="num">${fmt(x.budget)}</td><td class="num">${fmt(x.actual)}</td><td class="num">${x.rate}%</td></tr>`).join("")}</tbody></table>`
          : `<div class="muted">该年度无预算数据</div>`;
      } else {
        // 按月透视：行=科目×部门，列=月份
        const months = Array.from(new Set(rows.map((x) => x.period))).sort();
        const groups = {};
        rows.forEach((x) => { const k = `${x.account_code}|${x.account_name}|${x.dept}`; (groups[k] = groups[k] || []).push(x); });
        $("#ana-body").innerHTML = `<div class="muted" style="margin-bottom:8px">逐月预算 vs 实际（预算/实际/执行率%）</div><div class="panel" style="overflow-x:auto"><table class="grid"><thead><tr>
            <th>科目 / 部门</th>${months.map((mo) => `<th colspan="3" class="num">${esc(String(mo).slice(4, 6))}月</th>`).join("")}</tr>
          <tr><th></th>${months.map(() => `<th class="num">预算</th><th class="num">实际</th><th class="num">率</th>`).join("")}</tr></thead>
          <tbody>${Object.entries(groups).map(([k, items]) => {
            const m = {};
            items.forEach((i) => m[i.period] = i);
            return `<tr><td>${esc(k.split("|").slice(0, 2).join(" "))}${k.split("|")[2] ? `<div class="muted" style="font-size:11px">${esc(k.split("|")[2])}</div>` : ""}</td>
              ${months.map((mo) => { const i = m[mo]; return i ? `<td class="num">${fmt(i.budget)}</td><td class="num">${fmt(i.actual)}</td><td class="num">${i.rate}%</td>` : `<td class="num">—</td><td class="num">—</td><td class="num">—</td>`; }).join("")}</tr>`;
          }).join("")}</tbody></table></div>`;
      }
    } catch (e) { $("#ana-body").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  $("#ana-run").addEventListener("click", () => run(false));
  $("#ana-sum").addEventListener("click", () => run(true));
  $("#ana-run-print").addEventListener("click", () => { const el = $("#ana-body").querySelector("table"); printPreview("预算分析", el); });
}

// ===========================================================================
// 成本核算：计价方式配置 + 期末结价
// ===========================================================================
async function viewCost(main) {
  main.innerHTML = `<h2>成本核算</h2>
    <div class="toolbar">
      <button class="btn sm ${state.costTab === "config" ? "primary" : "ghost"}" id="ct-config">计价方式</button>
      <button class="btn sm ${state.costTab === "close" ? "primary" : "ghost"}" id="ct-close">期末结价</button>
      <button class="btn sm ${state.costTab === "recon" ? "primary" : "ghost"}" id="ct-recon">总账对账</button>
      <button class="btn sm ${state.costTab === "mfg" ? "primary" : "ghost"}" id="ct-mfg">制造成本</button>
    </div>
    <div id="cost-body" class="muted">加载中…</div>`;
  const tab = state.costTab || "config";
  $("#ct-config").onclick = () => { state.costTab = "config"; viewCost(main); };
  $("#ct-close").onclick = () => { state.costTab = "close"; viewCost(main); };
  $("#ct-recon").onclick = () => { state.costTab = "recon"; viewCost(main); };
  $("#ct-mfg").onclick = () => { state.costTab = "mfg"; viewCost(main); };
  const body = $("#cost-body");
  if (tab === "mfg") {
    body.className = "";
    body.innerHTML = `<div class="toolbar">
        <label>期间 <input id="mf-per" value="${esc(state.current)}" style="width:90px" /></label>
        <button class="btn sm" id="mf-wip">在产品（WIP）</button>
        <button class="btn sm" id="mf-var">差异分析</button>
        <button class="btn sm" id="mf-fc">成本预测</button>
        <span class="grow"></span>
        <label>分摊金额 <input id="mf-amt" style="width:90px" /></label>
        <label>基准 <select id="mf-base"><option value="cost">按成本占比</option><option value="labor">按直接人工</option><option value="qty">按计划产量</option></select></label>
        <button class="btn ghost sm" id="mf-try">试算分摊</button>
        <button class="btn sm" id="mf-apply">应用分摊</button>
      </div>
      <div id="mf-out" class="muted">选择动作后显示结果（WIP/差异/预测/分摊，与桌面端同源）</div>`;
    const per = () => $("#mf-per").value.trim();
    const show = (html) => { $("#mf-out").innerHTML = html; };
    const errBox = (e) => show(`<span style="color:var(--err)">${esc(e.message)}</span>`);
    $("#mf-wip").onclick = async () => {
      try {
        const r = await api(`/cost/wip?period=${encodeURIComponent(per())}`);
        const rows = r.rows || [];
        show(rows.length
          ? `<b>在产品（${esc(r.period)}）</b><table class="grid" style="margin-top:6px"><thead><tr><th>订单</th><th>产品</th><th class="num">数量</th><th class="num">材料</th><th class="num">人工</th><th class="num">制造费用</th><th class="num">合计</th></tr></thead><tbody>${rows.map((x) => `<tr><td>${esc(x.no)}</td><td>${esc(x.item_name)}</td><td class="num">${fmt(x.qty)}</td><td class="num">${fmt(x.material)}</td><td class="num">${fmt(x.labor)}</td><td class="num">${fmt(x.overhead)}</td><td class="num"><b>${fmt(x.total)}</b></td></tr>`).join("")}</tbody></table>`
          : `<div class="muted">该期间无未完工订单</div>`);
      } catch (e) { errBox(e); }
    };
    $("#mf-var").onclick = async () => {
      try {
        const r = await api(`/cost/variance?period=${encodeURIComponent(per())}`);
        const rows = r.rows || [];
        show(rows.length
          ? `<b>成本差异（实际 vs 标准，${esc(r.period)}）</b><table class="grid" style="margin-top:6px"><thead><tr><th>订单</th><th>产品</th><th class="num">计划量</th><th class="num">实际</th><th class="num">标准</th><th class="num">差异</th><th class="num">差异率</th></tr></thead><tbody>${rows.map((x) => `<tr${moneyNum(x.variance) !== 0 ? ` style="background:rgba(220,50,40,.07)"` : ""}><td>${esc(x.no)}</td><td>${esc(x.item_name)}</td><td class="num">${fmt(x.planned_qty)}</td><td class="num">${fmt(x.actual)}</td><td class="num">${fmt(x.standard)}</td><td class="num"><b>${fmt(x.variance)}</b></td><td class="num">${(x.variance_pct || 0).toFixed(2)}%</td></tr>`).join("")}</tbody></table>`
          : `<div class="muted">该期间无可分析订单</div>`);
      } catch (e) { errBox(e); }
    };
    $("#mf-fc").onclick = async () => {
      try {
        const r = await api(`/cost/forecast?period=${encodeURIComponent(per())}`);
        const rows = r.rows || [];
        show(rows.length
          ? `<b>成本预测（BOM 参考料本 × 计划量，${esc(r.period)}）</b><table class="grid" style="margin-top:6px"><thead><tr><th>订单</th><th>产品</th><th class="num">计划量</th><th class="num">预测料本</th></tr></thead><tbody>${rows.map((x) => `<tr><td>${esc(x.no)}</td><td>${esc(x.item_name)}</td><td class="num">${fmt(x.planned_qty)}</td><td class="num"><b>${fmt(x.forecast)}</b></td></tr>`).join("")}</tbody></table>`
          : `<div class="muted">该期间无可预测订单</div>`);
      } catch (e) { errBox(e); }
    };
    const doAlloc = async (apply) => {
      try {
        const r = await postJson("/cost/overhead", { period: parseInt(per(), 10) || 0, amount: $("#mf-amt").value.trim(), base: $("#mf-base").value, apply });
        const rows = r.rows || [];
        show(`<b>制造费用${apply ? "已应用" : "试算"}（${esc(r.base)}）</b><table class="grid" style="margin-top:6px"><thead><tr><th>订单</th><th class="num">分摊额</th></tr></thead><tbody>${rows.map((x) => `<tr><td>#${x.po_id}</td><td class="num">${fmt(x.amount)}</td></tr>`).join("")}</tbody></table>${apply ? "" : `<div class="muted" style="font-size:12px;margin-top:4px">试算未落库；点「应用分摊」写入归集</div>`}`);
        if (apply) toast("已应用制造费用分摊", "ok");
      } catch (e) { errBox(e); }
    };
    $("#mf-try").onclick = () => doAlloc(false);
    $("#mf-apply").onclick = () => doAlloc(true);
  } else if (tab === "config") {
    body.className = "";
    body.innerHTML = `<div class="toolbar"><button class="btn sm" id="cost-new">新增配置</button></div><div id="cost-list" class="muted">加载中…</div>`;
    $("#cost-new").onclick = () => openCostConfig(null);
    await loadCostConfigs();
    async function loadCostConfigs() {
      try {
        const r = await api("/cost/configs");
        const rows = r.rows || [];
        $("#cost-list").innerHTML = rows.length
          ? `<table class="grid"><thead><tr><th>存货</th><th>计价方式</th><th class="num">标准成本</th><th></th></tr></thead>
            <tbody>${rows.map((x) => `<tr><td>${esc(x.item)}</td><td>${esc(x.method_label)}</td>
              <td class="num">${fmt(x.standard_cost)}</td>
              <td class="row-actions"><button class="btn ghost sm" data-cfg="${esc(x.item)}">编辑</button>
              <button class="btn ghost sm" data-cfg-del="${esc(x.item)}">清除</button></td></tr>`).join("")}</tbody></table>`
          : `<div class="muted">尚未配置存货计价方式（默认移动加权平均）</div>`;
        $all("[data-cfg]").forEach((b) => b.onclick = () => openCostConfig(b.dataset.cfg));
        $all("[data-cfg-del]").forEach((b) => b.onclick = async () => {
          if (!(await confirmDialog(`清除 ${b.dataset.cfgDel} 的计价配置（恢复默认）？`, true))) return;
          try { await api(`/cost/configs/${encodeURIComponent(b.dataset.cfgDel)}/delete`, { method: "POST" }); toast("已清除", "ok"); loadCostConfigs(); } catch (e) { toast(e.message, "err"); }
        });
      } catch (e) { $("#cost-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
    }
  } else if (tab === "recon") {
    body.className = "";
    body.innerHTML = `<div class="toolbar">
        <label>期间 <input id="rc-per" value="${esc(state.current)}" style="width:90px" /></label>
        <button class="btn primary" id="rc-run">对账</button>
        <span class="muted" style="font-size:12px">库存流水金额（含结价调整）↔ 存货辅助余额（累计至期间）；建议先执行期末结价再对账</span>
      </div><div id="rc-out" class="muted">选择期间后点「对账」</div>`;
    $("#rc-run").onclick = async () => {
      try {
        const r = await api(`/cost/gl-reconcile?period=${encodeURIComponent($("#rc-per").value.trim())}`);
        const rows = r.rows || [];
        const nz = rows.filter((x) => moneyNum(x.diff) !== 0);
        $("#rc-out").innerHTML = `
          <div class="cards">
            <div class="card"><div class="k">库存侧合计</div><div class="v">${fmt(r.stock_total)}</div></div>
            <div class="card"><div class="k">总账侧合计</div><div class="v">${fmt(r.gl_total)}</div></div>
            <div class="card"><div class="k">差异（库存-总账）</div><div class="v" style="${moneyNum(r.diff_total) !== 0 ? "color:var(--err)" : ""}">${fmt(r.diff_total)}</div></div>
          </div>
          ${rows.length ? `<table class="grid" style="margin-top:10px"><thead><tr><th>存货</th><th class="num">库存金额</th><th class="num">总账金额</th><th class="num">差异</th></tr></thead><tbody>
            ${rows.map((x) => `<tr${moneyNum(x.diff) !== 0 ? ` style="background:rgba(220,50,40,.07)"` : ""}><td>${esc(x.item)}</td><td class="num">${fmt(x.stock_value)}</td><td class="num">${fmt(x.gl_value)}</td><td class="num"><b>${fmt(x.diff)}</b></td></tr>`).join("")}
          </tbody></table>
          <div style="margin-top:8px">${rows.length && nz.length === 0 ? `<span class="tag ok">✔ 对账平衡</span>` : `<span class="tag warn">${nz.length} 项存在差异（常见原因：单据未出凭证 / 未执行期末结价）</span>`}</div>`
          : `<div class="muted" style="margin-top:8px">两侧均无数据</div>`}`;
      } catch (e) { $("#rc-out").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
    };
  } else {
    body.className = "";
    body.innerHTML = `<div class="toolbar">
        <label>期间 <input id="ce-period" value="${esc(state.current)}" style="width:90px" /></label>
        <button class="btn" id="ce-preview">试算（不落账）</button>
        <button class="btn primary" id="ce-apply">期末结价（写入调整）</button>
      </div><div id="ce-list" class="muted">选择期间后试算</div>
      <div class="toolbar" style="margin-top:8px">
        <label>结转口径 <select id="sc-method"><option value="moving_average">移动加权平均</option><option value="fifo">先进先出</option><option value="month_average">全月一次加权</option></select></label>
        <button class="btn" id="sc-run">结转销售成本（借6401 / 贷140501）</button>
        <span class="muted" style="font-size:12px">仅统计销售出库（领料/形态转换不进6401）；本期无出库不生成凭证；同期间防重复</span>
      </div><div id="sc-out" class="muted"></div>`;
    const run = async (apply) => {
      const p = $("#ce-period").value.trim();
      try {
        const r = await api(`/cost/period-end?period=${encodeURIComponent(p)}&apply=${apply}`, apply ? { method: "POST" } : {});
        const rows = r.rows || [];
        const sumAdj = rows.reduce((a, x) => a + moneyNum(x.adjust), 0);
        $("#ce-list").innerHTML = rows.length
          ? `<div class="muted" style="margin-bottom:8px">期间 ${esc(r.period)}${apply ? "（已写入成本调整）" : "（试算）"} · 调整合计 ${moneyFmt(sumAdj)}</div>
            <table class="grid"><thead><tr><th>存货</th><th>计价方式</th><th class="num">结存数量</th><th class="num">结存金额</th><th class="num">单价</th><th class="num">调整额</th></tr></thead>
            <tbody>${rows.map((x) => `<tr><td>${esc(x.item)}</td><td>${esc(x.method)}</td>
              <td class="num">${fmt(x.end_qty)}</td><td class="num">${fmt(x.end_amount)}</td>
              <td class="num">${fmt(x.unit_cost)}</td>
              <td class="num" style="color:${moneyNum(x.adjust) < 0 ? "var(--err)" : "var(--ok)"}">${fmt(x.adjust)}</td></tr>`).join("")}</tbody></table>`
          : `<div class="muted">该期间无存货流水</div>`;
      } catch (e) { $("#ce-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
    };
    $("#ce-preview").addEventListener("click", () => run(false));
    $("#ce-apply").addEventListener("click", async () => { if (!(await confirmDialog("期末结价会写入成本调整流水（不影响数量），确认执行？", true))) return; run(true); });
  $("#sc-run").addEventListener("click", async () => {
    const p = $("#ce-period").value.trim();
    const label = $("#sc-method").selectedOptions[0].text;
    if (!(await confirmDialog(`按【${label}】结转 ${p} 的销售成本（借 6401 / 贷 140501）？`, true))) return;
    try {
      const ym = Number(p.replace(/-/g, ""));
      const r = await postJson("/cost/sales-cost", { period: ym, method: $("#sc-method").value });
      if (r.none) { toast(r.message, "ok"); $("#sc-out").textContent = r.message; }
      else { toast(`已生成销售成本结转凭证 #${r.voucher_id}`, "ok"); $("#sc-out").textContent = `凭证 #${r.voucher_id}（借 6401 / 贷 140501）`; }
    } catch (e) { toast(e.message, "err"); }
  });
  }
}

async function openCostConfig(item) {
  const mask = modal(`
    <h3>计价方式配置</h3>
    <div class="field"><label>存货编码</label><input id="c-item" value="${esc(item || "")}" ${item ? "disabled" : ""} /></div>
    <div class="field"><label>计价方式</label><select id="c-method">
      <option value="moving_average">移动加权平均</option>
      <option value="month_average">全月一次加权平均</option>
      <option value="fifo">先进先出</option>
      <option value="specific">个别计价</option>
      <option value="standard">标准成本</option></select></div>
    <div class="field"><label>标准成本单价</label><input id="c-std" value="0" /></div>
    <div class="foot"><button class="btn" id="c-save">保存</button><button class="btn ghost" id="c-cancel">取消</button></div>`);
  if (item) {
    try {
      const r = await api("/cost/configs");
      const cfg = (r.rows || []).find((x) => x.item === item);
      if (cfg) { $("#c-method", mask).value = cfg.method; $("#c-std", mask).value = cfg.standard_cost; }
    } catch (e) {}
  }
  $("#c-cancel", mask).onclick = closeModal;
  $("#c-save", mask).onclick = async () => {
    const it = item || $("#c-item", mask).value.trim();
    if (!it) { toast("存货编码必填", "err"); return; }
    try {
      await api("/cost/configs", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ item: it, method: $("#c-method", mask).value, standard_cost: $("#c-std", mask).value.trim() || "0" }) });
      toast("已保存", "ok"); closeModal(); rerenderView("cost", $("#main"));
    } catch (e) { toast(e.message, "err"); }
  };
}

// ===========================================================================
// 基础资料与系统功能（对齐桌面端 finui：科目 / 期初 / 档案 / 参数 / 日志 / 备份 / 模板）
// ===========================================================================

// 辅助核算维度（bit 与后端 AuxKind::bit() 对齐，仅用于把掩码渲染成标签）
const AUX_KINDS = [
  { code: "customer", label: "客户", bit: 1 },
  { code: "supplier", label: "供应商", bit: 2 },
  { code: "dept", label: "部门", bit: 4 },
  { code: "employee", label: "职员", bit: 8 },
  { code: "project", label: "项目", bit: 16 },
  { code: "item", label: "存货", bit: 32 },
  { code: "cashflow", label: "现金流量", bit: 64 },
  { code: "bank", label: "银行账户", bit: 128 },
];
const ACCT_CATEGORIES = [
  { code: "asset", label: "资产" }, { code: "liability", label: "负债" }, { code: "common", label: "共同" },
  { code: "equity", label: "权益" }, { code: "cost", label: "成本" }, { code: "income", label: "收入" }, { code: "expense", label: "费用" },
];
const FREQ_LABELS = { manual: "手工调用", monthly: "每月生成", quarterly: "每季生成", yearly: "每年生成" };

function auxMaskLabel(mask) {
  return AUX_KINDS.filter((k) => (mask & k.bit) !== 0).map((k) => k.label).join("、") || "—";
}
function acctCatLabel(code) {
  const c = ACCT_CATEGORIES.find((x) => x.code === code);
  return c ? c.label : code;
}
function dirLabel(code) { return code === "credit" ? "贷" : "借"; }

// ---------------- 会计科目 ----------------
async function viewAccounts(main) {
  main.innerHTML = `<h2>会计科目</h2><div class="muted">加载中…</div>`;
  let rows;
  try { rows = await api("/accounts"); } catch (e) { main.innerHTML = `<h2>会计科目</h2><div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; return; }
  state.accounts = rows;
  renderAccounts(main, rows);
}

function renderAccounts(main, rows) {
  const kw = (state.acctKw || "").trim();
  const shown = kw ? rows.filter((a) => a.code.includes(kw) || a.name.includes(kw)) : rows;
  main.innerHTML = `
    <h2>会计科目</h2>
    <div class="toolbar">
      <input id="acct-kw" placeholder="编码 / 名称" value="${esc(kw)}" style="width:160px" />
      <button class="btn" id="acct-search">查询</button>
      <div class="spacer"></div>
      ${session.platformAdmin ? `<button class="btn ghost" id="acct-fill">填充默认科目</button>` : ""}
      ${can("account_edit") ? `<button class="btn primary" id="acct-new">新增科目</button>` : ""}
    </div>
    <div class="panel" style="padding:0;overflow:auto;max-height:70vh">
      <table class="grid">
        <thead><tr><th>编码</th><th>名称</th><th>类别</th><th>方向</th><th>辅助核算</th><th>数量</th><th>币种</th><th>标志</th><th>状态</th><th>备注</th><th></th></tr></thead>
        <tbody>
          ${shown.length ? shown.map((a) => `
            <tr>
              <td>${esc(a.code)}</td>
              <td>${esc(a.name)}</td>
              <td>${esc(acctCatLabel(a.category))}</td>
              <td>${esc(dirLabel(a.dir))}</td>
              <td>${esc(auxMaskLabel(a.aux))}</td>
              <td>${esc(a.unit || "—")}</td>
              <td>${esc(a.currency || "—")}</td>
              <td>${a.is_cash ? "现金 " : ""}${a.is_bank ? "银行" : ""}${!a.is_cash && !a.is_bank ? "—" : ""}</td>
              <td>${a.disabled ? `<span class="tag err">停用</span>` : `<span class="tag ok">启用</span>`}</td>
              <td>${esc(a.memo)}</td>
              <td class="row-actions">
                ${can("account_edit") ? `<button class="btn sm ghost" data-act="edit" data-code="${esc(a.code)}">编辑</button>` : ""}
                ${can("account_edit") ? `<button class="btn sm ghost" data-act="del" data-code="${esc(a.code)}">删除</button>` : ""}
              </td>
            </tr>`).join("") : `<tr><td colspan="11" class="muted" style="text-align:center;padding:18px">无科目${kw ? "（无匹配）" : ""}，可点「填充默认科目」</td></tr>`}
        </tbody>
      </table>
    </div>`;
  $("#acct-search").addEventListener("click", () => { state.acctKw = $("#acct-kw").value; renderAccounts(main, rows); });
  $("#acct-kw").addEventListener("keydown", (e) => { if (e.key === "Enter") { state.acctKw = $("#acct-kw").value; renderAccounts(main, rows); } });
  const fill = $("#acct-fill");
  if (fill) fill.addEventListener("click", async () => {
    if (!(await confirmDialog("按编码推断补充系统内置科目表（已有的科目不动），继续？"))) return;
    try { const r = await api("/accounts/fill-defaults", { method: "POST" }); toast(`已补充 ${r.added || 0} 个科目`, "ok"); viewAccounts(main); }
    catch (e) { toast(e.message, "err"); }
  });
  if ($("#acct-new")) $("#acct-new").addEventListener("click", () => openAccountEditor(main, null));
  $all("[data-act]", main).forEach((b) => b.addEventListener("click", async () => {
    const code = b.dataset.code;
    if (b.dataset.act === "edit") {
      const acc = rows.find((x) => x.code === code);
      if (acc) openAccountEditor(main, acc);
    } else if (b.dataset.act === "del") {
      if (!(await confirmDialog(`确定删除科目 ${code}？已被凭证使用的科目无法删除。`, true))) return;
      try { await api(`/accounts/${encodeURIComponent(code)}`, { method: "DELETE" }); toast("已删除", "ok"); viewAccounts(main); }
      catch (e) { toast(e.message, "err"); }
    }
  }));
}

function openAccountEditor(main, acc) {
  const isEdit = !!acc;
  const a = acc || { code: "", name: "", category: "asset", dir: "debit", aux: 0, unit: null, currency: null, has_qty: false, is_cash: false, is_bank: false, cash_flow_item: null, bs_item: null, pl_item: null, disabled: false, memo: "" };
  const mask = modal(`
    <h3>${isEdit ? "编辑科目" : "新增科目"}</h3>
    <div class="field"><label>科目编码 *</label><input id="ac-code" value="${esc(a.code)}" ${isEdit ? "readonly" : ""} placeholder="1001" /></div>
    <div class="field"><label>科目名称 *</label><input id="ac-name" value="${esc(a.name)}" /></div>
    <div class="field"><label>类别</label>
      <select id="ac-cat">${ACCT_CATEGORIES.map((c) => `<option value="${c.code}" ${a.category === c.code ? "selected" : ""}>${c.label}</option>`).join("")}</select>
    </div>
    <div class="field"><label>余额方向</label>
      <select id="ac-dir"><option value="debit" ${a.dir !== "credit" ? "selected" : ""}>借</option><option value="credit" ${a.dir === "credit" ? "selected" : ""}>贷</option></select>
    </div>
    <div class="field"><label>辅助核算维度</label>
      <div style="display:flex;flex-wrap:wrap;gap:8px 16px">
        ${AUX_KINDS.map((k) => `<label style="display:inline-flex;align-items:center;gap:4px"><input type="checkbox" class="ac-aux" value="${k.code}" ${(a.aux & k.bit) ? "checked" : ""} />${k.label}</label>`).join("")}
      </div>
    </div>
    <div class="field"><label>数量单位（留空不核算数量）</label><input id="ac-unit" value="${esc(a.unit || "")}" placeholder="件 / 吨" /></div>
    <div class="field"><label>外币币种（留空只核算人民币）</label><input id="ac-cur" value="${esc(a.currency || "")}" placeholder="USD" /></div>
    <div class="field"><label>备注</label><input id="ac-memo" value="${esc(a.memo)}" /></div>
    <div class="field" style="display:flex;gap:20px">
      <label style="display:inline-flex;align-items:center;gap:4px"><input type="checkbox" id="ac-cash" ${a.is_cash ? "checked" : ""} />现金科目</label>
      <label style="display:inline-flex;align-items:center;gap:4px"><input type="checkbox" id="ac-bank" ${a.is_bank ? "checked" : ""} />银行科目</label>
      <label style="display:inline-flex;align-items:center;gap:4px"><input type="checkbox" id="ac-disabled" ${a.disabled ? "checked" : ""} />停用</label>
    </div>
    <div class="foot">
      <button class="btn ghost" id="ac-cancel">取消</button>
      <button class="btn primary" id="ac-save">保存</button>
    </div>
  `);
  $("#ac-cancel", mask).addEventListener("click", closeModal);
  $("#ac-save", mask).addEventListener("click", async () => {
    const code = $("#ac-code", mask).value.trim();
    const name = $("#ac-name", mask).value.trim();
    if (!code || !name) { toast("编码与名称必填", "err"); return; }
    const auxKinds = $all(".ac-aux", mask).filter((c) => c.checked).map((c) => c.value);
    const unit = $("#ac-unit", mask).value.trim();
    const cur = $("#ac-cur", mask).value.trim();
    const body = {
      account: Object.assign({}, a, {
        code, name,
        category: $("#ac-cat", mask).value,
        dir: $("#ac-dir", mask).value,
        unit: unit || null,
        currency: cur || null,
        has_qty: !!unit,
        is_cash: $("#ac-cash", mask).checked,
        is_bank: $("#ac-bank", mask).checked,
        disabled: $("#ac-disabled", mask).checked,
        memo: $("#ac-memo", mask).value.trim(),
      }),
      aux_kinds: auxKinds,
    };
    try {
      if (isEdit) await api("/accounts", { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
      else await api("/accounts", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
      toast("已保存", "ok"); closeModal(); viewAccounts(main);
    } catch (e) { toast(e.message, "err"); }
  });
}

async function viewBegin(main) {
  main.innerHTML = `<h2>期初建账</h2><div class="muted">加载中…</div>`;
  let rows, accounts;
  try {
    [rows, accounts] = await Promise.all([api("/begin"), api("/accounts")]);
  } catch (e) { main.innerHTML = `<h2>期初建账</h2><div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; return; }
  state.accounts = accounts;
  renderBegin(main, rows);
}

function renderBegin(main, rows) {
  if (!state.beginDraft) state.beginDraft = rows.map((r) => ({
    id: r.id, account_code: r.account_code,
    dir: String(r.year_begin).trim().startsWith("-") ? "credit" : "debit",
    yb: fmt(String(Math.abs(parseFloat(r.year_begin) || 0))),
    ad: fmt(r.debit_accum), ac: fmt(r.credit_accum), qty: r.qty_begin == null ? "" : fmt(r.qty_begin),
  }));
  const draft = state.beginDraft;
  const num = (s) => parseFloat(String(s).replace(/,/g, "")) || 0;
  const acctList = (state.accounts || []).filter((a) => !a.disabled);
  const nameOf = (code) => { const a = (state.accounts || []).find((x) => x.code === code); return a ? a.name : ""; };
  // 科目下拉：编码 + 名称，已在草稿里的剔除
  const usedCodes = () => new Set(draft.map((r) => r.account_code).filter(Boolean));
  const codeExists = (code) => !!(state.accounts || []).some((a) => a.code === code && !a.disabled);
  let lastCheck = null;   // 后端 precheck 结果（保存前刷新）

  const tally = () => {
    const s = { ybD: 0, ybC: 0, ad: 0, ac: 0 };
    draft.forEach((r) => {
      if (r.dir === "debit") s.ybD += num(r.yb); else s.ybC += num(r.yb);
      s.ad += num(r.ad); s.ac += num(r.ac);
    });
    s.diff = (s.ybD + s.ad) - (s.ybC + s.ac);
    s.balanced = Math.abs(s.diff) < 0.005;
    return s;
  };
  const payloadOf = () => draft
    .filter((r) => r.account_code)
    .map((r) => ({ account_code: r.account_code.trim(), dir: r.dir, yb: r.yb.replace(/,/g, ""), ad: r.ad.replace(/,/g, ""), ac: r.ac.replace(/,/g, ""), qty: r.qty ? r.qty.replace(/,/g, "") : null }));

  const render = () => {
    const s = tally();
    const used = usedCodes();
    const opts = (sel) => {
      const list = acctList.filter((a) => !used.has(a.code) || a.code === sel);
      return list.map((a) => `<option value="${esc(a.code)}" ${sel === a.code ? "selected" : ""}>${esc(a.code)} ${esc(a.name)}</option>`).join("");
    };
    main.innerHTML = `
      <h2>期初建账</h2>
      <div class="muted" style="margin:-6px 0 10px">
        快捷：<b>F7</b> 聚焦科目搜索 · 金额框内 <b>Enter</b> 追加一行并带出上一行 ·
        在表格里 <b>Ctrl+V</b> 可直接粘贴 Excel 区域
      </div>
      <div class="toolbar">
        <button class="btn" id="bg-add">添加科目行</button>
        <button class="btn" id="bg-gen">按科目表生成</button>
        <button class="btn" id="bg-paste">粘贴 Excel</button>
        <button class="btn" id="bg-imp">导入…</button>
        <button class="btn ghost" id="bg-undo">撤销上次导入</button>
        <button class="btn ghost" id="bg-tpl">下载模板</button>
        <div class="spacer"></div>
        <span class="muted" id="bg-dirty"></span>
        ${can("opening") ? `<button class="btn" id="bg-check">试算校验</button><button class="btn primary" id="bg-save">保存全部</button>` : ""}
      </div>
      <div class="panel" style="padding:0;overflow:auto;max-height:58vh">
        <table class="grid">
          <thead><tr><th style="width:190px">科目编码</th><th>科目名称</th><th style="width:70px">方向</th><th class="num" style="width:120px">年初余额</th><th class="num" style="width:120px">借方累计</th><th class="num" style="width:120px">贷方累计</th><th class="num" style="width:90px">数量</th><th style="width:60px"></th></tr></thead>
          <tbody id="bg-body">
            ${draft.length ? draft.map((r, i) => `
              <tr data-i="${i}">
                <td>
                  <input class="bg-code" data-i="${i}" value="${esc(r.account_code)}" placeholder="编码或搜索" style="width:100%" list="bg-acct-${i}" />
                  <datalist id="bg-acct-${i}">${opts(r.account_code)}</datalist>
                </td>
                <td class="muted" data-name="${i}">${esc(r.name || nameOf(r.account_code))}</td>
                <td><select class="bg-dir" data-i="${i}"><option value="debit" ${r.dir !== "credit" ? "selected" : ""}>借</option><option value="credit" ${r.dir === "credit" ? "selected" : ""}>贷</option></select></td>
                <td class="num"><input class="bg-yb num" data-i="${i}" value="${esc(r.yb)}" style="width:100%;text-align:right" /></td>
                <td class="num"><input class="bg-ad num" data-i="${i}" value="${esc(r.ad)}" style="width:100%;text-align:right" /></td>
                <td class="num"><input class="bg-ac num" data-i="${i}" value="${esc(r.ac)}" style="width:100%;text-align:right" /></td>
                <td class="num"><input class="bg-qty num" data-i="${i}" value="${esc(r.qty)}" style="width:100%;text-align:right" /></td>
                <td>${r.id > 0 ? `<span class="muted" style="font-size:12px">已有</span>` : `<button class="btn ghost sm" data-rm="${i}">移除</button>`}</td>
              </tr>`).join("") : `<tr><td colspan="8" class="muted" style="text-align:center;padding:18px">暂无期初数据 —— 点「添加科目行」手填，或「粘贴 Excel」/「导入…」批量建账</td></tr>`}
          </tbody>
        </table>
      </div>
      <div class="cards" style="margin-top:14px">
        <div class="card"><div class="k">年初借方合计</div><div class="v" style="font-size:16px">${fmt(s.ybD.toFixed(2))}</div></div>
        <div class="card"><div class="k">年初贷方合计</div><div class="v" style="font-size:16px">${fmt(s.ybC.toFixed(2))}</div></div>
        <div class="card"><div class="k">借方累计合计</div><div class="v" style="font-size:16px">${fmt(s.ad.toFixed(2))}</div></div>
        <div class="card"><div class="k">贷方累计合计</div><div class="v" style="font-size:16px">${fmt(s.ac.toFixed(2))}</div></div>
        <div class="card"><div class="k">试算差额</div><div class="v" style="font-size:16px;color:${s.balanced ? "var(--ok)" : "var(--err)"}">${s.balanced ? "✓ 平衡" : "✗ " + fmt(s.diff.toFixed(2))}</div></div>
      </div>
      <div id="bg-precheck" class="muted" style="margin-top:8px"></div>`;

    // ---- 单元格绑定 ----
    $all(".bg-code", main).forEach((inp) => {
      inp.oninput = () => {
        const i = +inp.dataset.i;
        draft[i].account_code = inp.value.trim();
        const cell = main.querySelector(`[data-name="${i}"]`);
        if (cell) cell.textContent = nameOf(draft[i].account_code) || (draft[i].codeExists === false ? "⚠ 科目不存在" : "");
        inp.style.borderColor = draft[i].account_code && !codeExists(draft[i].account_code) ? "var(--err)" : "";
        markDirty("期初未保存");
        updateDirtyHint();
      };
    });
    $all(".bg-dir", main).forEach((sel) => sel.onchange = () => { draft[+sel.dataset.i].dir = sel.value; render(); });
    $all(".bg-yb, .bg-ad, .bg-ac, .bg-qty", main).forEach((inp) => {
      inp.oninput = () => {
        const i = +inp.dataset.i;
        draft[i][{ "bg-yb": "yb", "bg-ad": "ad", "bg-ac": "ac", "bg-qty": "qty" }[inp.className.split(" ")[0]]] = inp.value;
        markDirty("期初未保存"); updateDirtyHint();
        refreshTotals();
      };
      // 金额框内 Enter → 追加一行并带出上一行的科目（连录同类科目时省一次选择）
      inp.onkeydown = (e) => {
        if (e.key !== "Enter") return;
        e.preventDefault();
        const i = +inp.dataset.i;
        const prev = draft[i];
        draft.splice(i + 1, 0, { id: 0, account_code: "", dir: prev.dir, yb: "", ad: "", ac: "", qty: "" });
        render();
        const nxt = main.querySelector(`.bg-code[data-i="${i + 1}"]`);
        if (nxt) nxt.focus();
      };
    });
    $all("[data-rm]", main).forEach((b) => b.onclick = () => { draft.splice(+b.dataset.rm, 1); markDirty("期初未保存"); render(); });

    // ---- 局部刷新合计（避免每次按键整表重绘，光标会跳）----
    function refreshTotals() {
      const t = tally();
      const cards = $all(".cards .card .v", main);
      if (cards.length >= 5) {
        cards[0].textContent = fmt(t.ybD.toFixed(2));
        cards[1].textContent = fmt(t.ybC.toFixed(2));
        cards[2].textContent = fmt(t.ad.toFixed(2));
        cards[3].textContent = fmt(t.ac.toFixed(2));
        cards[4].textContent = t.balanced ? "✓ 平衡" : "✗ " + fmt(t.diff.toFixed(2));
        cards[4].style.color = t.balanced ? "var(--ok)" : "var(--err)";
      }
    }
    function updateDirtyHint() {
      const el = $("#bg-dirty", main);
      if (el) el.textContent = dirtyCount ? "有未保存修改" : "";
    }
    // 表格内粘贴
    const body = $("#bg-body", main);
    if (body) body.addEventListener("paste", onPaste);
  };

  // 粘贴处理：解析 → 合并进 draft（替换选中行或追加）
  function onPaste(e) {
    const text = (e.clipboardData || window.clipboardData).getData("text");
    if (!text || text.indexOf("\t") < 0 && text.indexOf(",") < 0) return;
    e.preventDefault();
    const { rows: pr, bad } = parseBeginText(text, state.accounts);
    if (!pr.length) { toast("没识别到可导入的科目行（首列需为科目编码）", "err"); return; }
    // 覆盖同编码行，其余追加
    const byCode = new Map(draft.map((r, i) => [r.account_code, i]));
    let added = 0, replaced = 0;
    pr.forEach((n) => {
      const row = Object.assign({ id: 0, pasted: true }, n);
      const idx = byCode.get(n.account_code);
      if (idx != null && draft[idx].id === 0) { draft[idx] = Object.assign({}, draft[idx], n); replaced++; }
      else { draft.push(row); added++; }
    });
    markDirty("期初未保存");
    render();
    const msg = `已粘贴 ${pr.length} 行（新增 ${added} / 覆盖 ${replaced}）`;
    if (bad.length) toast(msg + `；${bad.length} 行有问题：第 ${bad[0].line} 行 ${bad[0].code} ${bad[0].why}`, "err");
    else toast(msg, "ok");
  }

  // ---- 工具栏 ----
  $("#bg-add").onclick = () => { draft.push({ id: 0, account_code: "", dir: "debit", yb: "", ad: "", ac: "", qty: "" }); markDirty("期初未保存"); render(); const n = main.querySelector(`.bg-code[data-i="${draft.length - 1}"]`); if (n) n.focus(); };

  // 按科目表生成：把全部「末级 + 未停用」科目铺成空白行，方向按科目默认方向预填。
  // 末级判定：不存在另一个未停用科目，其编码以该科目编码为前缀且更长。
  $("#bg-gen").onclick = () => {
    const accts = (state.accounts || []).filter((a) => !a.disabled);
    const codes = accts.map((a) => a.code);
    const isLeaf = (code) => !codes.some((c) => c !== code && c.length > code.length && c.startsWith(code));
    const leaves = accts.filter((a) => isLeaf(a.code));
    if (!leaves.length) return toast("没有可用的末级科目", "err");
    const existing = new Set(draft.filter((r) => r.account_code).map((r) => r.account_code));
    const dirOf = (a) => (a.dir === "credit" ? "credit" : "debit");
    const added = [];
    leaves.forEach((a) => {
      if (existing.has(a.code)) return;
      draft.push({ id: 0, account_code: a.code, name: a.name, dir: dirOf(a), yb: "", ad: "", ac: "", qty: "" });
      added.push(a.code);
    });
    if (!added.length) return toast("科目已全部在草稿中，无需生成", "err");
    markDirty("期初未保存");
    render();
    toast(`已生成 ${added.length} 个末级科目空行（方向已按科目默认预填）`, "ok");
  };
  $("#bg-paste").onclick = async () => {
    try { const t = await navigator.clipboard.readText(); if (!t) return toast("剪贴板是空的", "err"); onPaste({ clipboardData: { getData: () => t }, preventDefault() {} }); }
    catch (e) { toast("浏览器不允许读剪贴板，请直接在表格里按 Ctrl+V", "err"); }
  };
  $("#bg-tpl").onclick = () => { window.location = "/api/import/template?kind=begin"; };

  // 撤销上次期初导入：回滚到导入前的快照
  $("#bg-undo").onclick = async () => {
    if (!window.confirm("撤销会把期初余额恢复到上次「导入」之前的状态，确定吗？")) return;
    try {
      const r = await api("/import/undo-begin", { method: "POST" });
      toast(`已撤销，还原 ${r.count} 条期初`, "ok");
      state.beginDraft = null;
      clearDirty("期初未保存");
      viewBegin(main);
    } catch (e) { toast(e.message, "err"); }
  };

  // 导入：复用后端 analyze/run 两段式（与"数据导入"页同一套端点）
  $("#bg-imp").onclick = () => {
    const tpl = "科目编码\t科目名称\t方向\t年初余额\t借方累计\t贷方累计\t数量\n"
      + "1001\t库存现金\t借\t10000\t0\t0\t\n1002\t银行存款\t贷\t8000\t0\t0\t\n"
      + "1122\t应收账款\t借\t5000\t0\t0\t\n2202\t应付账款\t贷\t3000\t0\t0\t";
    const box = modal(`<h3>批量导入期初余额</h3>
      <div class="muted" style="margin:6px 0 10px">
        <span class="step">① 贴数据</span> 把 Excel 里的期初区域直接复制进下面文本框，或 <a href="#" id="bg-dl">下载带示例的模板</a> 填好再粘贴。
        <span class="step" style="margin-left:12px">② 预检</span> 点「预检科目」查缺漏。
        <span class="step" style="margin-left:12px">③ 导入</span> 点「执行导入」落账。
        <div class="muted" style="margin-top:4px">列：科目编码 / 科目名称 / 方向 / 年初余额 / 借方累计 / 贷方累计 / 数量</div>
      </div>
      <textarea id="bg-imp-text" rows="9" style="width:100%;font-family:monospace" placeholder="${esc(tpl)}"></textarea>
      <div id="bg-imp-res" class="muted" style="margin-top:8px;white-space:pre-wrap"></div>
      <div style="margin-top:10px;display:flex;gap:8px">
        <button class="btn" id="bg-imp-a">② 预检科目</button>
        <button class="btn primary" id="bg-imp-r">③ 执行导入</button>
      </div>`);
    const q = (s) => box.querySelector(s);
    q("#bg-dl").onclick = (e) => { e.preventDefault(); window.location = "/api/import/template?kind=begin"; };
    const mapping = {};
    q("#bg-imp-a").onclick = async () => {
      const text = q("#bg-imp-text").value;
      if (!text.trim()) return toast("请先粘贴内容", "err");
      try {
        const r = await api("/import/analyze", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ kind: "begin", text, template: "generic" }) });
        const missing = (r && r.missing) || [];
        if (missing.length === 0) {
          q("#bg-imp-res").textContent = "✅ 预检通过：科目都存在，可执行导入。";
          return;
        }
        const list = missing.slice(0, 12).map((m) => m.code + "×" + m.count).join("、") + (missing.length > 12 ? " …" : "");
        q("#bg-imp-res").innerHTML = `⚠ 有 ${missing.length} 个科目在账套中不存在：${esc(list)}<br>导入时这些行会被跳过。`;
        const btn = document.createElement("button");
        btn.className = "btn sm";
        btn.style.marginTop = "6px";
        btn.textContent = `一键补建这 ${missing.length} 个科目`;
        btn.onclick = async () => {
          btn.disabled = true; btn.textContent = "补建中…";
          try {
            const c = await api("/import/autocreate", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ kind: "begin", text, template: "generic" }) });
            toast(`已补建 ${c.ok} 个科目，可重新预检或直接导入`, "ok");
            q("#bg-imp-res").textContent = `✅ 已补建 ${c.ok} 个科目（类别/方向按编码自动推断，可在科目表里再改名称）。点「执行导入」即可。`;
          } catch (e) { toast("补建失败：" + e.message, "err"); btn.disabled = false; btn.textContent = `一键补建这 ${missing.length} 个科目`; }
        };
        q("#bg-imp-res").appendChild(btn);
      } catch (e) { q("#bg-imp-res").textContent = "预检失败：" + e.message; }
    };
    q("#bg-imp-r").onclick = async () => {
      const text = q("#bg-imp-text").value;
      if (!text.trim()) return toast("请先粘贴内容", "err");
      try {
        const r = await api("/import/run", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ kind: "begin", text, template: "generic", period: 0, mapping }) });
        const lines = [`✅ 成功导入 ${r.ok} 条`, r.skipped ? `⚠ 跳过 ${r.skipped} 条` : ""].filter(Boolean);
        if (r.warnings && r.warnings.length) { lines.push(""); r.warnings.slice(0, 15).forEach((w) => lines.push("  · " + w)); }
        q("#bg-imp-res").textContent = lines.join("\n");
        toast(`已导入 ${r.ok} 条`, "ok");
        state.beginDraft = null;
        clearDirty("期初未保存");
        closeModal();
        viewBegin(main);
      } catch (e) { q("#bg-imp-res").textContent = "导入失败：" + e.message; }
    };
  };

  const runPrecheck = async (silent) => {
    const el = $("#bg-precheck", main);
    if (!el) return null;
    if (!payloadOf().length) { el.textContent = ""; return null; }
    el.textContent = "校验中…";
    try {
      const r = await api("/begin/precheck", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(payloadOf()) });
      lastCheck = r;
      const bad = (r.invalid || []).map((x) => `${x.code}（${x.reason}）`).join("、");
      el.innerHTML = r.balanced
        ? `✓ 后端校验通过：${r.count} 条，借方 ${r.sum_debit} = 贷方 ${r.sum_credit}`
        : `✗ 后端校验：差额 ${r.diff}（借 ${r.sum_debit} / 贷 ${r.sum_credit}）` + (bad ? `<br>问题行：${esc(bad)}` : "");
      // 高亮问题行：先清掉上一轮高亮，再按 invalid 的 code 定位对应 <tr> 标红
      $all(".bg-row-err", main).forEach((tr) => tr.classList.remove("bg-row-err"));
      (r.invalid || []).forEach((x) => {
        const idx = draft.findIndex((d) => d.account_code === x.code);
        if (idx >= 0) {
          const tr = main.querySelector(`[data-i="${idx}"]`);
          if (tr) tr.classList.add("bg-row-err");
        }
      });
      return r;
    } catch (e) { el.textContent = "校验失败：" + e.message; return null; }
  };
  const bchk = $("#bg-check", main);
  if (bchk) bchk.onclick = () => runPrecheck(false);

  $("#bg-save").onclick = async () => {
    const payload = payloadOf();
    if (!payload.length) return toast("没有可保存的期初行", "err");
    // 保存前先跑一次后端预检：不平衡就别浪费一次写库往返
    const chk = await runPrecheck(true);
    if (chk && !chk.balanced) {
      toast(`试算不平衡，差额 ${chk.diff}，请调整后再保存`, "err");
      render();
      return;
    }
    if ((chk && (chk.invalid || []).length)) {
      toast(`有 ${chk.invalid.length} 行科目有问题，请先修正`, "err");
      return;
    }
    try {
      const r = await api("/begin", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(payload) });
      toast(`已保存 ${r.count || payload.length} 条期初`, "ok");
      state.beginDraft = null;
      clearDirty("期初未保存");
      viewBegin(main);
    } catch (e) { toast(e.message, "err"); }
  };

  render();
}

// ---------------- 辅助核算档案 ----------------
async function viewAux(main) {
  const kind = state.auxKind || "customer";
  main.innerHTML = `<h2>辅助核算档案</h2><div class="muted">加载中…</div>`;
  let rows;
  try { rows = await api(`/aux?kind=${encodeURIComponent(kind)}`); }
  catch (e) { main.innerHTML = `<h2>辅助核算档案</h2><div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; return; }
  renderAux(main, rows, kind);
}

function renderAux(main, rows, kind) {
  main.innerHTML = `
    <h2>辅助核算档案</h2>
    <div class="toolbar">
      ${AUX_KINDS.map((k) => `<button class="btn sm ${kind === k.code ? "primary" : "ghost"}" data-kind="${k.code}">${k.label}</button>`).join("")}
      <div class="spacer"></div>
      ${can("aux_edit") ? `<button class="btn primary" id="aux-new">新增${esc(AUX_KINDS.find((k) => k.code === kind).label)}</button>` : ""}
    </div>
    <div class="panel" style="padding:0;overflow:auto;max-height:65vh">
      <table class="grid">
        <thead><tr><th>编码</th><th>名称</th><th>上级编码</th><th>状态</th><th>备注</th><th></th></tr></thead>
        <tbody>
          ${rows.length ? rows.map((e) => `
            <tr>
              <td>${esc(e.code)}</td><td>${esc(e.name)}</td><td>${esc(e.parent_code || "—")}</td>
              <td>${e.disabled ? `<span class="tag err">停用</span>` : `<span class="tag ok">启用</span>`}</td>
              <td>${esc(e.memo)}</td>
              <td class="row-actions">
                ${can("aux_edit") ? `<button class="btn sm ghost" data-act="edit" data-id="${e.id}">编辑</button>` : ""}
                ${can("aux_edit") ? `<button class="btn sm ghost" data-act="del" data-id="${e.id}">删除</button>` : ""}
              </td>
            </tr>`).join("") : `<tr><td colspan="6" class="muted" style="text-align:center;padding:18px">暂无档案</td></tr>`}
        </tbody>
      </table>
    </div>`;
  $all("[data-kind]", main).forEach((b) => b.addEventListener("click", () => { state.auxKind = b.dataset.kind; viewAux(main); }));
  if ($("#aux-new")) $("#aux-new").addEventListener("click", () => openAuxEditor(main, null, kind));
  $all("[data-act]", main).forEach((b) => b.addEventListener("click", async () => {
    const id = parseInt(b.dataset.id, 10);
    if (b.dataset.act === "edit") {
      const e = rows.find((x) => x.id === id);
      if (e) openAuxEditor(main, e, kind);
    } else {
      if (!(await confirmDialog("确定删除该档案？", true))) return;
      try { await api(`/aux/${id}`, { method: "DELETE" }); toast("已删除", "ok"); rerenderView("aux", main); }
      catch (e2) { toast(e2.message, "err"); }
    }
  }));
}

function openAuxEditor(main, ent, kind) {
  const isEdit = !!ent;
  const e = ent || { id: 0, kind, code: "", name: "", parent_code: null, disabled: false, props: {}, memo: "" };
  const mask = modal(`
    <h3>${isEdit ? "编辑档案" : "新增档案"}（${esc(AUX_KINDS.find((k) => k.code === kind).label)}）</h3>
    <div class="field"><label>编码 *</label><input id="au-code" value="${esc(e.code)}" /></div>
    <div class="field"><label>名称 *</label><input id="au-name" value="${esc(e.name)}" /></div>
    ${kind === "customer" ? `<div class="field"><label>信用额度（0 = 不限；超出后订单「确认」被拒）</label><input id="au-credit" value="${esc((e.props && e.props.credit_limit) || "0")}" /></div>` : ""}
    ${kind === "item" ? `<div class="field"><label>保质期天数（0 = 不启用批次效期）</label><input id="au-shelf" value="${esc((e.props && e.props.shelf_life_days) || "0")}" /></div><div class="field"><label style="display:flex;gap:6px;align-items:center;font-weight:400"><input type="checkbox" id="au-qc" ${e.props && (e.props.qc_required === "1" || e.props.qc_required === "true") ? "checked" : ""} /> 启用来料检验（到货先入待检，质检转正后才可用）</label></div>
    <div class="field" style="display:flex;gap:12px;flex-wrap:wrap">
      <div><label>安全库存（低库存预警口径）</label><input id="au-safety" placeholder="0 = 不预警" style="width:130px" /></div>
      <div><label>前置期（天）</label><input id="au-lead" placeholder="0" style="width:90px" /></div>
      <div><label>批量（MRP 按批量取整）</label><input id="au-lot" placeholder="0" style="width:120px" /></div>
    </div>` : ""}
    <div class="field"><label>上级编码（分级档案用）</label><input id="au-parent" value="${esc(e.parent_code || "")}" /></div>
    <div class="field"><label>备注</label><input id="au-memo" value="${esc(e.memo)}" /></div>
    <div class="field"><label style="display:inline-flex;align-items:center;gap:4px"><input type="checkbox" id="au-disabled" ${e.disabled ? "checked" : ""} />停用</label></div>
    <div class="foot">
      <button class="btn ghost" id="au-cancel">取消</button>
      <button class="btn primary" id="au-save">保存</button>
    </div>
  `);
  $("#au-cancel", mask).addEventListener("click", closeModal);
  // 存货计划参数回显（安全库存/前置期/批量——异步填充，无记录时留默认）
  if (kind === "item" && isEdit) {
    api(`/item-plan?item=${encodeURIComponent(e.code)}`).then((r) => {
      const p = r && r.plan;
      if (!p) return;
      const s = $("#au-safety", mask), l = $("#au-lead", mask), t = $("#au-lot", mask);
      if (s) s.value = p.safety_stock;
      if (l) l.value = String(p.lead_days);
      if (t) t.value = p.lot_size;
    }).catch(() => {});
  }
  $("#au-save", mask).addEventListener("click", async () => {
    const code = $("#au-code", mask).value.trim();
    const name = $("#au-name", mask).value.trim();
    if (!code || !name) { toast("编码与名称必填", "err"); return; }
    const parent = $("#au-parent", mask).value.trim();
    const body = Object.assign({}, e, {
      kind, code, name,
      parent_code: parent || null,
      disabled: $("#au-disabled", mask).checked,
      memo: $("#au-memo", mask).value.trim(),
      props: Object.assign({}, e.props || {}, kind === "customer" ? { credit_limit: $("#au-credit", mask).value.trim() || "0" } : {}, kind === "item" ? { shelf_life_days: $("#au-shelf", mask).value.trim() || "0", qc_required: $("#au-qc", mask).checked ? "1" : "0" } : {}),
    });
    try {
      if (isEdit) await api(`/aux/${e.id}`, { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
      else await api("/aux", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
      // 存货计划参数随档案同存（低库存预警 ↔ 设置入口闭环）
      if (kind === "item") {
        try {
          await postJson("/item-plan", {
            item_code: code,
            safety_stock: ($("#au-safety", mask).value || "0").trim(),
            lead_days: parseInt($("#au-lead", mask).value, 10) || 0,
            lot_size: ($("#au-lot", mask).value || "0").trim(),
          });
        } catch (e2) { toast(`档案已保存，计划参数保存失败：${e2.message}`, "err"); }
      }
      toast("已保存", "ok"); closeModal(); rerenderView(state.view, main);
    } catch (err) { toast(err.message, "err"); }
  });
}

// ---------------- 账套参数 ----------------
async function viewOptions(main) {
  main.innerHTML = `<h2>账套参数</h2><div class="muted">加载中…</div>`;
  let o;
  try { o = await api("/options"); } catch (e) { main.innerHTML = `<h2>账套参数</h2><div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; return; }
  main.innerHTML = `
    <h2>账套参数</h2>
    <div class="panel" style="max-width:640px">
      <div class="field"><label>企业名称</label><input id="op-company" value="${esc(o.company)}" /></div>
      <div class="field"><label>纳税识别号</label><input id="op-taxno" value="${esc(o.tax_no)}" /></div>
      <div class="field"><label>本位币</label><input id="op-currency" value="${esc(o.base_currency)}" /></div>
      <div class="field"><label>启用期间（YYYYMM）</label><input id="op-start" value="${esc(o.start_period)}" /></div>
      <div class="field"><label>科目编码级长（逗号分隔，如 4,2,2,2,2）</label><input id="op-scheme" value="${esc((o.code_scheme || []).join(","))}" /></div>
      <div class="field"><label>凭证字方案（逗号分隔，如 记,收,付,转）</label><input id="op-words" value="${esc((o.voucher_words || []).join(","))}" /></div>
      <div class="field" style="display:flex;gap:12px;flex-wrap:wrap">
        <div><label>应收科目(客户)</label><input id="op-biz-ar" value="${esc((o.biz_accounts || {}).ar || "112201")}" style="width:90px" /></div>
        <div><label>应付科目(供应商)</label><input id="op-biz-ap" value="${esc((o.biz_accounts || {}).ap || "220201")}" style="width:90px" /></div>
        <div><label>收入科目</label><input id="op-biz-income" value="${esc((o.biz_accounts || {}).income || "600101")}" style="width:90px" /></div>
        <div><label>销项税科目</label><input id="op-biz-tax" value="${esc((o.biz_accounts || {}).tax_sales || "22210102")}" style="width:90px" /></div>
        <div><label>暂估材料科目</label><input id="op-biz-mat" value="${esc((o.biz_accounts || {}).material || "140301")}" style="width:90px" /></div>
        <div><label>默认资金账户</label><input id="op-biz-fund" value="${esc((o.biz_accounts || {}).fund || "100201")}" style="width:90px" /></div>
      </div>
      <p class="muted" style="font-size:12px">业务凭证自动生成的默认科目（收付款单/发货收入/暂估等），须为末级科目编码。</p>
      <div class="field" style="display:flex;gap:24px;flex-wrap:wrap">
        <label style="display:inline-flex;align-items:center;gap:4px"><input type="checkbox" id="op-audit" ${o.enable_audit ? "checked" : ""} />启用审核环节（未审核不能记账）</label>
        <label style="display:inline-flex;align-items:center;gap:4px"><input type="checkbox" id="op-qty" ${o.enable_qty ? "checked" : ""} />启用数量核算</label>
        <label style="display:inline-flex;align-items:center;gap:4px"><input type="checkbox" id="op-foreign" ${o.enable_foreign ? "checked" : ""} />启用外币核算</label>
        <label style="display:inline-flex;align-items:center;gap:4px"><input type="checkbox" id="op-cashier" ${o.require_cashier ? "checked" : ""} />出纳签字（涉及现金/银行的凭证记账前须签字）</label>
        <label>预算控制 <select id="op-budget"><option value="" ${!o.budget_control ? "selected" : ""}>关闭</option><option value="warn" ${o.budget_control === "warn" ? "selected" : ""}>超预算提醒（放行）</option><option value="strong" ${o.budget_control === "strong" ? "selected" : ""}>超预算强控（拒绝保存）</option></select></label>
        <label>单据前缀 <input id="op-pfx-po" value="${esc(((o.doc_prefixes || {}).po) || "")}" placeholder="CG" title="采购订单" style="width:56px" /> <input id="op-pfx-so" value="${esc(((o.doc_prefixes || {}).so) || "")}" placeholder="XS" title="销售订单" style="width:56px" /> <input id="op-pfx-req" value="${esc(((o.doc_prefixes || {}).req) || "")}" placeholder="QG" title="请购单" style="width:56px" /> <input id="op-pfx-quo" value="${esc(((o.doc_prefixes || {}).quo) || "")}" placeholder="BJ" title="报价单" style="width:56px" /> <input id="op-pfx-prod" value="${esc(((o.doc_prefixes || {}).prod) || "")}" placeholder="SC" title="生产订单" style="width:56px" /></label>
      </div>
      <p class="muted" style="font-size:12px">启用期间与科目级长影响科目编码校验与凭证编号，修改请谨慎；已开账后不建议改动。</p>
      ${can("sys_option") ? `<div class="foot" style="margin-top:10px"><button class="btn primary" id="op-save">保存参数</button></div>` : `<p class="muted">无修改权限（需要 sys_option）</p>`}
    </div>`;
  const save = $("#op-save");
  if (save) save.addEventListener("click", async () => {
    const scheme = $("#op-scheme").value.split(",").map((s) => parseInt(s.trim(), 10)).filter((n) => n > 0);
    const words = $("#op-words").value.split(/[,，]/).map((s) => s.trim()).filter(Boolean);
    const start = $("#op-start").value.trim();
    const body = Object.assign({}, o, {
      company: $("#op-company").value.trim(),
      tax_no: $("#op-taxno").value.trim(),
      base_currency: $("#op-currency").value.trim() || "CNY",
      start_period: /^\d{6}$/.test(start) ? parseInt(start, 10) : o.start_period,
      code_scheme: scheme.length ? scheme : o.code_scheme,
      voucher_words: words.length ? words : o.voucher_words,
      enable_audit: $("#op-audit").checked,
      enable_qty: $("#op-qty").checked,
      enable_foreign: $("#op-foreign").checked,
      require_cashier: $("#op-cashier").checked,
      budget_control: $("#op-budget").value,
      doc_prefixes: {
        po: $("#op-pfx-po").value.trim(), so: $("#op-pfx-so").value.trim(),
        req: $("#op-pfx-req").value.trim(), quo: $("#op-pfx-quo").value.trim(),
        prod: $("#op-pfx-prod").value.trim(),
      },
      biz_accounts: {
        ar: $("#op-biz-ar").value.trim() || "112201",
        ap: $("#op-biz-ap").value.trim() || "220201",
        income: $("#op-biz-income").value.trim() || "600101",
        tax_sales: $("#op-biz-tax").value.trim() || "22210102",
        material: $("#op-biz-mat").value.trim() || "140301",
        fund: $("#op-biz-fund").value.trim() || "100201",
      },
    });
    try { await api("/options", { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) }); toast("已保存账套参数", "ok"); }
    catch (e) { toast(e.message, "err"); }
  });
}

// ---------------- 操作日志 ----------------
async function viewLogs(main) {
  main.innerHTML = `
    <h2>操作日志</h2>
    <div class="toolbar">
      <input id="log-kw" placeholder="搜索关键字（用户 / 动作 / 详情）" style="width:220px" />
      <select id="log-limit">
        <option value="100">最近 100 条</option>
        <option value="200" selected>最近 200 条</option>
        <option value="500">最近 500 条</option>
        <option value="1000">最近 1000 条</option>
      </select>
      <button class="btn" id="log-query">查询</button>
    </div>
    <div class="panel" style="padding:0;overflow:auto;max-height:70vh">
      <table class="grid"><thead><tr><th>时间</th><th>用户</th><th>模块</th><th>动作</th><th>详情</th></tr></thead>
      <tbody id="log-rows"><tr><td colspan="5" class="muted" style="text-align:center;padding:18px">加载中…</td></tr></tbody></table>
    </div>`;
  async function load() {
    const q = $("#log-kw").value.trim();
    const limit = $("#log-limit").value;
    const rows = await api(`/logs?limit=${limit}${q ? `&q=${encodeURIComponent(q)}` : ""}`);
    $("#log-rows").innerHTML = rows.length ? rows.map((l) => `
      <tr>
        <td style="white-space:nowrap">${esc(l.ts)}</td>
        <td>${esc(l.user)}</td>
        <td><span class="tag">${esc(l.module)}</span></td>
        <td>${esc(l.action)}</td>
        <td class="muted">${esc(l.detail)}</td>
      </tr>`).join("") : `<tr><td colspan="5" class="muted" style="text-align:center;padding:18px">无日志</td></tr>`;
  }
  $("#log-query").addEventListener("click", () => load().catch((e) => toast(e.message, "err")));
  $("#log-kw").addEventListener("keydown", (e) => { if (e.key === "Enter") load().catch((err) => toast(err.message, "err")); });
  try { await load(); } catch (e) { $("#log-rows").innerHTML = `<tr><td colspan="5" style="color:var(--err);text-align:center;padding:18px">${esc(e.message)}</td></tr>`; }
}

// ---------------- 备份恢复 ----------------
async function viewBackup(main) {
  main.innerHTML = `
    <h2>备份恢复</h2>
    <div class="toolbar">
      <span class="muted">备份当前账套到服务器 backups 目录；恢复会先自动备份一次当前数据。</span>
      <div class="spacer"></div>
      ${can("backup") ? `<button class="btn primary" id="bk-new">立即备份</button>` : ""}
    </div>
    <div class="panel" style="padding:0;overflow:auto">
      <table class="grid"><thead><tr><th>备份文件</th><th class="num">大小</th><th>时间</th><th></th></tr></thead>
      <tbody id="bk-rows"><tr><td colspan="4" class="muted" style="text-align:center;padding:18px">加载中…</td></tr></tbody></table>
    </div>`;
  async function load() {
    const d = await api("/backups");
    const items = d.items || [];
    $("#bk-rows").innerHTML = items.length ? items.map((b) => {
      const kb = b.size / 1024;
      const size = kb > 1024 ? (kb / 1024).toFixed(2) + " MB" : kb.toFixed(1) + " KB";
      return `<tr>
        <td>${esc(b.name)}</td>
        <td class="num">${size}</td>
        <td class="muted">${esc(String(b.mtime).replace(/\.\d+ /, " "))}</td>
        <td class="row-actions">${can("backup") ? `<button class="btn sm ghost" data-restore="${esc(b.name)}">恢复</button>` : ""}</td>
      </tr>`;
    }).join("") : `<tr><td colspan="4" class="muted" style="text-align:center;padding:18px">暂无备份</td></tr>`;
    $all("[data-restore]", main).forEach((btn) => btn.addEventListener("click", async () => {
      const name = btn.dataset.restore;
      if (!(await confirmDialog(`确定从 ${name} 恢复账套？当前数据将先自动备份一份，但恢复后本账套将回到备份时点的状态。`, true))) return;
      try {
        await api("/restore", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ file: name }) });
        toast("恢复完成", "ok"); load();
      } catch (e) { toast(e.message, "err"); }
    }));
  }
  const mk = $("#bk-new");
  if (mk) mk.addEventListener("click", async () => {
    try { const r = await api("/backups", { method: "POST" }); toast(`已备份：${r.name}`, "ok"); load(); }
    catch (e) { toast(e.message, "err"); }
  });
  try { await load(); } catch (e) { $("#bk-rows").innerHTML = `<tr><td colspan="4" style="color:var(--err);text-align:center;padding:18px">${esc(e.message)}</td></tr>`; }
}

// ---------------- 凭证模板 ----------------
async function viewTemplates(main) {
  await ensureAccounts();
  main.innerHTML = `
    <h2>凭证模板</h2>
    <div class="toolbar">
      <button class="btn sm ${!state.tplTab || state.tplTab === "list" ? "primary" : "ghost"}" id="tpl-tab-list">模板列表</button>
      <button class="btn sm ${state.tplTab === "due" ? "primary" : "ghost"}" id="tpl-tab-due">本期到期</button>
      <div class="spacer"></div>
      ${can("voucher_new") && (!state.tplTab || state.tplTab === "list") ? `<button class="btn primary" id="tpl-new">新增模板</button>` : ""}
    </div>
    <div id="tpl-body" class="muted">加载中…</div>`;
  const body = $("#tpl-body");
  const tab = state.tplTab || "list";
  $("#tpl-tab-list").onclick = () => { state.tplTab = "list"; viewTemplates(main); };
  $("#tpl-tab-due").onclick = () => { state.tplTab = "due"; viewTemplates(main); };
  const refresh = () => rerenderView("templates", main);

  if (tab === "list") {
    let rows;
    try { rows = await api("/templates"); } catch (e) { body.innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; return; }
    body.className = "";
    body.innerHTML = `
      <div class="panel" style="padding:0;overflow:auto">
        <table class="grid"><thead><tr><th>名称</th><th class="num">分录数</th><th>频率</th><th>生效期间</th><th>上次生成</th><th>状态</th><th>备注</th><th></th></tr></thead>
        <tbody>
          ${rows.length ? rows.map((t) => `
            <tr>
              <td><b>${esc(t.name)}</b></td>
              <td class="num">${t.entries.length}</td>
              <td>${esc(FREQ_LABELS[t.freq] || t.freq)}</td>
              <td>${t.start_period || "—"} ~ ${t.end_period || "—"}</td>
              <td>${t.last_period || "—"}</td>
              <td>${t.active ? `<span class="tag ok">启用</span>` : `<span class="tag err">停用</span>`}</td>
              <td class="muted">${esc(t.memo)}</td>
              <td class="row-actions">
                ${can("voucher_new") ? `<button class="btn sm ghost" data-act="edit" data-id="${t.id}">编辑</button>` : ""}
                ${can("voucher_new") ? `<button class="btn sm ghost" data-act="gen" data-id="${t.id}">生成凭证</button>` : ""}
                ${can("voucher_new") ? `<button class="btn sm ghost" data-act="del" data-id="${t.id}">删除</button>` : ""}
              </td>
            </tr>`).join("") : `<tr><td colspan="8" class="muted" style="text-align:center;padding:18px">暂无模板，点「新增模板」创建</td></tr>`}
        </tbody></table>
      </div>`;
    $all("[data-act]", body).forEach((b) => b.addEventListener("click", async () => {
      const id = parseInt(b.dataset.id, 10);
      const t = rows.find((x) => x.id === id);
      if (b.dataset.act === "edit" && t) openTemplateEditor(refresh, t);
      else if (b.dataset.act === "gen" && t) await genVoucherFromTemplate(t);
      else if (b.dataset.act === "del") {
        if (!(await confirmDialog(`确定删除模板「${t.name}」？`, true))) return;
        try { await api(`/templates/${id}`, { method: "DELETE" }); toast("已删除", "ok"); refresh(); }
        catch (e) { toast(e.message, "err"); }
      }
    }));
    if ($("#tpl-new")) $("#tpl-new").addEventListener("click", () => openTemplateEditor(refresh, null));
  } else {
    const period = ymm(state.current || "") || ymm(today().slice(0, 7));
    let rows;
    try { rows = await api(`/templates/due?period=${period}`); } catch (e) { body.innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; return; }
    body.className = "";
    body.innerHTML = `
      <div class="muted" style="margin-bottom:8px">期间 ${period} 内应生成的模板（按频率与生效区间判断）：</div>
      <div class="panel" style="padding:0;overflow:auto">
        <table class="grid"><thead><tr><th>名称</th><th>频率</th><th class="num">分录数</th><th>上次生成</th><th></th></tr></thead>
        <tbody>
          ${rows.length ? rows.map((t) => `
            <tr>
              <td><b>${esc(t.name)}</b></td>
              <td>${esc(FREQ_LABELS[t.freq] || t.freq)}</td>
              <td class="num">${t.entries.length}</td>
              <td>${t.last_period || "从未"}</td>
              <td class="row-actions">${can("voucher_new") ? `<button class="btn sm primary" data-gen="${t.id}">生成凭证</button>` : ""}</td>
            </tr>`).join("") : `<tr><td colspan="5" class="muted" style="text-align:center;padding:18px">本期无到期模板</td></tr>`}
        </tbody></table>
      </div>`;
    $all("[data-gen]", body).forEach((b) => b.addEventListener("click", async () => {
      const t = rows.find((x) => x.id === parseInt(b.dataset.gen, 10));
      if (t) { await generateTemplateVoucher(t, period); refresh(); }
    }));
  }
}

// 按模板直接生成凭证并回写 last_period（走后端 generate，用于「本期到期」闭环）
async function generateTemplateVoucher(t, period) {
  if (!t.entries.length) { toast("模板没有分录", "err"); return null; }
  if (!(await confirmDialog(`按模板「${t.name}」生成 ${period} 的记账凭证？金额为空的科目将按 0 记账。`))) return null;
  try {
    const r = await api(`/templates/${t.id}/generate`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ period: ymm(period), date: "" }) });
    toast(`已生成凭证 #${r.id}`, "ok");
    return r;
  } catch (e) { toast(e.message, "err"); return null; }
}

// 由模板打开凭证编辑器（空金额分录预填为 0，可在编辑器内补全）——用于手工调用场景
async function genVoucherFromTemplate(t) {
  if (!t.entries.length) { toast("模板没有分录", "err"); return; }
  const unPriced = t.entries.filter((e) => !String(e.amount || "").trim());
  if (unPriced.length) {
    if (!(await confirmDialog(`模板有 ${unPriced.length} 条分录未填金额，将预填为 0，打开凭证编辑器后请补全。继续？`))) return;
  }
  await openVoucherEditor(null, t.entries);
}

function openTemplateEditor(refresh, t) {
  const isEdit = !!t;
  const tpl = t || { id: 0, name: "", memo: "", entries: [{ summary: "", account_code: "", dir: "debit", amount: "", aux: {} }], freq: "manual", start_period: null, end_period: null, last_period: null, active: true };
  const mask = modal(`
    <h3>${isEdit ? "编辑模板" : "新增模板"}</h3>
    <div class="field"><label>模板名称 *</label><input id="tp-name" value="${esc(tpl.name)}" /></div>
    <div class="field"><label>频率</label>
      <select id="tp-freq">
        <option value="manual" ${tpl.freq === "manual" ? "selected" : ""}>手工调用（录凭证时选用）</option>
        <option value="monthly" ${tpl.freq === "monthly" ? "selected" : ""}>每月生成</option>
        <option value="quarterly" ${tpl.freq === "quarterly" ? "selected" : ""}>每季生成</option>
        <option value="yearly" ${tpl.freq === "yearly" ? "selected" : ""}>每年生成</option>
      </select>
    </div>
    <div class="field" style="display:flex;gap:12px">
      <div><label>生效起始期间（YYYYMM）</label><input id="tp-start" value="${tpl.start_period || ""}" style="width:110px" /></div>
      <div><label>生效结束期间</label><input id="tp-end" value="${tpl.end_period || ""}" style="width:110px" /></div>
      <div style="align-self:flex-end"><label style="display:inline-flex;align-items:center;gap:4px"><input type="checkbox" id="tp-active" ${tpl.active ? "checked" : ""} />启用</label></div>
    </div>
    <div class="field"><label>备注</label><input id="tp-memo" value="${esc(tpl.memo)}" /></div>
    <div class="field">
      <label>分录（金额留空 = 生成时待填）</label>
      <table class="grid" id="tp-entries"><thead><tr><th>摘要</th><th>科目</th><th>方向</th><th>金额</th><th></th></tr></thead><tbody></tbody></table>
      <button class="btn ghost sm" id="tp-add" style="margin-top:6px">加分录</button>
    </div>
    <div class="foot">
      <button class="btn ghost" id="tp-cancel">取消</button>
      <button class="btn primary" id="tp-save">保存</button>
    </div>
  `, true);
  const tbody = $("#tp-entries tbody", mask);
  function renderRows() {
    tbody.innerHTML = tpl.entries.map((e, i) => `
      <tr>
        <td><input class="te-sum" data-i="${i}" value="${esc(e.summary)}" style="width:100%" /></td>
        <td>${accountOptions()}</td>
        <td><select class="te-dir" data-i="${i}"><option value="debit" ${e.dir !== "credit" ? "selected" : ""}>借</option><option value="credit" ${e.dir === "credit" ? "selected" : ""}>贷</option></select></td>
        <td><input class="te-amt" data-i="${i}" value="${esc(e.amount)}" style="width:100px;text-align:right" /></td>
        <td><button class="btn ghost sm" data-rm="${i}">×</button></td>
      </tr>`).join("");
    $all("select.acct-sel", tbody).forEach((sel, i) => { sel.value = tpl.entries[i].account_code; sel.onchange = () => tpl.entries[i].account_code = sel.value; });
    $all(".te-sum", tbody).forEach((inp) => inp.oninput = () => tpl.entries[+inp.dataset.i].summary = inp.value);
    $all(".te-dir", tbody).forEach((sel) => sel.onchange = () => tpl.entries[+sel.dataset.i].dir = sel.value);
    $all(".te-amt", tbody).forEach((inp) => inp.oninput = () => tpl.entries[+inp.dataset.i].amount = inp.value.trim());
    $all("[data-rm]", tbody).forEach((b) => b.onclick = () => { tpl.entries.splice(+b.dataset.rm, 1); renderRows(); });
  }
  renderRows();
  $("#tp-add", mask).onclick = () => { tpl.entries.push({ summary: "", account_code: "", dir: "debit", amount: "", aux: {} }); renderRows(); };
  $("#tp-cancel", mask).addEventListener("click", closeModal);
  $("#tp-save", mask).addEventListener("click", async () => {
    const name = $("#tp-name", mask).value.trim();
    if (!name) { toast("模板名称必填", "err"); return; }
    if (tpl.entries.some((e) => !e.account_code)) { toast("每条分录都要选科目", "err"); return; }
    const start = $("#tp-start", mask).value.trim();
    const end = $("#tp-end", mask).value.trim();
    const body = Object.assign({}, tpl, {
      id: isEdit ? tpl.id : 0,
      name,
      freq: $("#tp-freq", mask).value,
      start_period: /^\d{6}$/.test(start) ? parseInt(start, 10) : null,
      end_period: /^\d{6}$/.test(end) ? parseInt(end, 10) : null,
      active: $("#tp-active", mask).checked,
      memo: $("#tp-memo", mask).value.trim(),
    });
    try {
      if (isEdit) await api(`/templates/${tpl.id}`, { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
      else await api("/templates", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
      toast("已保存模板", "ok"); closeModal(); if (refresh) refresh();
    } catch (e) { toast(e.message, "err"); }
  });
}

// ===========================================================================
// 工资管理（对齐桌面端：工资表 / 个税明细 / 凭证生成）
// ===========================================================================
function nextPeriod(p) {
  const [y, m] = String(p).split("-").map(Number);
  const nm = m + 1 > 12 ? 1 : m + 1, ny = m + 1 > 12 ? y + 1 : y;
  return `${ny}-${String(nm).padStart(2, "0")}`;
}
const CLAIM_STATUS = [
  { code: "", label: "全部状态" },
  { code: "draft", label: "草稿" },
  { code: "submitted", label: "待审批" },
  { code: "approved", label: "已批准" },
  { code: "rejected", label: "已驳回" },
  { code: "paid", label: "已付款" },
];

async function viewPayroll(main) {
  const tab = state.payTab || "sheet";
  const period = state.payPeriod || state.current || today().slice(0, 7);
  main.innerHTML = `
    <h2>工资管理</h2>
    <div class="toolbar">
      <button class="btn sm ${tab === "sheet" ? "primary" : "ghost"}" id="py-tab-sheet">工资表</button>
      <button class="btn sm ${tab === "tax" ? "primary" : "ghost"}" id="py-tab-tax">个税明细</button>
      <button class="btn sm ${tab === "voucher" ? "primary" : "ghost"}" id="py-tab-voucher">凭证生成</button>
      <div class="spacer"></div>
      <button class="btn ghost sm" id="py-prev">◀ 上期</button>
      <label>期间 <input id="py-period" value="${esc(period)}" style="width:90px" /></label>
      <button class="btn ghost sm" id="py-next">下期 ▶</button>
      <button class="btn" id="py-refresh">刷新</button>
      ${can("export") ? `<button class="btn ghost sm" id="py-export">导出 CSV</button>` : ""}
      ${can("export") ? `<button class="btn ghost sm" id="py-bank">银行代发</button>` : ""}
      <button class="btn ghost sm" id="py-slip">工资条</button>
      <button class="btn ghost sm" id="py-taxrep">个税申报表</button>
    </div>
    <div id="py-body" class="muted">加载中…</div>`;
  const body = $("#py-body");
  const switchTab = (t) => { state.payTab = t; viewPayroll(main); };
  const switchPeriod = (p) => { state.payPeriod = p; viewPayroll(main); };
  $("#py-tab-sheet").onclick = () => switchTab("sheet");
  $("#py-tab-tax").onclick = () => switchTab("tax");
  $("#py-tab-voucher").onclick = () => switchTab("voucher");
  $("#py-prev").onclick = () => switchPeriod(prevPeriod(period));
  $("#py-next").onclick = () => switchPeriod(nextPeriod(period));
  $("#py-refresh").onclick = () => switchPeriod($("#py-period").value.trim() || period);
  $("#py-period").addEventListener("keydown", (e) => { if (e.key === "Enter") switchPeriod($("#py-period").value.trim() || period); });
  if ($("#py-export")) $("#py-export").onclick = () => window.open(`/api/export/payroll?period=${encodeURIComponent(period)}`, "_blank");
  if ($("#py-bank")) $("#py-bank").onclick = () => window.open(`/api/payroll/bank-file?period=${encodeURIComponent(ymm(period))}`, "_blank");
  // 工资条：单人本期 + 本年累计（打印预览）
  $("#py-slip").onclick = () => {
    const m = modal(`<h3>工资条</h3>
      <div class="field"><label>员工编码 *</label><input id="ps-emp" placeholder="如 E001" /></div>
      <div id="ps-out" class="muted">输入员工编码后查询</div>
      <div class="foot"><button class="btn primary" id="ps-load">查询</button><button class="btn ghost" id="ps-print">打印预览</button><button class="btn ghost" id="ps-close">关闭</button></div>`);
    $("#ps-close", m).onclick = closeModal;
    $("#ps-load", m).onclick = async () => {
      const emp = $("#ps-emp", m).value.trim();
      if (!emp) { toast("请输入员工编码", "err"); return; }
      try {
        const r = await api(`/payroll/slip?period=${encodeURIComponent(ymm(period))}&employee=${encodeURIComponent(emp)}`);
        const p = r.payroll, y = r.ytd;
        $("#ps-out", m).innerHTML = `<table class="grid" id="ps-table"><tbody>
          <tr><td>员工</td><td>${esc(p.employee)} ${esc(p.dept || "")}</td><td>期间</td><td>${esc(r.period)}</td></tr>
          <tr><td>应发</td><td>${fmt(p.gross)}</td><td>社保(个人)</td><td>${fmt(p.social)}</td></tr>
          <tr><td>公积金(个人)</td><td>${fmt(p.housing)}</td><td>其他扣除</td><td>${fmt(p.deduction)}</td></tr>
          <tr><td>专项附加</td><td>${fmt(p.additional)}</td><td>计税基数</td><td>${fmt(p.tax_base)}</td></tr>
          <tr><td>个税</td><td>${fmt(p.tax)}</td><td><b>实发</b></td><td><b>${fmt(p.net)}</b></td></tr>
          <tr><td>单位社保</td><td>${fmt(p.social_co)}</td><td>单位公积金</td><td>${fmt(p.housing_co)}</td></tr>
          <tr><td>本年累计收入</td><td>${fmt(y.income)}</td><td>累计已预扣个税</td><td>${fmt(y.withheld)}（${y.months} 个月）</td></tr>
        </tbody></table>`;
      } catch (e) { $("#ps-out", m).innerHTML = `<span style="color:var(--err)">${esc(e.message)}</span>`; }
    };
    $("#ps-print", m).onclick = () => { const t = $("#ps-table", m); if (!t) { toast("先查询", "err"); return; } printPreview("工资条", t); };
  };
  // 个税申报表（全员工资薪金本期口径，打印预览）
  $("#py-taxrep").onclick = async () => {
    try {
      const r = await api(`/payroll/tax-report?period=${encodeURIComponent(ymm(period))}`);
      const trows = r.rows || [];
      const m = modal(`<h3>个税申报表（${esc(r.period)}）</h3>
        <table class="grid" id="tr-table"><thead><tr><th>员工</th><th>部门</th><th class="num">本期收入</th><th class="num">专项扣除</th><th class="num">专项附加</th><th class="num">计税基数</th><th class="num">个税</th><th class="num">实发</th></tr></thead>
        <tbody>${trows.length ? trows.map((x) => `<tr><td>${esc(x.employee)}</td><td>${esc(x.dept || "")}</td><td class="num">${fmt(x.income)}</td><td class="num">${fmt(x.special)}</td><td class="num">${fmt(x.additional)}</td><td class="num">${fmt(x.tax_base)}</td><td class="num">${fmt(x.tax)}</td><td class="num">${fmt(x.net)}</td></tr>`).join("") : `<tr><td colspan="8" class="muted">本期无工资记录</td></tr>`}</tbody></table>
        <div class="foot"><button class="btn ghost" id="tr-print">打印预览</button><button class="btn ghost" id="tr-close">关闭</button></div>`);
      $("#tr-close", m).onclick = closeModal;
      $("#tr-print", m).onclick = () => printPreview("个税申报表", $("#tr-table", m));
    } catch (e) { toast(e.message, "err"); }
  };

  const ymm6 = ymm(period);
  let rows = [], employees = [];
  try {
    [rows, employees] = await Promise.all([
      api(`/payroll?period=${ymm6}`),
      api("/aux?kind=employee").catch(() => []),
    ]);
  } catch (e) { body.innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; return; }
  const empName = (code) => { const e = employees.find((x) => x.code === code); return e ? e.name : ""; };
  const voucherLink = (id) => (id ? `<a href="#" data-voucher="${id}">凭证 #${id}</a>` : "—");

  if (tab === "sheet") {
    body.className = "";
    const total = (k) => rows.reduce((s, r) => s + (parseFloat(r[k]) || 0), 0);
    body.innerHTML = `
      <div class="toolbar">
        <span class="muted">共 ${rows.length} 人 · 个税与实发由后端按累计预扣预缴法自动计算</span>
        <div class="spacer"></div>
        ${can("voucher_new") ? `<button class="btn primary" id="py-new">新增工资行</button>` : ""}
      </div>
      <div class="panel" style="padding:0;overflow:auto;max-height:58vh">
        <table class="grid">
          <thead><tr><th>员工</th><th>部门</th><th class="num">应发</th><th class="num">社保(个人)</th><th class="num">公积金(个人)</th><th class="num">其他扣除</th><th class="num">专项附加</th><th class="num">计税基数</th><th class="num">个税</th><th class="num">实发</th><th class="num">单位社保</th><th class="num">单位公积金</th><th>凭证</th><th></th></tr></thead>
          <tbody>
            ${rows.length ? rows.map((r) => `
              <tr>
                <td title="${esc(r.employee)}">${esc(empName(r.employee) || r.employee)}</td>
                <td>${esc(r.dept) || "—"}</td>
                <td class="num">${fmt(r.gross)}</td><td class="num">${fmt(r.social)}</td>
                <td class="num">${fmt(r.housing)}</td><td class="num">${fmt(r.deduction)}</td>
                <td class="num">${fmt(r.additional)}</td><td class="num">${fmt(r.tax_base)}</td>
                <td class="num">${fmt(r.tax)}</td><td class="num"><b>${fmt(r.net)}</b></td>
                <td class="num">${fmt(r.social_co)}</td><td class="num">${fmt(r.housing_co)}</td>
                <td>${[["计提", r.voucher_id], ["社保", r.social_voucher_id], ["发放", r.paid_voucher_id]].filter(([, id]) => id).map(([k, id]) => `<a href="#" data-voucher="${id}">${k}#${id}</a>`).join(" ") || "—"}</td>
                <td class="row-actions">
                  ${can("voucher_new") && !r.voucher_id ? `<button class="btn sm ghost" data-edit="${r.id}">改</button>` : ""}
                  ${can("voucher_new") && !r.voucher_id ? `<button class="btn sm ghost" data-del="${r.id}">删</button>` : ""}
                </td>
              </tr>`).join("") : `<tr><td colspan="14" class="muted" style="text-align:center;padding:18px">${period} 无工资数据</td></tr>`}
          </tbody>
        </table>
      </div>
      ${rows.length ? `<div class="cards" style="margin-top:12px">
        <div class="card"><div class="k">应发合计</div><div class="v" style="font-size:16px">${fmt(total("gross").toFixed(2))}</div></div>
        <div class="card"><div class="k">个税合计</div><div class="v" style="font-size:16px">${fmt(total("tax").toFixed(2))}</div></div>
        <div class="card"><div class="k">实发合计</div><div class="v" style="font-size:16px">${fmt(total("net").toFixed(2))}</div></div>
        <div class="card"><div class="k">单位社保+公积金</div><div class="v" style="font-size:16px">${fmt((total("social_co") + total("housing_co")).toFixed(2))}</div></div>
      </div>` : ""}`;
    $all("[data-voucher]", body).forEach((a) => a.addEventListener("click", (e) => { e.preventDefault(); openVoucherEditor(parseInt(a.dataset.voucher, 10)); }));
    const newBtn = $("#py-new");
    if (newBtn) newBtn.addEventListener("click", () => openPayrollEditor(main, null, period, employees));
    $all("[data-edit]", body).forEach((b) => b.addEventListener("click", () => {
      const r = rows.find((x) => x.id === parseInt(b.dataset.edit, 10));
      if (r) openPayrollEditor(main, r, period, employees);
    }));
    $all("[data-del]", body).forEach((b) => b.addEventListener("click", async () => {
      if (!(await confirmDialog("确定删除该工资行？", true))) return;
      try { await api(`/payroll/${b.dataset.del}`, { method: "DELETE" }); toast("已删除", "ok"); rerenderView("payroll", main); }
      catch (e) { toast(e.message, "err"); }
    }));
  } else if (tab === "tax") {
    body.className = "";
    body.innerHTML = `
      <div class="toolbar">
        <label>员工 <select id="py-emp"><option value="">选择职员…</option>${employees.map((e) => `<option value="${esc(e.code)}">${esc(e.code)} ${esc(e.name)}</option>`).join("")}</select></label>
        <button class="btn" id="py-tax-run">查询累计</button>
      </div>
      <div id="py-tax-box" class="muted">选择员工后查看本年至上月的累计数与本期的个税计算。</div>`;
    $("#py-tax-run").addEventListener("click", async () => {
      const code = $("#py-emp").value;
      if (!code) { toast("请选择员工", "err"); return; }
      const box = $("#py-tax-box");
      try {
        const cur = rows.find((r) => r.employee === code);
        if (!cur) { box.innerHTML = `<div class="muted">${esc(empName(code) || code)} 在 ${period} 没有工资数据，请先在「工资表」录入。</div>`; return; }
        const ytd = await api(`/payroll/ytd?period=${ymm6}&employee=${encodeURIComponent(code)}`);
        box.innerHTML = `
          <div class="cards" style="margin-top:12px">
            <div class="card"><div class="k">累计收入（本年至上月）</div><div class="v" style="font-size:16px">${fmt(ytd.income)}</div></div>
            <div class="card"><div class="k">累计专项扣除（社保+公积金）</div><div class="v" style="font-size:16px">${fmt(ytd.special)}</div></div>
            <div class="card"><div class="k">累计专项附加扣除</div><div class="v" style="font-size:16px">${fmt(ytd.additional)}</div></div>
            <div class="card"><div class="k">累计已预扣个税</div><div class="v" style="font-size:16px">${fmt(ytd.withheld)}</div></div>
            <div class="card"><div class="k">已有月数</div><div class="v" style="font-size:16px">${ytd.months}</div></div>
          </div>
          <div class="cards" style="margin-top:12px">
            <div class="card"><div class="k">本期应发</div><div class="v" style="font-size:16px">${fmt(cur.gross)}</div></div>
            <div class="card"><div class="k">本期计税基数</div><div class="v" style="font-size:16px">${fmt(cur.tax_base)}</div></div>
            <div class="card"><div class="k">本期个税</div><div class="v" style="font-size:16px">${fmt(cur.tax)}</div></div>
            <div class="card"><div class="k">本期实发</div><div class="v" style="font-size:16px">${fmt(cur.net)}</div></div>
          </div>`;
      } catch (e) { box.innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
    });
  } else {
    // 凭证生成：默认科目与桌面端 finui VoucherCfg 一致
    body.className = "";
    body.innerHTML = `
      <div class="panel" style="max-width:660px">
        <div class="field" style="display:flex;gap:12px;flex-wrap:wrap">
          <div><label>凭证日期（留空 = 期间末日）</label><input id="pv-date" type="date" value="" style="width:150px" /></div>
        </div>
        <div class="field" style="display:flex;gap:12px;flex-wrap:wrap">
          <div><label>费用科目</label><input id="pv-expense" value="660201" style="width:90px" /></div>
          <div><label>应付工资</label><input id="pv-wage" value="221101" style="width:90px" /></div>
          <div><label>应付社保</label><input id="pv-social" value="221103" style="width:90px" /></div>
          <div><label>应付公积金</label><input id="pv-housing" value="221104" style="width:90px" /></div>
          <div><label>其他应付款(个人)</label><input id="pv-personal" value="2241" style="width:90px" /></div>
          <div><label>银行存款</label><input id="pv-bank" value="100201" style="width:90px" /></div>
          <div><label>应交个税</label><input id="pv-tax" value="222107" style="width:90px" /></div>
        </div>
        <p class="muted" style="font-size:12px">计提凭证按部门拆分借方费用；社保缴纳与工资发放凭证走银行存款；同一期同类型凭证只生成一次，重复点击幂等返回同一张。</p>
        ${rows.length ? `<div class="muted" style="font-size:12px;margin-bottom:4px">本期凭证状态：计提 ${rows[0].voucher_id ? `<a href="#" data-voucher="${rows[0].voucher_id}">#${rows[0].voucher_id}</a>` : "未生成"} · 社保缴纳 ${rows[0].social_voucher_id ? `<a href="#" data-voucher="${rows[0].social_voucher_id}">#${rows[0].social_voucher_id}</a>` : "未生成"} · 发放 ${rows[0].paid_voucher_id ? `<a href="#" data-voucher="${rows[0].paid_voucher_id}">#${rows[0].paid_voucher_id}</a>` : "未生成"}</div>` : ""}
        <div class="foot" style="margin-top:6px;display:flex;gap:10px">
          ${rows.length ? `
          <button class="btn primary" id="pv-accrue">生成计提凭证</button>
          <button class="btn" id="pv-social">生成社保缴纳凭证</button>
          <button class="btn" id="pv-pay">生成发放凭证</button>` : `<span class="muted">本期无工资数据，先在「工资表」录入。</span>`}
        </div>
        <div id="pv-result" class="muted" style="margin-top:10px"></div>
      </div>`;
    const q = `period=${ymm6}`;
    const runVoucher = (url, bodyObj, label) => (async () => {
      if (!(await confirmDialog(`确认为 ${period} 生成${label}？`))) return;
      try {
        const r = await api(`${url}?${q}`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(bodyObj) });
        $("#pv-result").innerHTML = r.id ? `<span style="color:var(--ok)">已生成凭证 #${r.id}，<a href="#" id="pv-open">点击查看</a></span>` : `<span class="muted">无可生成内容（金额全为零）</span>`;
        if (r.id) $("#pv-open").addEventListener("click", (e) => { e.preventDefault(); openVoucherEditor(r.id); });
      } catch (e) { toast(e.message, "err"); }
    });
    if ($("#pv-accrue")) $("#pv-accrue").onclick = runVoucher("/payroll/accrue", { date: $("#pv-date").value, expense: $("#pv-expense").value.trim(), wage_payable: $("#pv-wage").value.trim(), social_payable: $("#pv-social").value.trim(), housing_payable: $("#pv-housing").value.trim() }, "计提凭证");
    if ($("#pv-social")) $("#pv-social").onclick = runVoucher("/payroll/social-pay", { date: $("#pv-date").value, social_payable: $("#pv-social").value.trim(), housing_payable: $("#pv-housing").value.trim(), personal_payable: $("#pv-personal").value.trim(), bank_account: $("#pv-bank").value.trim() }, "社保缴纳凭证");
    if ($("#pv-pay")) $("#pv-pay").onclick = runVoucher("/payroll/pay", { date: $("#pv-date").value, payable_account: $("#pv-wage").value.trim(), bank_account: $("#pv-bank").value.trim(), tax_account: $("#pv-tax").value.trim(), social_account: $("#pv-personal").value.trim() }, "发放凭证");
    $all("[data-voucher]", body).forEach((a) => a.addEventListener("click", (e) => { e.preventDefault(); openVoucherEditor(parseInt(a.dataset.voucher, 10)); }));
  }
}

function openPayrollEditor(main, row, period, employees) {
  const isEdit = !!row;
  const r = row || { employee: "", dept: "", gross: "", social: "", housing: "", deduction: "", additional: "", social_co: "", housing_co: "", memo: "" };
  const empKnown = r.employee && employees.some((e) => e.code === r.employee);
  // 只渲染一份选项：已知员工在列表里（编辑时）不再额外加裸编码项，避免重复
  const empOpts = employees.map((e) => `<option value="${esc(e.code)}" ${r.employee === e.code ? "selected" : ""}>${esc(e.code)} ${esc(e.name)}</option>`).join("")
    + (r.employee && !empKnown ? `<option value="${esc(r.employee)}" selected>${esc(r.employee)}（档案外）</option>` : "");
  const mask = modal(`
    <h3>${isEdit ? `编辑工资行（${esc(empNameIn(employees, r.employee) || r.employee)}）` : "新增工资行"} · ${esc(period)}</h3>
    <div class="field" style="display:flex;gap:12px;flex-wrap:wrap">
      <div><label>员工 *</label>${employees.length ? `<select id="pw-emp"><option value="">选择职员…</option>${empOpts}</select>` : `<input id="pw-emp" value="${esc(r.employee)}" placeholder="职员编码" />`}</div>
      <div><label>部门</label><input id="pw-dept" value="${esc(r.dept)}" style="width:110px" /></div>
    </div>
    <div class="field" style="display:flex;gap:12px;flex-wrap:wrap">
      <div><label>应发工资 *</label><input id="pw-gross" value="${esc(r.gross)}" style="width:110px;text-align:right" /></div>
      <div><label>社保(个人)</label><input id="pw-social" value="${esc(r.social)}" style="width:100px;text-align:right" /></div>
      <div><label>公积金(个人)</label><input id="pw-housing" value="${esc(r.housing)}" style="width:100px;text-align:right" /></div>
      <div><label>其他扣除</label><input id="pw-ded" value="${esc(r.deduction)}" style="width:100px;text-align:right" /></div>
      <div><label>专项附加扣除</label><input id="pw-add" value="${esc(r.additional)}" style="width:110px;text-align:right" /></div>
    </div>
    <div class="field" style="display:flex;gap:12px;flex-wrap:wrap">
      <div><label>单位社保</label><input id="pw-sco" value="${esc(r.social_co)}" style="width:100px;text-align:right" /></div>
      <div><label>单位公积金</label><input id="pw-hco" value="${esc(r.housing_co)}" style="width:100px;text-align:right" /></div>
      <div><label>备注</label><input id="pw-memo" value="${esc(r.memo)}" style="width:220px" /></div>
    </div>
    <p class="muted" style="font-size:12px">个税按累计预扣预缴法自动计算，实发 = 应发 − 社保 − 公积金 − 其他扣除 − 个税。</p>
    <div class="foot">
      <button class="btn ghost" id="pw-cancel">取消</button>
      <button class="btn primary" id="pw-save">计算并保存</button>
    </div>
  `);
  $("#pw-cancel", mask).addEventListener("click", closeModal);
  $("#pw-save", mask).addEventListener("click", async () => {
    const empSel = $("#pw-emp", mask);
    const employee = (empSel.value || "").trim();
    if (!employee) { toast("员工必填", "err"); return; }
    const body = {
      employee,
      dept: $("#pw-dept", mask).value.trim(),
      gross: $("#pw-gross", mask).value.trim() || "0",
      social: $("#pw-social", mask).value.trim() || "0",
      housing: $("#pw-housing", mask).value.trim() || "0",
      deduction: $("#pw-ded", mask).value.trim() || "0",
      additional: $("#pw-add", mask).value.trim() || "0",
      social_co: $("#pw-sco", mask).value.trim() || "0",
      housing_co: $("#pw-hco", mask).value.trim() || "0",
      memo: $("#pw-memo", mask).value.trim(),
    };
    try {
      const out = await api(`/payroll?period=${ymm(period)}`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
      toast(`已保存：个税 ${fmt(out.tax)}，实发 ${fmt(out.net)}`, "ok");
      closeModal(); rerenderView("payroll", main);
    } catch (e) { toast(e.message, "err"); }
  });
}
function empNameIn(employees, code) { const e = (employees || []).find((x) => x.code === code); return e ? e.name : ""; }

// ===========================================================================
// 费用报销（草稿 → 提交 → 审批 → 支付 → 生成凭证）
// ===========================================================================
async function viewClaims(main) {
  await ensureAccounts();
  const period = state.clmPeriod || state.current || today().slice(0, 7);
  const status = state.clmStatus == null ? "" : state.clmStatus;
  main.innerHTML = `
    <h2>费用报销</h2>
    <div class="toolbar">
      <button class="btn ghost sm" id="cl-prev">◀ 上期</button>
      <label>期间 <input id="cl-period" value="${esc(period)}" style="width:90px" /></label>
      <button class="btn ghost sm" id="cl-next">下期 ▶</button>
      <label>状态 <select id="cl-status">${CLAIM_STATUS.map((s) => `<option value="${s.code}" ${status === s.code ? "selected" : ""}>${s.label}</option>`).join("")}</select></label>
      <button class="btn" id="cl-refresh">刷新</button>
      <div class="spacer"></div>
      ${can("export") ? `<button class="btn ghost sm" id="cl-export">导出 CSV</button>` : ""}
      ${can("voucher_new") ? `<button class="btn primary" id="cl-new">新增报销单</button>` : ""}
    </div>
    <div id="cl-body" class="muted">加载中…</div>`;
  const switchTo = (p, s) => { state.clmPeriod = p; state.clmStatus = s; viewClaims(main); };
  $("#cl-prev").onclick = () => switchTo(prevPeriod(period), status);
  $("#cl-next").onclick = () => switchTo(nextPeriod(period), status);
  $("#cl-refresh").onclick = () => switchTo($("#cl-period").value.trim() || period, $("#cl-status").value);
  if ($("#cl-export")) $("#cl-export").onclick = () => {
    const qs = new URLSearchParams({ period, status: $("#cl-status").value });
    window.open(`/api/export/claims?${qs.toString()}`, "_blank");
  };
  if ($("#cl-new")) $("#cl-new").addEventListener("click", () => openClaimEditor(main, null, period));

  const body = $("#cl-body");
  let rows;
  try {
    const q = `period=${ymm(period)}${status ? `&status=${status}` : ""}`;
    rows = await api(`/claims?${q}`);
  } catch (e) { body.innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; return; }
  body.className = "";
  const badge = (s) => {
    const map = { draft: ["tag", "草稿"], submitted: ["tag warn", "待审批"], approved: ["tag ok", "已批准"], rejected: ["tag err", "已驳回"], paid: ["tag ok", "已付款"] };
    const [cls, label] = map[s] || ["tag", s];
    return `<span class="${cls}">${label}</span>`;
  };
  // 状态流转动作（与桌面端 actions() 一致）
  const actionsOf = (r) => {
    switch (r.status) {
      case "draft": return [["提交", "submitted"]];
      case "submitted": return [["审批通过", "approved"], ["驳回", "rejected"]];
      case "approved": return [["支付", "paid"]];
      case "rejected": return [["退回草稿", "draft"]];
      default: return [];
    }
  };
  const total = rows.reduce((s, r) => s + (parseFloat(r.amount) || 0), 0);
  body.innerHTML = `
    <div class="panel" style="padding:0;overflow:auto;max-height:62vh">
      <table class="grid">
        <thead><tr><th>单号</th><th>日期</th><th>申请人</th><th>部门</th><th>事由</th><th class="num">金额</th><th>状态</th><th>凭证</th><th></th></tr></thead>
        <tbody>
          ${rows.length ? rows.map((r) => `
            <tr>
              <td><a href="#" data-view="${r.id}"><b>${esc(r.no)}</b></a></td>
              <td>${esc(r.biz_date)}</td>
              <td>${esc(r.applicant)}</td>
              <td>${esc(r.dept) || "—"}</td>
              <td>${esc(r.reason)}</td>
              <td class="num">${fmt(r.amount)}</td>
              <td>${badge(r.status)}</td>
              <td>${r.voucher_id ? `<a href="#" data-voucher="${r.voucher_id}">#${r.voucher_id}</a>` : "—"}</td>
              <td class="row-actions">
                ${can("voucher_new") && ["draft", "rejected"].includes(r.status) && !r.voucher_id ? `<button class="btn sm ghost" data-edit="${r.id}">改</button><button class="btn sm ghost" data-del="${r.id}">删</button>` : ""}
                ${can("voucher_new") ? actionsOf(r).map(([label, to]) => `<button class="btn sm ghost" data-trans="${r.id}" data-to="${to}">${label}</button>`).join("") : ""}
                ${can("voucher_new") && r.status === "paid" && !r.voucher_id ? `<button class="btn sm primary" data-vgen="${r.id}">生成凭证</button>` : ""}
              </td>
            </tr>`).join("") : `<tr><td colspan="9" class="muted" style="text-align:center;padding:18px">${period} 无报销单</td></tr>`}
        </tbody>
      </table>
    </div>
    ${rows.length ? `<div class="muted" style="margin-top:10px">本期报销金额合计 <b>${fmt(total.toFixed(2))}</b></div>` : ""}`;
  const reload = () => rerenderView("claims", main);
  $all("[data-view]", body).forEach((a) => a.addEventListener("click", (e) => {
    e.preventDefault();
    const r = rows.find((x) => x.id === parseInt(a.dataset.view, 10));
    if (r) showClaimDetail(r);
  }));
  $all("[data-voucher]", body).forEach((a) => a.addEventListener("click", (e) => { e.preventDefault(); openVoucherEditor(parseInt(a.dataset.voucher, 10)); }));
  $all("[data-edit]", body).forEach((b) => b.addEventListener("click", () => {
    const r = rows.find((x) => x.id === parseInt(b.dataset.edit, 10));
    if (r) openClaimEditor(main, r, period);
  }));
  $all("[data-del]", body).forEach((b) => b.addEventListener("click", async () => {
    if (!(await confirmDialog("删除后不可恢复（已生成凭证的单据需先删除凭证），确定删除？", true))) return;
    try { await api(`/claims/${b.dataset.del}`, { method: "DELETE" }); toast("已删除", "ok"); reload(); }
    catch (e) { toast(e.message, "err"); }
  }));
  $all("[data-trans]", body).forEach((b) => b.addEventListener("click", async () => {
    const to = b.dataset.to;
    try { const out = await api(`/claims/${b.dataset.trans}/transition`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ status: to }) }); toast(out && out.pending ? `已审批 → 下一节点：${out.pending}` : "状态已更新", "ok"); reload(); }
    catch (e) { toast(e.message, "err"); }
  }));
  $all("[data-vgen]", body).forEach((b) => b.addEventListener("click", () => {
    const r = rows.find((x) => x.id === parseInt(b.dataset.vgen, 10));
    if (!r) return;
    const mask = modal(`
      <h3>生成报销凭证 · ${esc(r.no)}</h3>
      <div class="field"><label>贷方支付科目（如 100201 银行存款）*</label><input id="cv-pay" value="100201" /></div>
      <p class="muted" style="font-size:12px">借方按明细行的费用科目拆分；明细合计必须等于单据金额。</p>
      <div class="foot">
        <button class="btn ghost" id="cv-cancel">取消</button>
        <button class="btn primary" id="cv-ok">生成</button>
      </div>`);
    $("#cv-cancel", mask).onclick = closeModal;
    $("#cv-ok", mask).onclick = async () => {
      const pay = $("#cv-pay", mask).value.trim();
      if (!pay) { toast("请填写支付科目", "err"); return; }
      try {
        const out = await api(`/claims/${r.id}/voucher`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ pay_account: pay }) });
        toast(`已生成凭证 #${out.id}`, "ok"); closeModal(); reload();
      } catch (e) { toast(e.message, "err"); }
    };
  }));
}

function showClaimDetail(c) {
  const stLabel = (CLAIM_STATUS.find((s) => s.code === c.status) || {}).label || c.status;
  const mask = modal(`
    <h3>报销单 ${esc(c.no)}</h3>
    <div class="muted" style="line-height:1.9;font-size:13px">
      日期：${esc(c.biz_date)} · 申请人：${esc(c.applicant)}${c.dept ? ` · 部门：${esc(c.dept)}` : ""}<br />
      事由：${esc(c.reason)}<br />
      金额：<b>${fmt(c.amount)}</b> · 状态：${esc(stLabel)}${c.approver ? ` · 审批人：${esc(c.approver)}${c.approved_at ? `（${esc(c.approved_at)}）` : ""}` : ""}${c.payer ? ` · 付款人：${esc(c.payer)}${c.paid_at ? `（${esc(c.paid_at)}）` : ""}` : ""}
    </div>
    <div class="panel" style="padding:0;margin-top:10px;overflow:auto">
      <table class="grid"><thead><tr><th>费用科目</th><th class="num">金额</th><th>备注</th></tr></thead>
      <tbody>${(c.items || []).map((i) => `<tr><td>${esc(i.expense_account)}</td><td class="num">${fmt(i.amount)}</td><td class="muted">${esc(i.memo)}</td></tr>`).join("") || `<tr><td colspan="3" class="muted" style="text-align:center">无明细</td></tr>`}</tbody></table>
    </div>
    <div class="foot"><button class="btn ghost" id="cd-close">关闭</button></div>`);
  $("#cd-close", mask).addEventListener("click", closeModal);
}

function openClaimEditor(main, claim, period) {
  const isEdit = !!claim;
  const c = claim ? JSON.parse(JSON.stringify(claim)) : {
    biz_date: periodLastDay(period), applicant: "", dept: "", reason: "", amount: "",
    items: [{ expense_account: "", amount: "", memo: "" }],
  };
  const mask = modal(`
    <h3>${isEdit ? `编辑报销单 ${esc(claim.no)}` : "新增报销单"} · ${esc(period)}</h3>
    <div id="cm-flow"></div>
    <div class="field" style="display:flex;gap:12px;flex-wrap:wrap">
      <div><label>业务日期 *</label><input id="cm-date" type="date" value="${esc(c.biz_date)}" style="width:150px" /></div>
      <div><label>申请人 *</label><input id="cm-applicant" value="${esc(c.applicant)}" style="width:110px" /></div>
      <div><label>部门</label><input id="cm-dept" value="${esc(c.dept)}" style="width:110px" /></div>
      <div><label>单据金额 *</label><input id="cm-amount" value="${esc(c.amount)}" style="width:110px;text-align:right" /></div>
    </div>
    <div class="field"><label>事由 *</label><input id="cm-reason" value="${esc(c.reason)}" style="width:100%" /></div>
    <div class="field">
      <label>费用明细（借方科目 + 金额，合计须等于单据金额）</label>
      <table class="grid" id="cm-items"><thead><tr><th>费用科目</th><th>金额</th><th>备注</th><th></th></tr></thead><tbody></tbody></table>
      <button class="btn ghost sm" id="cm-add" style="margin-top:6px">添加明细行</button>
    </div>
    <div class="foot">
      <button class="btn ghost" id="cm-cancel">取消</button>
      <button class="btn primary" id="cm-save">保存草稿</button>
    </div>
  `, true);
  const tbody = $("#cm-items tbody", mask);
  // 单据流程条：审批进度横幅（当前节点/最近动作，未配置流程不显示）
  if (isEdit && claim && claim.id) {
    api(`/workflows/instance-for?biz_type=claim&id=${claim.id}`).then((f) => {
      if (!f.found) return;
      const el = $("#cm-flow", mask);
      if (!el) return;
      const cls = f.status === "approved" ? "ok" : f.status === "rejected" ? "bad" : "";
      const label = { running: "审批中", approved: "已通过", rejected: "已驳回" }[f.status] || f.status;
      const last = (f.log || [])[f.log.length - 1];
      el.innerHTML = `<div class="flow-bar ${cls}"><b>流程</b> ${esc(label)} · 当前节点「${esc(f.current_label)}」${last ? ` · 最近 ${esc(last.who)} ${last.action === "approve" ? "通过" : "驳回"} @ ${esc(last.at)}` : ""}<span class="muted" style="margin-left:auto">${esc(f.flow_name)}</span></div>`;
    }).catch(() => {});
  }
  function renderRows() {
    tbody.innerHTML = c.items.map((i, k) => `
      <tr>
        <td>${accountOptions()}</td>
        <td><input class="ci-amt" data-i="${k}" value="${esc(i.amount)}" style="width:110px;text-align:right" /></td>
        <td><input class="ci-memo" data-i="${k}" value="${esc(i.memo)}" style="width:100%" /></td>
        <td><button class="btn ghost sm" data-rm="${k}">×</button></td>
      </tr>`).join("");
    $all("select.acct-sel", tbody).forEach((sel, k) => { sel.value = c.items[k].expense_account; sel.onchange = () => c.items[k].expense_account = sel.value; });
    $all(".ci-amt", tbody).forEach((inp) => inp.oninput = () => c.items[+inp.dataset.i].amount = inp.value.trim());
    $all(".ci-memo", tbody).forEach((inp) => inp.oninput = () => c.items[+inp.dataset.i].memo = inp.value);
    $all("[data-rm]", tbody).forEach((b) => b.onclick = () => { c.items.splice(+b.dataset.rm, 1); renderRows(); });
  }
  renderRows();
  $("#cm-add", mask).onclick = () => { c.items.push({ expense_account: "", amount: "", memo: "" }); renderRows(); };
  $("#cm-cancel", mask).addEventListener("click", closeModal);
  $("#cm-save", mask).addEventListener("click", async () => {
    const applicant = $("#cm-applicant", mask).value.trim();
    const reason = $("#cm-reason", mask).value.trim();
    if (!applicant) { toast("申请人必填", "err"); return; }
    if (!reason) { toast("事由必填", "err"); return; }
    if (c.items.some((i) => !i.expense_account)) { toast("每条明细都要选费用科目", "err"); return; }
    const body = {
      period: ymm(period),
      biz_date: $("#cm-date", mask).value,
      applicant, reason,
      dept: $("#cm-dept", mask).value.trim(),
      amount: $("#cm-amount", mask).value.trim() || "0",
      items: c.items.map((i) => ({ expense_account: i.expense_account, amount: i.amount || "0", memo: i.memo })),
    };
    try {
      if (isEdit) await api(`/claims/${claim.id}`, { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
      else {
        const out = await api("/claims", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
        toast(`已创建草稿 ${out.no}`, "ok");
        closeModal(); rerenderView("claims", main); return;
      }
      toast("已保存", "ok"); closeModal(); rerenderView("claims", main);
    } catch (e) { toast(e.message, "err"); }
  });
}

// 启动：先尝试恢复已有会话。
// - /me 成功：已进入某账套，直接进应用
// - /me 失败但 /books 成功：已登录平台但未选账套 → 账套选择页
// - 都失败：显示登录页
(async function boot() {
  try {
    const me = await api("/me");
    session.user = me;
    await afterLogin();
  } catch (e1) {
    try {
      const b = await api("/books");
      showBookPicker(b);
    } catch (e2) {
      showLogin();
    }
  }
})();
