// FinBook Web 前端 — 基础层：HTTP 会话 / 提示与弹窗 / 全局快捷键 / 表格与页签基础 / 设备与权限
//
// 这一份是 `crates/finweb/static/app.js` 拆分出来的（原单文件 14k 行、295 个顶层函数）。
// 拆分方式：**经典 script，按原始行序切分，不重排任何代码**——顶层 function/const 的
// 提升与声明顺序完全保持原样，所以跨文件不存在 TDZ/未定义问题；只有 `VIEWS` 路由表
// 因为引用全部视图函数，被挪到了最后加载的 app-hr.js。
// 加载顺序由 `index.html` 的 <script> 顺序决定（也是 `main.rs` 里 asset_version 的清单顺序）。
// 新增文件必须同步三处：index.html、main.rs 的 asset_version 清单、tools/lib-app-src.js 的解析（自动）。
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

// ===========================================================================
// 顶栏进度条：所有请求在途时显示一条 2px 进度线
// ---------------------------------------------------------------------------
// 改造前没有任何全局「正在请求」指示。视图自己写死「加载中…」，于是网络抖动
// 或后端慢时，用户看到的和「本来就没数据」完全一样，只能干等或刷新。这里
// 做成全局计数：任何时刻有请求在途就亮起，全部结束就淡出。
let inflight = 0;
let progressEl = null;
function progressOn() {
  if (!progressEl) {
    progressEl = document.createElement("div");
    progressEl.id = "top-progress";
    document.body.appendChild(progressEl);
  }
  inflight++;
  progressEl.classList.add("on");
  // 前进到 90% 然后等真实完成——匀速走到 100% 再回落，避免「假完成」
  const pct = Math.min(90, 18 + inflight * 14);
  progressEl.style.width = pct + "%";
}
function progressOff() {
  inflight = Math.max(0, inflight - 1);
  if (inflight > 0) return;
  if (!progressEl) return;
  progressEl.style.width = "100%";
  setTimeout(() => {
    if (inflight === 0) {
      progressEl.classList.remove("on");
      progressEl.style.width = "0";
    }
  }, 220);
}

// ===========================================================================
// api()：统一请求入口
// ---------------------------------------------------------------------------
// 2026-09 加的三件事，都是为了「挂住的请求不会变成永远转圈」：
//   1. **超时**。原来没有 AbortSignal，后端卡住时 fetch 永远 pending，界面停在
//      「加载中…」，用户唯一出路是刷新（刷新会丢掉已填的表单）。现在默认 30s
//      主动中止，并抛出**可识别**的错误，让视图能给出重试按钮。
//   2. **网络错误翻译**。原来 fetch 抛的是 `TypeError: Failed to fetch`，
//      直接 toast 出来对会计毫无意义。现在按「断网 / 请求被中止 / 超时」
//      三种情况给中文说明。
//   3. **GET 失败重试一次**。内网偶发丢包很常见，一次 502/网络错误就重试能挡住
//      大部分；**只对 GET 重试**，写操作绝不重试（重试 = 可能重复提交单据）。
// ---------------------------------------------------------------------------
const API_TIMEOUT_MS = 30000;
const API_RETRY_MS = 600;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// 视图用来判断「要不要给重试按钮」：fetch 已经拉黑，视图可以直接重调 api()
class TimeoutError extends Error {
  constructor(ms, path) { super(`请求超时（超过 ${Math.round(ms / 1000)} 秒）`); this.name = "TimeoutError"; this.path = path; }
}
class OfflineError extends Error {
  constructor(path) { super("连不上服务器，请检查网络后重试"); this.name = "OfflineError"; this.path = path; }
}

