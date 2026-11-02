// FinBook Web 前端 — 路由与哈希、侧栏、视图状态快照、通知中心、表格增强、外壳渲染
//
// 这一份是 `crates/finweb/static/app.js` 拆分出来的（原单文件 14k 行、295 个顶层函数）。
// 拆分方式：**经典 script，按原始行序切分，不重排任何代码**——顶层 function/const 的
// 提升与声明顺序完全保持原样，所以跨文件不存在 TDZ/未定义问题；只有 `VIEWS` 路由表
// 因为引用全部视图函数，被挪到了最后加载的 app-hr.js。
// 加载顺序由 `index.html` 的 <script> 顺序决定（也是 `main.rs` 里 asset_version 的清单顺序）。
// 新增文件必须同步三处：index.html、main.rs 的 asset_version 清单、tools/lib-app-src.js 的解析（自动）。
/// 异步回调里的整页重绘守卫：请求返回时用户可能已切到别的视图，
/// 直接用旧视图覆盖会打断当前界面（表现为新视图一直停在"加载中…"）。
function rerenderView(id, main) {
  if (state.view === id) VIEWS[id](main);
}

let shellBuilt = false;

// ===========================================================================
// A. 路由（hash）+ 视图状态保留 + 侧栏搜索/收藏/最近
// ---------------------------------------------------------------------------
// 改造前只有 `state.view` 一个变量，没有 location.hash、没有 history：
//   · 浏览器**后退键无效**——从报表点进明细账，回不去上一张报表；
//   · **无法把页面发给同事**（「你去处理一下期末」只能口头说进哪个菜单）；
//   · **刷新必然丢位置**（state.view 没进 localStorage）。
// 会计一天要在凭证/明细账/期末/报表/对账之间来回跳，这三条都是每天要付的税。
//
// 这里只做**加法**：state.view 仍然是唯一真相，hash 只是它的镜像。所有
// 切视图的地方统一走 goView()，它负责「改 state → 同步 hash → 存旧视图态 →
// 渲染 → 还原新视图态」。
// ===========================================================================

const NAV_INDEX = (() => {
  const m = {};
  NAV_ITEMS.forEach((n) => { m[n.id] = n; });
  return m;
})();
function navLabel(id) { return (NAV_INDEX[id] && NAV_INDEX[id].label) || id; }
function navGroup(id) { return (NAV_INDEX[id] && NAV_INDEX[id].group) || ""; }

// ---- 视图状态（筛选条件 / 滚动位置）--------------------------------------
// 为什么要「切走再切回要保留」：会计在凭证页按摘要搜出 20 张，翻两页看完去
// 明细账对一下，再回来时搜索词、状态筛选、翻到第几页全被清空，只能重打一遍。
// renderMain() 每次重建 #main 并重新调用视图函数，各视图的 DOM 值是全新的，
// 所以必须在**离开时快照、进入时还原**。
//
// 存什么：视口里所有有 id 的 input/select/textarea 的值 + 滚动位置。
// 不存 checkbox 的选中态（很多是「全选」这种瞬时态，存了反而怪）。
// 局部变量驱动的视图（像凭证列表的 q/status 直接读 DOM）能自动受益。
const viewState = Object.create(null);
const SCROLL_KEY = "__scroll__";
function saveViewState(id, main) {
  if (!main || !id) return;
  const snap = { v: 1, fields: {}, scroll: main.scrollTop || 0 };
  $all("input[id], select[id], textarea[id]", main).forEach((el) => {
    if (el.type === "checkbox" || el.type === "radio" || el.type === "file") return;
    // 只存有意义的值：空值不必占地方
    if (el.value === "") return;
    // `__` 开头的选项是**入口跳转哨兵**（导入页 __bank「转到银行对账」），不是用户
    // 选的值。存进快照 → 回到该页 restoreViewState 还原它、并在 300ms 后给它派发
    // change → syncKind 见到 __bank 又跳一次 → 离开时再存回 __bank：一进「数据导入」
    // 就被拽去银行对账、永远回不来（2026-10-06 用户反馈）。哨兵一律不进快照。
    if (el.tagName === "SELECT" && el.value.indexOf("__") === 0) return;
    snap.fields[el.id] = el.value;
  });
  viewState[id] = snap;
}
function restoreViewState(id, main) {
  const snap = viewState[id];
  if (!snap || !main) return;
  // 视图函数是 async 的：它在 await 之后才填表，立刻还原只能还原到骨架上。
  // 所以还原分两轮：先还原同步渲染出来的（工具栏筛选框），再在微任务 +
  // 短延时后各来一次，覆盖 await 之后才出现的（列表区的筛选框）。
  const apply = () => {
    if (state.view !== id) return;
    if (main !== document.getElementById("main")) return;
    Object.keys(snap.fields).forEach((k) => {
      const el = document.getElementById(k);
      if (!el) return;
      if (el.tagName === "SELECT" || el.tagName === "TEXTAREA" || el.type === "text" || el.type === "search" || el.type === "month" || !el.type) {
        if (el.value !== snap.fields[k]) el.value = snap.fields[k];
      }
    });
    if (snap.scroll) main.scrollTop = snap.scroll;
  };
  apply();
  Promise.resolve().then(apply);
  setTimeout(apply, 60);
  setTimeout(apply, 260);
  // 改了筛选值要触发视图自己的查询，否则只是把框填上了、数据没跟着变。
  // 用 'input' 事件（change 对 text 不触发）且只在值真的变了才发，避免死循环。
  setTimeout(() => {
    if (state.view !== id) return;
    Object.keys(snap.fields).forEach((k) => {
      const el = document.getElementById(k);
      if (!el || el.value !== snap.fields[k]) return;
      el.dispatchEvent(new Event("change", { bubbles: true }));
    });
  }, 300);
}

