// FinBook Web 前端 — 试算平衡、科目明细、资产负债表 / 利润表 / 现金流量表、打印预览
//
// 这一份是 `crates/finweb/static/app.js` 拆分出来的（原单文件 14k 行、295 个顶层函数）。
// 拆分方式：**经典 script，按原始行序切分，不重排任何代码**——顶层 function/const 的
// 提升与声明顺序完全保持原样，所以跨文件不存在 TDZ/未定义问题；只有 `VIEWS` 路由表
// 因为引用全部视图函数，被挪到了最后加载的 app-hr.js。
// 加载顺序由 `index.html` 的 <script> 顺序决定（也是 `main.rs` 里 asset_version 的清单顺序）。
// 新增文件必须同步三处：index.html、main.rs 的 asset_version 清单、tools/lib-app-src.js 的解析（自动）。
async function viewReports(main) {
  main.innerHTML = `
    <h2>报表中心</h2>
    <div class="toolbar">
      <label>从 <input id="r-from" type="month" value="${esc(state.current || "")}" style="width:90px" /></label>
      <label>至 <input id="r-to" type="month" value="${esc(state.current || "")}" style="width:90px" /></label>
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
      ${can("export") ? `<button class="btn ghost sm" id="r-xlsx" title="金额是真数字，能直接求和">导出 Excel</button>
      <button class="btn ghost sm" id="r-export">导出 CSV</button>
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
    $("#r-xlsx").addEventListener("click", () => { const f = $("#r-from").value, t = $("#r-to").value; downloadUrl(`/api/reports/trial-balance/xlsx?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`); });
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
    @media print{body{font-size:12px;margin:0;}}
    .pbar{position:sticky;top:0;z-index:9;display:flex;gap:14px;align-items:center;background:#1a1a1a;color:#fff;padding:8px 14px;font-family:system-ui,sans-serif;font-size:12px;}
    .pbar button{background:#fff;color:#111;border:0;border-radius:4px;padding:6px 14px;font-size:13px;cursor:pointer;}
    @media print{.pbar{display:none;}}</style></head>
    <body>
    <div class="pbar"><button onclick="window.print()">🖨 打印本页</button><span>纸张 / 份数在打印对话框中选择；关闭本页即取消</span></div>
    <h2>${esc(title)}</h2>
    <div class="meta"><span>${esc(session.user ? session.user.display_name : "")}</span><span>打印时间：${esc(today())}</span></div>
    ${tableEl.outerHTML}
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

// 报表口径行：把服务端**实际**采用的期间回显出来。
// 前端填的期间与后端解析结果可能不一致（留空走默认、手输错误被 400 拦下），
// 只显示输入框的值时，「金额全是 0」到底是没数据还是期间没生效就分不清。
function statementMetaHtml(r, label) {
  const t = (r && r.table) || {};
  const sub = label == null ? (t.subtitle || "") : label;
  const from = (r && r.from) || "?", to = (r && r.to) || "?";
  return `<div class="muted" style="margin-bottom:8px">${esc(sub)}
    <span title="服务端实际取数期间；若与你填的不一致，说明期间没传到后端">（服务端取数：${esc(from)} ~ ${esc(to)}）</span>
  </div>`;
}

// 整张表所有单元格都是 0 —— 用于利润表空态提示。
// 只在「全零」时提示，有任何非零就不出现，避免误导。
function tableAllZero(t) {
  const rows = (t && t.rows) || [];
  return rows.length > 0 && rows.every((r) => (r.values || []).every((v) => moneyNum(v) === 0));
}

async function viewBalanceSheet(main) {
  main.innerHTML = `<h2>资产负债表</h2>
    <div class="toolbar">
      <label>从 <input id="bs-from" type="month" value="${esc(state.current)}" style="width:90px" /></label>
      <label>至 <input id="bs-to" type="month" value="${esc(state.current)}" style="width:90px" /></label>
      <button class="btn primary" id="bs-run">查询</button>
      ${can("export") ? `<button class="btn ghost sm" id="bs-xlsx" title="导出 资产负债表（金额是真数字）">导出 Excel</button>` : ""}
      <button class="btn ghost sm" id="bs-print">打印预览</button>
    </div>
    <div id="bs-result" class="muted">填写期间后点击查询</div>`;
  const load = async () => {
    const f = $("#bs-from").value.trim(), t = $("#bs-to").value.trim();
    try {
      const r = await api(`/reports/balance-sheet?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`);
      $("#bs-result").innerHTML = statementMetaHtml(r) + statementTableHtml(r.table);
    } catch (e) { toast(e.message, "err"); }
  };
  $("#bs-run").addEventListener("click", load);
  if ($("#bs-xlsx")) $("#bs-xlsx").addEventListener("click", () => { const f = $("#bs-from").value, t = $("#bs-to").value; downloadUrl(`/api/reports/balance-sheet/xlsx?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`); });
  $("#bs-print").addEventListener("click", () => { const f = $("#bs-from").value, t = $("#bs-to").value; window.open(`/api/reports/balance-sheet/print?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`, "_blank"); });
  load();
}

async function viewIncomeStatement(main) {
  main.innerHTML = `<h2>利润表</h2>
    <div class="toolbar">
      <label>从 <input id="is-from" type="month" value="${esc(state.current)}" style="width:90px" /></label>
      <label>至 <input id="is-to" type="month" value="${esc(state.current)}" style="width:90px" /></label>
      <button class="btn primary" id="is-run">查询</button>
      ${can("export") ? `<button class="btn ghost sm" id="is-xlsx" title="导出 利润表（金额是真数字）">导出 Excel</button>` : ""}
      <button class="btn ghost sm" id="is-print">打印预览</button>
    </div>
    <div id="is-result" class="muted">填写期间后点击查询</div>`;
  const load = async () => {
    const f = $("#is-from").value.trim(), t = $("#is-to").value.trim();
    try {
      const r = await api(`/reports/income-statement?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`);
      // 全零不等于「坏了」：利润表只统计已记账凭证，且剔除结转损益凭证。
      // 不解释清楚，用户只能对着 0.00 猜，之前就是因此报「利润表不正常」。
      const zero = tableAllZero(r.table)
        ? `<div class="banner unset" style="margin-bottom:8px">
             <b>本期各项目都是 0.00</b>：该期间没有<b>已记账</b>的损益发生额。
             利润表只统计已记账凭证（草稿 / 已审核未记账都不算），并剔除系统生成的结转损益凭证。
             请先确认左上角账套与期间，再到「凭证」查看本期 6xxx 损益科目是否已记账。
           </div>`
        : "";
      $("#is-result").innerHTML = statementMetaHtml(r) + zero + statementTableHtml(r.table);
    } catch (e) { toast(e.message, "err"); }
  };
  $("#is-run").addEventListener("click", load);
  if ($("#is-xlsx")) $("#is-xlsx").addEventListener("click", () => { const f = $("#is-from").value, t = $("#is-to").value; downloadUrl(`/api/reports/income-statement/xlsx?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`); });
  $("#is-print").addEventListener("click", () => { const f = $("#is-from").value, t = $("#is-to").value; window.open(`/api/reports/income-statement/print?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`, "_blank"); });
  load();
}

async function viewCashFlow(main) {
  main.innerHTML = `<h2>现金流量表</h2>
    <div class="toolbar">
      <label>从 <input id="cf-from" type="month" value="${esc(state.current)}" style="width:90px" /></label>
      <label>至 <input id="cf-to" type="month" value="${esc(state.current)}" style="width:90px" /></label>
      <button class="btn primary" id="cf-run">查询</button>
      ${can("export") ? `<button class="btn ghost sm" id="cf-xlsx" title="导出 现金流量表（金额是真数字）">导出 Excel</button>` : ""}
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
        <tr><td colspan="3" class="muted">${r.ties ? "✔ 净增加额与货币资金变动勾稽一致" : "✖ 勾稽不符"}</td></tr>
        <tr><td colspan="3" class="${r.fully_assigned ? "muted" : ""}" style="${r.fully_assigned ? "" : "color:var(--warn-t)"}">${esc(r.assignment_note || "")}</td></tr>
        ${r.prev ? `<tr><td colspan="3" class="muted">上期（${esc(r.prev.from)}~${esc(r.prev.to)}）：经营净额 ${esc(r.prev.operating_net)}　净增加额 ${esc(r.prev.net_increase)}　—— 本期经营净额变动 <b style="color:${String(r.delta.operating_net).startsWith("-") ? "var(--err,#c00)" : "var(--ok,#2e7d32)"}">${esc(r.delta.operating_net)}</b></td></tr>` : ""}`;
      $("#cf-result").innerHTML = statementMetaHtml(r, `${r.from} 至 ${r.to}`) + `
        <table class="grid"><thead><tr>${head}</tr></thead><tbody>
        ${section("经营活动产生的现金流量", r.operating, r.operating_net)}
        ${section("投资活动产生的现金流量", r.investing, r.investing_net)}
        ${section("筹资活动产生的现金流量", r.financing, r.financing_net)}
        ${tail}</tbody></table>`;
    } catch (e) { toast(e.message, "err"); }
  };
  $("#cf-run").addEventListener("click", load);
  if ($("#cf-xlsx")) $("#cf-xlsx").addEventListener("click", () => { const f = $("#cf-from").value, t = $("#cf-to").value; downloadUrl(`/api/reports/cash-flow/xlsx?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`); });
  $("#cf-print").addEventListener("click", () => { const f = $("#cf-from").value, t = $("#cf-to").value; window.open(`/api/reports/cash-flow/print?from=${encodeURIComponent(f)}&to=${encodeURIComponent(t)}`, "_blank"); });
  load();
}