async function apiOnce(path, opts, timeoutMs) {
  const ctl = ("AbortController" in window) ? new AbortController() : null;
  let timedOut = false;
  let timer = null;
  if (ctl) {
    timer = setTimeout(() => { timedOut = true; ctl.abort(); }, timeoutMs);
  }
  try {
    const merged = Object.assign({ credentials: "same-origin" }, opts);
    if (ctl) merged.signal = ctl.signal;
    return await fetch(API + path, merged);
  } catch (e) {
    // AbortError 只在我们自己超时后出现；别的 AbortError 视为用户取消，不报错
    if (timedOut) throw new TimeoutError(timeoutMs, path);
    if (e && e.name === "AbortError") throw e;
    // 浏览器对「连不上」一律抛 TypeError，DOM 层面无法再细分
    throw new OfflineError(path);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

async function api(path, opts = {}) {
  const method = (opts.method || "GET").toUpperCase();
  const timeout = Number(opts.timeout) > 0 ? Number(opts.timeout) : API_TIMEOUT_MS;
  const o = Object.assign({}, opts);
  delete o.timeout;
  // 只对 GET 重试一次：写操作重试会造成重复单据
  const attempts = method === "GET" && o.retry !== false ? 2 : 1;
  let lastErr = null;
  for (let i = 0; i < attempts; i++) {
    progressOn();
    let r;
    try {
      r = await apiOnce(path, o, timeout);
    } catch (e) {
      progressOff();
      lastErr = e;
      // 用户主动取消（切视图/关弹窗）不算失败，直接抛出
      if (e && e.name === "AbortError") throw e;
      // 超时不重试——已经等了 30s，再等一轮只会更让人绝望
      if (e instanceof TimeoutError) throw e;
      if (i + 1 < attempts) { await sleep(API_RETRY_MS); continue; }
      throw e;
    }
    progressOff();
    let data = null;
    try { data = await r.json(); } catch (e) {}
    if (r.status === 401) {
      if (path !== "/setup/status" && path !== "/login") { session.user = null; render(); }
      throw new Error((data && data.error) || "未登录");
    }
    if (!r.ok) {
      let msg = (data && data.error) || ("请求失败 " + r.status);
      if (r.status === 403) msg += "（权限不足，请联系管理员开通该权限或调整岗位）";
      const err = new Error(msg);
      err.status = r.status;
      // 5xx / 429 属于服务端临时问题，GET 值得再来一次
      if (i + 1 < attempts && (r.status >= 500 || r.status === 429)) { lastErr = err; await sleep(API_RETRY_MS); continue; }
      throw err;
    }
    // 列偏好：每次数据到达后重挂（异步填充的表格也能获得列菜单与隐藏样式）
    try { autoColPrefs(); } catch (e) {}
    // 通用列头排序 + 列宽拖拽（同点补挂，异步表也能获得）
    try { autoColSort(); } catch (e) {}
    try { autoColResize(); } catch (e) {}
    // 单元格省略号补 title（悬停可见全文）
    try { autoCellTitles(); } catch (e) {}
    // 通用后处理：异步填充的表也要过一遍（负数红字 / 空态 / 溢出收纳）
    try { autoNumbers(); } catch (e) {}
    try { autoEmptyRows(); } catch (e) {}
    try { autoOverflowMenus(); } catch (e) {}
    return data;
  }
  throw lastErr || new Error("请求失败");
}


// ===========================================================================
// 虚拟滚动列表：只在可视区域渲染行，滚动时增量更新
// 适用于大数据量列表（明细账、往来明细等），避免一次性渲染数千行 DOM
// ---------------------------------------------------------------------------
// 用法：
//   const vs = createVirtualScroll(container, {
//     rowHeight: 44,
//     data: rows,
//     render: (index, item) => `<tr>...</tr>`,
//     height: "56vh",
//   });
//   vs.update(newData);   // 换数据重绘
//   vs.destroy();         // 卸载
// ===========================================================================
function createVirtualScroll(container, { rowHeight, data, render, height }) {
  container.innerHTML = "";

  const viewport = document.createElement("div");
  viewport.style.overflow = "auto";
  viewport.style.height = height;
  viewport.style.position = "relative";
  container.appendChild(viewport);

  const placeholder = document.createElement("div");
  placeholder.style.height = data.length * rowHeight + "px";
  viewport.appendChild(placeholder);

  const content = document.createElement("div");
  content.style.position = "absolute";
  content.style.top = "0";
  content.style.left = "0";
  content.style.right = "0";
  viewport.appendChild(content);

  let range = { s: -1, e: -1 };

  function renderVisible() {
    const st = viewport.scrollTop;
    const vh = viewport.clientHeight;
    const s = Math.max(0, Math.floor(st / rowHeight));
    const e = Math.min(data.length, Math.ceil((st + vh) / rowHeight) + 1);
    if (s === range.s && e === range.e) return;
    range = { s, e };
    content.style.transform = `translateY(${s * rowHeight}px)`;
    content.innerHTML = data.slice(s, e).map((item, i) => render(s + i, item)).join("");
  }

  viewport.addEventListener("scroll", renderVisible, { passive: true });
  renderVisible();

  return {
    update(newData) {
      data.length = 0;
      data.push(...newData);
      placeholder.style.height = data.length * rowHeight + "px";
      range = { s: -1, e: -1 };
      renderVisible();
    },
    destroy() {
      viewport.removeEventListener("scroll", renderVisible);
      container.innerHTML = "";
    }
  };
}

const MAX_TOASTS = 5;
function toast(msg, kind) {
  const wrap = document.getElementById("toast");
  // 上限：挤掉最旧。但**错误不挤错误**——错误是唯一必须被读到的信息。
  // 原来「超过 5 个就 removeChild(firstChild)」会把最早的错误挤掉，而错误
  // 恰恰是最需要看的那条。
  const sticky = wrap.querySelectorAll(".toast.err");
  while (wrap.children.length - sticky.length >= MAX_TOASTS) {
    const victim = Array.from(wrap.children).find((c) => !c.classList.contains("err"));
    if (!victim) break;
    wrap.removeChild(victim);
  }
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
  // 改造前一律 2600ms 自动消失。财务的错误信息普遍很长——比如
  // 「期间格式不正确：2026-01-01（应为 202601 或 2026-01）」——2.6 秒读不完，
  // 提示就没了，用户只能猜哪里错了。现在：err 常驻到手动关；warn 6s；
  // 成功类按长度自适应（短 2.6s，长 6s），短提示仍不占屏。
  const isErr = kind === "err";
  const ms = isErr ? 0 : kind === "warn" ? 6000 : msg.length > 24 ? 6000 : 2600;
  if (ms > 0) setTimeout(dismiss, ms);
  else t.classList.add("sticky");
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
  const KIND = { voucher: "凭证", po: "采购订单", so: "销售订单", req: "请购单", claim: "报销单", aux: "档案" };
  let timer = null;
  const run = async () => {
    const kw = input.value.trim();
    if (!kw) { box.className = "muted"; box.textContent = "输入关键词即时搜索"; return; }
    try {
      const r = await api(`/quick-search?q=${encodeURIComponent(kw)}`);
      const rows = r.rows || [];
      box.className = "";
      box.innerHTML = rows.length
        ? rows.map((x, i) => `<button class="btn ghost" data-qs="${i}" data-view="${esc(x.view)}" data-auxkind="${esc(x.aux_kind || "")}" style="display:flex;width:100%;justify-content:space-between;gap:8px;margin-bottom:4px;text-align:left">
            <span><span class="tag">${KIND[x.kind] || esc(x.kind)}</span> ${esc(x.label)}</span>
            <span class="muted" style="font-size:12px;white-space:nowrap">${esc(x.sub || "")}</span></button>`).join("")
        : `<div class="muted">无匹配结果</div>`;
      $all("[data-qs]", mask).forEach((b) => b.onclick = () => { closeModal(); if (b.dataset.auxkind) state.auxKind = b.dataset.auxkind; goView(b.dataset.view); });
    } catch (e) { box.innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  input.oninput = () => { clearTimeout(timer); timer = setTimeout(run, 260); };
  input.onkeydown = (e) => {
    if (e.key === "Enter") { e.preventDefault(); const b = mask.querySelector("[data-qs]"); if (b) b.click(); }
    else if (e.key === "Escape") { closeModal(); }
  };
}

// ---- 批量粘贴弹窗（辅助档案 / 期初余额共用）--------------------------------
//
// 为什么是「先预览再导入」而不是一点就写库：粘贴几乎一定带偏差 —— 多一行表头、
// 带上合计行、金额列粘成了两列。直接写库的后果是**静默的脏数据**：用户粘完
// 关掉窗口，几周后对账才发现多了个「合计」科目。预览让用户在落库前看见
// 「哪几行会被导入、哪几行会被跳过、为什么跳过」。
//
// 解析在后端（fincore::batch），与桌面端**同一份代码** —— 两端解析结果必须
// 一致，否则用户会发现「桌面上能粘的，网页上粘不了」。
//
// `opts`:
//   title      弹窗标题
//   hint       说明文案（数组，每行一段）
//   example    写法示例（放进等宽块）
//   placeholder  文本框占位符
//   parse      POST 解析 → { rows, total, importable, skipped, empty, trial? }
//   importRows PUT 导入，参数是 rows 里 skip 为空的那些
//   cols       预览表格列定义 [{ key, label, cls }]
//   trial      是否显示试算区（期初要，档案不要）
//   onDone     导入成功后回调（刷新列表）
function batchPasteDialog(opts) {
  let preview = null;
  let busy = false;

  const mask = modal(render(), true);
  const root = $(".modal", mask);

  function render() {
    const hint = (opts.hint || []).map((h) => `<p class="muted" style="margin:0 0 10px;line-height:1.7">${esc(h)}</p>`).join("");
    return `
      <h3>${esc(opts.title)}</h3>
      ${hint}
      ${opts.example ? `<pre style="background:var(--z-050,#f7f7f8);padding:10px 12px;border-radius:4px;font-size:12px;line-height:1.8;margin:0 0 12px;overflow-x:auto">${esc(opts.example)}</pre>` : ""}
      <textarea id="bp-text" rows="9" style="width:100%;font-family:var(--mono,monospace);font-size:12.5px;line-height:1.7"
        placeholder="${esc(opts.placeholder || "")}"></textarea>
      <div id="bp-msg" style="margin-top:10px"></div>
      <div id="bp-preview"></div>
      <div class="foot">
        <button class="btn ghost" id="bp-cancel">关闭</button>
        <span class="grow"></span>
        // 「预览」默认可用，「导入」默认禁用 —— 启用逻辑在 setBusy 里。
        // 两个都写 disabled 的话，预览按钮永远点不动（setBusy 只在预览/导入
        // 动作里被调用，而点预览正是第一个动作）。实测：E2E 里 #bp-preview-btn
        // 一直是 disabled，4 条全挂在这里。
        <button class="btn" id="bp-preview-btn">预览</button>
        <button class="btn primary" id="bp-ok" disabled>导入</button>
      </div>`;
  }

  function setMsg(html) { $("#bp-msg", root).innerHTML = html; }
  function setBusy(on) {
    busy = on;
    ["bp-preview-btn", "bp-ok"].forEach((id) => {
      const b = $("#" + id, root);
      if (b) b.disabled = on || (id === "bp-ok" && !previewHasImportable());
    });
  }
  function previewHasImportable() {
    return !!preview && preview.rows.some((r) => !r.skip);
  }

  function renderPreview() {
    const box = $("#bp-preview", root);
    if (!preview) { box.innerHTML = ""; return; }
    const imp = preview.rows.filter((r) => !r.skip);
    const skip = preview.rows.length - imp.length;

    let head = `<div style="margin:8px 0 6px"><b>解析到 ${preview.total} 条，其中 ${imp.length} 条将导入</b>`;
    if (skip > 0) {
      head += `<span style="color:var(--err);margin-left:10px">${skip} 条会跳过（不会覆盖已有档案）</span>`;
    }
    head += `</div>`;

    let body = "";
    // 期初要试算：Web 端是「粘贴后立刻提交」，没有草稿区慢慢看，
    // 不给试算用户就完全不知道平不平
    if (opts.trial && preview.trial) {
      const t = preview.trial;
      const col = t.balanced ? "var(--ok, #16a34a)" : "var(--err)";
      // 试算区用 bp-trial 做标识单独定位。预览表格里坏行也渲染金额，
// 断言「坏行不进试算」时必须只看试算区，否则表格里的金额会误伤断言。
      body += `<div id="bp-trial" style="font-size:12.5px;margin-bottom:8px">
        本次借方合计 <b>${esc(t.debit)}</b> ／ 贷方合计 <b>${esc(t.credit)}</b> ／
        差额 <b style="color:${col}">${esc(t.diff)}</b>
        ${t.balanced ? "（平）" : "（不平 —— 请检查是否漏了对方科目）"}
      </div>`;
    }
    const cols = (opts.cols || []).map((c) =>
      `<th class="${c.cls || ""}">${esc(c.label)}</th>`).join("");
    const rows = preview.rows.map((r) => {
      const bad = !!r.skip;
      return `<tr${bad ? ' style="color:var(--err)"' : ""}>${(opts.cols || []).map((c) =>
        `<td class="${c.cls || ""}">${esc(r[c.key] == null ? "" : r[c.key])}</td>`).join("")}</tr>`;
    }).join("");
    body += `<div style="max-height:260px;overflow:auto"><table class="grid">
      <thead><tr>${cols}</tr></thead><tbody>${rows}</tbody></table></div>`;

    box.innerHTML = head + body;
  }

  $("#bp-cancel", root).addEventListener("click", () => closeModal());

  $("#bp-preview-btn", root).addEventListener("click", async () => {
    if (busy) return;
    const text = $("#bp-text", root).value;
    if (!text.trim()) { setMsg('<span style="color:var(--err)">先粘贴内容再点预览</span>'); return; }
    setBusy(true); setMsg('<span class="muted">解析中…</span>');
    try {
      const r = await opts.parse(text);
      if (r.empty || !r.rows.length) {
        preview = null;
        setMsg('<span style="color:var(--err)">没解析出内容。每行一条，列之间用逗号或 Tab 隔开就行。</span>');
      } else {
        preview = r;
        setMsg("");
      }
      renderPreview();
    } catch (e) {
      preview = null;
      setMsg(`<span style="color:var(--err)">${esc(e.message)}</span>`);
    } finally {
      setBusy(false);
    }
  });

  $("#bp-ok", root).addEventListener("click", async () => {
    if (busy || !previewHasImportable()) return;
    const rows = preview.rows.filter((r) => !r.skip);
    if (!rows.length) return;
    setBusy(true); setMsg('<span class="muted">导入中…</span>');
    try {
      const r = await opts.importRows(rows);
      const skipped = r.skipped || 0;
      let msg = r.inserted ? `已导入 ${r.inserted} 条` : "没有可导入的新档案（编码都已存在）";
      if (skipped) msg += `，跳过 ${skipped} 条（编码已存在）`;
      toast(msg, "ok");
      closeModal();
      if (opts.onDone) opts.onDone(r);
    } catch (e) {
      setMsg(`<span style="color:var(--err)">${esc(e.message)}</span>`);
      setBusy(false);
    }
  });

  // ⌘/Ctrl+Enter 直接导入：预览之后不用再移开手去点按钮
  root.addEventListener("keydown", (e) => {
    if ((e.metaKey || e.ctrlKey) && e.key === "Enter" && previewHasImportable()) {
      $("#bp-ok", root).click();
    }
  });

  // 文本一变，之前那份预览就**过期**了。继续点「导入」会导入他已经改掉的文本
  // 解析出来的行 —— 正是这个两步流程要防的那类错。所以直接清掉预览并禁用导入。
  $("#bp-text", root).addEventListener("input", () => {
    if (!preview) return;
    preview = null;
    renderPreview();
    setMsg('<span class="muted">内容改过了，请重新点「预览」</span>');
    setBusy(false);
  });

  return mask;
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

/// 填原因对话框：返回填写的字符串；取消返回 null。
///
/// 存在的理由：有些动作**必须**留下理由才能被审计（凭证更正、作废原因、
/// 折扣审批驳回…）。用 `confirmDialog` 只能拿到 yes/no，理由就得塞进
/// `prompt()` —— 那会绕开 modalStack，Esc 关闭和 Ctrl+Enter 提交都失效。
function reasonDialog(title, hint, placeholder) {
  return new Promise((resolve) => {
    const root = document.getElementById("modal-root");
    const mask = document.createElement("div");
    mask.className = "modal-mask";
    mask.setAttribute("role", "dialog");
    mask.setAttribute("aria-modal", "true");
    mask.setAttribute("aria-label", title);
    mask.innerHTML = `<div class="modal">
      <h3>${esc(title)}</h3>
      <p class="muted" style="margin:0 0 12px;line-height:1.6;white-space:pre-wrap">${esc(hint)}</p>
      <textarea id="rd-text" rows="3" style="width:100%" placeholder="${esc(placeholder || "")}"></textarea>
      <div class="foot">
        <button class="btn ghost" id="rd-cancel">取消</button>
        <button class="btn primary" id="rd-ok">确定</button>
      </div>
    </div>`;
    root.appendChild(mask);
    modalStack.push(mask);
    const ta = $("#rd-text", mask);
    const done = (val) => {
      document.removeEventListener("keydown", onKey, true);
      const i = modalStack.indexOf(mask);
      if (i >= 0) modalStack.splice(i, 1);
      mask.remove();
      resolve(val);
    };
    // 空原因按「取消」处理：让必填在前端也拦一道，而不是发一次注定 400 的请求。
    // 但**不静默吞掉** —— 用户点了确定却什么都没发生会以为按钮坏了，
    // 所以给校验反馈。
    const submit = () => {
      const val = ta.value.trim();
      if (!val) { toast("请填写原因：没有原因的记录事后查不出来", "err"); ta.focus(); return; }
      done(val);
    };
    const onKey = (e) => {
      if (e.key === "Escape") { e.stopPropagation(); done(null); }
    };
    document.addEventListener("keydown", onKey, true);
    mask.addEventListener("click", (e) => { if (e.target === mask) done(null); });
    $("#rd-ok", mask).onclick = submit;
    $("#rd-cancel", mask).onclick = () => done(null);
    // Ctrl+Enter 提交（与全局快捷键一致）
    ta.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) { e.preventDefault(); submit(); }
    });
    ta.focus();
  });
}
// Esc 关闭最近弹窗
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape" && modalStack.length) closeModal();
});