// ---- hash 路由 ------------------------------------------------------------
let suppressHash = false;
function readHashView() {
  const h = (location.hash || "").replace(/^#\/?/, "").trim();
  if (!h) return null;
  // 允许 #/view/子路径（未来用），只取第一段
  const id = h.split("/")[0];
  return VIEWS[id] ? id : null;
}
function syncHash(id) {
  const want = "#/" + id;
  if (location.hash === want) return;
  suppressHash = true;
  // replaceState 而不是直接赋值 location.hash：直接赋值会往历史里塞一条，
  // 于是「后退」变成「后退两步」这种更难解释的行为。首屏用 replace，之后
  // 由 goView 用 pushState。
  history.replaceState(history.state, "", want);
  setTimeout(() => { suppressHash = false; }, 0);
}
function goView(id, opts = {}) {
  if (!id || !VIEWS[id]) return;
  if (state.view === id && !opts.force) { renderMain(); return; }
  // 离开旧视图：存它的筛选/滚动
  const old = document.getElementById("main");
  if (old) saveViewState(state.view, old);
  state.view = id;
  if (!opts.fromPop) {
    // push 一条历史，后退键才有意义
    const want = "#/" + id;
    if (location.hash !== want) {
      suppressHash = true;
      history.pushState({ view: id }, "", want);
      setTimeout(() => { suppressHash = false; }, 0);
    }
  }
  touchRecent(id);
  // shell 没建好时不要动页签：登录 / 建账流程里也会走 goView，
  // 那时 openTabs 还是空数组，activateTab 会把一个残缺的集合写进 localStorage
  if (shellBuilt) activateTab(id);
  renderSidebar();
  renderMain();
  restoreViewState(id, document.getElementById("main"));
}
window.addEventListener("popstate", () => {
  if (suppressHash) return;
  const id = readHashView();
  if (!id) return;
  if (id === state.view) return;
  const old = document.getElementById("main");
  if (old) saveViewState(state.view, old);
  state.view = id;
  if (shellBuilt) activateTab(id);
  renderSidebar();
  renderMain();
  restoreViewState(id, document.getElementById("main"));
});
// 登录/切换账套后强制回工作台
// **必须连 hash 一起重置**：否则上一个账套停在 #/vouchers 的用户，切换账套后
// 会落在新账套的凭证页上——而他刚换账套，一条新凭证都还没有，看起来像坏了。
let bootHashOff = false;
function resetToDashboard() {
  state.view = "dashboard";
  viewState.dashboard = null;
  bootHashOff = true;
  syncHash("dashboard");
}

// 登录前用户可能带着上次的 hash（退出登录不清 hash），登进来要能落在那个页面
function applyHashOnBoot() {
  if (bootHashOff) { bootHashOff = false; return; }
  const id = readHashView();
  if (id && id !== "dashboard") state.view = id;
  else syncHash(state.view);
}

// ---- 侧栏：搜索 / 收藏 / 最近 -------------------------------------------
// 69 个功能平铺在 16 组里，Ctrl+K 只搜数据不搜功能。会计每天实际只用 4-5 个
// 页面。三个改动都是纯前端（localStorage），不碰后端权限判定：
//   · 搜索框：按功能名/分组名过滤。**空查询时展示全部**——这一点是硬要求，
//     E2E 全靠 `.nav-item[data-view=...]` 点击进入页面，隐藏了就全超时。
//   · 收藏（星标）：点导航项右侧的 ☆ 置顶成「常用」。
//   · 最近：最近 8 个去过的页面，去重后在收藏下面。**原分组里那一份保留** —— 最近只是
//     多给一个入口，不把菜单项搬走（2026-10-06 用户反馈：点过之后导航项「消失」了）。
//
// ⚠️ 「收藏 / 最近」必须做两道过滤，缺任何一道都是权限泄漏（实测发现）：
//   1. **按 isNavVisible 过滤**。原来只判 `NAV_INDEX[id]` 存在，于是出纳能在
//      「最近」里看到并点进「期初建账 / 固定资产 / 期末处理」——这三个都要求
//      他没有的权限（opening / account_edit / period_close），点进去只得到
//      「权限不足」。主列表一直是正确过滤的，只有这条路径漏了。
//   2. **按登录账号分区**。原来所有账号共用 `nav_fav` / `nav_recent` 两个 key，
//      同一台电脑上会计访问过的模块，出纳登录后一眼看到 —— 既是入口泄漏，
//      也是隐私泄漏（能看到同事在做什么）。
const LS_SEC = "nav_sec_collapsed";
const MAX_RECENT = 8;

// ---- 精简模式：默认只铺「日常真用得到」的十几个功能 -----------------------
//
// 69 个功能平铺 16 组，新用户建完账打开软件，第一眼看到的是 MRP、工艺路线、
// 预算预警这类跟他完全无关的东西 —— 他要找「期初建账」得在一屏文字里翻。
// 桌面端已经这么做了（NavItem::daily_core），这里对齐。
//
// **三条不变量**，少一条这个模式就变成了「功能被删了」：
//
//   1. **搜索框永远搜全部**。69 项里挑 12 项出来靠的是「藏起来，不是拿掉」：
//      任何人打任意功能名都能搜到并跳过去。搜索时的数据源是全量 NAV_ITEMS，
//      不看这个开关 —— 否则精简模式就成了功能阉割。
//   2. **路由完全不受影响**。goView(hash) 该去哪去哪，精简只决定侧栏画不画出来。
//   3. **开关随时可切**，且按账号分区（跟收藏/最近一致）—— 一个会计用精简，
//      不该影响同一台电脑上出纳看到的菜单。
//
// 默认开：新用户受益最大。老用户不清空 localStorage 就维持原样（见 simpleNav）。
// key 用 acctKey() 分区，和收藏/最近一致。

// 日常真正会用到的功能，与桌面端 state.rs 的 NavItem::daily_core 对齐。
//
// 这里是 16 项而不是桌面端的 12 项，差异来自 Web 端把报表拆成了多个导航项
// （balance-sheet / income-statement / cash-flow / reports 各占一项），
// 只留「报表中心」一个的话，新手进利润表得点两次。两边覆盖的功能集合是
// 等价的，只是切分粒度不同 —— 别为了凑数字把报表藏起来。
//
// 另外多留了 aux / imports / templates 三项：建账期和日常都要反复进，
// 藏起来等于让新手每次都去搜索框里打「辅助」两个字。
const SIMPLE_NAV = [
  "dashboard",      // 工作台
  "vouchers",       // 记账凭证（录凭证 + 查询）
  "begin",          // 期初建账
  "accounts",       // 会计科目
  "aux",            // 辅助档案
  "ledger",         // 明细账
  "reports",        // 报表中心
  "balance-sheet",  // 资产负债表
  "income-statement", // 利润表
  "cash-flow",      // 现金流量表
  "period-end",     // 期末处理
  "bank",           // 银行对账
  "settle",         // 往来核销
  "imports",        // 数据导入
  "templates",      // 凭证模板
  "backup",         // 备份恢复
];

/** 精简模式是否开启。**没有存过就默认开**（新用户），存过就按用户的选择。
 *  用 null 而不是 false 区分「从没设置过」和「用户显式关掉了」——
 *  后者必须被尊重，否则用户每次关掉刷新后又自己开回来。 */
function simpleNav() {
  const v = lsGet(acctKey("nav_simple"), null);
  return v === null ? true : !!v;
}

function lsGet(k, d) { try { const v = localStorage.getItem(k); return v ? JSON.parse(v) : d; } catch (e) { return d; } }
function lsSet(k, v) { try { localStorage.setItem(k, JSON.stringify(v)); } catch (e) {} }
// 账号分区的 key：`nav_fav@acc` / `nav_recent@cash`。未登录时退回匿名分区，
// 避免把上一个账号的数据算到新账号头上。
function acctKey(base) {
  const u = (session.user && (session.user.username || session.user.display_name)) || "_anon";
  return base + "@" + u;
}
function getFavs() {
  return lsGet(acctKey("nav_fav"), []).filter((id) => NAV_INDEX[id] && isNavVisible(NAV_INDEX[id]));
}
function toggleFav(id) {
  const f = lsGet(acctKey("nav_fav"), []).filter((x) => NAV_INDEX[x]);
  const i = f.indexOf(id);
  if (i >= 0) f.splice(i, 1); else f.unshift(id);
  lsSet(acctKey("nav_fav"), f);
}
function getRecent() {
  const favs = getFavs();
  return lsGet(acctKey("nav_recent"), [])
    .filter((id) => NAV_INDEX[id] && isNavVisible(NAV_INDEX[id]) && favs.indexOf(id) < 0);
}
function touchRecent(id) {
  if (!NAV_INDEX[id] || !isNavVisible(NAV_INDEX[id]) || id === "dashboard") return;
  let r = lsGet(acctKey("nav_recent"), []).filter((x) => NAV_INDEX[x]);
  r = r.filter((x) => x !== id);
  r.unshift(id);
  lsSet(acctKey("nav_recent"), r.slice(0, MAX_RECENT));
}
function isNavVisible(n) {
  const pa = !!session.platformAdmin;
  if (n.perm && !can(n.perm)) return false;
  if (n.admin && !(session.user && session.user.is_admin)) return false;
  if (n.platform && !pa) return false;
  return true;
}

// 侧栏搜索关键词（不持久化：刷新后回到全量展示，符合 E2E 与直觉）
let navQuery = "";
// 搜索输入防抖定时器：避免每次按键都重渲染全量 69 项导航
let navSearchTimer = null;
function navMatches(n, q) {
  if (!q) return true;
  if (n.label.indexOf(q) >= 0) return true;
  if (n.group && n.group.indexOf(q) >= 0) return true;
  return false;
}
function renderSidebar() {
  const box = document.getElementById("sidebar");
  if (!box) return;
  const q = navQuery.trim();
  const all = NAV_ITEMS.filter(isNavVisible);
  // 搜索结果**永远来自全量** all，不受精简模式影响。
  // 这是精简模式的第 1 条不变量：藏起来 ≠ 拿掉。打「MRP」必须搜得到，
  // 否则这个模式就成了功能阉割（而不是「新手少看到无关的东西」）。
  const hit = all.filter((n) => navMatches(n, q));
  const simple = !q && simpleNav();
  // 铺开列表：精简态只铺 16 项，非精简态铺全量。
  const listed = simple ? all.filter((n) => SIMPLE_NAV.includes(n.id)) : all;
  const hiddenCount = all.length - listed.length;
  const favs = getFavs().map((id) => NAV_INDEX[id]).filter((n) => n && navMatches(n, q));
  const recents = getRecent().map((id) => NAV_INDEX[id]).filter((n) => n && navMatches(n, q));
  const collapsed = lsGet(LS_SEC, {});

  // 第三个参数 cls 是这一份的 class：原分组/搜索结果用默认的 nav-item，
  // 「最近」那一份传 nav-jump。**不能都用 nav-item** —— 同一个 data-view 出现两份，
  // `.nav-item[data-view=x]`（E2E 里到处是这个写法）会一次匹配两个：Playwright 严格
  // 模式直接报错，`toHaveCount(1)` 也变成 2。
  const itemHtml = (n, extra, cls) => {
    const on = n.id === state.view;
    return `<div class="nav-fav-row">
      <button class="${cls || "nav-item"}${on ? " active" : ""}" data-view="${n.id}"${on ? ' aria-current="page"' : ""} title="${esc(n.label)}${n.group ? " · " + esc(n.group) : ""}">${esc(n.label)}</button>
      <button class="nav-star${getFavs().includes(n.id) ? " on" : ""}" data-fav="${n.id}" title="${getFavs().includes(n.id) ? "取消常用" : "加为常用"}（点导航项右侧星星）" aria-label="把${esc(n.label)}加为常用">${getFavs().includes(n.id) ? "★" : "☆"}</button>
    </div>${extra || ""}`;
  };
  const secHtml = (key, title, body, count) => {
    if (!body) return "";
    const col = !!collapsed[key];
    return `<div class="nav-sec${col ? " collapsed" : ""}" data-sec="${key}" role="button" tabindex="0" aria-expanded="${col ? "false" : "true"}">
        <span class="caret">▼</span><span class="u-grow">${esc(title)}</span>${count ? `<span class="ss-count">${count}</span>` : ""}
      </div><div class="nav-body">${body}</div>`;
  };

  let html = `<div class="side-search">
      <div class="ss-wrap">
        <span class="ss-icon">${icon("search")}</span>
        <input id="nav-q" type="search" placeholder="搜功能（如：明细账）" value="${esc(navQuery)}" autocomplete="off" aria-label="搜索功能" />
        ${q ? `<button class="ss-clear" id="nav-q-clear" title="清空" aria-label="清空搜索">✕</button>` : ""}
      </div>
      ${q ? `<div class="ss-count">匹配 ${hit.length} / ${all.length} 个功能</div>` : ""}
      <label class="ss-simple" title="${simple
        ? "精简模式：只铺日常用得到的功能，其余 ${hiddenCount} 个仍可用上方搜索框找到"
        : "已显示全部 ${all.length} 个功能"}">
        <input type="checkbox" id="nav-simple" ${simple ? "checked" : ""} />
        <span>${simple ? `精简模式（${all.length} 个里只显示 ${listed.length} 个）` : `显示全部功能（${all.length} 个）`}</span>
      </label>
    </div>`;

  if (favs.length) html += secHtml("fav", "常用", favs.map((n) => itemHtml(n)).join(""), favs.length);
  if (recents.length) html += secHtml("recent", "最近", recents.map((n) => itemHtml(n, "", "nav-jump")).join(""), recents.length);

  if (q) {
    // 搜索态：只列命中项，不再按分组铺开（否则「搜明细账」还是一大屏）
    if (!hit.length) {
      html += `<div class="nav-none">没有匹配「${esc(navQuery)}」的功能</div>`;
    } else {
      // 搜索结果里**也要给星标**：用户正是「搜出来 → 觉得常用 → 收藏」这个
      // 路径找到功能的，只在全量列表给星标等于把这条路堵死。
      html += `<div class="nav-body">${hit.map((n) => itemHtml(n)).join("")}</div>`;
    }
  } else {
    // 全量：按分组铺开（改造前就是这个行为，E2E 依赖）
    // 每个分组包在自己的 .nav-body 里，折叠靠 `.nav-sec.collapsed + .nav-body`
    // 的相邻选择器生效——所以项必须在 body 内部，不能平铺在 header 后面。
    // 只排「收藏」：常用是用户显式的置顶，移出原分组说得通；「最近」是**被动**记的，
    // 把它从原分组排掉就等于「点一下菜单项，菜单项就没了」（2026-10-06 用户反馈）。
    const rest = listed.filter((n) => !favs.includes(n));
    const groups = [];
    const gmap = {};
    rest.forEach((n) => {
      const g = n.group || "";
      if (!gmap[g]) { gmap[g] = []; groups.push(g); }
      gmap[g].push(n);
    });
    groups.forEach((g) => {
      const col = !!collapsed["g:" + g];
      html += `<div class="nav-sec${col ? " collapsed" : ""}" data-sec="g:${esc(g)}" role="button" tabindex="0" aria-expanded="${col ? "false" : "true"}">
          <span class="caret">▼</span><span class="u-grow">${esc(g)}</span><span class="ss-count">${gmap[g].length}</span>
        </div><div class="nav-body">${gmap[g].map((n) => itemHtml(n)).join("")}</div>`;
    });
    if (!groups.length) html += `<div class="nav-none">没有可见的功能</div>`;
  }
  box.innerHTML = html;

  const qi = document.getElementById("nav-q");
  if (qi) {
    qi.addEventListener("input", () => {
      navQuery = qi.value;
      const pos = qi.selectionStart;
      // 防抖：连续输入时只在停止 150ms 后重渲染一次侧栏
      if (navSearchTimer) clearTimeout(navSearchTimer);
      navSearchTimer = setTimeout(() => {
        navSearchTimer = null;
        renderSidebar();
        const again = document.getElementById("nav-q");
        if (again) { again.focus(); try { again.setSelectionRange(pos, pos); } catch (e) {} }
      }, 150);
    });
    // Ctrl+K / Ctrl+F 之外的入口：在侧栏里按 / 直接聚焦搜索
    qi.addEventListener("keydown", (e) => {
      if (e.key === "Escape") { navQuery = ""; renderSidebar(); }
      if (e.key === "Enter") {
        const first = $(".nav-item[data-view]", box);
        if (first) { goView(first.dataset.view); const f = document.getElementById("nav-q"); if (f) f.focus(); }
      }
    });
  }
  const clr = document.getElementById("nav-q-clear");
  const simpleBox = document.getElementById("nav-simple");
  if (simpleBox) {
    simpleBox.addEventListener("change", () => {
      lsSet(acctKey("nav_simple"), simpleBox.checked);
      // 不重载整个应用：只重画侧栏。切换显示偏好不该打断正在填的表单。
      renderSidebar();
    });
  }
  if (clr) clr.onclick = () => { navQuery = ""; renderSidebar(); const f = document.getElementById("nav-q"); if (f) f.focus(); };
  // 分组折叠 / 收藏：事件委托挂在侧栏容器上（每次重绘只绑一次）
  $all("[data-sec]", box).forEach((el) => {
    el.addEventListener("click", () => {
      const key = el.dataset.sec;
      const c = lsGet(LS_SEC, {});
      c[key] = !c[key];
      lsSet(LS_SEC, c);
      renderSidebar();
    });
  });
  $all("[data-fav]", box).forEach((el) => {
    el.addEventListener("click", (e) => { e.stopPropagation(); toggleFav(el.dataset.fav); renderSidebar(); });
  });
}

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
  // 按钮现在是 icon-only（顶栏空间有限），档位文字放进 title/aria-label，
  // 不要再写进 textContent —— 写了会把 30px 宽的按钮撑成两行。
  if (el) {
    el.textContent = "Aa";
    el.title = `界面字号：${d.label}（点击切换）`;
    el.setAttribute("aria-label", `界面字号：${d.label}`);
  }
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
// 待办提示点：与未读数字分开，避免「全部已读」清不掉数字
function ntTodoDot(n) {
  const b = document.getElementById("bell");
  if (b) b.dataset.todo = n > 0 ? "1" : "0";
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
    // 铃铛数字只算「未读消息」。待办是「该干的活」不是「没看过的消息」——
    // 两者混在一个数字里，「全部已读」就永远清不到 0，看着像按钮坏了。
    // 待办改用铃铛上的小红点单独提示，不占未读计数。
    const unread = r.unread_events || 0;
    const todoN = r.todo_count != null ? r.todo_count : (r.todos || []).filter((t) => t.count > 0).length;
    const was = parseInt(document.getElementById("bell-n")?.textContent || "0", 10) || 0;
    ntBadge(unread);
    ntTodoDot(todoN);
    if (!first && unread > was) {
      ntPulse();
      const now = Date.now();
      if (now - _ntToastAt > 5 * 60 * 1000) {
        _ntToastAt = now;
        toast(`通知：新动态 ${r.unread_events} 条${todoN ? ` · 待办 ${todoN} 项` : ""}`, "ok");
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
  // 待办不画未读点：它不是「没读过的消息」，是自己的活儿。数量在右侧标签上。
  let html = todos.length
    ? `<div class="nt-sec">我的待办（点击处理）</div>` + todos.map((t) => `
        <button class="nt-item" data-nt-view="${esc(t.view)}"><div class="nt-t"><b>${esc(t.label)}</b><span class="tag" style="margin-left:auto">${t.count}</span></div><div class="nt-m">${esc(t.domain)} · 直达处理页</div></button>`).join("")
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
        <div class="nt-item"><div class="nt-t">${ev.unread ? '<span class="nt-dot"></span>' : ""}<b>${esc(ev.user || "系统")}</b> ${esc(ev.action)}<span class="muted" style="margin-left:auto;font-size:11px">${esc((ev.ts || "").slice(11, 16))}</span></div><div class="nt-m">${esc(ev.module)} · ${esc(ev.detail)}</div></div>`).join("")
    : "";
  html += G(groups.today, "今天") + G(groups.yday, "昨天") + G(groups.older, "更早");
  if (!html) html = `<div class="nt-empty">没有待办，也暂无动态</div>`;
  body.innerHTML = html;
  $all("[data-nt-view]", body).forEach((b) => b.onclick = () => {
    ntCloseDrawer();
    goView(b.dataset.ntView);
  });
}
async function ntMarkRead() {
  // 已读 = 水位推到服务端当前时间（notices 返回的 now 与日志同钟同格式）。
  // 推完水位必须**重新取数重渲染**：未读点由服务端按水位逐条下发，
  // 早先版本用 inline style 遮 .nt-dot，下次重渲染就全露回来，看着像按钮没生效。
  try {
    const r = await api(`/notices?since=${encodeURIComponent(ntMark())}`);
    if (!r || !r.now) return;
    ntSetMark(r.now);
    const after = await api(`/notices?since=${encodeURIComponent(ntSet(r.now))}`);
    ntBadge(after.unread_events || 0);
    ntTodoDot(after.todo_count != null ? after.todo_count : 0);
    ntRender(after);
    toast("已全部标为已读", "ok");
  } catch (e) { toast(e.message, "err"); }
}
function ntSet(ts) { ntSetMark(ts); return ts; }
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

// ===========================================================================
// B. 通用 DOM 后处理（一次修好 69 个视图的共性问题）
// ---------------------------------------------------------------------------
// 逐个视图手改 69 遍既不现实也高风险（改错一处就是某个页面白屏，而且没人
// 能说清是哪次改动引入的）。这里改成**渲染后统一扫一遍 DOM**，凡是符合
// 约定结构的元素都自动套上。约定只有两条，且**默认全部关闭**：
//   · <td class="num"> 里的数字 → 负数红字、零值淡化（无条件生效，纯装饰）
//   · 空 tbody / 占位行 → 结构化空态（只替换"纯文字占位行"，绝不动带按钮的行）
//   · data-more="N" / data-rowmenu="N" → 溢出收纳（**必须显式标注**）
// 为什么溢出收纳不默认开：E2E 大量用 `page.click("#某个按钮")`，按钮一旦被
// 收进折叠菜单就不可见，Playwright 会一直等到超时。默认开 = 必然打破测试。
// ===========================================================================

// ---- B1. 数字排版：负数红字 / 零值淡化 ---------------------------------
// 改造前 .num 只有右对齐 + 等宽数字，方向要靠读负号。财务界面靠「负红正黑」
// 一眼分方向，缺这条就得逐个数字确认。
const NUM_RE = /-?\d[\d,]*\.?\d*/;
function autoNumbers(root) {
  const r = root || document.getElementById("main");
  if (!r) return;
  $all("td.num, th.num", r).forEach((el) => {
    el.classList.remove("neg", "zero");
    // 只看第一个数字：摘要/备注列里如果有「-」但不在数字前（如「科目-差额」）不算
    const txt = (el.textContent || "").trim();
    const m = txt.match(NUM_RE);
    if (!m) return;
    const raw = m[0];
    const n = parseFloat(raw.replace(/,/g, ""));
    if (!isFinite(n)) return;
    // 前缀是负号（允许 ¥ $ ( 等货币符号与括号记负法在前面）
    const before = txt.slice(0, m.index).replace(/[\s¥￥$€£,]/g, "");
    const isNeg = raw.startsWith("-") || /\($/.test(before) || /^\(/.test(txt.replace(/[\s¥￥$€£,]/g, ""));
    if (isNeg && n !== 0) el.classList.add("neg");
    else if (n === 0) el.classList.add("zero");
  });
}

// ---- B2. 空态：把裸 tbody / 占位行换成结构化空态 -------------------------
// 改造前 .empty-state 组件只被凭证页用了 1 次，其余 68 个视图空数据时是裸表格
// 或永远「加载中…」，用户分不清「没数据」「加载失败」「没权限」。
//
// 只在**确定是空**时才动，两种判定：
//   1. tbody 完全没有子元素；
//   2. tbody 只有一个 tr、只有一个 colspan 单元格、**里面没有按钮/输入框**，
//      且文字是已知占位词（「暂无数据」…）。
// 条件 2 里的「没有按钮」是关键：像 loadVouchers 的失败态会放一个「重试」
// 按钮，那种行绝不能被覆盖——覆盖了就等于把重试入口删了。
//
// ⚠️「加载中…」**不在**占位词里，这是本轮修掉的一个真 bug。
// 「加载中」是**待定**状态，不是空、也不是失败。把它当空态处理会渲染出
// 「⚠ 加载失败 / 数据没能取回来，可能是网络问题或会话过期 / 重试」——
// 而那一刻**什么都没失败**，请求还在飞。实测出纳打开「固定资产」（无
// account_edit 权限）时 permanently 停在这个假错误态：上面一行红字写着
// 「没有「科目维护」权限」，下面跟着一个永远点不成功的「重试」，用户会去
// 排查根本不存在的网络问题。
// 真正的失败由各视图自己的 catch 报（api() 抛错时不会走到空态处理）。
const LOADING_PLACEHOLDERS = /^(加载中|加载中…|加载中\.\.\.|加载)$/;
const EMPTY_PLACEHOLDERS = /^(暂无数据|无数据|没有数据|暂无|空空如也|—|-|)$/;
function emptyStateHtml(table, mode, detail) {
  const t = (table.dataset || {}) ;
  const title = t.emptyTitle || (mode === "error" ? "加载失败" : mode === "noperm" ? "没有权限" : "暂无数据");
  const hint = t.emptyHint || (mode === "error" ? "数据没能取回来，可能是网络问题或会话过期。" : "");
  const iconCh = mode === "error" ? "⚠" : mode === "noperm" ? "🔒" : "◌";
  const actions = [];
  if (mode !== "error") {
    // 通用建议动作：去导入 / 去新建，取决于表上有没有挂 data-empty-action
    const act = t.emptyAction;
    if (act) actions.push(`<button class="btn ghost sm" data-empty-go="${esc(act)}">${esc(t.emptyActionLabel || "前往")}</button>`);
  }
  actions.push(`<button class="btn ghost sm" data-empty-retry>重试</button>`);
  return `<div class="empty-state${mode === "error" ? " is-error" : mode === "noperm" ? " is-noperm" : ""}">
      <div class="es-icon">${iconCh}</div>
      <div class="es-title">${esc(title)}</div>
      ${hint ? `<div class="es-hint">${esc(hint)}</div>` : ""}
      ${mode === "error" && detail ? `<div class="es-detail">${esc(detail)}</div>` : ""}
      <div class="es-actions">${actions.join("")}</div>
    </div>`;
}
function autoEmptyRows(root) {
  const r = root || document.getElementById("main");
  if (!r) return;
  // 页面已经说了「没权限/出错」时也不补空态 —— 判定已下沉到占位词分类上：
  // 「加载中…」不再是占位词（见 EMPTY_PLACEHOLDERS 上方注释），所以无权限的
  // 页面不会再被叠上一个「⚠ 加载失败 + 重试」。
  $all("table.grid", r).forEach((table) => {
    if (table.dataset.noEmpty) return;
    const tb = table.tBodies[0];
    if (!tb) return;
    // ① 空 tbody
    if (tb.children.length === 0) {
      const cols = (table.tHead && table.tHead.rows[0] ? table.tHead.rows[0].cells.length : 1) || 1;
      tb.innerHTML = `<tr><td colspan="${cols}">${emptyStateHtml(table, "empty")}</td></tr>`;
      return;
    }
    // ② 单行占位
    if (tb.children.length !== 1) return;
    const tr = tb.children[0];
    if (tr.tagName !== "TR" || tr.children.length !== 1) return;
    const td = tr.children[0];
    if (td.tagName !== "TD") return;
    // 有交互元素 = 是有意义的行（重试按钮、跳转链接、错误详情），不动
    if (td.querySelector("button, a, input, select, textarea")) return;
    const txt = (td.textContent || "").trim();
    // 「还在加载」既不是空态也不是失败态：原样留着。视图自己的 catch 会把真正的
    // 错误写进摘要行；这里再补一个「加载失败 + 重试」只会说谎。
    if (LOADING_PLACEHOLDERS.test(txt)) return;
    if (!EMPTY_PLACEHOLDERS.test(txt)) return;
    const cols = (table.tHead && table.tHead.rows[0] ? table.tHead.rows[0].cells.length : 1) || 1;
    td.innerHTML = emptyStateHtml(table, "empty");
  });
}

// 空态里的按钮：点「重试」= 触发该视图自己的查询（点刷新按钮 / 回车搜索框）
document.addEventListener("click", (e) => {
  const retry = e.target.closest("[data-empty-retry]");
  if (retry) {
    e.preventDefault();
    const scope = retry.closest("#main") || document;
    const btn = $(".toolbar button[id$='-refresh'], .toolbar #v-refresh, .toolbar #r-go", scope);
    if (btn) { btn.click(); return; }
    // 没有显式刷新按钮就重跑当前视图
    const main = document.getElementById("main");
    if (main) { const fn = VIEWS[state.view]; if (fn) fn(main); }
    return;
  }
  const go = e.target.closest("[data-empty-go]");
  if (go && VIEWS[go.dataset.emptyGo]) { e.preventDefault(); goView(go.dataset.emptyGo); }
});

// ---- B3. 溢出收纳：工具栏「更多」/ 行内「⋯」----------------------------
// 改造前采购订单工具栏 8 个按钮、销售订单 9 个，全平铺一行，把「重排断号」
// 这类危险操作和「查询」摆在同一排同权重；行内更挤，整张表被按钮撑到要横滚。
//
// **必须显式标注 data-more / data-rowmenu 才会生效**，且标注时要确认：
//   ① 收进去的按钮没有 id 被 E2E 或外部按 id 引用；
//   ② 该按钮不是首屏唯一入口（收进菜单后要多点一次）。
// 危险项放菜单底部并加 .danger 分隔线，视觉上和日常操作分开。
function closeAllMenus() {
  $all(".tool-menu, .row-menu").forEach((m) => m.remove());
  $all(".menu-open-btn").forEach((b) => b.classList.remove("menu-open-btn"));
}
document.addEventListener("click", (e) => {
  // 点外面关掉所有弹出菜单
  if (!e.target.closest(".tool-more, .row-menu-wrap")) closeAllMenus();
});
document.addEventListener("keydown", (e) => { if (e.key === "Escape") closeAllMenus(); });

function buildMenu(items, cls) {
  const menu = document.createElement("div");
  menu.className = cls;
  items.forEach((el) => {
    if (el === "---") { menu.appendChild(document.createElement("hr")); return; }
    const b = document.createElement("button");
    b.type = "button";
    b.textContent = el.dataset.mlabel || el.textContent.trim();
    b.className = el.classList.contains("danger") ? "danger" : "";
    b.addEventListener("click", (ev) => {
      ev.stopPropagation();
      closeAllMenus();
      // 点菜单项 = 转发到原按钮的 click。事件监听器绑在元素本身，
      // 元素被搬进隐藏容器也不受影响，所以原按钮的功能完全不变。
      el.click();
    });
    menu.appendChild(b);
  });
  return menu;
}
// 危险项沉底并加分隔线（视觉上和日常操作分开）
function menuItems(rest) {
  const items = rest.slice();
  const i = items.findIndex((x) => x.classList.contains("danger"));
  if (i > 0) { const d = items.splice(i, 1); items.push("---", d[0]); }
  return items;
}
function autoOverflowMenus(root) {
  const r = root || document.getElementById("main");
  if (!r) return;
  // 工具栏：data-more="N" = 前 N 个保持平铺，其余收进「更多」
  $all('.toolbar[data-more]', r).forEach((tb) => {
    if (tb.dataset.moreDone) return;
    const keep = parseInt(tb.dataset.more, 10);
    if (!isFinite(keep) || keep < 0) return;
    const kids = Array.from(tb.children).filter((c) => c.tagName === "BUTTON" && !c.classList.contains("spacer"));
    if (kids.length <= keep + 1) return;
    const rest = kids.slice(keep);
    const wrap = document.createElement("div");
    wrap.className = "tool-more menu-open-btn";
    const btn = document.createElement("button");
    btn.className = "btn ghost sm";
    btn.type = "button";
    btn.textContent = "⋯";
    btn.title = "更多操作（" + rest.length + " 项）";
    btn.setAttribute("aria-haspopup", "true");
    btn.setAttribute("aria-label", "更多操作");
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      if (wrap.querySelector(".tool-menu")) { closeAllMenus(); return; }
      closeAllMenus();
      wrap.appendChild(buildMenu(menuItems(rest), "tool-menu"));
    });
    wrap.appendChild(btn);
    tb.appendChild(wrap);
    // 真正**搬走**：只 insertBefore 换个位置的话按钮还在工具栏里露着，
    // 菜单就成了「多出一份」的重复入口。这里搬进 hidden 容器（保留在 DOM，
    // 监听器与 id 都不丢），再用菜单里的副本转发点击。
    const src = document.createElement("div");
    src.hidden = true;
    tb.appendChild(src);
    rest.forEach((el) => src.appendChild(el));
    tb.dataset.moreDone = "1";
  });
  // 行内：td[data-rowmenu="N"] = 前 N 个按钮平铺，其余进「⋯」
  $all("td[data-rowmenu]", r).forEach((td) => {
    if (td.dataset.rowmenuDone) return;
    const keep = parseInt(td.dataset.rowmenu, 10);
    if (!isFinite(keep) || keep < 0) return;
    const btns = Array.from(td.children).filter((c) => c.tagName === "BUTTON");
    if (btns.length <= keep + 1) return;
    const rest = btns.slice(keep);
    const wrap = document.createElement("span");
    wrap.className = "row-menu-wrap menu-open-btn";
    const btn = document.createElement("button");
    btn.className = "btn ghost sm";
    btn.type = "button";
    btn.textContent = "⋯";
    btn.title = "更多操作（" + rest.length + " 项）";
    btn.setAttribute("aria-haspopup", "true");
    btn.setAttribute("aria-label", "更多操作");
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      if (wrap.querySelector(".row-menu")) { closeAllMenus(); return; }
      closeAllMenus();
      wrap.appendChild(buildMenu(menuItems(rest), "row-menu"));
    });
    wrap.appendChild(btn);
    td.appendChild(wrap);
    const src = document.createElement("span");
    src.hidden = true;
    td.appendChild(src);
    rest.forEach((el) => src.appendChild(el));
    td.dataset.rowmenuDone = "1";
  });
}

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
  const periodOpts = state.periods.map((p) => `<option value="${p}" ${p === state.current ? "selected" : ""}>${p}</option>`).join("");
  // 面包屑：改造前视图名只出现在 <main> 的 <h2> 里，顶栏看不出在哪。
  // 现在顶栏常驻「账套 / 分组 / 当前页」，截图发人、浏览器标签页都能看出位置。
  const crumb = (() => {
    const g = navGroup(state.view);
    const book = (state.bookOptions && state.bookOptions.company) || "";
    return `<div class="crumb" id="crumb">
        ${book ? `<span class="crumb-book" title="${esc(book)}">${esc(book)}</span>` : ""}
        ${book && g ? `<span class="crumb-sep">/</span>` : ""}
        ${g ? `<span>${esc(g)}</span><span class="crumb-sep">/</span>` : ""}
        <span class="crumb-cur">${esc(navLabel(state.view))}</span>
      </div>`;
  })();
  app.innerHTML = `
    <div class="app">
      <div class="topbar">
        <button class="hamburger" id="menu-btn" aria-label="打开菜单">☰</button>
        <span class="logo" id="logo-home" title="回到工作台">FinBook</span>
        <span class="who">${esc(u.display_name)}（${esc(u.role_label)}）</span>
        <select id="period-sel" title="会计期间">${periodOpts}</select>
        ${crumb}
        <span class="grow"></span>
        <div class="tb-actions">
        <button class="btn ghost sm icon-only" id="ui-scale-btn" title="界面字号（点击切换）" aria-label="界面字号">Aa</button>
        <button class="btn ghost sm bell" id="bell" title="通知（待办与动态）" aria-label="通知">🔔<span class="bell-n" id="bell-n" data-zero="1">0</span></button>
        <button class="btn ghost sm icon-only" id="help-btn" title="快捷键与帮助（按 ? 唤起）" aria-label="快捷键与帮助">?</button>
        <button class="btn ghost sm icon-only" id="switch-book" title="切换账套" aria-label="切换账套">⇄</button>
        <button class="btn ghost sm icon-only" id="change-pwd" title="修改口令" aria-label="修改口令">⚿</button>
        <button class="btn ghost sm icon-only" id="logout" title="退出登录" aria-label="退出登录">⏻</button>
        </div>
      </div>
      <div class="sidebar" id="sidebar"></div>
      <div class="tab-bar" id="tab-bar" role="tablist" aria-label="已打开的页面"></div>
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
  $("#logo-home").addEventListener("click", () => { closeMenu(); goView("dashboard"); });
  $("#menu-btn").addEventListener("click", toggleMenu);
  $("#help-btn").addEventListener("click", openShortcuts);
  $("#side-mask").addEventListener("click", closeMenu);
  // 事件委托：nav-item 只在 sidebar 容器上绑一次。
  // 注意 star（收藏）按钮在 nav-item 外面一层，必须先排掉，否则点星星会跳页面。
  $(".sidebar").addEventListener("click", (e) => {
    const btn = e.target.closest(".nav-item, .nav-jump");
    if (!btn) return;
    if (e.target.closest("[data-fav]")) return;
    closeMenu();
    goView(btn.dataset.view);
  });
  $("#period-sel").addEventListener("change", async (e) => {
    state.current = e.target.value;
    try { await api("/period", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ ymm: ymm(e.target.value) }) }); } catch (err) {}
    // 换期间 = 换数据范围，旧视图的筛选/页码不再适用：清掉快照重新起
    viewState[state.view] = null;
    goView(state.view, { force: true });
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
  // 侧栏内容交给 renderSidebar()（搜索/收藏/最近都在那里）
  renderSidebar();
  // 页签（这段以前是死代码，从没被调用过）
  initTabs();
  shellBuilt = true;
}

function render() {
  // session.user 有 perms = 已进入账套（PublicUser）；只有平台身份（PlatformUser）时停在账套选择页
  if (!session.user) { shellBuilt = false; showLogin(); return; }
  if (!session.user.perms) { shellBuilt = false; showBookPicker(); return; }
  if (!shellBuilt) renderShell();
  // 首屏：如果 URL 带了 #/view（刷新或同事发来的链接），落在那个页面上。
  // 放在 renderShell 之后：面包屑要用 shell 里的 DOM 节点。
  applyHashOnBoot();
  updateCrumb();
  renderMain();
}

// 顶栏面包屑：只改文字不重建 DOM（切视图时 shell 不重绘）
function updateCrumb() {
  const el = document.getElementById("crumb");
  if (!el) return;
  const g = navGroup(state.view);
  const book = (state.bookOptions && state.bookOptions.company) || "";
  el.innerHTML = `
    ${book ? `<span class="crumb-book" title="${esc(book)}">${esc(book)}</span>` : ""}
    ${book && g ? `<span class="crumb-sep">/</span>` : ""}
    ${g ? `<span>${esc(g)}</span><span class="crumb-sep">/</span>` : ""}
    <span class="crumb-cur">${esc(navLabel(state.view))}</span>`;
  document.title = `${navLabel(state.view)} · FinBook`;
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
  // 图表注册表按视图生命周期清空：旧视图的 chartStore 条目对应的 DOM 已经不存在，
  // 留着既浪费内存也会让 id 越滚越大
  chartStore.clear();
  updateCrumb();
  const fn = VIEWS[state.view] || viewDashboard;
  const r = fn(main);
  // 列偏好 / 排序 / 列宽 / title：作用于同步建出来的骨架，可以立刻做
  try { autoColPrefs(); } catch (e) {}
  try { autoColSort(); } catch (e) {}
  try { autoColResize(); } catch (e) {}
  try { autoCellTitles(); } catch (e) {}
  try { autoNumbers(); } catch (e) {}
  try { autoOverflowMenus(); } catch (e) {}
  // 空态识别必须**等视图的异步加载跑完**。
  //
  // `fn(main)` 返回的是 Promise，绝大多数视图在第一个 await 上就会先把
  // <tbody> 填成「加载中…」，再 await api()。原来这里不等，直接判空态，于是
  // 「加载中…」被当成"加载失败"渲染成「⚠ 加载失败 / 可能是网络问题 / 重试」——
  // 而那一刻请求还在飞，什么都没失败。数据回来后 api() 的成功路径会重跑一次
  // 空态识别把它覆盖掉，所以正常页面看不出来；**加载失败时就永久停在假错误态**：
  // 实测出纳打开「固定资产」（无 account_edit）同时看到「没有「科目维护」权限」
  // 和一个永远点不成功的「重试」。
  //
  // 改成等 Promise settle：数据到了就正常判空，没到就等 api() 自己那条路径。
  // 视图若抛错（同步 throw）也要走一遍，否则它自己渲染的失败态拿不到后处理。
  Promise.resolve(r).then(
    () => { try { autoEmptyRows(); } catch (e) {} },
    () => { try { autoEmptyRows(); } catch (e) {} }
  );
  return r;
}

async function logout() {
  try { await api("/logout", { method: "POST" }); } catch (e) {}
  closeAllModals();
  session.user = null;
  session.platformAdmin = false;
  render();
}
