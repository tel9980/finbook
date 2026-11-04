// FinBook Web 前端 — 首页与经营概览 + 金额/百分比格式化 + 图表 SVG 生成
//
// 这一份是 `crates/finweb/static/app.js` 拆分出来的（原单文件 14k 行、295 个顶层函数）。
// 拆分方式：**经典 script，按原始行序切分，不重排任何代码**——顶层 function/const 的
// 提升与声明顺序完全保持原样，所以跨文件不存在 TDZ/未定义问题；只有 `VIEWS` 路由表
// 因为引用全部视图函数，被挪到了最后加载的 app-hr.js。
// 加载顺序由 `index.html` 的 <script> 顺序决定（也是 `main.rs` 里 asset_version 的清单顺序）。
// 新增文件必须同步三处：index.html、main.rs 的 asset_version 清单、tools/lib-app-src.js 的解析（自动）。
// ===========================================================================
// ---------------------------------------------------------------------------
// 首页首屏：本期经营结果 + 钱在哪
//
// 这两块必须并排：利润是**账面**赚的，「钱在哪」是真正压在外面的钱。
// 小厂常见的死法是账面赚钱、钱全压在客户手里 —— 只看净利润看不出来。
// 数据全部来自 /api/dashboard 的 brief 字段（后端复用报表那条取数路径）。
// ---------------------------------------------------------------------------
function homeHero(d) {
  const b = d && d.brief;
  if (!b) return "";
  const np = moneyNum(b.net_profit);
  const ncls = np > 0 ? "pos" : np < 0 ? "neg" : "flat";
  const diff = moneyNum(b.net_profit_diff);
  const diffTxt = diff === 0
    ? "与上月持平"
    : `较上月 ${diff > 0 ? "▲" : "▼"} <b>${moneyFmt(Math.abs(diff))}</b>`;
  const marginTxt = b.margin == null ? "" : ` · 利润率 <b>${pctFmt(b.margin)}</b>`;
  const costTop = (b.cost_top || [])
    .map((c) => `<b>${esc(c.name)}</b> ${esc(c.amount)}`).join(" · ");
  const unposted = Number(b.unposted) > 0
    ? `<div class="hh-warn">⚠ ${Number(b.unposted)} 张凭证还没记账，未计入上面的数字</div>` : "";

  const m = b.money || {};
  const netR = !!m.net_is_receivable;
  const extra = [];
  if (moneyNum(m.advance_out) !== 0) extra.push(`预付供应商 ${esc(m.advance_out)}`);
  if (moneyNum(m.advance_in) !== 0) extra.push(`预收客户 ${esc(m.advance_in)}`);
  const overdue = Number(m.overdue_90_count) > 0
    ? `<div class="hh-warn">⚠ ${Number(m.overdue_90_count)} 家客户的钱超过 90 天没收回，共 ${esc(m.overdue_90)}，该催了</div>` : "";

  // 账龄：60 天黄、90 天红。老板扫一眼就知道先打哪个电话。
  const partyRow = (p) => {
    const days = Number(p.max_days) || 0;
    const dcls = days >= 90 ? "age w90" : days >= 60 ? "age w60" : "age";
    // 同一家既是客户又是供应商（宏源那种）：把另一边和净额一起写出来，
    // 否则老板看到两行「他欠我 15,000」「我欠他 28,000」还得自己心算。
    const both = p.both
      ? `<span class="both">另一方向 ${esc(p.also)}，净${p.net_is_receivable ? "他欠我" : "我欠他"} <span class="nett">${esc(p.net)}</span></span>`
      : "";
    return `<div class="hm-row">
      <span class="nm" title="${esc(p.name)}">${esc(p.name)}${both}</span>
      <span class="amt">${esc(p.amount)}</span>
      <span class="${dcls}">${days}天</span>
    </div>`;
  };
  const list = (arr, emptyTxt) => (arr && arr.length)
    ? arr.map(partyRow).join("")
    : `<div class="hm-empty">${emptyTxt}</div>`;

  return `
  <section class="home-hero">
    <div class="hh">
      <div class="hh-head"><span>本期经营结果</span><span>${esc(b.period)}</span></div>
      <div>
        <div class="hh-np ${ncls}"><span class="cur">¥</span>${esc(b.net_profit)}</div>
        <div class="hh-sub">${diffTxt}${marginTxt}</div>
      </div>
      <div class="hh-pair">
        <div><span class="k">营业收入</span><b>${esc(b.revenue)}</b></div>
        <div><span class="k">营业成本</span><b>${esc(b.cost)}</b></div>
        <div><span class="k">本年累计净利润</span><b>${esc(b.net_profit_ytd)}</b></div>
      </div>
      ${costTop ? `<div class="hh-cost">钱都花哪了：${costTop}</div>` : ""}
      ${unposted}
    </div>
    <div class="hh">
      <div class="hh-head"><span>钱在哪</span><span>别人欠我 / 我欠别人</span></div>
      <div class="hm-grid">
        <div class="hm"><span class="k">别人欠我</span><b class="pos">${esc(m.receivable)}</b></div>
        <div class="hm"><span class="k">我欠别人</span><b class="neg">${esc(m.payable)}</b></div>
        <div class="hm"><span class="k">手头现金</span><b>${esc(m.cash)}</b></div>
        <div class="hm"><span class="k">净头寸（${netR ? "别人欠我" : "我欠别人"}）</span><b class="${netR ? "pos" : "neg"}">${esc(m.net)}</b></div>
      </div>
      ${extra.length ? `<div class="hh-cost">另有：${extra.join(" · ")}</div>` : ""}
      <div class="hm-lists">
        <div><h5>欠我最多的客户</h5>${list(m.top_customers, "还没有赊账")}</div>
        <div><h5>我欠最多的供应商</h5>${list(m.top_suppliers, "没有欠款")}</div>
      </div>
      ${overdue}
    </div>
  </section>`;
}

