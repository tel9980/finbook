// FinBook Web 前端 — 预算分析、成本配置、科目表、辅助核算、期初余额、选项、日志、备份、凭证模板
//
// 这一份是 `crates/finweb/static/app.js` 拆分出来的（原单文件 14k 行、295 个顶层函数）。
// 拆分方式：**经典 script，按原始行序切分，不重排任何代码**——顶层 function/const 的
// 提升与声明顺序完全保持原样，所以跨文件不存在 TDZ/未定义问题；只有 `VIEWS` 路由表
// 因为引用全部视图函数，被挪到了最后加载的 app-hr.js。
// 加载顺序由 `index.html` 的 <script> 顺序决定（也是 `main.rs` 里 asset_version 的清单顺序）。
// 新增文件必须同步三处：index.html、main.rs 的 asset_version 清单、tools/lib-app-src.js 的解析（自动）。
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
        <label>分摊金额 <input id="mf-amt" style="width:90px" placeholder="留空=取5101本期" title="留空则直接取制造费用科目（5101）本期借方发生额" /></label>
        <label>基准 <select id="mf-base"><option value="cost">按成本占比</option><option value="labor">按直接人工</option><option value="qty">按计划产量</option><option value="hours">按报工工时</option></select></label>
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
        show(`<b>制造费用${apply ? "已应用" : "试算"}（${esc(r.base)}，金额 ${fmt(r.amount)}）</b><table class="grid" style="margin-top:6px"><thead><tr><th>订单</th><th class="num">分摊额</th></tr></thead><tbody>${rows.map((x) => `<tr><td>#${x.po_id}</td><td class="num">${fmt(x.amount)}</td></tr>`).join("")}</tbody></table>${apply ? "" : `<div class="muted" style="font-size:12px;margin-top:4px">试算未落库；点「应用分摊」写入归集</div>`}`);
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
      ${can("export") ? `<button class="btn ghost" id="acct-export" title="导出全部科目（编码/名称/类别/方向/备注），自建科目改完能导回">导出 Excel</button>` : ""}
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
  if ($("#acct-export")) $("#acct-export").addEventListener("click", () => downloadUrl("/api/export/accounts"));
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

// ---------------- 期初建账 ----------------
// 导入整本《期初收集表》。
//
// 为什么单开一个入口而不是塞进「数据导入」：收集表是四张纸一起看的，只导一张纸
// 就会出现「应收台账有数、资产负债表没数」这种合起来不对的状态。后端也是先算平
// 再落库，不平整批拒绝。
/// 期初导入**之后**的对账视图。
///
/// 与「导入期初收集表」的分工：那边回答「这份表能不能导」，这边回答「导完之后账上
/// 是不是真的变成了表里那个样子」。两次来的数据都可能对，但中间任何一步出问题，
/// 只有这边能发现 —— 所以它不能并进导入弹窗，要能**随时**单独看。
async function openBeginReconcile() {
  let r;
  try {
    r = await api("/books/opening-sheet/reconcile");
  } catch (e) {
    toast("读取对账结果失败：" + e.message, "err");
    return;
  }
  const sheetRows = (r.sheets || []).map((s) => `
    <tr>
      <td>${s.label}</td>
      <td class="r">${s.rows}</td>
      <td class="r num">${s.total}</td>
      <td class="muted u-sm">${s.hint}</td>
    </tr>`).join("");
  const checkRows = (r.checks || []).map((c) => `
    <tr>
      <td style="width:26px">${c.ok ? "✅" : "❌"}</td>
      <td>${c.name}</td>
      <td class="muted u-sm">${c.detail}</td>
    </tr>`).join("");
  const body = r.empty
    ? `<div class="muted" style="padding:14px 0">还没有期初数据。先「下载期初模板」填好，再「导入期初收集表」。</div>`
    : `
      <div class="card" style="margin-bottom:12px">
        <div style="font-size:15px;font-weight:600;margin-bottom:6px;color:${r.ok ? "var(--ok)" : "var(--err)"}">${r.one_liner}</div>
        <div class="muted u-sm">借方 ${r.debit_total} ｜ 贷方 ${r.credit_total} ｜ 资产 ${r.asset_total} ｜ 负债 ${r.liab_total} ｜ 权益 ${r.equity_total}</div>
      </div>
      <table class="tbl" style="margin-bottom:14px">
        <thead><tr><th>那张纸</th><th class="r">行数</th><th class="r">合计</th><th>落到了哪里</th></tr></thead>
        <tbody>${sheetRows}</tbody>
      </table>
      <table class="tbl">
        <thead><tr><th></th><th>交叉校验</th><th>数字</th></tr></thead>
        <tbody>${checkRows}</tbody>
      </table>
      ${(r.issues && r.issues.length) ? `<div style="margin-top:12px;padding:10px 12px;border-radius:8px;background:var(--warn-t);color:var(--warn);line-height:1.7">
        <b>怎么改：</b>期初是推倒重来的活，别在报表上补差。改表 → 清掉期初 → 重新导入，比逐条调账可靠。
      </div>` : ""}`;
  const mask = modal(
    `<h3>期初对账</h3>${body}
     <div style="margin-top:16px;text-align:right"><button class="btn" data-close>关闭</button></div>`,
    true
  );
  mask.querySelector("[data-close]").onclick = () => closeModal();
}

function openImportCollection() {
  const mask = modal(`
    <h3>导入期初收集表</h3>
    <p class="muted" style="font-size:12px;line-height:1.7;margin:0 0 14px">
      上传工厂填好的<b>《期初收集表.xlsx》</b>（四张纸：① 客户应收 ② 供应商应付 ③ 账户余额 ④ 基本资料）。
      会一次做完三件事：建往来台账（算账龄、出对账单用）、写 112201/220201 的科目期初、写各账户与实收资本的期初。
      <br>期初<b>借贷不平会整批拒绝</b>，并告诉你差多少 —— 不会写进半个账套。
    </p>
    <p class="muted" style="font-size:12px;line-height:1.7;margin:0 0 12px">
      还没有表？<a href="#" id="ic-tpl" style="color:var(--accent)">下载空模板</a> ——
      里面已经列好你这个账套的科目、客户、供应商（编码名称都填好了），只需填金额。
    </p>
    <label style="display:block;margin-bottom:12px">选择文件
      <input id="ic-file" type="file" accept=".xlsx,.xls,.ods" style="width:100%;margin-top:4px" />
    </label>
    <div class="muted" id="ic-msg" style="min-height:18px;font-size:12px;line-height:1.5"></div>
    <div class="foot"><button class="btn ghost" id="ic-preview">先预检（不落库）</button><button class="btn primary" id="ic-ok">开始导入</button><button class="btn ghost" id="ic-cancel">取消</button></div>
  `);
  const msg = (t) => { const el = $("#ic-msg", mask); if (el) el.innerHTML = t; };
  $("#ic-cancel", mask).onclick = () => mask.remove();
  $("#ic-tpl", mask).onclick = (e) => { e.preventDefault(); downloadUrl("/books/opening-sheet/template"); };
  // 预检：期初是边整理边填的，整本填完之前唯一能验证的办法以前只有「导一次试试」。
  // 预检走同一条后端路径，只算平不落库 —— 改一天、验一天。
  $("#ic-preview", mask).onclick = async () => {
    const f = ($("#ic-file", mask).files || [])[0];
    if (!f) { msg('<span style="color:var(--err)">先选一个 .xlsx 文件</span>'); return; }
    const btn = $("#ic-preview", mask);
    btn.disabled = true;
    msg(`正在预检 ${esc(f.name)}…`);
    const fd = new FormData();
    fd.append("file", f);
    try {
      const r = await api("/books/opening-sheet/preview", { method: "POST", body: fd, timeout: 5 * 60 * 1000 });
      const diff = parseFloat(String(r.diff || "0").replace(/,/g, "")) || 0;
      const warn = (r.warnings || []).length
        ? `<br><b>${r.warnings.length} 条提示</b>：<div style="margin-top:4px">${r.warnings.map(esc).join("<br>")}</div>`
        : "";
      msg(`<span style="color:var(--ok)">预检通过，没有落库</span> ——
        应收 ${r.ar} 笔、应付 ${r.ap} 笔、科目期初 ${r.begin} 条；
        借 ${r.debit_total} = 贷 ${r.credit_total}${diff ? `（差 ${r.diff}）` : ""}${warn}`);
    } catch (e) {
      msg(`<span style="color:var(--err)">${esc(e.message)}</span>`);
    } finally {
      btn.disabled = false;
    }
  };
  $("#ic-ok", mask).onclick = async () => {
    const f = ($("#ic-file", mask).files || [])[0];
    if (!f) { msg('<span style="color:var(--err)">先选一个 .xlsx 文件</span>'); return; }
    const okBtn = $("#ic-ok", mask);
    okBtn.disabled = true;
    msg(`正在导入 ${esc(f.name)}…`);
    const fd = new FormData();
    fd.append("file", f);
    try {
      const r = await api("/books/opening-sheet", { method: "POST", body: fd, timeout: 5 * 60 * 1000 });
      mask.remove();
      const warn = (r.warnings || []).length ? `；注意 ${r.warnings.length} 条提示` : "";
      toast(`已导入：应收 ${r.ar} 笔、应付 ${r.ap} 笔、科目期初 ${r.begin} 条（借 ${r.debit_total} = 贷 ${r.credit_total}）${warn}`, "ok");
      if ((r.warnings || []).length) {
        modal(`<h3>导入完成，有几处要你确认</h3>
          <div class="muted u-sm" style="line-height:1.8">${r.warnings.map(esc).join("<br>")}</div>
          <div class="foot"><button class="btn primary" id="iw-ok">我知道了</button></div>`);
        $("#iw-ok", document.getElementById("modal-root")).onclick = () => closeModal();
      }
      state.beginDraft = null;
      goView("begin", { force: true });
    } catch (e) {
      okBtn.disabled = false;
      msg(`<span style="color:var(--err)">${esc(e.message)}</span>`);
    }
  };
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

// rows：BeginRow 列表；year_begin 带符号（借正贷负），前端拆成 方向 + 金额 文本框
function renderBegin(main, rows) {
  if (!state.beginDraft) {
    state.beginDraft = rows.map((r) => ({
      id: r.id, account_code: r.account_code,
      // orig_code：库里的原始编码。改了编码的行保存时要删旧行（见 #bg-save），
      // 没有它就无法判断"用户改过编码"还是"本来就是这个编码"。
      orig_code: r.account_code,
      dir: String(r.year_begin).trim().startsWith("-") ? "credit" : "debit",
      yb: fmt(String(Math.abs(parseFloat(r.year_begin) || 0))),
      ad: fmt(r.debit_accum), ac: fmt(r.credit_accum), qty: r.qty_begin == null ? "" : fmt(r.qty_begin),
    }));
    // 草稿重建 = 之前那批"待删 id"已经作废。带着旧 id 去删，会删掉这一轮刚读回来的行。
    state.beginRemoved = [];
  }
  const draft = state.beginDraft;
  const num = (s) => parseFloat(String(s).replace(/,/g, "")) || 0;
  // 试算合计单独抽出来：输入时**只改这几个数字**，不重建整张表。
  //
  // 为什么必须实时：改造前合计只在 render() 里算，而 render() 只在「添加/移除
  // 科目行」时触发。实测填入 154 万真实数据，卡片仍显示 0.00 / ✓平衡；故意
  // 造成 165,999 的不平衡，也照样显示「✓ 平衡」。这个指示器是会计在录入阶段
  // 唯一的笔误守卫，死了就等于没有。
  //
  // 又为什么不能靠 render()：那会重建 <input>，正在输入的那个框失焦、
  // 光标跳到开头，打字根本没法连贯。
  const computeTotals = () => {
    const sumYbDebit = draft.filter((r) => r.dir === "debit").reduce((s, r) => s + num(r.yb), 0);
    const sumYbCredit = draft.filter((r) => r.dir === "credit").reduce((s, r) => s + num(r.yb), 0);
    const sumAd = draft.reduce((s, r) => s + num(r.ad), 0);
    const sumAc = draft.reduce((s, r) => s + num(r.ac), 0);
    const balanced = Math.abs((sumYbDebit + sumAd) - (sumYbCredit + sumAc)) < 0.005;
    return { sumYbDebit, sumYbCredit, sumAd, sumAc, balanced };
  };
  const paintTotals = () => {
    const box = $("#bg-totals", main);
    if (!box) return;
    const t = computeTotals();
    // font-size:16px 是刻意的：.card .v 默认 26px，期初金额动辄七位数
    // （"1,544,100.00"），26px 下会撑破卡片。原来是写在 render() 里的内联样式，
    // 抽到 paintTotals 时必须一起搬过来，否则是场看不见的版式回归。
    const cell = (k, v, color, id) =>
      `<div class="card"><div class="k">${k}</div><div class="v" style="font-size:16px${color ? `;color:${color}` : ""}"${id ? ` id="${id}"` : ""}>${v}</div></div>`;
    box.innerHTML =
      cell("年初借方合计", fmt(t.sumYbDebit.toFixed(2))) +
      cell("年初贷方合计", fmt(t.sumYbCredit.toFixed(2))) +
      cell("借方累计合计", fmt(t.sumAd.toFixed(2))) +
      cell("贷方累计合计", fmt(t.sumAc.toFixed(2))) +
      cell(
        // 「试算平衡」是会计术语，小厂老板看不懂；这里说结论就行。
        // id="bg-balance" 不能动：E2E 靠它断言「不平衡 + 差额」
        "账对不对",
        // 保留「平衡 / 不平衡」这两个词：E2E（e2e/tests/admin.spec.js）按
        // 「不平衡」和「✓ 平衡」断言实时刷新，改字会让闸门失效。术语在标题上
        // 已经换成「账对不对」，正文这半句老板也读得懂，就不再动。
        t.balanced
          ? "✓ 平衡"
          : "✗ 不平衡，借方比贷方多 " +
            fmt(Math.abs((t.sumYbDebit + t.sumAd) - (t.sumYbCredit + t.sumAc)).toFixed(2)),
        t.balanced ? "var(--ok)" : "var(--err)",
        // id 要留着：E2E 靠 #bg-balance 断言"不平衡 + 差额"，
        // 抽成 cell() 时顺手丢掉的正是这个锚点
        "bg-balance"
      );
    // 账上有数据却报「平衡」是最糟的形态（空表恒等），给个明确信号
    const hasAny = draft.some((r) => num(r.yb) || num(r.ad) || num(r.ac));
    const note = $("#bg-balance-note", main);
    if (note) note.textContent = !hasAny && t.balanced ? "尚未填写任何金额" : "";
  };
  const render = () => {
    const nameOf = (code) => { const a = (state.accounts || []).find((x) => x.code === code); return a ? a.name : ""; };
    // 合计不在这里算了 —— 抽到 computeTotals/paintTotals，才能随输入实时刷新。
    // 留着一份"只在重绘时算"的副本，正是原来那个恒显示 ✓平衡 的死守卫。
    main.innerHTML = `
      <h2>期初建账</h2>
      <div class="toolbar">
        <button class="btn" id="bg-add">添加科目行</button>
        ${can("opening") ? `<button class="btn" id="bg-batch">批量粘贴期初</button>` : ""}
        <div class="spacer"></div>
        ${can("export") ? `<button class="btn ghost" id="bg-export" title="导出已保存的期初余额（科目编码/名称/方向/期初/累计），改完可以导回">导出 Excel</button>` : ""}
        ${can("opening") ? `<button class="btn ghost" id="bg-collect" title="上传填好的《期初收集表.xlsx》：客户应收/供应商应付/账户余额/实收资本一次导完，先算平再落库">导入期初收集表</button>` : ""}
      ${can("opening") ? `<button class="btn ghost" id="bg-tpl" title="下载空模板：已列好你这个账套的科目、客户、供应商，只需填金额">下载期初模板</button>` : ""}
      ${can("opening") ? `<button class="btn ghost" id="bg-check" title="导入之后点这里：把四张纸的落库情况与台账/科目/借贷/资产负债结构交叉核对，任何两处差一个数都会指出来">导入后对账</button>` : ""}
        ${can("opening") ? `<button class="btn primary" id="bg-save">保存全部</button>` : ""}
      </div>
      <div class="panel" style="padding:0;overflow:auto;max-height:60vh">
        <table class="grid">
          <thead><tr><th>科目编码</th><th>科目名称</th><th>方向</th><th class="num">年初余额</th><th class="num">借方累计</th><th class="num">贷方累计</th><th class="num">数量</th><th></th></tr></thead>
          <tbody>
            ${draft.length ? draft.map((r, i) => `
              <tr>
                <td><input class="bg-code" data-i="${i}" value="${esc(r.account_code)}" style="width:110px" /></td>
                <td class="muted">${esc(nameOf(r.account_code))}</td>
                <td><select class="bg-dir" data-i="${i}"><option value="debit" ${r.dir !== "credit" ? "selected" : ""}>借</option><option value="credit" ${r.dir === "credit" ? "selected" : ""}>贷</option></select></td>
                <td class="num"><input class="bg-yb num" data-i="${i}" value="${esc(r.yb)}" style="width:120px;text-align:right" /></td>
                <td class="num"><input class="bg-ad num" data-i="${i}" value="${esc(r.ad)}" style="width:120px;text-align:right" /></td>
                <td class="num"><input class="bg-ac num" data-i="${i}" value="${esc(r.ac)}" style="width:120px;text-align:right" /></td>
                <td class="num"><input class="bg-qty num" data-i="${i}" value="${esc(r.qty)}" style="width:90px;text-align:right" /></td>
                <td class="row-actions">
                  ${r.id > 0 ? `<span class="muted" style="font-size:12px">已有</span>` : ""}
                  <button class="btn ghost sm" data-rm="${i}" title="移除这一行">移除</button>
                </td>
              </tr>`).join("") : `<tr><td colspan="8" class="muted" style="text-align:center;padding:18px">暂无期初数据，点「添加科目行」开始建账</td></tr>`}
          </tbody>
        </table>
      </div>
      <div class="cards" id="bg-totals" style="margin-top:14px"></div>
      <div class="muted u-sm" id="bg-balance-note" style="margin-top:6px;min-height:16px"></div>`;
    paintTotals();
    $all(".bg-code", main).forEach((inp) => inp.oninput = () => { draft[+inp.dataset.i].account_code = inp.value.trim(); });
    if ($("#bg-export")) $("#bg-export").addEventListener("click", () => downloadUrl("/api/export/begin"));
    if ($("#bg-collect")) $("#bg-collect").addEventListener("click", () => openImportCollection());
    if ($("#bg-tpl")) $("#bg-tpl").addEventListener("click", () => downloadUrl("/books/opening-sheet/template"));
    if ($("#bg-check")) $("#bg-check").addEventListener("click", () => openBeginReconcile());
    $all(".bg-dir", main).forEach((sel) => sel.onchange = () => { draft[+sel.dataset.i].dir = sel.value; paintTotals(); });
    $all(".bg-yb", main).forEach((inp) => inp.oninput = () => { draft[+inp.dataset.i].yb = inp.value; paintTotals(); });
    $all(".bg-ad", main).forEach((inp) => inp.oninput = () => { draft[+inp.dataset.i].ad = inp.value; paintTotals(); });
    $all(".bg-ac", main).forEach((inp) => inp.oninput = () => { draft[+inp.dataset.i].ac = inp.value; paintTotals(); });
    $all(".bg-qty", main).forEach((inp) => inp.oninput = () => draft[+inp.dataset.i].qty = inp.value);
    // 「移除」要把已有行的 id 记进待删清单，否则界面看着行没了、库里纹丝不动。
    //
    // 原先是两处叠加：① 保存只提交"有科目编码"的行，后端是 upsert（ON CONFLICT
    // DO UPDATE），**没有任何删除语义**；② 更前面一步，已经保存过的行压根**不渲染
    // 移除按钮**（r.id > 0 时那格显示"已有"），编码框还是 readonly —— 也就是
    // 会计录错一个科目之后，在界面上**没有任何办法把它删掉或改掉**。
    // 实测：移除 140501 → 保存 → DB 纹丝不动，试算平衡从平衡变成差 999,999。
    //
    // 按 id 精确删，不做"整表替换"：同一张表可能还有别人维护的行（带辅助核算
    // 维度的期初），按提交集合反推该删谁会误删。
    $all("[data-rm]", main).forEach((b) => b.onclick = () => {
      const gone = draft[+b.dataset.rm];
      if (gone && gone.id > 0) (state.beginRemoved = state.beginRemoved || []).push(gone.id);
      draft.splice(+b.dataset.rm, 1);
      render();
    });
    $("#bg-add").onclick = () => { draft.push({ id: 0, account_code: "", orig_code: "", dir: "debit", yb: "", ad: "", ac: "", qty: "" }); render(); };
    if ($("#bg-batch")) $("#bg-batch").onclick = () => openBeginBatch(main);
    $("#bg-save").onclick = async () => {
      // 改了编码的已有行 = 删旧行 + 按新编码插入。upsert 的键是 (account_code,
      // aux_key)，只提交新编码的话旧行会留在库里，变成"同一个科目两笔期初"。
      // 所以这里把 orig_code 与当前编码不一致的行 id 也算进待删清单。
      const deleteIds = (state.beginRemoved || []).slice();
      const payload = [];
      for (const r of draft) {
        const code = (r.account_code || "").trim();
        if (!code) continue;
        if (r.id > 0 && (r.orig_code || "") !== code) deleteIds.push(r.id);
        payload.push({ account_code: code, dir: r.dir, yb: r.yb.replace(/,/g, ""), ad: r.ad.replace(/,/g, ""), ac: r.ac.replace(/,/g, ""), qty: r.qty ? r.qty.replace(/,/g, "") : null });
      }
      // 录入了金额但试算不平：先拦一道，别让不平衡的期初落库。
      // 后端也会校验（见 save_begin 的科目存在性检查），但这里能给出
      // 精确到差额的提示，且不必往返一趟。
      const t = computeTotals();
      const hasAny = draft.some((r) => num(r.yb) || num(r.ad) || num(r.ac));
      if (hasAny && !t.balanced) {
        const diff = Math.abs((t.sumYbDebit + t.sumAd) - (t.sumYbCredit + t.sumAc)).toFixed(2);
        if (!(await confirmDialog(`期初的借方和贷方对不上，差 ${diff}。\n\n` +
          `期初不平，后面每张报表都会带着这个差额走。仍要保存吗？`, true))) return;
      }
      try {
        const r = await api("/begin", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ rows: payload, delete_ids: deleteIds }) });
        const n = (r && r.count) || payload.length;
        const d = (r && r.deleted) || 0;
        toast(`已保存 ${n} 条期初${d ? `，删除 ${d} 条` : ""}`, "ok");
        state.beginDraft = null;
        state.beginRemoved = null;
        viewBegin(main);
      } catch (e) { toast(e.message, "err"); }
    };
  };
  render();
}

