// FinBook Web 前端 — 经营总览、凭证列表与凭证编辑器
//
// 这一份是 `crates/finweb/static/app.js` 拆分出来的（原单文件 14k 行、295 个顶层函数）。
// 拆分方式：**经典 script，按原始行序切分，不重排任何代码**——顶层 function/const 的
// 提升与声明顺序完全保持原样，所以跨文件不存在 TDZ/未定义问题；只有 `VIEWS` 路由表
// 因为引用全部视图函数，被挪到了最后加载的 app-hr.js。
// 加载顺序由 `index.html` 的 <script> 顺序决定（也是 `main.rs` 里 asset_version 的清单顺序）。
// 新增文件必须同步三处：index.html、main.rs 的 asset_version 清单、tools/lib-app-src.js 的解析（自动）。
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
      ${chartBox("line", "累计收入与成本", labels, cumSeries, 230, "元")}
      <div class="chart-note">红线圆点 = 异常月份（偏离年内均值 ±2σ 或出现亏损）；折线为 1 月起累计值。</div>
    </div>
    <div class="panel" style="margin-top:14px">
      <div class="chart-head"><b>当月发生额（逐月对比）</b><span class="lg">${legend(monthGroups)}</span></div>
      ${chartBox("bar", "当月发生额", labels, monthGroups, 230, "元")}
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

    <h2 style="margin:24px 0 4px">业务运营总览</h2>
    <p class="muted" style="margin:0 0 10px">
      上半部分是会计口径（收入 / 成本 / 利润），这里是业务口径（订单 / 库存 / 生产 / 资金 / 往来）。
      数据取自已记账业务单据，含近 6 期走势；点「进入」跳到对应模块看明细。
    </p>
    <div id="ov-biz" class="muted">业务数据加载中…</div>

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
  loadBizOverview(main);
}

// 业务域 → 对应模块入口（点「进入」跳转）。找不到模块就不显示按钮，不给死链。
const BIZ_ENTRY = {
  "销售": "so-doc", "采购": "po-doc", "仓管": "inv-warehouse", "库存": "inv-warehouse",
  "生产": "mps", "资金": "funds", "成本": "cost", "凭证": "vouchers",
  "报销": "claims", "审批": "approval",
};
const BIZ_ORDER = ["凭证", "资金", "销售", "采购", "仓管", "生产", "成本", "报销", "审批"];