// 我的工作台：账套状态（凭证数等固定卡）+ 按岗位权限动态拼装的
// 业务卡片 / 我的待办 / 多期趋势（GET /api/workbench，无新权限位）
// ===========================================================================
async function viewDashboard(main) {
  main.innerHTML = `<h2>我的工作台</h2><div class="muted">加载中…</div>`;
  // 三个接口互不依赖，必须并行发。串行等的时候，手机上 4G 每一次往返
  // 都是 150–300ms，首屏白等的时间就是三次 RTT 之和（实测服务端三段
  // 263ms，串行到用户手上接近 1 秒）。
  const [dRes, statusRes, wbRes] = await Promise.allSettled([
    api("/dashboard"),
    api("/setup/status"),
    api(`/workbench?periods=${Number(state.wbPeriods) || 12}`),
  ]);
  if (dRes.status !== "fulfilled") {
    const msg = (dRes.reason && dRes.reason.message) || String(dRes.reason);
    main.innerHTML = `<h2>我的工作台</h2><div class="muted" style="color:var(--err)">${esc(msg)}</div>`;
    return;
  }
  const d = dRes.value;
  const status = statusRes.status === "fulfilled" ? statusRes.value : { admin_set: false };
  let wb = null;
  if (wbRes.status === "fulfilled") wb = wbRes.value;
  else { const m = (wbRes.reason && wbRes.reason.message) || String(wbRes.reason); toast(`工作台数据加载失败：${m}`, "err"); }
  const adminBanner = status.admin_set
    ? `<div class="banner set">✅ 管理员账号已设定</div>`
    : `<div class="banner unset">🔧 管理员账号未设定 —— 首次成功登录的账号将自动成为系统管理员。</div>`;
  const nSel = `<label style="font-size:12px;margin-left:auto">趋势期数 <select id="wb-n">${
    [6, 12, 24].map((n) => `<option value="${n}" ${Number(state.wbPeriods || 12) === n ? "selected" : ""}>${n} 期</option>`).join("")
  }</select></label>`;
  const group = (arr) => { const m = {}; (arr || []).forEach((x) => { (m[x.domain] = m[x.domain] || []).push(x); }); return m; };
  let wbHtml = { todo: "", cards: "", trend: "", nSel: "", hint: "" };
  if (wb) {
    // 只渲染账套「用过」的业务域（`active` 由后端 findb::workbench::domain_active
    // 给出，桌面端读同一份）。小工厂只有一个人、一个全权限账号，按权限过滤等于
    // 什么都过滤不掉，「生产在产 0 份」「临期批次 0 个」会一直挂在首屏把真正要
    // 看的挤下去。判定口径是**历史累计有没有单据**，不是本月有没有——后者会让
    // 同一块区域时有时无，比一直显示更糊涂。
    const active = Array.isArray(wb.active) ? wb.active : null;
    const keep = (dom) => !active || active.includes(dom);
    const cg = group((wb.cards || []).filter((c) => keep(c.domain)));
    const tg = group((wb.trends || []).filter((t) => keep(t.domain)));
    // 被藏起来的域：与其让老板以为功能坏了，不如明说「没启用，要用去这里」
    const hidden = active
      ? [...new Set((wb.cards || []).map((c) => c.domain))].filter((d) => !active.includes(d))
      : [];
    const cardsHtml = Object.keys(cg).map((dom) => `
      <div class="wb-sec"><h4 class="sec-title">${esc(dom)}</h4>
        <div class="cards">${cg[dom].map((c) => `<div class="card"><div class="k">${esc(c.label)}</div><div class="v">${esc(c.value)}${c.unit === "元" || c.unit === "件" ? `<span style="font-size:12px;font-weight:400;opacity:.65"> ${esc(c.unit)}</span>` : ""}</div></div>`).join("")}</div>
      </div>`).join("");
    // 待办提到工作台最上面（原来排在两排卡片之后，要往下滚才看到）
    const todos = (wb.todos || []).filter((t) => keep(t.domain));
    const todoHtml = todos.length ? `
      <div class="wb-sec"><h4 class="sec-title">我的待办</h4>
        <div class="u-row wrap">${todos.map((t) => `
          <button class="btn ${t.count > 0 ? "primary" : "ghost"}" data-wb-go="${esc(t.view)}" ${t.count > 0 ? "" : "disabled"} style="display:inline-flex;gap:8px;align-items:center">${esc(t.label)}<b>${t.count}</b></button>`).join("")}
        </div>
      </div>` : "";
    const trendHtml = Object.keys(tg).map((dom) => `
      <div class="wb-sec"><h4 class="sec-title">${esc(dom)}</h4>
        <div style="display:grid;grid-template-columns:repeat(auto-fill,minmax(400px,1fr));gap:12px">
          ${tg[dom].map((t) => `<div class="panel" style="margin:0;padding:10px 12px">
            <div class="u-row"><b style="font-size:13px">${esc(t.title)}</b><span class="muted" style="font-size:12px">单位：${esc(t.unit)}</span></div>
            ${chartBox("line", t.title, t.periods, t.series.map((s) => ({ name: s.name, color: s.color, values: s.points })), 190, t.unit)}
          </div>`).join("")}
        </div>
      </div>`).join("");
    // 藏起来的域给一句说明：小厂最怕「昨天还在的东西今天不见了」，
    // 而这些模块本来就不在它的业务范围里（没仓管、没有生产工单、不做报销审批）。
    const hintHtml = hidden.length ? `
      <div class="wb-sec"><div class="u-dim u-sm">
        ${esc(hidden.join("、"))} 这一块本账套还没有用过，所以先不显示。
        要用的话照常去左侧菜单操作，录入第一张单据后这里就会出来。
      </div></div>` : "";
    // 分三块返回：待办（首屏）→ 账套参考值 → 业务卡与趋势。
    // 原来是一坨 wbHtml 顺序拼接，待办被两排参考卡片压在下面。
    wbHtml = { todo: todoHtml, cards: cardsHtml, trend: trendHtml, nSel, hint: hintHtml };
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
    ${homeHero(d)}
    ${wbHtml.todo}
    <details class="wb-ref" ${firstRun ? "" : "open"}>
      <summary class="u-dim u-sm">账套概况</summary>
      <div class="cards u-mt10">
        <div class="card"><div class="k">当前会计期间</div><div class="v">${esc(d.current_period)}</div></div>
        <div class="card"><div class="k">已结账至</div><div class="v">${esc(d.closed_upto || "未结账")}</div></div>
        <div class="card"><div class="k">公司名称</div><div class="v" style="font-size:16px">${esc(d.company || "—")}</div></div>
        <div class="card"><div class="k">凭证数</div><div class="v">${esc(d.vouchers)}</div></div>
        <div class="card"><div class="k">分录数</div><div class="v">${esc(d.entries)}</div></div>
        <div class="card"><div class="k">科目数</div><div class="v">${esc(d.accounts)}</div></div>
      </div>
    </details>
    <div class="toolbar" style="border:0;padding:6px 0">${wbHtml.nSel}</div>
    ${wbHtml.cards}
    ${wbHtml.hint}
    ${wbHtml.trend}`;
  if ($("#wb-n")) $("#wb-n").onchange = (e) => { state.wbPeriods = Number(e.target.value); viewDashboard(main); };
  $all("[data-wb-go]").forEach((b) => b.onclick = () => goView(b.dataset.wbGo));
  $all("[data-fr-go]").forEach((b) => b.onclick = () => goView(b.dataset.frGo));
}

// ===========================================================================
// 管理员 · 账目总览（只读视角，仅系统管理员可见）
// ===========================================================================

// 文件下载：不要用 window.open(url, "_blank")
//
// 导出端点都带 `Content-Disposition: attachment`，用 window.open 打开时
// ① 浏览器按「弹窗」处理，容易被拦掉——用户点了完全没反应，也不知道去哪找文件；
// ② 没被拦时新标签立刻下载完关掉，同样没有任何反馈。
// 同源 <a download> 走正常下载通道，触发下载且不新开标签，也不吃弹窗拦截。
function downloadUrl(url) {
  const a = document.createElement("a");
  a.href = url;
  a.rel = "noopener";
  a.style.display = "none";
  document.body.appendChild(a);
  a.click();
  setTimeout(() => a.remove(), 0);
}

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
function lineChartSvg(labels, series, height, opts) {
  const W = 760, H = height || 250;
  // padR 默认 14：X 轴标签是 text-anchor="middle"，最后一个标签以最右刻度为中心，
  // 会被 viewBox 右缘裁掉尾巴（"2026-12" 显示成 "2026-1"）。
  // 放大视图传大一点的 padR，因为那里恰恰是要看清刻度的地方。
  const padL = 62, padR = (opts && opts.padR) || 14, padT = 16, padB = 30;
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

  return `<svg viewBox="0 0 ${W} ${H}" preserveAspectRatio="${(opts && opts.par) || "none"}" class="${(opts && opts.cls) || "chart"}">${grid}${zeroAxis}${lines}${dots}${yticks}${xlabels}</svg>`;
}

// 分组柱状图（当月发生额）。groups: [{name, color, values, anomalies}]
function barChartSvg(labels, groups, height, opts) {
  const W = 760, H = height || 250;
  const padL = 62, padR = (opts && opts.padR) || 14, padT = 16, padB = 30;
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
  return `<svg viewBox="0 0 ${W} ${H}" preserveAspectRatio="${(opts && opts.par) || "none"}" class="${(opts && opts.cls) || "chart"}">${grid}${zeroAxis}${bars}${yticks}${xlabels}</svg>`;
}

// ===========================================================================
// 图表点击放大
// ---------------------------------------------------------------------------
// 走势图原来只有 170~240px 高，塞在仪表盘/报表的网格单元里，十几期的走势基本
// 分不出拐点，想细看只能去「报表中心」里那个有大图的页面。这里让图本身可点，
// 点开用同一份数据重画一张大的。
//
// 数据不塞进 data-* 属性（序列可能上百个点，转义后体积大且容易出错），
// 而是存进 chartStore，DOM 上只放一个自增 id。视图重建时 store 会被清空，
// 所以不会随切页面无限增长。
// ===========================================================================
let chartSeq = 0;
const chartStore = new Map();

function renderChart(kind, labels, data, height, opts) {
  return kind === "bar" ? barChartSvg(labels, data, height, opts) : lineChartSvg(labels, data, height, opts);
}

/**
 * 画一张可点击放大的图。
 * @param kind    "line" | "bar"
 * @param title   放大后的标题
 * @param labels  X 轴标签
 * @param data    series / groups
 * @param height  小图高度（px）
 * @param unit    放大后显示的单位
 */
function chartBox(kind, title, labels, data, height, unit) {
  const svg = renderChart(kind, labels, data, height);
  if (!svg) return "";
  const id = "ch" + (++chartSeq);
  chartStore.set(id, { kind, title, labels, data, unit, smallH: height });
  return `<div class="chart-box" data-chart="${id}" role="button" tabindex="0"
       aria-label="放大查看${esc(title)}" title="点击放大">${svg}<span class="chart-zoom">⤢ 点击放大</span></div>`;
}

function openChartBig(id) {
  const c = chartStore.get(id);
  if (!c) return;
  // 放大图换 preserveAspectRatio：默认的 "none" 会把图硬拉满容器、圆点被压成椭圆，
  // 坐标轴文字也会横向变形。放大正是要看清刻度，所以这里必须保持原比例。
  // padR 加大：X 轴标签是居中锚点，最后一个会被 viewBox 右缘裁掉尾巴。
  const big = renderChart(c.kind, c.labels, c.data, 420, { par: "xMidYMid meet", cls: "chart chart-big", padR: 46 });
  const legend = (c.data || []).filter((s) => s.name).map((s) =>
    `<span class="lg-item"><i style="background:${esc(s.color)}"></i>${esc(s.name)}</span>`).join("");
  const mask = modal(`<h3>${esc(c.title)}</h3>
    <div class="chart-big">
      <div class="chart-big-meta">
        <div class="lg">${legend}</div>
        ${c.unit ? `<span class="muted" style="font-size:12px">单位：${esc(c.unit)}</span>` : ""}
      </div>
      ${big}
      <div class="muted" style="font-size:12px;margin-top:8px">共 ${c.labels.length} 期 · Esc 关闭</div>
    </div>
    <div class="foot"><button class="btn ghost" data-chart-close>关闭</button></div>`, true);
  const cb = $("[data-chart-close]", mask);
  if (cb) cb.onclick = closeModal;
}

// 事件委托：图是渲染出来的，用委托避免每次挂完还得重绑
document.addEventListener("click", (e) => {
  const box = e.target.closest && e.target.closest("[data-chart]");
  if (box) { e.preventDefault(); openChartBig(box.dataset.chart); }
});
document.addEventListener("keydown", (e) => {
  if (e.key !== "Enter" && e.key !== " ") return;
  const box = e.target.closest && e.target.closest("[data-chart]");
  if (box) { e.preventDefault(); openChartBig(box.dataset.chart); }
});

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