// ---------------- 辅助核算档案 ----------------
async function viewAux(main) {
  const kind = state.auxKind || "customer";
  main.innerHTML = `<h2>辅助核算档案</h2><div class="muted">加载中…</div>`;
  // 停用档案默认不列（凭证/收付款单选客商时不该出现），但基础资料页必须能
  // 把它们调出来 —— 否则「停用」在网页端是单向操作：停掉之后再也找不到，
  // 也就没法恢复。
  // 唯一来源是 state.auxShowDisabled：勾选框的 checked 与这里的请求参数都读它。
  // （第一版把它存成局部变量 showOff，再在 renderAux 里引用 —— 那是另一个函数，
  //   运行到那一步直接 ReferenceError，整个档案页白屏。`node --check` 只验语法，
  //   验不出「引用了作用域外的变量」，这条是 CI 的 E2E 抓到的。）
  const showOff = !!state.auxShowDisabled;
  let rows;
  try {
    rows = await api(`/aux?kind=${encodeURIComponent(kind)}${showOff ? "&include_disabled=1" : ""}`);
  }
  catch (e) { main.innerHTML = `<h2>辅助核算档案</h2><div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; return; }
  renderAux(main, rows, kind);
}

function renderAux(main, rows, kind) {
  main.innerHTML = `
    <h2>辅助核算档案</h2>
    <div class="toolbar">
      ${AUX_KINDS.map((k) => `<button class="btn sm ${kind === k.code ? "primary" : "ghost"}" data-kind="${k.code}">${k.label}</button>`).join("")}
      <div class="spacer"></div>
      <label class="muted" style="display:flex;align-items:center;gap:4px;font-size:12px">
        <input type="checkbox" id="aux-showoff" ${state.auxShowDisabled ? "checked" : ""}> 显示停用
      </label>
      ${can("export") ? `<button class="btn ghost" id="aux-export" title="导出当前类型的全部档案；改完可以原样导回去">导出 Excel</button>` : ""}
      ${can("aux_edit") ? `<button class="btn" id="aux-batch">批量粘贴</button>` : ""}
      ${can("aux_edit") ? `<button class="btn" id="aux-preset" hidden>预置常用档案</button>` : ""}
      ${can("aux_edit") ? `<button class="btn primary" id="aux-new">新增${esc(AUX_KINDS.find((k) => k.code === kind).label)}</button>` : ""}
    </div>
    <div id="aux-hint" class="muted" style="font-size:12px;margin-bottom:6px"></div>
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
  // 空档案引导 + 预置按钮都从后端拿清单，不在前端硬编码：
  // 只有 fincore 的 AuxPreset 一份来源，改一处就够；也避免前端说
  // 「有预置」而后端返回空，按钮点了没反应。
  loadAuxPreset(main, rows, kind);
  $all("[data-kind]", main).forEach((b) => b.addEventListener("click", () => { state.auxKind = b.dataset.kind; viewAux(main); }));
  if ($("#aux-showoff")) $("#aux-showoff").addEventListener("change", (ev) => {
    state.auxShowDisabled = ev.target.checked;
    viewAux(main);
  });
  if ($("#aux-new")) $("#aux-new").addEventListener("click", () => openAuxEditor(main, null, kind));
  if ($("#aux-export")) $("#aux-export").addEventListener("click", () => downloadUrl(`/api/export/aux?kind=${encodeURIComponent(kind)}`));
  if ($("#aux-batch")) $("#aux-batch").addEventListener("click", () => openAuxBatch(main, kind));
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

/// 辅助档案的批量粘贴窗口。
///
/// 与桌面端 `views/aux_batch.rs` 同一套解析（在后端 `fincore::batch::auxiliary`），
/// 两边的写法支持必须一致 —— 用户会发现「桌面上能粘的，网页上粘不了」。
function openAuxBatch(main, kind) {
  const label = AUX_KINDS.find((k) => k.code === kind).label;
  const body = (text) => api("/aux-batch", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ kind, text }),
  });
  const jsonBody = (payload) => ({
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(payload),
  });

  batchPasteDialog({
    title: `批量粘贴${label}`,
    hint: [
      `把 Excel 里的${label}名单复制过来粘贴，每行一条。<b>先预览再导入</b> —— 粘错位（多带表头、带上合计行）会让你先看见，不会直接变成脏档案。`,
      `两种写法都行：<b>两列「编码,名称」</b>（有自己的编码体系时用）；<b>只写名称</b>（编码自动编号 C01、C02…，不用自己想编码）。`,
      `编码已存在的会<b>跳过，不会覆盖</b>你原来的档案。`,
    ],
    example: `${label}编码\t${label}名称\nC01\t杭州明远贸易有限公司\nC02\t宁波华信电子有限公司`,
    placeholder: `${label}编码,${label}名称\nC01,杭州明远贸易有限公司\nC02,宁波华信电子有限公司`,
    parse: body,
    importRows: (rows) => api("/aux-batch", jsonBody({ kind, rows })),
    cols: [
      { key: "code", label: "编码" },
      { key: "name", label: "名称" },
      { key: "skip", label: "状态", cls: "num" },
    ],
    onDone: () => rerenderView("aux", main),
  });
}