// 账目总览 · 业务运营总览（管理员只读）
//
// 数据全部来自既有端点，不新增查询口径：
// - /workbench         已按 9 个业务域出卡片 + 近 N 期走势，口径与工作台一致
// - /settle/aging      往来账龄（应收 1122 / 应付 2202），workbench 没有往来域，
//                       但对财务管理员来说往来余额与账龄是最该先看的一屏，所以单独接上
//
// 独立失败不牵连：workbench 挂了还能看账龄，账龄挂了还能看业务卡片。
async function loadBizOverview(main) {
  const box = $("#ov-biz", main);
  if (!box) return;
  const [wb, ar, ap] = await Promise.all([
    api("/workbench?periods=6").catch((e) => ({ __err: e.message })),
    api("/settle/aging?account=1122").catch(() => null),
    api("/settle/aging?account=2202").catch(() => null),
  ]);

  if (wb && wb.__err) {
    box.innerHTML = `<div class="panel"><div style="color:var(--err)">业务数据加载失败：${esc(wb.__err)}</div></div>`;
    return;
  }
  const cardsByDom = {};
  ((wb && wb.cards) || []).forEach((c) => { (cardsByDom[c.domain] = cardsByDom[c.domain] || []).push(c); });
  const trendsByDom = {};
  ((wb && wb.trends) || []).forEach((t) => { (trendsByDom[t.domain] = trendsByDom[t.domain] || []).push(t); });

  const wcard = (c) => `<div class="card"><div class="k">${esc(c.label)}</div>
    <div class="v">${esc(c.value)}${c.unit === "元" || c.unit === "件" ? `<span style="font-size:12px;font-weight:400;opacity:.65"> ${esc(c.unit)}</span>` : ""}</div></div>`;

  // 每个业务域一块：指标卡 + 该域的走势图 + 跳转入口
  const panels = BIZ_ORDER.filter((d) => cardsByDom[d] || trendsByDom[d]).map((dom) => {
    const entry = BIZ_ENTRY[dom];
    const goto = entry && VIEWS[entry] ? `<button class="btn ghost sm" data-biz-go="${entry}">进入</button>` : "";
    const ts = (trendsByDom[dom] || []).map((t) => `<div class="panel" style="margin:0;padding:10px 12px">
        <div style="display:flex;align-items:center;gap:8px"><b style="font-size:13px">${esc(t.title)}</b><span class="muted" style="font-size:12px">单位：${esc(t.unit)}</span></div>
        ${chartBox("line", t.title, t.periods, (t.series || []).map((s) => ({ name: s.name, color: s.color, values: s.points })), 180, t.unit)}
      </div>`).join("");
    return `<div class="panel" style="margin-top:12px">
      <div style="display:flex;align-items:center;gap:8px;flex-wrap:wrap">
        <b>${esc(dom)}</b><span class="grow"></span>${goto}
      </div>
      <div class="cards" style="margin-top:8px">${(cardsByDom[dom] || []).map(wcard).join("")}</div>
      ${ts ? `<div style="display:grid;grid-template-columns:repeat(auto-fill,minmax(400px,1fr));gap:12px;margin-top:10px">${ts}</div>` : ""}
    </div>`;
  }).join("");

  // ---- 往来：应收 / 应付余额 + 账龄分布（workbench 未覆盖的业务域）----
  const aging = (data) => {
    if (!data || !data.rows) return null;
    const buckets = data.buckets || [];
    const sums = buckets.map((_, i) => data.rows.reduce((a, r) => a + moneyNum((r.amounts || [])[i]), 0));
    const net = data.rows.reduce((a, r) => a + moneyNum(r.net), 0);
    const overdue = data.rows.filter((r) => Number(r.max_days) > 0);
    return { buckets, sums, net, parties: data.rows.length, overdueCount: overdue.length,
      overdueAmt: overdue.reduce((a, r) => a + moneyNum(r.net), 0) };
  };
  const arA = aging(ar), apA = aging(ap);
  const settlePanel = (arA || apA) ? (() => {
    const side = (label, a, color) => {
      if (!a) return `<div class="card"><div class="k">${label}余额</div><div class="v">—</div></div>`;
      const worst = a.overdueCount > 0
        ? `<div class="card" style="border-color:var(--warn)"><div class="k">${label}逾期</div>
             <div class="v" style="color:var(--warn)">${moneyFmt(a.overdueAmt)}<span style="font-size:12px;font-weight:400;opacity:.65"> / ${a.overdueCount} 户</span></div></div>`
        : `<div class="card"><div class="k">${label}逾期</div><div class="v">0.00</div></div>`;
      return `<div class="card"><div class="k">${label}余额</div><div class="v" style="font-size:18px">${moneyFmt(a.net)}</div></div>
        <div class="card"><div class="k">${label}往来户数</div><div class="v">${a.parties}</div></div>${worst}`;
    };
    const chart = (arA && apA && arA.buckets.length)
      ? `<div class="panel" style="margin:0;padding:10px 12px;margin-top:10px">
          <div style="display:flex;align-items:center;gap:8px"><b style="font-size:13px">往来账龄分布</b>
            <span class="muted" style="font-size:12px">单位：元</span>
            <span class="grow"></span>
            <button class="btn ghost sm" data-biz-go="settle">进入往来核销</button></div>
          <div style="display:grid;grid-template-columns:repeat(auto-fill,minmax(420px,1fr));gap:12px;margin-top:8px">
            <div><div class="muted" style="font-size:12px;margin-bottom:2px">应收</div>
              ${chartBox("bar", "应收账龄分布", arA.buckets, [{ name: "应收", color: "#2563eb", values: arA.sums }], 200, "元")}</div>
            <div><div class="muted" style="font-size:12px;margin-bottom:2px">应付</div>
              ${chartBox("bar", "应付账龄分布", apA.buckets, [{ name: "应付", color: "#f59e0b", values: apA.sums }], 200, "元")}</div>
          </div>
        </div>`
      : `<div class="toolbar" style="margin-top:8px"><button class="btn ghost sm" data-biz-go="settle">进入往来核销</button></div>`;
    return `<div class="panel" style="margin-top:12px">
      <div style="display:flex;align-items:center;gap:8px"><b>往来</b>
        <span class="muted" style="font-size:12px">应收 1122 / 应付 2202，只认已记账分录</span></div>
      <div class="cards" style="margin-top:8px">${side("应收", arA)}${side("应付", apA)}</div>
      ${chart}
    </div>`;
  })() : "";

  box.innerHTML = panels + settlePanel
    || `<div class="panel"><div class="muted">本期暂无业务数据。业务模块录单后这里会出现订单、库存、生产与往来概览。</div></div>`;
  $all("[data-biz-go]", main).forEach((b) => b.onclick = () => goView(b.dataset.bizGo));
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
// 关键：过滤**不触发 change 事件**，只更新 <option> 列表；change 仅在用户显式选中（或 Enter/↓）时发生。
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
  if (list.some((a) => a.code === cur)) {
    sel.value = cur;
  } else if (list.length) {
    // 过滤后列表不含当前值：只置首项，**不 dispatch change** —— 避免触发 renderRows 导致焦点丢失
    sel.value = list[0].code;
  }
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

// 工具栏 data-more="3"：前 3 个（新增凭证 / 查询 / 批量记账）保持平铺。
// 这三个是两岗公司的**每日**动作，「批量记账」尤其不该藏进菜单里多点一次。
// 收进「更多」的是低频项：重排断号（标了 danger，会沉底并加分隔线）、套打、导出。
async function viewVouchers(main) {
  // 分页状态放在这里而不是全局：viewVouchers 每次都重建 #main，
  // 页码得自己记住（切走再切回由 saveViewState/restoreViewState 负责筛选框，
  // 页码属于「同一视图内部」的状态，跟着视图对象走最省事）。
  if (!viewVouchers.page) viewVouchers.page = 1;
  if (!viewVouchers.size) viewVouchers.size = 50;
  main.innerHTML = `
    <h2>记账凭证</h2>
    <div class="toolbar" data-more="3">
      ${can("voucher_new") ? `<button class="btn sm" id="new-v">新增凭证</button>` : ""}
      <input id="v-q" placeholder="摘要 / 凭证号 / 科目" style="width:200px" />
      <select id="v-status">
        <option value="">全部状态</option>
        <option value="draft">未记账</option>
        <option value="audited">已审核</option>
        <option value="posted">已记账</option><option value="void">已作废</option>
      </select>
      <button class="btn ghost sm" id="v-refresh">查询</button>
      ${can("voucher_post") ? `<button class="btn ghost sm" id="v-batch">批量记账</button>` : ""}
      ${can("voucher_edit") ? `<button class="btn ghost sm danger" id="v-renumber" title="把本期间已记账凭证的断号重排为连续">重排断号</button>` : ""}
      ${can("report") ? `<button class="btn ghost sm" id="v-printform">凭证套打</button>` : ""}
      ${can("export") ? `<button class="btn ghost sm" id="v-export">导出 CSV</button>` : ""}
      <span class="spacer"></span>
      <span class="muted">期间：${esc(state.current || "")}</span>
    </div>
    <div class="panel"><table class="grid" id="v-table"
      data-empty-title="本期间没有凭证"
      data-empty-hint="点「新增凭证」录第一张；已有旧账套数据可到「数据导入」迁移。"
      data-empty-action="imports" data-empty-action-label="去导入"><thead><tr>
      ${can("voucher_post") ? `<th style="width:26px"><input type="checkbox" id="v-all" title="全选本页未记账" /></th>` : ""}
      <th>期间</th><th>日期</th><th>凭证号</th><th>摘要</th><th class="num">借方</th><th class="num">贷方</th><th>状态</th><th>制单</th><th></th>
    </tr></thead><tbody><tr><td colspan="${can("voucher_post") ? 10 : 9}" class="muted">加载中…</td></tr></tbody></table></div>`;
  if (can("voucher_new")) $("#new-v").addEventListener("click", () => openVoucherEditor(null));
  // 改筛选条件一律回到第 1 页：留着第 7 页去搜一个只有 2 条结果的关键词，
  // 看到的就是「空列表 + 第 7/7 页」这种没法解释的界面
  $("#v-refresh").addEventListener("click", () => { viewVouchers.page = 1; loadVouchers(); });
  $("#v-q").addEventListener("keydown", (e) => { if (e.key === "Enter") { viewVouchers.page = 1; loadVouchers(); } });
  $("#v-status").addEventListener("change", () => { viewVouchers.page = 1; loadVouchers(); });
  if ($("#v-all")) $("#v-all").addEventListener("change", (e) => { $all(".v-sel").forEach((c) => c.checked = e.target.checked); });
  if ($("#v-batch")) $("#v-batch").addEventListener("click", batchPost);
  if ($("#v-printform")) $("#v-printform").addEventListener("click", () => {
    window.open(`/api/vouchers/print-form?period=${encodeURIComponent(state.current || "")}`, "_blank");
  });
  if ($("#v-export")) $("#v-export").addEventListener("click", () => {
    const qs = new URLSearchParams({ period: (state.current || "").replace("-", ""), q: $("#v-q").value, status: $("#v-status").value });
    downloadUrl(`/api/export/vouchers?${qs.toString()}`);
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

// 凭证列表：分页 + 全量合计
//
// 改造前走 `/vouchers`，那个接口默认 limit=200 且升序排列 —— 一个期间超过
// 200 张后，后面的凭证在界面上**完全看不到且没有任何提示**。对月均两三百张的
// 单子这是常态，不是边角情况。现在走 `/vouchers/page`：能翻到最后一页，
// 并且给出全期借贷合计（会计核对时看的就是这个数，不是本页的数）。
async function loadVouchers() {
  const table = $("#v-table");
  if (!table) return;
  const tb = table.tBodies[0];
  const q = $("#v-q").value.trim();
  const st = $("#v-status").value;
  const size = viewVouchers.size || 50;
  const qs = new URLSearchParams({
    period: (state.current || "").replace("-", ""),
    page: String(viewVouchers.page || 1),
    page_size: String(size),
  });
  if (q) qs.set("q", q);
  if (st) qs.set("status", st);
  const colspan = can("voucher_post") ? 10 : 9;
  let data;
  try {
    data = await api(`/vouchers/page?${qs.toString()}`);
  } catch (e) {
    // 失败态保留重试入口：整行被换成纯文字空态的话重试按钮就没了
    tb.innerHTML = `<tr><td colspan="${colspan}"><div class="empty-state is-error">
      <div class="es-icon">⚠</div>
      <div class="es-title">凭证加载失败</div>
      <div class="es-detail">${esc(e.message)}</div>
      <div class="es-actions"><button class="btn ghost sm" data-empty-retry>重试</button></div>
    </div></td></tr>`;
    const ft = table.tFoot;
    if (ft) ft.remove();
    renderVoucherPager(table, { total: 0, page: 1, page_size: size, debit_total: "0", credit_total: "0" }, 1);
    return;
  }
  const rows = data.items || [];
  const total = data.total || 0;
  const pages = Math.max(1, Math.ceil(total / size));
  // 请求的是第 7 页但只有 3 页（刚删完 / 换了期间）→ 退到最后一页并重取
  if (viewVouchers.page > pages && total > 0) { viewVouchers.page = pages; return loadVouchers(); }
  if (viewVouchers.page > 1 && total === 0) { viewVouchers.page = 1; return loadVouchers(); }

  if (!rows.length) {
    // 交回通用空态组件处理（它会读 table 上的 data-empty-* 属性）
    tb.innerHTML = "";
    renderVoucherPager(table, data, pages);
    autoEmptyRows();
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
  renderVoucherPager(table, data, pages);
  try { autoNumbers(); } catch (e) {}
}

// 合计行 + 分页器
//
// 合计放 **tfoot**（sticky 底部，滚动长表时一直看得见），而且是**全期合计**
// 而不是本页合计 —— 会计核对的是「本期间借方 = 贷方」，按页汇总会让人以为
// 账不平（翻到第 3 页数字突然变了）。文案里写明「全期」，避免误解。
function renderVoucherPager(table, data, pages) {
  const total = data.total || 0;
  const size = data.page_size || viewVouchers.size || 50;
  const page = viewVouchers.page || 1;
  const colspan = can("voucher_post") ? 10 : 9;
  let tf = table.tFoot;
  if (!tf) {
    tf = document.createElement("tfoot");
    table.appendChild(tf);
  }
  if (!total) { tf.innerHTML = ""; return; }
  const d = data.debit_total || "0";
  const c = data.credit_total || "0";
  const balanced = fmtMoneyNum(d) === fmtMoneyNum(c);
  tf.innerHTML = `<tr class="totals">
      <td colspan="${can("voucher_post") ? 5 : 4}"><span class="tot-label">全期合计（${total} 张，不随翻页变化）</span></td>
      <td class="num strong">${esc(d)}</td>
      <td class="num strong">${esc(c)}</td>
      <td colspan="3">${balanced
        ? `<span class="tag ok">借贷平衡</span>`
        : `<span class="tag err" title="全期借方合计与贷方合计不等，请检查">借贷不平</span>`}</td>
    </tr>`;
  // 分页条挂在表格外面（.panel 内），不进 tfoot
  let bar = $("#v-pager");
  if (!bar) {
    bar = document.createElement("div");
    bar.id = "v-pager";
    bar.className = "pagination";
    table.parentNode.appendChild(bar);
  }
  const from = total === 0 ? 0 : (page - 1) * size + 1;
  const to = Math.min(total, page * size);
  const go = (p) => { viewVouchers.page = Math.min(pages, Math.max(1, p)); loadVouchers(); window.scrollTo(0, 0); };
  const nums = [];
  const push = (p) => nums.push(`<button class="pg-btn${p === page ? " active" : ""}" data-pg="${p}">${p}</button>`);
  push(1);
  if (pages > 6) {
    const lo = Math.max(2, page - 1), hi = Math.min(pages - 1, page + 1);
    if (lo > 2) nums.push(`<span class="pg-info" style="margin:0">…</span>`);
    for (let p = lo; p <= hi; p++) push(p);
    if (hi < pages - 1) nums.push(`<span class="pg-info" style="margin:0">…</span>`);
    push(pages);
  } else {
    for (let p = 2; p <= pages; p++) push(p);
  }
  bar.innerHTML = `<span class="pg-info">第 ${from}–${to} 张 / 共 ${total} 张 · 第 ${page} / ${pages} 页</span>
    <button class="pg-btn" data-pg="prev" ${page <= 1 ? "disabled" : ""} title="上一页">‹</button>
    ${nums.join("")}
    <button class="pg-btn" data-pg="next" ${page >= pages ? "disabled" : ""} title="下一页">›</button>
    <span class="pg-size">每页
      <select id="v-size">${[20, 50, 100, 200].map((n) => `<option value="${n}" ${n === size ? "selected" : ""}>${n}</option>`).join("")}</select>
    </span>`;
  $all("[data-pg]", bar).forEach((b) => b.addEventListener("click", () => {
    const v = b.dataset.pg;
    go(v === "prev" ? page - 1 : v === "next" ? page + 1 : parseInt(v, 10));
  }));
  const sz = $("#v-size", bar);
  if (sz) sz.addEventListener("change", () => { viewVouchers.size = parseInt(sz.value, 10) || 50; viewVouchers.page = 1; loadVouchers(); });
}
// 金额字符串 → 数值（只为比较借贷是否相等，不参与展示）
function fmtMoneyNum(s) {
  return parseFloat(String(s == null ? "" : s).replace(/[^0-9.\-]/g, "")) || 0;
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
  // 「记账」按钮必须与后端闸门口径一致：**账套开着审核环节时，草稿不能记账**
  // （voucher_post 会返回 400「请先审核凭证再记账」）。早先这里只看 status，
  // 审核环节默认开启后仍给未审核的草稿显示「记账」，用户一点就被拒——按钮亮着
  // 却用不了，是最容易被当成 bug 的一种 UX。
  const auditOn = !!(state.bookOptions && state.bookOptions.enable_audit);
  const canPost = auditOn ? status === "audited" : (status === "draft" || status === "audited");
  // 更正链提示：两张凭证互相认得，才查得出「哪张在冲哪张」。
  // 没有它，已作废的原凭证和新凭证只能靠时间金额去猜。
  const am = v.amend || null;
  const amendBanner = am && (am.amends_id || am.amended_by)
    ? `<div class="muted" style="margin:0 0 12px;padding:8px 10px;border:1px solid #ccc;border-radius:6px;line-height:1.7">
        ${am.amends_id ? `本凭证是 <b>更正链</b>产生的凭证，更正自 <b>${esc(am.amends_no || am.amends_id)}</b>。<br/>` : ""}
        ${am.amended_by ? `本凭证已被 <b>${esc(am.amended_by_no || am.amended_by)}</b> 红冲，由 <b>${esc(am.amended_by_new_no || am.amended_by_new)}</b> 更正重做。<br/>` : ""}
        ${am.reason ? `更正原因：${esc(am.reason)}` : ""}
       </div>`
    : "";
  // 已记账的更正链凭证（原 + 红冲 + 更正）必须成套存在：单独反记账或再红冲一次，
  // 账上就会少一张或多一张，净额直接错。所以这几个按钮一律不给 ——
  // 亮着却用不了是最容易被当成 bug 的一种 UX，守卫必须与按钮显隐同步。
  const inAmendSet = status === "posted" && !!(am && (am.amended_by || am.amends_id));
  const mask = modal(`
    <h3>记账凭证 ${esc(voucher_no)} <span class="muted" style="font-size:13px">${({ draft: "未记账", audited: "已审核", posted: "已记账", void: "已作废" })[status] || esc(status)}${v.cashier ? `　出纳:${esc(v.cashier)}` : ""}</span></h3>
    ${amendBanner}
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
    <div style="margin-top:10px" class="muted">合计：借 <b id="v-dt">0.00</b> 　贷 <b id="v-ct">0.00</b> 　差额 <b id="v-diff">0.00</b></div>
    <div id="v-attach" class="muted" style="margin-top:8px">附件加载中…</div>
    <div class="foot">
      ${editable ? `<button class="btn" id="v-save">保存</button>` : ""}
      ${editable ? `<button class="btn ghost" id="v-save-new">保存并新增</button>` : ""}
      ${can("voucher_audit") && id > 0 && status === "draft" ? `<button class="btn ghost" id="v-audit">审核</button>` : ""}
      ${can("voucher_unaudit") && id > 0 && status === "audited" ? `<button class="btn ghost" id="v-unaudit">反审核</button>` : ""}
      ${can("cashier_sign") && id > 0 && (status === "draft" || status === "audited") && !v.cashier ? `<button class="btn ghost" id="v-sign">出纳签字</button>` : ""}
      ${can("cashier_sign") && id > 0 && (status === "draft" || status === "audited") && v.cashier ? `<button class="btn ghost" id="v-unsign">取消签字</button>` : ""}
      ${can("voucher_post") && canPost ? `<button class="btn primary" id="v-post">记账</button>` : ""}
      ${can("voucher_unpost") && status === "posted" && !inAmendSet ? `<button class="btn ghost" id="v-unpost">反记账</button>` : ""}
      ${can("voucher_new") && id > 0 && status !== "void" && !inAmendSet ? `<button class="btn ghost" id="v-reverse">红字冲销</button>` : ""}
      ${can("voucher_delete") && id > 0 && (status === "draft" || status === "audited") ? `<button class="btn ghost" id="v-void">作废</button>` : ""}
      ${can("voucher_delete") && id > 0 && status === "void" && !(am && am.amended_by) ? `<button class="btn ghost" id="v-void-back">恢复作废</button>` : ""}
      ${can("voucher_delete") && id > 0 && status === "posted" && !(am && am.amended_by) ? `<button class="btn ghost" id="v-amend">更正</button>` : ""}
      ${can("voucher_delete") && (status === "draft" || status === "audited") ? `<button class="btn danger" id="v-del">删除</button>` : ""}
      <button class="btn ghost" id="v-close">关闭</button>
    </div>
  `, true);

  const tbody = $("#v-entries tbody", mask);
  let expanded = -1;
  // 行 DOM 缓存：index → { tr, detailTr, select, sumInp, debitInp, creditInp, auxBtn, delBtn }
  const rowCache = new Map();

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

  // —— 增量渲染 —— 只在首次/加行/删行时 rebuild 结构；其余只改单行
  function renderRows() {
    const oldSize = rowCache.size;
    const newSize = v.entries.length;

    if (oldSize === 0) {
      // 首次：全量建行
      v.entries.forEach((e, i) => buildRow(i));
    } else if (newSize > oldSize) {
      // 只加了行：为新行建 DOM
      for (let i = oldSize; i < newSize; i++) buildRow(i);
    } else if (newSize < oldSize) {
      // 删了行：移除多余行的 DOM，重新编号剩余行
      for (let i = newSize; i < oldSize; i++) {
        const rc = rowCache.get(i);
        if (rc) { rc.tr.remove(); if (rc.detailTr) rc.detailTr.remove(); rowCache.delete(i); }
      }
      // 重新编号剩余行的行号列
      for (let i = 0; i < newSize; i++) {
        const rc = rowCache.get(i);
        if (rc) rc.tr.cells[0].textContent = i + 1;
      }
    }
    // 无论增删，都要同步每行的数据到 DOM（科目/摘要/金额/辅助按钮文案）
    v.entries.forEach((e, i) => syncRow(i));
    recalc();
  }

  // 为 index 创建一行 <tr> + 可选的详情行，并绑定事件
  function buildRow(i) {
    const e = v.entries[i];
    const miss = auxMissing(e);
    const hasAux = (e.aux && Object.keys(e.aux).some((k) => e.aux[k])) || e.qty || e.cf;
    const detailHtml = expanded === i ? auxDetailHtml(e, i) : "";

    const tr = document.createElement("tr");
    tr.innerHTML = `
      <td>${i + 1}</td>
      <td>${accountOptions()}</td>
      <td><input class="e-sum" value="${esc(e.summary)}" style="width:100%" ${editable ? "" : "disabled"} /></td>
      <td class="num"><input class="e-d num" value="${esc(e.debit)}" style="width:110px;text-align:right" ${editable ? "" : "disabled"} /></td>
      <td class="num"><input class="e-c num" value="${esc(e.credit)}" style="width:110px;text-align:right" ${editable ? "" : "disabled"} /></td>
      <td><button class="btn ghost sm e-aux" title="辅助核算/数量/现金流量" ${editable ? "" : "disabled"} style="${miss ? "color:var(--err)" : ""}">${miss ? "补录!" : (hasAux ? "已填" : "⋯")}</button></td>
      <td>${editable ? `<button class="btn ghost sm e-del">×</button>` : ""}</td>
    `;
    tbody.appendChild(tr);

    let detailTr = null;
    if (detailHtml) {
      detailTr = document.createElement("tr");
      detailTr.innerHTML = `<td colspan="7" style="background:rgba(0,0,0,0.03)">${detailHtml}</td>`;
      tbody.appendChild(detailTr);
    }

    const select = tr.querySelector("select.acct-sel");
    const sumInp = tr.querySelector(".e-sum");
    const debitInp = tr.querySelector(".e-d");
    const creditInp = tr.querySelector(".e-c");
    const auxBtn = tr.querySelector(".e-aux");
    const delBtn = tr.querySelector(".e-del");

    rowCache.set(i, { tr, detailTr, select, sumInp, debitInp, creditInp, auxBtn, delBtn });

    // 绑定事件（只绑一次，行复用时保留）
    if (select) {
      select.value = e.account_code;
      select.onchange = () => {
        v.entries[i].account_code = select.value;
        // 只同步该行，不整表重绘
        syncRow(i);
      };
    }
    if (sumInp) sumInp.oninput = () => { v.entries[i].summary = sumInp.value; };
    if (debitInp) debitInp.oninput = () => { v.entries[i].debit = debitInp.value; recalc(); autoClearCredit(i); };
    if (creditInp) creditInp.oninput = () => { v.entries[i].credit = creditInp.value; recalc(); autoClearDebit(i); };
    if (delBtn) delBtn.onclick = () => {
      v.entries.splice(i, 1);
      v.entries.forEach((ee, k) => ee.line = k + 1);
      expanded = -1;
      renderRows();
    };
    if (auxBtn) auxBtn.onclick = () => {
      expanded = (expanded === i ? -1 : i);
      // 只切换详情行显隐
      const rc = rowCache.get(i);
      if (rc) {
        if (expanded === i) {
          // 展开：若详情行不存在则建，存在则显示
          if (!rc.detailTr) {
            rc.detailTr = document.createElement("tr");
            rc.detailTr.innerHTML = `<td colspan="7" style="background:rgba(0,0,0,0.03)">${auxDetailHtml(v.entries[i], i)}</td>`;
            rc.tr.after(rc.detailTr);
            bindAuxInputs(i);
            const closeAux = $("#v-aux-close", rc.detailTr);
            if (closeAux) closeAux.onclick = () => { expanded = -1; renderRows(); };
          } else {
            rc.detailTr.style.display = "";
          }
        } else {
          // 收起：隐藏详情行
          if (rc.detailTr) rc.detailTr.style.display = "none";
        }
      }
    };
  }

  // 将 model 第 i 行同步到 DOM（不重建行结构，只改值/文案/选中态）
  function syncRow(i) {
    const e = v.entries[i];
    const rc = rowCache.get(i);
    if (!rc) return;

    // 科目 select：更新选中值，并同步 <option> 列表（保持当前过滤关键词）
    if (rc.select) {
      const kw = rc.select.previousElementSibling?.value?.trim?.().toLowerCase?.() || "";
      const list = (state.accounts || []).filter((a) => !kw || a.code.toLowerCase().includes(kw) || (a.name || "").toLowerCase().includes(kw));
      const cur = rc.select.value;
      rc.select.innerHTML = list.length
        ? list.map((a) => `<option value="${esc(a.code)}">${esc(a.code)} ${esc(a.name)}</option>`).join("")
        : `<option value="">无匹配</option>`;
      if (list.some((a) => a.code === cur)) rc.select.value = cur;
      else if (list.length) rc.select.value = list[0].code;
      // 同步 model（onchange 已绑定，这里不再触发 change）
    }

    // 摘要/借/贷输入框
    if (rc.sumInp && rc.sumInp.value !== e.summary) rc.sumInp.value = e.summary;
    if (rc.debitInp && rc.debitInp.value !== e.debit) rc.debitInp.value = e.debit;
    if (rc.creditInp && rc.creditInp.value !== e.credit) rc.creditInp.value = e.credit;

    // 辅助按钮文案/颜色
    const miss = auxMissing(e);
    const hasAux = (e.aux && Object.keys(e.aux).some((k) => e.aux[k])) || e.qty || e.cf;
    if (rc.auxBtn) {
      rc.auxBtn.textContent = miss ? "补录!" : (hasAux ? "已填" : "⋯");
      rc.auxBtn.style.color = miss ? "var(--err)" : "";
    }

    // 详情行若展开着，同步其输入框值
    if (expanded === i && rc.detailTr) {
      const newDetail = auxDetailHtml(e, i);
      if (rc.detailTr.innerHTML !== newDetail) rc.detailTr.innerHTML = `<td colspan="7" style="background:rgba(0,0,0,0.03)">${newDetail}</td>`;
      bindAuxInputs(i);
      const closeAux = $("#v-aux-close", rc.detailTr);
      if (closeAux) closeAux.onclick = () => { expanded = -1; renderRows(); };
    }
  }

  // 辅助核算详情行的输入框绑定（每次展开/同步时重新绑）
  function bindAuxInputs(i) {
    const e = v.entries[i];
    $all(".aux-in", tbody).forEach((inp) => {
      if (parseInt(inp.dataset.i, 10) !== i) return;
      inp.oninput = () => {
        const k = inp.dataset.k;
        if (k === "qty" || k === "price" || k === "cf" || k === "currency" || k === "rate" || k === "amount_for") { e[k] = inp.value; }
        else { e.aux = e.aux || {}; e.aux[k] = inp.value; }
        syncRow(i); // 同步辅助按钮文案
      };
    });
  }

  // 借方输入 > 0 时自动清零贷方（反之亦然），避免手动清空
  function autoClearCredit(i) {
    const e = v.entries[i];
    const d = parseFloat(e.debit) || 0;
    if (d > 0 && (parseFloat(e.credit) || 0) > 0) {
      e.credit = "0";
      const rc = rowCache.get(i);
      if (rc && rc.creditInp) rc.creditInp.value = "0";
    }
  }
  function autoClearDebit(i) {
    const e = v.entries[i];
    const c = parseFloat(e.credit) || 0;
    if (c > 0 && (parseFloat(e.debit) || 0) > 0) {
      e.debit = "0";
      const rc = rowCache.get(i);
      if (rc && rc.debitInp) rc.debitInp.value = "0";
    }
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
  if ($("#v-amend", mask)) $("#v-amend", mask).onclick = async () => {
    const reason = await reasonDialog(
      "更正凭证",
      "将按准则做「红字冲销 + 蓝字重做」：本凭证保持已记账，另生成一张红冲凭证抵消它、再生成一张更正凭证（原期间、原日期）。请写明更正原因 —— 原因会同时记入两张凭证的摘要与操作日志，事后审计只查得到这些。\n\n" +
      "三张凭证会成套生效，之后不能单独反记账或作废其中一张（否则净额会错）。",
      "例：金额录错，应为 1200"
    );
    if (!reason) return;
    try {
      const r = await api(`/vouchers/${v.id}/amend`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ reason }) });
      toast("已更正，原凭证已作废；新凭证待记账", "ok");
      closeModal();
      loadVouchers();
      // 直接打开新凭证：更正后的下一步就是审核/记账，跳过去少一次点击，
      // 也免得用户在列表里找「哪张是新的」。
      if (r && r.new_id) openVoucherEditor(r.new_id);
    } catch (e) { toast(e.message, "err"); }
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