// 快捷键帮助面板（顶栏「?」或按 ?）
function openShortcuts() {
  const rows = [
    ["/", "定位侧栏功能搜索框（搜功能名，如「明细账」）——69 个功能平铺时的主要出路"],
    ["Ctrl+K", "全局快速搜索（凭证 / 采购 / 销售 / 请购 / 报销 / 档案）"],
    ["浏览器后退", "回到上一个页面（地址栏现在是 #/页面名，可直接发给同事）"],
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
    <div class="muted" style="font-size:12px;margin-top:8px">侧栏：点导航项右侧 ☆ 加为「常用」（置顶）；分组可折叠；顶部搜索框找功能。所有列表：点列头排序、拖列边调宽、右上「列▾」显隐列（偏好自动记忆）；列表带分页与合计行；顶栏「Aa」调字号。</div>
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
  // /：定位侧栏功能搜索框。69 个功能平铺 16 组，不给一个搜索入口就得靠翻。
  // 输入态不触发（否则录单打不出斜杠），Esc 在框里回车清空由 renderSidebar 管。
  if (e.key === "/" && !e.ctrlKey && !e.metaKey && !e.altKey && !["INPUT", "SELECT", "TEXTAREA"].includes(tag)) {
    const q = document.getElementById("nav-q");
    if (q) {
      e.preventDefault();
      q.focus();
      q.select();
      document.body.classList.add("menu-open"); // 窄屏下把抽屉拉出来，否则看不见框
      return;
    }
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

// ---------- 表格分组与汇总 ----------
const GROUP_CONFIG = {};
function setTableGroup(key, config) { GROUP_CONFIG[key] = config; }
function clearTableGroup(key) { delete GROUP_CONFIG[key]; }
function groupTableByKey(tbl, key) {
  const cfg = GROUP_CONFIG[key];
  if (!cfg || !tbl || tbl.dataset.grouped) return;
  tbl.dataset.grouped = "1";
  const { fieldIdx, subtotalIdx, startRow = 1 } = cfg;
  const tb = tbl.tBodies[0];
  if (!tb) return;
  const rows = [...tb.rows].slice(startRow);
  let lastKey = null, groupRows = [];
  const flush = () => {
    if (!groupRows.length || lastKey === null) return;
    let html = `<tr class="subtotal"><td colspan="${tbl.rows[0]?.cells.length || 5}">小计</td>${subtotalIdx.map((si) => `<td class="num">${fmt(groupRows.reduce((s, r) => s + (parseFloat(r.cells[si]?.textContent?.replace(/[,\s¥%]/g, "")) || 0), 0))}</td>`).join("")}</tr>`;
    groupRows[groupRows.length - 1].insertAdjacentHTML("afterend", html);
  };
  rows.forEach((r) => {
    const k = r.cells[fieldIdx]?.textContent?.trim() || "";
    if (k !== lastKey) { flush(); lastKey = k; groupRows = []; }
    groupRows.push(r);
  });
  flush();
}

// ---------- 高级筛选面板 ----------
function createFilterPanel(container, onApply) {
  const fields = [
    { key: "date", label: "日期", type: "date" },
    { key: "amount", label: "金额", type: "number" },
    { key: "status", label: "状态", type: "select", options: ["全部", "草稿", "已审核", "已记账", "作废"] },
    { key: "keyword", label: "关键字", type: "text" },
  ];
  const panel = document.createElement("div");
  panel.className = "filter-panel";
  panel.innerHTML = `
    <div class="fp-header"><b>${icon("filter")} 高级筛选</b></div>
    <div class="fp-conditions">
      <div class="fp-row">
        <select data-fp="field">${fields.map((f) => `<option value="${f.key}">${f.label}</option>`).join("")}</select>
        <select data-fp="op">
          <option value="=">等于</option><option value="!=">不等于</option>
          <option value=">">大于</option><option value="<">小于</option>
          <option value=">=">≥</option><option value="<=">≤</option>
          <option value="like">包含</option><option value="in">属于</option>
        </select>
        <input data-fp="value" type="text" placeholder="值" style="flex:1;min-width:120px" />
      </div>
    </div>
    <div class="fp-actions">
      <button class="btn primary sm" data-fp="apply">${icon("check")} 应用</button>
      <button class="btn ghost sm" data-fp="reset">${icon("refresh")} 重置</button>
    </div>`;
  container.appendChild(panel);
  $("[data-fp='apply']", panel).onclick = () => {
    const field = $("[data-fp='field']", panel).value;
    const op = $("[data-fp='op']", panel).value;
    const value = $("[data-fp='value']", panel).value.trim();
    onApply({ field, op, value });
  };
  $("[data-fp='reset']", panel).onclick = () => { $("[data-fp='value']", panel).value = ""; onApply(null); };
  return panel;
}

// ---------- 分页器 ----------
function createPagination(container, total, pageSize, onPage) {
  const pg = document.createElement("div");
  pg.className = "pagination";
  let current = 1;
  const render = () => {
    const totalPages = Math.ceil(total / pageSize);
    const start = (current - 1) * pageSize + 1;
    const end = Math.min(total, current * pageSize);
    const pageBtn = (n, label, active, disabled) => `<button class="pg-btn${active ? " active" : ""}" data-pg="${n}" ${disabled ? "disabled" : ""}>${label || n}</button>`;
    let btns = pageBtn(1, icon("chevronLeft"), false, current === 1);
    btns += pageBtn(current - 1, "上一页", false, current === 1);
    const pages = [];
    for (let i = Math.max(1, current - 2); i <= Math.min(totalPages, current + 2); i++) pages.push(i);
    btns += pages.map((i) => pageBtn(i, null, i === current)).join("");
    btns += pageBtn(current + 1, "下一页", false, current === totalPages);
    btns += pageBtn(totalPages, icon("chevronRight"), false, current === totalPages);
    pg.innerHTML = `<span class="pg-info">共 ${total} 条，显示 ${start}-${end}</span>${btns}
      <span class="pg-size">每页 <select data-pg-size>${[10, 25, 50, 100].map((n) => `<option value="${n}"${n === pageSize ? " selected" : ""}>${n}</option>`).join("")}</select> 条</span>`;
    $all(".pg-btn[data-pg]", pg).forEach((b) => { b.onclick = () => { if (!b.disabled) { current = parseInt(b.dataset.pg); onPage(current); render(); } }; });
    $("[data-pg-size]", pg).onchange = (e) => { current = 1; onPage(1, parseInt(e.target.value)); };
  };
  render();
  container.appendChild(pg);
  return { reload: (t, ps) => { total = t; pageSize = ps || pageSize; current = 1; render(); } };
}

// ---------- 行内编辑 ----------
function makeCellEditable(cell, onSave) {
  if (cell.dataset.editable || cell.classList.contains("cell-editing")) return;
  cell.dataset.editable = "1";
  cell.classList.add("cell-editable");
  cell.addEventListener("dblclick", () => {
    if (cell.classList.contains("cell-editing")) return;
    const oldVal = cell.textContent.trim();
    cell.classList.add("cell-editing");
    cell.innerHTML = `<input type="text" value="${esc(oldVal)}" />`;
    const input = cell.querySelector("input");
    input.focus(); input.select();
    const save = () => { const v = input.value.trim(); cell.classList.remove("cell-editing"); cell.textContent = v || oldVal; if (v && v !== oldVal) onSave(v); };
    input.addEventListener("blur", save);
    input.addEventListener("keydown", (e) => { if (e.key === "Enter") input.blur(); if (e.key === "Escape") { input.value = oldVal; input.blur(); } });
  });
}

// ---------- 右键菜单 ----------
let ctxMenuEl = null;
function showContextMenu(x, y, items) {
  hideContextMenu();
  ctxMenuEl = document.createElement("div");
  ctxMenuEl.className = "context-menu";
  items.forEach((item) => {
    if (item === "-") { ctxMenuEl.innerHTML += `<div class="cm-sep"></div>`; return; }
    const el = document.createElement("div");
    el.className = "cm-item" + (item.danger ? " danger" : "");
    el.innerHTML = `${item.icon ? icon(item.icon) : ""}<span>${esc(item.label)}</span>`;
    el.onclick = () => { item.action?.(); hideContextMenu(); };
    ctxMenuEl.appendChild(el);
  });
  document.body.appendChild(ctxMenuEl);
  const r = ctxMenuEl.getBoundingClientRect();
  ctxMenuEl.style.left = Math.min(x, window.innerWidth - r.width - 8) + "px";
  ctxMenuEl.style.top = Math.min(y, window.innerHeight - r.height - 8) + "px";
}
function hideContextMenu() { if (ctxMenuEl) { ctxMenuEl.remove(); ctxMenuEl = null; } }
document.addEventListener("click", (e) => { if (!e.target.closest(".context-menu")) hideContextMenu(); });
document.addEventListener("contextmenu", (e) => {
  const tbl = e.target.closest("table.grid");
  if (!tbl) return;
  e.preventDefault();
  showContextMenu(e.clientX, e.clientY, [
    { icon: "copy", label: "复制", action: () => navigator.clipboard.writeText(e.target.textContent) },
    { icon: "edit", label: "编辑", action: () => makeCellEditable(e.target.closest("td"), () => {}) },
    "-",
    { icon: "sort", label: "升序", action: () => e.target.closest("th")?.click() },
    { icon: "sortDesc", label: "降序", action: () => { const th = e.target.closest("th"); if (th) { th.click(); th.click(); } } },
    "-",
    { icon: "eye", label: "隐藏列", action: () => { const th = e.target.closest("th"); if (th) th.style.display = "none"; } },
  ]);
});

// ---------- 多页签 ----------
//
// 这段代码早就写好了，但从来没有被调用过（死代码），所以「跳到别的页面后原来的
// 消失、想回去得滚回侧栏顶部」一直是现状。现在接线，并且**所有切换都走
// goView()** —— 直接改 state.view 再 renderMain 会绕过 hash 同步、筛选状态
// 保存/还原、最近访问记录，等于把上一轮做的那些功能在页签路径上全部作废。
let openTabs = [];
let activeTabId = null;
const MAX_TABS = 12;
const LS_TABS = "open_tabs";

// 刷新后页签还在：页签集合属于「会话布局」，和 localStorage 里的列偏好、
// 字号是一个层级的东西。过滤掉已经不在导航里的（权限变过 / 页面被删）。
function tabsLoad() {
  let ids = [];
  try { ids = JSON.parse(localStorage.getItem(LS_TABS) || "[]"); } catch (e) { ids = []; }
  if (!Array.isArray(ids)) ids = [];
  const valid = ids.filter((id) => typeof id === "string" && VIEWS[id] && NAV_INDEX[id]);
  if (!valid.includes("dashboard")) valid.unshift("dashboard");
  return valid.slice(0, MAX_TABS);
}
function tabsSave() {
  try { localStorage.setItem(LS_TABS, JSON.stringify(openTabs.map((t) => t.id))); } catch (e) {}
}
function initTabs() {
  openTabs = tabsLoad().map((id) => ({ id, label: navLabel(id) }));
  activateTab(state.view);
}
// 打开并激活一个页签（切视图时统一走这里）。超上限时丢最旧的非工作台页签。
function activateTab(id) {
  if (!VIEWS[id]) return;
  if (!openTabs.find((t) => t.id === id)) {
    openTabs.push({ id, label: navLabel(id) });
    // 满了就丢最早的一个，但绝不动当前页和工作台（工作台是回退兜底）
    if (openTabs.length > MAX_TABS) {
      const dropAt = openTabs.findIndex((t) => t.id !== "dashboard" && t.id !== id);
      if (dropAt >= 0) openTabs.splice(dropAt, 1);
    }
  }
  activeTabId = id;
  tabsSave();
  renderTabs();
}
function closeTab(id) {
  const i = openTabs.findIndex((t) => t.id === id);
  if (i < 0) return;
  const wasActive = id === activeTabId;
  openTabs.splice(i, 1);
  if (!openTabs.length) openTabs.push({ id: "dashboard", label: navLabel("dashboard") });
  tabsSave();
  if (wasActive) {
    // 落到相邻的页签（左邻优先，和浏览器一致）
    const next = openTabs[Math.min(i, openTabs.length - 1)];
    goView(next.id, { force: true });
  } else {
    renderTabs();
  }
}
function renderTabs() {
  const bar = document.getElementById("tab-bar");
  if (!bar) return;
  bar.innerHTML = openTabs.map((t) => `
    <div class="tab-item${t.id === activeTabId ? " active" : ""}" data-tab="${t.id}"
         role="tab" tabindex="${t.id === activeTabId ? 0 : -1}"
         aria-selected="${t.id === activeTabId}" title="${esc(t.label)}">
      <span>${esc(t.label)}</span>
      ${openTabs.length > 1 ? `<span class="tab-close" data-tab-close="${t.id}" role="button" aria-label="关闭${esc(t.label)}页签">${icon("close")}</span>` : ""}
    </div>`).join("");
  $all(".tab-item", bar).forEach((el) => {
    el.addEventListener("click", (e) => {
      if (e.target.closest("[data-tab-close]")) return;
      goView(el.dataset.tab);
    });
  });
  $all("[data-tab-close]", bar).forEach((el) => {
    el.addEventListener("click", (e) => { e.stopPropagation(); closeTab(el.dataset.tabClose); });
  });
  // 让当前页签可见（页签多到溢出时，新加的在最右，可能已经在可视区外）
  const cur = bar.querySelector('.tab-item.active');
  if (cur && cur.scrollIntoView) { try { cur.scrollIntoView({ block: "nearest", inline: "nearest" }); } catch (e) { /* 老浏览器忽略 */ } }
}

// ---------- 气泡提示 ----------
let tooltipEl = null;
function showTooltip(target, content) {
  hideTooltip();
  tooltipEl = document.createElement("div");
  tooltipEl.className = "tooltip";
  tooltipEl.innerHTML = content;
  document.body.appendChild(tooltipEl);
  const r = target.getBoundingClientRect();
  const tr = tooltipEl.getBoundingClientRect();
  let left = r.left + r.width / 2 - tr.width / 2;
  left = Math.max(8, Math.min(left, window.innerWidth - tr.width - 8));
  let top = r.top - tr.height - 6;
  if (top < 8) top = r.bottom + 6;
  tooltipEl.style.left = left + "px"; tooltipEl.style.top = top + "px";
}
function hideTooltip() { if (tooltipEl) { tooltipEl.remove(); tooltipEl = null; } }

// ---------- 设备指纹 ----------
function deviceId() {
  let id = localStorage.getItem("finbook_device_id");
  if (!id) { id = (crypto.randomUUID ? crypto.randomUUID() : "d-" + Math.random().toString(36).slice(2) + Date.now()); localStorage.setItem("finbook_device_id", id); }
  return id;
}
function deviceName() { return (navigator.platform || "Web") + " · " + navigator.userAgent.slice(0, 40); }

// ---------- 权限 ----------
function can(p) { return session.user && session.user.perms.indexOf(p) >= 0; }