/// 拉该类别的预置清单，按「有 / 无预置」×「库空 / 不空」给不同提示。
async function loadAuxPreset(main, rows, kind) {
  const hint = $("#aux-hint", main);
  const btn = $("#aux-preset", main);
  let p;
  try { p = await api(`/aux-presets?kind=${encodeURIComponent(kind)}`); }
  catch (e) { return; } // 端点不可用不影响主流程

  if (!p.available) {
    // 这一类没有通用档案名。与其藏起按钮让用户到处找，不如说清楚为什么。
    if (hint) {
      hint.innerHTML = `没有可预置的${esc(p.label)}。${["客户", "供应商", "存货", "职员", "项目"].indexOf(p.label) >= 0
        ? "这几类的名字没有通用值，预置一个假的会跟着凭证进报表 —— 请点右上「新增」填你自己的。"
        : ""}`;
    }
    if (btn) btn.remove();
    return;
  }

  const names = p.items.map((x) => `${x.code} ${x.name}`).join("、");
  if (hint) {
    if (!rows.length) {
      hint.innerHTML = `还没有任何${esc(p.label)}。带${esc(p.label)}核算的科目<b>必须</b>填这一项才能记凭证，
        没有档案就一张凭证也存不了。点「预置常用档案」建出通用条目：${esc(names)}（建完请按你公司的情况改名）。`;
    } else {
      hint.textContent = `可预置：${names}（编码相同的已有档案不会被覆盖）`;
    }
  }
  if (btn) {
    btn.hidden = false;
    btn.title = `将新建：${names}。已有的编码不会被覆盖。`;
    btn.addEventListener("click", async () => {
      if (!(await confirmDialog(`预置${p.label}常用档案？`,
        `将新建 ${p.items.length} 条：${names}\n这些都是通用占位，建完请按你公司的情况改名或补全。编码相同的已有档案不会被覆盖。`,
        true))) return;
      try {
        const r = await api("/aux-presets", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ kind }),
        });
        let msg = r.inserted ? `已预置 ${r.inserted} 条${p.label}档案，请按你公司的情况改名。`
                             : `${p.label}档案已经都有对应的了，没有新增。`;
        if (r.skipped && r.skipped.length) {
          msg += ` 已有 ${r.skipped.length} 条被跳过（编码相同）：${r.skipped.join("、")}。你的原档案没动。`;
        }
        toast(msg, "ok");
        rerenderView("aux", main);
      } catch (e) { toast(e.message, "err"); }
    });
  }
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
// 套打偏移：夹在 ±20mm 内；输入非法时沿用旧值（不要让一次手误把版式打歪）
function clampI16(v, fallback) {
  const n = parseInt(v, 10);
  if (!Number.isFinite(n)) return Number.isFinite(+fallback) ? +fallback : 0;
  return Math.max(-20, Math.min(20, n));
}

async function viewOptions(main) {
  main.innerHTML = `<h2>账套参数</h2><div class="muted">加载中…</div>`;
  let o;
  try { o = await api("/options"); } catch (e) { main.innerHTML = `<h2>账套参数</h2><div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; return; }
  // 打印设置：老账套没有这个字段时用与 fincore::print::PrintOptions 一致的默认值
  const p = Object.assign({
    orientation: "auto", font_pt: 11, margin_mm: 12,
    show_company: true, show_printed_at: true, show_page_no: true, show_signatures: true,
    sign_line: "制表：　　　　　审核：　　　　　负责人：",
    // 套打纸型：凭证/账簿缺省 A4，单据缺省「跟随模板（空串）」
    voucher_paper: "A4", ledger_paper: "A4", doc_paper: "",
  }, o.print || {});
  const pOr = p.orientation, pFnt = p.font_pt, pMar = p.margin_mm;
  // 套打偏移：老账套没有这两个字段，缺省 0
  const pOx = Number.isFinite(+p.offset_x_mm) ? +p.offset_x_mm : 0;
  const pOy = Number.isFinite(+p.offset_y_mm) ? +p.offset_y_mm : 0;
  // 纸型下拉：选项与后端 fincore::print::paper_choices() 逐项对应，
  // 两边各写一份清单的话，加纸型时必然漏掉一头。
  const PAPER_CHOICES = [
    ["A4", "A4（210×297）"],
    ["A5", "A5（210×148）"],
    ["third", "三等分联（99×210）"],
    ["241x140", "241×140 连续纸"],
    ["", "跟随单据模板"],
  ];
  const paperSel = (id, cur, allowFollow) => {
    const v = (cur || "").trim();
    const opts = PAPER_CHOICES.filter(([k]) => allowFollow || k !== "");
    const has = opts.some(([k]) => k.toLowerCase() === v.toLowerCase());
    return `<select id="${id}">${opts
      .map(([k, l]) => `<option value="${k}" ${has && k.toLowerCase() === v.toLowerCase() ? "selected" : ""}>${l}</option>`)
      .join("")}${has ? "" : `<option value="${esc(v)}" selected>${esc(v || "跟随单据模板")}</option>`}</select>`;
  };
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
        <label style="display:inline-flex;align-items:center;gap:4px"><input type="checkbox" id="op-production" ${o.enable_production ? "checked" : ""} />启用生产制造（MRP / 排产 / 工序报工）</label>
        <label>预算控制 <select id="op-budget"><option value="" ${!o.budget_control ? "selected" : ""}>关闭</option><option value="warn" ${o.budget_control === "warn" ? "selected" : ""}>超预算提醒（放行）</option><option value="strong" ${o.budget_control === "strong" ? "selected" : ""}>超预算强控（拒绝保存）</option></select></label>
        <label>单据前缀 <input id="op-pfx-po" value="${esc(((o.doc_prefixes || {}).po) || "")}" placeholder="CG" title="采购订单" style="width:56px" /> <input id="op-pfx-so" value="${esc(((o.doc_prefixes || {}).so) || "")}" placeholder="XS" title="销售订单" style="width:56px" /> <input id="op-pfx-req" value="${esc(((o.doc_prefixes || {}).req) || "")}" placeholder="QG" title="请购单" style="width:56px" /> <input id="op-pfx-quo" value="${esc(((o.doc_prefixes || {}).quo) || "")}" placeholder="BJ" title="报价单" style="width:56px" /> <input id="op-pfx-prod" value="${esc(((o.doc_prefixes || {}).prod) || "")}" placeholder="SC" title="生产订单" style="width:56px" /></label>
      </div>
      <p class="muted" style="font-size:12px">启用期间与科目级长影响科目编码校验与凭证编号，修改请谨慎；已开账后不建议改动。</p>
      <h3 style="margin:18px 0 4px">打印预览</h3>
      <p class="muted" style="font-size:12px;margin-top:0">账套级设置：存一次，本账套所有报表打印都用同一套版式（桌面版也读这里）。</p>
      <div class="field" style="display:flex;gap:20px;flex-wrap:wrap;align-items:end">
        <label>纸张方向
          <select id="op-print-orient">
            <option value="auto" ${pOr === "auto" ? "selected" : ""}>自动（列多转横向）</option>
            <option value="portrait" ${pOr === "portrait" ? "selected" : ""}>纵向</option>
            <option value="landscape" ${pOr === "landscape" ? "selected" : ""}>横向</option>
          </select>
        </label>
        <label>正文字号 <input id="op-print-font" type="number" min="9" max="14" value="${pFnt}" style="width:64px" /> pt</label>
        <label>页边距 <input id="op-print-margin" type="number" min="5" max="30" value="${pMar}" style="width:64px" /> mm</label>
      </div>
      <div class="field" style="display:flex;gap:20px;flex-wrap:wrap">
        <label style="display:inline-flex;align-items:center;gap:4px"><input type="checkbox" id="op-print-company" ${p.show_company ? "checked" : ""} />编制单位</label>
        <label style="display:inline-flex;align-items:center;gap:4px"><input type="checkbox" id="op-print-time" ${p.show_printed_at ? "checked" : ""} />打印时间</label>
        <label style="display:inline-flex;align-items:center;gap:4px"><input type="checkbox" id="op-print-pageno" ${p.show_page_no ? "checked" : ""} />页码</label>
        <label style="display:inline-flex;align-items:center;gap:4px"><input type="checkbox" id="op-print-sign" ${p.show_signatures ? "checked" : ""} />落款</label>
      </div>
      <div class="field"><label>落款文字（制表 / 审核 / 负责人，可自行改）</label><input id="op-print-signline" value="${esc(p.sign_line)}" /></div>
      <div class="field" style="display:flex;gap:20px;flex-wrap:wrap;align-items:end">
        <label>套打偏移 左右 <input id="op-print-ox" type="number" min="-20" max="20" value="${pOx}" style="width:64px" /> mm</label>
        <label>上下 <input id="op-print-oy" type="number" min="-20" max="20" value="${pOy}" style="width:64px" /> mm</label>
        <span class="muted" style="font-size:12px">正数=向右/向下。凭证、账簿、单据套打走预印纸时用它对位</span>
      </div>
      <div class="field" style="display:flex;gap:20px;flex-wrap:wrap;align-items:end">
        <label>凭证纸型 ${paperSel("op-print-vpaper", p.voucher_paper, false)}</label>
        <label>账簿纸型 ${paperSel("op-print-lpaper", p.ledger_paper, false)}</label>
        <label>单据纸型 ${paperSel("op-print-dpaper", p.doc_paper, true)}</label>
        <span class="muted" style="font-size:12px">凭证多为 A5 / 241×140 连续纸；账簿 A4；单据可「跟随单据模板」</span>
      </div>
      <div class="toolbar" style="margin-top:6px">
        <button class="btn ghost" id="op-print-preview">试打一张（按上面设置，未保存也生效）</button>
        <span class="muted" style="font-size:12px">页码靠浏览器「页边距盒子」实现，Chrome 131 之前的旧内核会整块忽略——勾了页码纸上却没有，就用新版浏览器。</span>
      </div>
      ${can("sys_option") ? `<div class="foot" style="margin-top:10px;display:flex;gap:8px;align-items:center;flex-wrap:wrap">
        <button class="btn primary" id="op-save">保存参数</button>
        <button class="btn ghost" id="op-export" title="导出成 JSON 文件，换机器时带走">导出配置</button>
        <button class="btn ghost" id="op-import" title="从 JSON 文件恢复；编码级长/启用期间与当前账套不同会被拒绝">导入配置</button>
        <input type="file" id="op-import-file" accept=".json,application/json" style="display:none">
        <button class="btn ghost" id="op-reset" title="只恢复偏好（打印版式/预算控制/单据前缀/生产模块）；企业名称、编码级长、启用期间、默认科目不动">恢复默认</button>
      </div>` : `<p class="muted">无修改权限（需要 sys_option）</p>`}
    </div>`;
  const prev = $("#op-print-preview");
  if (prev) prev.addEventListener("click", () => {
    // 未保存也能试：把当前表单里的版式直接拼进打印页的地址栏参数
    const q = new URLSearchParams({
      po_orient: $("#op-print-orient").value,
      po_font: $("#op-print-font").value,
      po_margin: $("#op-print-margin").value,
      po_company: $("#op-print-company").checked ? "1" : "0",
      po_time: $("#op-print-time").checked ? "1" : "0",
      po_pageno: $("#op-print-pageno").checked ? "1" : "0",
      po_sign: $("#op-print-sign").checked ? "1" : "0",
      po_signline: $("#op-print-signline").value,
      po_ox: $("#op-print-ox").value,
      po_oy: $("#op-print-oy").value,
    });
    const cur = state.current || "";
    window.open(`/api/reports/trial-balance/print?from=${encodeURIComponent(cur)}&to=${encodeURIComponent(cur)}&${q}`, "_blank");
  });
  if ($("#op-export")) $("#op-export").addEventListener("click", () => {
    downloadUrl("/api/options/export");
  });
  if ($("#op-import")) $("#op-import").addEventListener("click", () => $("#op-import-file").click());
  if ($("#op-import-file")) $("#op-import-file").addEventListener("change", async (ev) => {
    const f = ev.target.files && ev.target.files[0];
    if (!f) return;
    try {
      const text = await f.text();
      let body;
      try { body = JSON.parse(text); } catch (e) { throw new Error(`不是合法的 JSON：${e.message}`); }
      const r = await api("/options/import", { method: "POST", body });
      // 服务端会把采纳后的完整参数返回，用它覆盖本地缓存 —— 否则点完「保存参数」
      // 会拿旧的 o 覆盖一遍，导入等于白做
      o = (r && r.options) ? r.options : o;
      toast("配置已导入", "ok");
      viewOptions(main);
    } catch (e) { toast(e.message, "err"); }
  });
  if ($("#op-reset")) $("#op-reset").addEventListener("click", async () => {
    if (!confirm("恢复默认只会重置「打印版式 / 预算控制 / 单据前缀 / 生产模块」。\n企业名称、科目编码级长、启用期间、业务默认科目都不会动。继续？")) return;
    try {
      const r = await api("/options/reset", { method: "POST", body: {} });
      o = (r && r.options) ? r.options : o;
      toast("已恢复默认设置", "ok");
      viewOptions(main);
    } catch (e) { toast(e.message, "err"); }
  });
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
      enable_production: $("#op-production").checked,
      require_cashier: $("#op-cashier").checked,
      budget_control: $("#op-budget").value,
      doc_prefixes: {
        po: $("#op-pfx-po").value.trim(), so: $("#op-pfx-so").value.trim(),
        req: $("#op-pfx-req").value.trim(), quo: $("#op-pfx-quo").value.trim(),
        prod: $("#op-pfx-prod").value.trim(),
      },
      // 整个对象覆盖式保存：不把 print 带回去，用户刚调好的版式会被重置成默认
      print: Object.assign({}, p, {
        orientation: $("#op-print-orient").value,
        font_pt: Math.min(14, Math.max(9, parseInt($("#op-print-font").value, 10) || p.font_pt)),
        margin_mm: Math.min(30, Math.max(5, parseInt($("#op-print-margin").value, 10) || p.margin_mm)),
        show_company: $("#op-print-company").checked,
        show_printed_at: $("#op-print-time").checked,
        show_page_no: $("#op-print-pageno").checked,
        show_signatures: $("#op-print-sign").checked,
        sign_line: $("#op-print-signline").value,
        offset_x_mm: clampI16($("#op-print-ox").value, p.offset_x_mm),
        offset_y_mm: clampI16($("#op-print-oy").value, p.offset_y_mm),
        voucher_paper: $("#op-print-vpaper").value,
        ledger_paper: $("#op-print-lpaper").value,
        doc_paper: $("#op-print-dpaper").value,
      }),
      biz_accounts: {
        ar: $("#op-biz-ar").value.trim() || "112201",
        ap: $("#op-biz-ap").value.trim() || "220201",
        income: $("#op-biz-income").value.trim() || "600101",
        tax_sales: $("#op-biz-tax").value.trim() || "22210102",
        material: $("#op-biz-mat").value.trim() || "140301",
        fund: $("#op-biz-fund").value.trim() || "100201",
      },
    });
    try {
      await api("/options", { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
      // 用服务端实际存下的对象回填：否则再点一次保存会把这次的值又覆盖回旧值
      o = Object.assign({}, o, body);
      toast("已保存账套参数", "ok");
    } catch (e) { toast(e.message, "err"); }
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
      ${can("backup") ? `<button class="btn primary" id="bk-new">立即备份</button>
      <button class="btn ghost" id="bk-download">备份并下载打包</button>` : ""}
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
  const dl = $("#bk-download");
  if (dl) dl.addEventListener("click", () => { location.href = "/api/backup/download"; });
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
