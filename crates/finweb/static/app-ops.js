// FinBook Web 前端 — 报工、所有者权益、期间对比、日记账、对账、客户、预算预警、期末结账、固定资产、银行、往来核销
//
// 这一份是 `crates/finweb/static/app.js` 拆分出来的（原单文件 14k 行、295 个顶层函数）。
// 拆分方式：**经典 script，按原始行序切分，不重排任何代码**——顶层 function/const 的
// 提升与声明顺序完全保持原样，所以跨文件不存在 TDZ/未定义问题；只有 `VIEWS` 路由表
// 因为引用全部视图函数，被挪到了最后加载的 app-hr.js。
// 加载顺序由 `index.html` 的 <script> 顺序决定（也是 `main.rs` 里 asset_version 的清单顺序）。
// 新增文件必须同步三处：index.html、main.rs 的 asset_version 清单、tools/lib-app-src.js 的解析（自动）。
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
      <button class="btn ghost sm" id="wr-new">新建生产订单</button>
      <button class="btn ghost sm" id="wr-out">新建委外</button>
      <button class="btn ghost sm" id="wr-fee">确认加工费</button>
    </div>
    <div id="wr-ops" class="muted">选择生产订单后加载工序</div>`;
  let orders = [];
  try {
    const r = await api("/prod");
    orders = r.orders || [];
  } catch (e) { toast(e.message, "err"); }
  const verTag = (o) => (o.routing_version ? ` @${esc(o.routing_version)}` : "");
  $("#wr-po").innerHTML = orders.map((o) => `<option value="${o.id}">${o.order_kind === "outsourcing" ? "【委外】" : ""}${esc(o.no)} · ${esc(o.item_name)}${verTag(o)}（${o.status}）</option>`).join("") || `<option value="">当前期间无生产订单</option>`;
  // 工厂链动作：下达（MRP 页）→ 开工 → 领料出库 → 完工入库（BOM 在「维护BOM」编辑）
  const selOrder = () => orders.find((o) => String(o.id) === $("#wr-po").value);
  const syncCq = () => {
    const o = selOrder();
    $("#wr-cq").value = o ? String(Math.max(0, Number(o.planned_qty) - Number(o.completed_qty))) : "0";
  };
  $("#wr-po").onchange = syncCq;
  syncCq();
  const reloadView = () => rerenderView("work-report", main);
  $("#wr-new").onclick = () => openProdOrderEditor(reloadView);
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
    <div class="toolbar"><label>期间 <input id="eq-period" type="month" value="${esc(state.current)}" style="width:90px" /></label>
    <button class="btn primary" id="eq-run">查询</button>
      ${can("export") ? `<button class="btn ghost sm" id="eq-xlsx" title="导出 所有者权益变动表（金额是真数字）">导出 Excel</button>` : ""}
      <button class="btn ghost sm" id="eq-run-print">打印预览</button></div>
    <div id="eq-result" class="muted">填写期间后点击查询</div>`;
  $("#eq-run-print").addEventListener("click", () => { const el = $("#eq-result").querySelector("table"); printPreview("所有者权益变动表", el); });
  if ($("#eq-xlsx")) $("#eq-xlsx").addEventListener("click", () => {
    const p = $("#eq-period").value.trim();
    downloadUrl(`/api/reports/equity/xlsx?from=${encodeURIComponent(p)}&to=${encodeURIComponent(p)}`);
  });
  $("#eq-run").addEventListener("click", async () => {
    const p = $("#eq-period").value.trim();
    try {
      const r = await api(`/reports/equity?period=${encodeURIComponent(p)}`);
      const s = r.statement;
      const rows = [...s.lines, s.total];
      $("#eq-result").innerHTML = `<table><thead><tr><th>项目</th><th>本年年初余额</th><th>本年增减变动</th><th>本年年末余额</th></tr></thead>
        <tbody>${rows.map((l) => `<tr><td>${esc(l.name)}</td><td class="r">${fmt(l.begin)}</td><td class="r">${fmt(l.change)}</td><td class="r">${fmt(l.end)}</td></tr>`).join("")}</tbody></table>
        <div class="muted">${r.ties ? "✔ 年初 + 增减 = 年末，勾稽通过" : "✖ 勾稽不符"}</div>`;
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
  $("#cp-run-print").addEventListener("click", () => { const el = $("#cp-result").querySelector("table"); printPreview("报表对比分析", el); });
  $("#cp-run").addEventListener("click", async () => {
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
      <label>期间 <input id="dl-from" type="month" value="${esc(state.current)}" style="width:90px" /></label>
      <button class="btn primary" id="dl-run">查询</button>
      <button class="btn ghost sm" id="dl-run-print">打印预览</button></div>
    <div id="dl-result" class="muted">填写科目与期间后点击查询</div>`;
  $("#dl-run-print").addEventListener("click", () => { const el = $("#dl-result").querySelector("table"); printPreview("科目日报表", el); });
  $("#dl-run").addEventListener("click", async () => {
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
  $("#rc-run-print").addEventListener("click", () => { const el = $("#rc-result").querySelector("table"); printPreview("期末对账", el); });
  $("#rc-run").addEventListener("click", async () => {
    const p = $("#rc-period").value.trim();
    try {
      const r = await api(`/reports/reconcile?period=${encodeURIComponent(p)}`);
      const items = r.items || [];
      $("#rc-result").innerHTML = items.map((i) => `<div class="card"><div class="row"><b>${esc(i.name)}</b><span class="tag ${i.ok ? "ok" : "err"}">${i.ok ? "通过" : "异常"}</span></div><div class="muted">${esc(i.detail)}</div></div>`).join("") || `<div class="muted">暂无对账数据</div>`;
    } catch (e) { toast(e.message, "err"); }
  });
}

// ===========================================================================
// 客户管理（只读视图层）
// ===========================================================================
//
// 【为什么只读】客户主数据在辅助档案（aux_entity kind='customer'），全系统唯一一份。
// 这个页面只做「以客户为主体」的**汇总与明细**：余额口径由后端
// findb::customers 保证走 settle::open_entries（H-3：只认已记账）。
//
// 页面上**故意没有**「编辑客户」按钮 —— 改档案仍然只有辅助档案页一个入口。
// 加第二个能改客户的地方 = 两份主数据 + 一个同步问题，且必然出现
// 「销售单据认的客户」与「客户管理里看到的客户」对不上。
//
// 【列表默认平铺】查过三个事实：建库种子里没有任何 aux_entity 行、
// 全仓库 parent_code: Some(...) 0 处、四张业务表都不存层级路径。
// 树视图是为一个不存在的数据形态做界面。见 docs/客户管理模块设计.md §5.3。
async function viewCustomers(main) {
  main.innerHTML = `<h2>客户管理</h2>
    <div class="toolbar">
      <label>期间 <input id="cu-period" value="${esc((state.current || '').replace('-', ''))}" style="width:90px" /></label>
      <label>搜索 <input id="cu-q" placeholder="编码/名称" style="width:140px" /></label>
      <label style="font-size:12px"><input type="checkbox" id="cu-only-open" checked /> 只看有欠款</label>
      <button class="btn primary sm" id="cu-load">查询</button>
      <button class="btn ghost sm" id="cu-print">打印预览</button>
      ${can("export") ? `<button class="btn ghost sm" id="cu-export" title="导出客户档案（编码/名称/备注），改完能导回">导出 Excel</button>` : ""}
      <span class="grow"></span>
      <span class="muted" style="font-size:12px">余额只算**已记账**凭证；改档案请到「辅助档案」</span>
    </div>
    <div id="cu-sum" class="muted" style="margin-bottom:6px"></div>
    <div id="cu-list" class="muted">加载中…</div>
    <div id="cu-pager" style="margin-top:8px"></div>`;

  const pager = createPagination($("#cu-pager"), 0, 50, (p, ps) => { state.cuPage = p; state.cuPageSize = ps; load(); });

  const load = async () => {
    const per = $("#cu-period").value.trim();
    const q = $("#cu-q").value.trim();
    const only = $("#cu-only-open").checked ? "1" : "";
    const page = state.cuPage || 1;
    const pageSize = state.cuPageSize || 50;
    try {
      const r = await api(`/customers?period=${encodeURIComponent(per)}&q=${encodeURIComponent(q)}&only_open=${only}&page=${page}&page_size=${pageSize}`);
      const rows = r.rows || [];
      // 顶部汇总用后端给的字段，不在前端重算 —— 前端重算就是第二套口径。
      $("#cu-sum").textContent = rows.length
        ? `${r.total} 个客户 · ${r.open_count} 个有未核销 · 未核销合计 ${r.open_sum}`
        : "";
      $("#cu-list").innerHTML = rows.length
        ? `<table class="grid"><thead><tr>
            <th>编码</th><th>名称</th><th class="num">应收余额</th><th class="num">未核销</th>
            <th class="num">笔数</th><th>最长账龄</th><th class="num">期初挂账</th><th></th>
          </tr></thead><tbody>${rows.map((x) => {
            const neg = Number(x.balance) < 0;
            return `<tr>
              <td>${esc(x.code)}</td>
              <td>${esc(x.name)}${x.disabled ? ' <span class="tag warn">停用</span>' : ""}</td>
              <td class="num"${neg ? ' style="color:var(--ok)"' : ""}>${fmt(x.balance)}</td>
              <td class="num"><b>${fmt(x.open_amount)}</b></td>
              <td class="num">${Number(x.open_count) || ""}</td>
              <td>${x.oldest_open_date ? esc(x.oldest_open_date) : "—"}</td>
              <td class="num">${fmt(x.opening_balance)}</td>
              <td class="row-actions"><button class="btn ghost sm" data-cu-open="${esc(x.code)}">往来明细</button></td>
            </tr>`;
          }).join("")}</tbody></table>`
        : `<div class="muted">没有符合条件的客户${only ? "（都不欠款，或账上没有应收记录）" : ""}</div>`;
      // 更新分页器
      pager.reload(r.total, pageSize);
      $("#cu-list").querySelectorAll("[data-cu-open]").forEach((b) => {
        b.onclick = () => openCustomerDetail(b.dataset.cuOpen, $("#cu-period").value.trim());
      });
    } catch (e) {
      $("#cu-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`;
    }
  };
  $("#cu-load").addEventListener("click", load);
  $("#cu-q").addEventListener("keydown", (e) => { if (e.key === "Enter") load(); });
  // 期间变化时自动重查：用户改了期间却在看上一期的数字，是最容易犯的错之一。
  $("#cu-period").addEventListener("change", () => { state.cuPage = 1; load(); });
  $("#cu-only-open").addEventListener("change", () => { state.cuPage = 1; load(); });
  $("#cu-print").addEventListener("click", () => {
    printPreview("客户往来汇总", $("#cu-list").querySelector("table"));
  });
  if ($("#cu-export")) $("#cu-export").addEventListener("click", () => downloadUrl("/api/export/aux?kind=customer"));
  // 初始化分页状态
  state.cuPage = 1;
  state.cuPageSize = 50;
  await load();
}

/// 客户详情（弹窗）：顶部汇总 + 往来明细 + 信用占用
function openCustomerDetail(code, period) {
  const m = modal(`<h3>客户 ${esc(code)}</h3>
    <div class="subtabs" id="cd-subtabs">
      <button class="subtab on" data-cd="lines">往来明细</button>
      <button class="subtab" data-cd="aging">账龄</button>
      <button class="subtab" data-cd="trade">交易历史</button>
      <button class="subtab" data-cd="dunning">催款</button>
    </div>
    <div id="cd-body" class="muted">加载中…</div>
    <div class="foot"><button class="btn ghost" id="cd-close">关闭</button></div>`);
  $("#cd-close", m).onclick = closeModal;
  // 子视图切换：只换显隐，不重新取数。
  // 账龄是一次性算好的（它只依赖 period + as_of，不随当前子视图变），
  // 每次切换都重取会让人多点一下就多等一个来回。
  const showSub = (which) => {
    $all("[data-cd]", m).forEach((b) => b.classList.toggle("on", b.dataset.cd === which));
    ["lines", "aging", "trade", "dunning"].forEach((k) => {
      const el = $("#cd-" + k, m);
      if (el) el.style.display = k === which ? "" : "none";
    });
  };
  $all("[data-cd]", m).forEach((b) => (b.onclick = () => showSub(b.dataset.cd)));
  (async () => {
    try {
      const r = await api(`/customers/${encodeURIComponent(code)}?period=${encodeURIComponent(period || "")}`);
      const c = r.customer || {};
      const s = c.summary || {};
      const lines = c.lines || [];
      const cr = r.credit;
      const neg = Number(s.balance) < 0;
      $("#cd-body", m).innerHTML = `
        <div class="panel" style="margin:0 0 10px">
          <div style="display:flex;gap:18px;flex-wrap:wrap;font-size:13px">
            <span><span class="muted">应收余额</span> <b${neg ? ' style="color:var(--ok)"' : ""}>${fmt(s.balance)}</b></span>
            <span><span class="muted">未核销</span> <b>${fmt(s.open_amount)}</b>（${Number(s.open_count) || 0} 笔）</span>
            <span><span class="muted">期初挂账</span> <b>${fmt(s.opening_balance)}</b></span>
            <span><span class="muted">最早未核销</span> ${s.oldest_open_date ? esc(s.oldest_open_date) : "—"}</span>
            ${cr ? `<span><span class="muted">信用占用</span> ${fmt(cr.receivable)} / ${Number(cr.limit) > 0 ? fmt(cr.limit) : "<span class=\"muted\">未设额度</span>"}${cr.over ? ' <span class="tag err">超额度</span>' : ""}</span>` : ""}
          </div>
        </div>
        <div id="cd-lines" style="max-height:340px;overflow:auto">
        ${lines.length ? `<table class="grid"><thead><tr>
          <th>日期</th><th>单据</th><th>摘要</th><th>科目</th>
          <th class="num">金额</th><th class="num">已核销</th><th class="num">未核销</th>
        </tr></thead><tbody>${lines.map((l) => `<tr${l.from_opening ? ' style="color:var(--muted)"' : ""}>
          <td>${esc(l.date)}</td>
          <td>${esc(l.doc_no)}</td>
          <td>${esc(l.summary || "")}</td>
          <td>${esc(l.account_code)}</td>
          <td class="num">${fmt(l.amount)}</td>
          <td class="num">${fmt(l.settled)}</td>
          <td class="num">${Number(l.open) !== 0 ? `<b>${fmt(l.open)}</b>` : ""}</td>
        </tr>`).join("")}</tbody></table>` : `<div class="muted">没有往来记录</div>`}
        </div>`;
      // 三个子面板**同步**插占位再异步填数：否则用户在占位插入前点 Tab，
      // showSub 找不到元素，等数据回来时面板仍是 display:none —— 点了没反应。
      $("#cd-body", m).insertAdjacentHTML(
        "beforeend",
        '<div id="cd-aging" style="display:none"><div class="muted">加载中…</div></div>' +
          '<div id="cd-trade" style="display:none"><div class="muted">加载中…</div></div>' +
          '<div id="cd-dunning" style="display:none"><div class="muted">加载中…</div></div>',
      );
      renderAging();
      renderTrade();
      renderDunning();
    } catch (e) {
      $("#cd-body", m).innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`;
    }
  })();

  // 账龄面板。**独立取数、独立失败**：账龄接口挂了不该把明细一起带走
  //（明细是这一屏的主体，账龄是补充）。第一版把两个请求塞进同一个 try，
  // 于是账龄一失败整个弹窗变成一行红字，用户连自己欠多少都看不到。
  function renderAging() {
    // 不传 as_of：用后端默认的「期间期末」。前端若传今天，查历史期间时
    // 天数会按今天算 —— 「2025 年的账龄」在 2026 年看会全落进最老的一档，
    // 那不是账龄，那是错。
    api(`/customers/${encodeURIComponent(code)}/aging?period=${encodeURIComponent(period || "")}`)
      .then((r) => {
        const el = $("#cd-aging", m);
        if (!el) return; // 弹窗已关
        const a = r.aging || {};
        const bs = a.buckets || [];
        const as = a.amounts || [];
        if (a.empty) {
          el.innerHTML = `<div class="muted">无未核销余额（账龄基准日 ${esc(r.as_of || "")}）</div>`;
          return;
        }
        el.innerHTML = `<div class="muted" style="font-size:12px;margin-bottom:4px">
            账龄基准日 ${esc(r.as_of || "")}；分档与「往来核销 → 账龄分析」同一套。
          </div>
          <table class="grid"><thead><tr><th>区间</th><th class="num">金额</th></tr></thead><tbody>
          ${bs.map((b, i) => `<tr>
            <td>${esc(b)}</td>
            <td class="num">${Number(as[i]) === 0 ? '<span class="muted">—</span>' : `<b>${esc(as[i])}</b>`}</td>
          </tr>`).join("")}</tbody>
          <tfoot class="totals"><tr><td>合计</td><td class="num"><b>${esc(a.total)}</b></td></tr></tfoot></table>
          ${Number(a.credit_total) !== 0
            ? `<div class="muted" style="font-size:12px;margin-top:4px">另有贷方余额（预收） ${esc(a.credit_total)}，不计入上面各区间。</div>`
            : ""}
          ${Number(a.credit_total) === 0 && Number(a.max_days) > 0
            ? `<div class="muted" style="font-size:12px;margin-top:4px">最长账龄 ${a.max_days} 天</div>`
            : ""}`;
      })
      .catch((e) => {
        const el = $("#cd-aging", m);
        if (el) el.innerHTML = `<div style="color:var(--err)">账龄取数失败：${esc(e.message)}</div>`;
      });
  }

  // 交易历史面板：报价 / 订单 / 发货 / 收款。
  // 默认「本年」（from=年初、to=期间）—— 与账龄一样独立取数、独立失败，
  // 挂了不影响往来明细这一屏的主体。
  function renderTrade() {
    api(`/customers/${encodeURIComponent(code)}/trade?period=${encodeURIComponent(period || "")}`)
      .then((r) => {
        const el = $("#cd-trade", m);
        if (!el) return; // 弹窗已关
        const rows = r.rows || [];
        const yr = String(r.from || "").replace(/\D/g, "").slice(0, 4);
        el.innerHTML = rows.length
          ? `<div class="muted" style="font-size:12px;margin-bottom:4px">${esc(yr)} 年：报价 / 订单 / 发货 / 收款，按日期倒序（收款取收付款单，为会计口径）</div>
          <table class="grid"><thead><tr>
            <th>日期</th><th>类型</th><th>单据</th><th>摘要</th>
            <th class="num">金额</th><th class="num">数量</th><th>状态</th>
          </tr></thead><tbody>${rows.map((x) => `<tr>
            <td>${esc(x.date)}</td>
            <td>${esc(x.kind_label)}</td>
            <td>${esc(x.doc_no || "")}</td>
            <td>${esc(x.summary || "")}</td>
            <td class="num">${x.kind === "shipment" ? '<span class="muted">—</span>' : fmt(x.amount)}</td>
            <td class="num">${Number(String(x.qty || "").replace(/,/g, "")) ? esc(x.qty) : ""}</td>
            <td>${esc(x.status_label || "")}</td>
          </tr>`).join("")}</tbody></table>`
          : `<div class="muted">该期间没有交易记录</div>`;
      })
      .catch((e) => {
        const el = $("#cd-trade", m);
        if (el) el.innerHTML = `<div style="color:var(--err)">交易历史取数失败：${esc(e.message)}</div>`;
      });
  }

  // 催款记录面板：催款单 + 往来对账单。金额/状态都来自后端各自模块，前端不重算。
  function renderDunning() {
    api(`/customers/${encodeURIComponent(code)}/dunning`)
      .then((r) => {
        const el = $("#cd-dunning", m);
        if (!el) return;
        const rows = r.rows || [];
        el.innerHTML = rows.length
          ? `<table class="grid"><thead><tr>
               <th>日期</th><th>类型</th><th>单号</th><th>期间 / 备注</th>
               <th class="num">金额</th><th class="num">笔数</th><th>状态</th>
             </tr></thead><tbody>${rows.map((x) => `<tr>
               <td>${esc(x.date)}</td>
               <td>${esc(x.kind_label)}</td>
               <td>${esc(x.no || "")}</td>
               <td>${esc(x.summary || "")}</td>
               <td class="num">${fmt(x.amount)}</td>
               <td class="num">${Number(x.item_count) || ""}</td>
               <td>${esc(x.status_label || "")}</td>
             </tr>`).join("")}</tbody></table>`
          : `<div class="muted">暂无催款单或对账单</div>`;
      })
      .catch((e) => {
        const el = $("#cd-dunning", m);
        if (el) el.innerHTML = `<div style="color:var(--err)">催款记录取数失败：${esc(e.message)}</div>`;
      });
  }
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
  $("#ba-run-print").addEventListener("click", () => { const el = $("#ba-result").querySelector("table"); printPreview("预算预警", el); });
  $("#ba-run").addEventListener("click", async () => {
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
      // 两个接口一起取：precheck 给抬头数字，wizard 给打勾清单
      const [chk, wiz] = await Promise.all([
        api(`/periods/${encodeURIComponent(p)}/precheck`),
        api(`/periods/${encodeURIComponent(p)}/wizard`),
      ]);
      $("#pe-status", main).innerHTML = `期间 <b>${esc(chk.period)}</b>　已结账至：<b>${esc(chk.closed_upto || "—")}</b>　损益科目：<b>${chk.pl_count}</b> 个　本年利润余额：<b>${esc(chk.profit_balance)}</b>`;
      const mark = (st) => (st.ok ? "✔" : st.blocking ? "✖" : "⚠");
      const color = (st) => (st.ok ? "var(--ok,#2e7d32)" : st.blocking ? "var(--err,#c00)" : "var(--warn-t,#b46a00)");
      const rows = (wiz.steps || []).map((st, i) => `<tr>
          <td style="white-space:nowrap">${i + 1}. ${esc(st.label)}</td>
          <td style="color:${color(st)};white-space:nowrap">${mark(st)} ${st.ok ? "完成" : st.blocking ? "未完成" : "提醒"}</td>
          <td>${esc(st.detail)}${st.ok ? "" : `<br><span class="muted">${esc(st.hint)}</span>`}</td>
        </tr>`).join("");
      const tail = wiz.closed
        ? `<div class="muted">${esc(wiz.period)} 已结账。要修改请先反结账。</div>`
        : wiz.can_close
          ? `<div style="color:var(--ok,#2e7d32)">✔ 清单已全部通过，可以结账</div>`
          : `<div style="color:var(--err,#c00)">✖ 还有 ${(wiz.steps || []).filter((x) => x.blocking && !x.ok).length} 项必须做完才能结账</div>`;
      $("#pe-issues", main).innerHTML = `<div class="panel"><b>月结向导</b>
        <table class="grid" style="margin-top:6px"><thead><tr><th>步骤</th><th>状态</th><th>现状与该做什么</th></tr></thead><tbody>${rows}</tbody></table>
        <div style="margin-top:8px">${tail}</div></div>`;
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
      ${can("voucher_new") ? `<button class="btn ghost" id="bk-gen">未勾对的生成凭证</button>` : ""}
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
    const list = (arr, f) => (arr || []).length ? `<ul style="margin:4px 0 0 16px">${arr.map((x) => `<li>${esc(f(x))}</li>`).join("")}</ul>` : `<span class="muted">无</span>`;
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
  if ($("#bk-gen", main)) $("#bk-gen", main).onclick = async () => {
    const p = periodOf(), acct = accountOf();
    // 明确说清这条路子的边界，再让用户点：认不出的行会被留下，
    // 生成的凭证只含银行侧 + 往来侧、不含收入成本侧。
    const m = modal(`<h3>对未勾对的流水生成凭证</h3>
      <div class="muted" style="font-size:12px;line-height:1.7;margin-bottom:8px">
        期间 <b>${esc(p)}</b> · 科目 <b>${esc(acct)}</b><br>
        · 只处理<b>还没勾对</b>的流水；已勾对的不动。<br>
        · 摘要里能认出<b>客户名</b>的进账 → 借 银行 / 贷 应收；认出<b>供应商名</b>的支出 → 借 应付 / 贷 银行。<br>
        · 认不出的<b>原样留在对账页</b>，不会给你挂「待定」科目 —— 那种挂账要到期末才暴露。<br>
        · 生成的凭证是<b>草稿</b>：还要审核、还要出纳签字才能记账。<b>不会自动记账。</b><br>
        · 凭证只含银行侧与往来侧，<b>不含收入与成本侧</b>。适用于「往来已记好、只差银行侧没记」；
        若销售/采购尚未入账，应收/应付会出现负数。
      </div>
      <div class="foot"><button class="btn primary" id="bg-ok">生成</button><button class="btn ghost" id="bg-cancel">取消</button></div>`);
    $("#bg-cancel", m).onclick = closeModal;
    $("#bg-ok", m).onclick = async () => {
      closeModal();
      try {
        const r = await post("/bank/gen-vouchers", { ymm: parseInt(p, 10), account: acct, tolerance: 0 });
        if (!r.generated && !r.skipped.length) { toast("没有可处理的流水（可能都已勾对）", "err"); return; }
        // 跳过的行逐条说原因 —— 用户要知道哪些没处理、为什么
        const m2 = modal(`<h3>生成结果</h3>
          <div style="font-size:13px;line-height:1.7">
            已生成 <b>${r.generated}</b> 张凭证草稿，并已把 ${r.linked} 条流水勾上。<br>
            ${r.skipped.length ? `<b>跳过 ${r.skipped.length} 条</b>（原样留在对账页）：<ul style="margin:6px 0 0 18px;padding:0">${
              r.skipped.map((s) => `<li>${esc(s.date)} ${esc(s.summary)} ${esc(s.amount)}<br><span class="muted">↳ ${esc(s.reason)}</span></li>`).join("")
            }</ul>` : ""}
            <p class="muted" style="margin-top:10px;font-size:12px">${esc(r.note || "")}</p>
          </div>
          <div class="foot"><button class="btn primary" id="bg2-ok">知道了</button></div>`);
        $("#bg2-ok", m2).onclick = closeModal;
        load();
      } catch (e) { toast(e.message, "err"); }
    };
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
      <label title="互抵时若两侧科目不同（如客户应收抵供应商应付），把对方科目填这里，两张未核销清单会合并显示，才能挑到跨科目的两行">对方科目 <input id="st-acct2" placeholder="如 2202" style="width:90px" /></label>
      <label>截止期间 <input id="st-upto" value="${esc(cur)}" style="width:90px" /></label>
      <label title="核销/账龄/催款只认已记账分录（全仓 H-3 口径）。勾上可把待记账的往来一并列出查看，但它们不可核销——对标金蝶：未审核单据在列表里可见，但不参与核销。"><input type="checkbox" id="st-draft" /> 含未记账</label>
      <button class="btn" id="st-load">查询未核销</button>
      <div class="spacer"></div>
      <label>尾差 <input id="st-tol" value="0.01" style="width:60px" /></label>
      ${can("voucher_new") ? `<button class="btn ghost" id="st-auto">自动核销</button>` : ""}
      ${can("voucher_new") ? `<button class="btn ghost" id="st-manual">手工核销</button>` : ""}
      ${can("voucher_new") ? `<button class="btn ghost" id="st-manual-diff" title="实收/实付与挂账有零头时，把差异记入损益，被核销方一次结清">差异核销</button>` : ""}
      ${can("voucher_new") ? `<button class="btn ghost" id="st-manual-cross" title="A 的付款冲 B 的货款 / 三方抵账：借方与贷方分录的科目或往来单位可以不同，系统自动出一张过渡凭证把两边接上">跨单位互抵</button>` : ""}
      <button class="btn ghost" id="st-aging">账龄分析</button>
      ${can("export") ? `<button class="btn ghost" id="st-aging-xlsx" title="把账龄表导出成 Excel（同一份取数，界面上的数字即文件里的数字）">账龄导出</button>` : ""}
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
        <button class="btn ghost sm" id="dn-export">导出 CSV</button>
      </div>
      <div id="dn-list" class="muted" style="margin-top:8px">加载中…</div>
    </div>
    <div class="panel" style="margin-top:12px">
      <div style="display:flex;align-items:center;gap:8px;flex-wrap:wrap"><h4 style="margin:0">往来对账单（对客户/供应商）</h4><span class="grow"></span>
        <span class="muted" style="font-size:12px" id="stm-total"></span>
        <button class="btn ghost sm" id="stm-reload">刷新</button>
        ${can("voucher_new") ? `<button class="btn primary sm" id="stm-new">新建对账单</button>` : ""}
      </div>
      <div class="muted" style="font-size:12px;margin-top:4px">按期间出账单：期初 + 本期发生 = 期末，逐行带滚动余额；只取已记账分录。发出后数字锁定，客户回签确认即闭环。</div>
      <div id="stm-list" class="muted" style="margin-top:8px">加载中…</div>
    </div>
    <div class="panel" style="margin-top:12px">
      <div style="display:flex;align-items:center;gap:8px;flex-wrap:wrap"><h4 style="margin:0">往来对冲单（应收 ↔ 应付相互抵销）</h4><span class="grow"></span>
        <button class="btn ghost sm" id="off-reload">刷新</button>
      </div>
      <div class="muted" style="font-size:12px;margin-top:4px">某公司既欠我们（应收）、我们又欠它（应付）时，月末轧差只结净额；也可以是<b>两家不同公司</b>——客户 A 欠我们、我们欠供应商 B，三方约定 A 直接把钱顶给 B（三方抵账），客户填 A、供应商填 B 即可。录单 → 审核出「借 应付 / 贷 应收」凭证，并对两端各自按先进先出核销，两边往来同时变清。对冲额不得超过任一端未核销合计。</div>
      ${can("voucher_new") ? `<div class="toolbar" style="margin-top:6px">
        <label>日期 <input id="off-date" style="width:110px" /></label>
        <label>客户(应收) <input id="off-cust" style="width:100px" placeholder="C01" /></label>
        <label>供应商(应付) <input id="off-sup" style="width:100px" placeholder="S01" /></label>
        <label>对冲金额 <input id="off-amt" style="width:90px" /></label>
        <label>备注 <input id="off-memo" style="width:140px" /></label>
        <button class="btn primary sm" id="off-new">新增对冲单</button>
      </div>` : ""}
      <div id="off-list" class="muted" style="margin-top:8px">加载中…</div>
    </div>
    <div class="panel" style="margin-top:12px">
      <div style="display:flex;align-items:center;gap:8px;flex-wrap:wrap"><h4 style="margin:0">收款认领（一笔钱认给多个客户）</h4><span class="grow"></span>
        <button class="btn ghost sm" id="cl-reload">刷新</button>
      </div>
      <div class="muted" style="font-size:12px;margin-top:4px">银行流水里的一笔钱常常不是一个人的：A 打来 45,000，其中 15,000 是替 B 付的。挑一笔收款、勾几笔挂账、填金额，系统就按你的判断落核销；跨客户的那几条自动出「借 原客户 / 贷 真客户」的过渡凭证（直接记账，认领完账上就干净了）。</div>
      <div class="toolbar" style="margin-top:6px">
        <label>往来科目 <input id="cl-acct" value="112201" style="width:90px" /></label>
        <label>期间 <input id="cl-period" value="${esc(cur)}" style="width:90px" /></label>
        <label>收款 <select id="cl-inflow" style="min-width:340px"></select></label>
        <button class="btn ghost sm" id="cl-suggest" title="先清同一个客户的（钱属于他的最常见），再按日期清别人的（代收代付）——只是建议，判断权在你">按建议勾选</button>
        ${can("voucher_new") ? `<button class="btn primary sm" id="cl-apply">认领</button>` : ""}
      </div>
      <div id="cl-list" class="muted" style="margin-top:8px">加载中…</div>
    </div>`;
  let selFrom = null, selTo = null, rows = [];
  // 未核销分录可能上千条（大账套一次能拉出几百 KB），默认只取前 200 条，
  // 由服务端给出总数，界面上给「加载更多」。截断了却不告诉用户，
  // 就会出现「明明还欠着，核销页里找不到那张单」——这才是最坏的错。
  let pageLimit = 200, pageTotal = 0, pageCut = false;
  const post = (path, body) => api(path, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body || {}) });
  const acct = () => $("#st-acct", main).value.trim();
  const acct2 = () => ($("#st-acct2", main) ? $("#st-acct2", main).value.trim() : "");
  const upto = () => $("#st-upto", main).value.trim();

  // ---- 收款认领：一笔收款 → 认给多笔挂账（同客户普通核销，跨客户自动过渡） ----
  let clInflows = [], clCands = [], clSugg = [];
  const clAcct = () => ($("#cl-acct", main) ? $("#cl-acct", main).value.trim() : "112201");
  const clPeriod = () => ($("#cl-period", main) ? $("#cl-period", main).value.trim() : "").replace(/[^0-9]/g, "");
  function clRender() {
    const sel = $("#cl-inflow", main);
    const curId = sel && sel.value ? parseInt(sel.value, 10) : (clInflows[0] ? clInflows[0].entry : 0);
    const inflow = clInflows.find((i) => i.entry === curId);
    if (!inflow) {
      $("#cl-list", main).innerHTML = `<div class="muted" style="padding:12px">这个期间没有「收到钱还没认完」的分录。钱都要么认完了，要么还没记收款凭证。</div>`;
      return;
    }
    const sugg = new Map(((clSugg.find((x) => x.inflow === curId) || {}).picks || []).map((p) => [p.entry, p.amount]));
    const rowsHtml = clCands.map((c) => `<tr${c.party === inflow.party ? ' style="background:var(--hover)"' : ""}>
      <td><input type="checkbox" data-cl-pick="${c.entry}" /></td>
      <td>${esc(c.date)}</td><td><b>${esc(c.party || "—")}</b>${c.party === inflow.party ? ' <span class="muted">（同一客户）</span>' : ""}</td>
      <td>${esc(c.summary)}</td><td class="num">${esc(c.open)}</td>
      <td><input data-cl-amt="${c.entry}" style="width:100px" placeholder="${esc(sugg.get(c.entry) || "")}" /></td></tr>`).join("");
    $("#cl-list", main).innerHTML = `
      <div style="margin-bottom:6px">这笔收款记在 <b>${esc(inflow.party || "—")}</b> 名下，${esc(inflow.date)}，总额 ${esc(inflow.amount)}，<b>还没认 ${esc(inflow.open)}</b>（凭证摘要：${esc(inflow.summary)}）</div>
      <table class="grid"><thead><tr><th></th><th>日期</th><th>客户</th><th>摘要</th><th class="num">未清</th><th>认领金额</th></tr></thead>
      <tbody>${rowsHtml || `<tr><td colspan="6" class="muted" style="text-align:center;padding:16px">没有挂着未收的应收单</td></tr>`}</tbody></table>
      <div class="muted" style="font-size:12px;margin-top:6px">认领金额合计不能超过这笔收款未认的 ${esc(inflow.open)}。跨客户的那几条会自动生成过渡凭证，凭证上写得明明白白：借 原客户 / 贷 真客户。</div>`;
    $all("#cl-list [data-cl-pick]", main).forEach((b) => {
      b.onchange = () => {
        const amt = $(`#cl-list [data-cl-amt="${b.dataset.clPick}"]`, main);
        if (b.checked && amt && !amt.value) {
          const c = clCands.find((x) => String(x.entry) === b.dataset.clPick);
          amt.value = (sugg.get(parseInt(b.dataset.clPick, 10)) || (c ? c.open : "")).toString();
        }
      };
    });
  }
  async function clLoad() {
    const a = clAcct(), p = clPeriod();
    try {
      const d = await api(`/claim?account=${encodeURIComponent(a)}&period=${encodeURIComponent(p)}`);
      clInflows = d.inflows || [];
      clCands = d.candidates || [];
      clSugg = d.suggestions || [];
      const sel = $("#cl-inflow", main);
      sel.innerHTML = clInflows.length
        ? clInflows.map((i) => `<option value="${i.entry}">${esc(i.date)}　${esc(i.party || "—")}　未认 ${esc(i.open)}　${esc(i.summary)}</option>`).join("")
        : `<option value="">（没有待认领的收款）</option>`;
      sel.onchange = clRender;
      clRender();
    } catch (e) {
      $("#cl-list", main).innerHTML = `<div class="err" style="padding:12px">${esc(e.message || String(e))}</div>`;
    }
  }

  function render() {
    // 未记账行只读：可查看、不可选为核销方（服务端 settle 也会拦，双保险）
    const posted = rows.filter((r) => r.posted !== false);
    const draftRows = rows.filter((r) => r.posted === false);
    const sum = (list) => list.reduce((a, r) => a + (parseFloat(String(r.open).replace(/,/g, "")) || 0), 0);
    const shown = posted.length;
    let s = pageCut
      ? `未核销 <b>${pageTotal}</b> 笔，已显示前 <b>${shown}</b> 笔（合计 <b>${sum(posted).toFixed(2)}</b> 是已显示部分）`
      : `未核销 <b>${posted.length}</b> 笔，合计 <b>${sum(posted).toFixed(2)}</b>`;
    if (draftRows.length) s += `　<span class="muted">（另有 ${draftRows.length} 笔未记账，仅供查看、不可核销，合计 ${sum(draftRows).toFixed(2)}）</span>`;
    $("#st-sum", main).innerHTML = s;
    const rowHtml = (r) => {
      const draft = r.posted === false;
      return `<tr${draft ? ' style="opacity:.65"' : ""}><td>${esc(r.date)}</td><td>${esc(r.voucher_no)}${draft ? ' <span class="muted">未记账</span>' : ""}</td><td>${esc(r.aux_key || "—")}</td><td>${esc(r.summary)}</td><td>${esc(r.dir)}</td><td class="num">${esc(r.debit === "0.00" ? r.credit : r.debit)}</td><td class="num">${esc(r.settled)}</td><td class="num">${esc(r.open)}</td><td>
      ${draft ? `<span class="muted" style="font-size:12px">待记账</span>` : `<button class="btn sm ${selFrom === r.entry_id ? "primary" : "ghost"}" data-from="${r.entry_id}">原单</button>
      <button class="btn sm ${selTo === r.entry_id ? "primary" : "ghost"}" data-to="${r.entry_id}">收/付</button>`}</td></tr>`;
    };
    $("#st-table", main).innerHTML = `<table class="grid"><thead><tr><th>日期</th><th>凭证号</th><th>往来对象</th><th>摘要</th><th>方向</th><th class="num">发生额</th><th class="num">已核销</th><th class="num">未核销</th><th>选择</th></tr></thead><tbody>${rows.length ? rows.map(rowHtml).join("") : `<tr><td colspan="9" class="muted" style="text-align:center;padding:16px">没有未核销分录</td></tr>`}</tbody></table>
      ${pageCut ? `<div style="margin-top:8px"><button class="btn sm ghost" id="st-more">再显示 200 笔（共 ${pageTotal} 笔）</button> <span class="muted" style="font-size:12px">想直接找到某张单，可在上面「往来对象」里填客户/供应商编码，或先把对方科目填上</span></div>` : ""}`;
    const more = $("#st-more", main);
    if (more) more.onclick = () => { pageLimit += 200; load(); };
    // 页面里还有催款单/对账函等按钮也用 data-to（值是 sent/settled 等状态），
    // 不能在全页范围按 data-to 绑定，否则会把它们的点击劫持成核销选择。
    $all("#st-table [data-from]", main).forEach((b) => b.onclick = () => { selFrom = parseInt(b.dataset.from, 10); render(); });
    $all("#st-table [data-to]", main).forEach((b) => b.onclick = () => { selTo = parseInt(b.dataset.to, 10); render(); });
  }

  async function load() {
    const a = acct(), a2 = acct2(), u = upto();
    const withDraft = $("#st-draft", main) && $("#st-draft", main).checked ? "&draft=1" : "";
    try {
      pageTotal = 0; pageCut = false;
      const lim = `&limit=${pageLimit}`;
      const d = await api(`/settle/open?account=${encodeURIComponent(a)}&upto=${encodeURIComponent(u)}${withDraft}${lim}`);
      rows = d.rows || [];
      pageTotal = d.total || rows.length;
      pageCut = !!d.truncated;
      // 互抵对方科目：把两张未核销清单合并，才能同时挑到跨科目（应收 ↔ 应付）的两行
      if (a2 && a2 !== a) {
        const d2 = await api(`/settle/open?account=${encodeURIComponent(a2)}&upto=${encodeURIComponent(u)}${withDraft}${lim}`);
        const seen = new Set(rows.map((r) => r.entry_id));
        (d2.rows || []).forEach((r) => { if (!seen.has(r.entry_id)) rows.push(r); });
        // 两个科目各算各的总数，互抵页要的是「两边加起来还差多少笔」
        pageTotal += d2.total || 0;
        pageCut = pageCut || !!d2.truncated;
      }
      selFrom = selTo = null;
      $("#st-extra", main).innerHTML = "";
      render();
    } catch (e) { $("#st-sum", main).textContent = e.message; }
  }

  // 往来期初明细（迁移数据）：列表 + 合计 + 修改 / 删除；「账龄分析」自动包含期初行（影子挂账）
  // v1.8.83：导错不必删掉重录，点「改」直接改。校验口径与 CSV 导入一致，全在服务端。
  const openArapEditor = (o) => {
    const m = modal(`<h3>修改往来期初明细</h3>
      <div style="display:grid;grid-template-columns:1fr 1fr;gap:8px">
        <label>类型 <select id="ao-kind"><option value="ar" ${o.kind === "ar" ? "selected" : ""}>应收</option><option value="ap" ${o.kind === "ap" ? "selected" : ""}>应付</option></select></label>
        <label>客商编码 <input id="ao-party" value="${esc(o.party_code)}" /></label>
        <label>客商名称 <input id="ao-name" value="${esc(o.party_name || "")}" /></label>
        <label>单据号 <input id="ao-doc" value="${esc(o.doc_no)}" /></label>
        <label>单据日期 <input id="ao-date" value="${esc(o.doc_date)}" placeholder="YYYY-MM-DD" /></label>
        <label>金额 <input id="ao-amount" value="${esc(o.amount)}" /></label>
        <label>备注 <input id="ao-memo" value="${esc(o.memo || "")}" /></label>
      </div>
      <div class="foot"><button class="btn primary" id="ao-save">保存</button><button class="btn ghost" id="ao-cancel">取消</button></div>`);
    $("#ao-cancel", m).onclick = closeModal;
    $("#ao-save", m).onclick = async () => {
      const body = {
        kind: $("#ao-kind", m).value,
        party_code: $("#ao-party", m).value.trim(),
        party_name: $("#ao-name", m).value.trim(),
        doc_no: $("#ao-doc", m).value.trim(),
        doc_date: $("#ao-date", m).value.trim(),
        amount: $("#ao-amount", m).value.trim(),
        memo: $("#ao-memo", m).value.trim(),
      };
      try {
        await api(`/arap-opening/${o.id}`, { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
        toast("已保存", "ok"); closeModal(); loadArap();
      } catch (e) { toast(e.message, "err"); }
    };
  };
  const loadArap = async () => {
    try {
      const r = await api("/arap-opening");
      const arows = r.rows || [];
      $("#arap-total").textContent = arows.length ? `应收合计 ${r.total_ar} · 应付合计 ${r.total_ap}` : "";
      $("#arap-list").innerHTML = arows.length
        ? `<table class="grid"><thead><tr><th>类型</th><th>客商</th><th>名称</th><th>单据号</th><th>单据日期</th><th class="num">金额</th><th>备注</th><th>录入</th><th></th></tr></thead><tbody>${arows.map((o) => `<tr><td>${o.kind === "ar" ? '<span class="tag">应收</span>' : '<span class="tag warn">应付</span>'}</td><td>${esc(o.party_code)}</td><td>${esc(o.party_name || "—")}</td><td>${esc(o.doc_no)}</td><td>${esc(o.doc_date)}</td><td class="num">${fmt(o.amount)}</td><td>${esc(o.memo || "")}</td><td>${esc(o.created_by)}</td>
            <td class="row-actions">${can("voucher_new") ? `<button class="btn ghost sm" data-arap-edit="${o.id}">改</button><button class="btn ghost sm" data-arap-del="${o.id}">删</button>` : ""}</td></tr>`).join("")}</tbody></table>
          <p class="muted" style="font-size:12px;margin-top:6px">期初明细为影子挂账：进入「账龄分析」（单据号标「期初」），暂不参与自动核销（核销 v2）；金额总额仍以科目期初为准。</p>`
        : `<div class="muted">暂无往来期初（点「去导入」按单据批量迁移）</div>`;
      $all("[data-arap-edit]", main).forEach((b) => b.onclick = () => openArapEditor(arows.find((o) => String(o.id) === b.dataset.arapEdit)));
      $all("[data-arap-del]", main).forEach((b) => b.onclick = async () => {
        if (!(await confirmDialog("删除该期初明细行？", true))) return;
        try { await api(`/arap-opening/${b.dataset.arapDel}`, { method: "DELETE" }); toast("已删除", "ok"); loadArap(); } catch (e) { toast(e.message, "err"); }
      });
    } catch (e) { $("#arap-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  loadArap();
  if ($("#arap-reload", main)) $("#arap-reload", main).onclick = loadArap;
  if ($("#arap-goimport", main)) $("#arap-goimport", main).onclick = () => goView("imports");

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
      <table class="grid" id="dn-table"><thead><tr><th>单据</th><th>日期</th><th>摘要</th><th>方向</th><th class="num">金额</th></tr></thead><tbody>${(d.detail || []).map((x) => `<tr><td>${esc(x.doc_no)}</td><td>${esc(x.date)}</td><td>${esc(x.summary)}</td><td>${esc(x.direction || (d.kind === "ar" ? "应收" : "应付"))}</td><td class="num">${fmt(x.amount)}</td></tr>`).join("")}
      <tr><td colspan="4"><b>${d.kind === "ar" ? "应收" : "应付"}合计</b></td><td class="num"><b>${fmt(d.amount)}</b></td></tr>${(d.detail || []).some((x) => x.direction && x.direction !== "应收" && x.direction !== "应付") ? `<tr><td colspan="4" class="muted">其中预收/预付（我方欠对方，不计入上面的合计）</td><td class="num muted">${fmt((d.detail || []).filter((x) => x.direction === "预收" || x.direction === "预付").reduce((s2, x) => s2 + (x.amount || 0), 0))}</td></tr>` : ""}</tbody></table>
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
  if ($("#dn-export", main)) $("#dn-export", main).onclick = async () => {
    try {
      const kind = prompt("类型：ar=催款单 / ap=对账函", "ar");
      if (!kind || !["ar", "ap"].includes(kind.trim())) { toast("类型必须是 ar 或 ap", "err"); return; }
      const period = $("#st-upto", main).value.trim() || state.current;
      // 导 Excel 而不是 CSV：CSV 只有一行汇总，客户问「哪几张单凑的 5 万」答不上来。
      // xlsx 有两个 Sheet：汇总 + 逐单明细（明细带方向，预收/预付不计入合计）。
      downloadUrl(`/api/export/dunning/xlsx?period=${encodeURIComponent(period)}&kind=${encodeURIComponent(kind.trim())}`);
      toast("导出完成", "ok");
    } catch (e) { toast(e.message, "err"); }
  };

  // ---- 往来对账单（对标金蝶「客户对账单」：期初 + 本期发生 = 期末 + 客户回签）----
  const ymmOf = (s) => parseInt(String(s || "").replace(/-/g, ""), 10) || 0;
  const loadStatements = async () => {
    try {
      const r = await api("/settle/statements");
      const srows = r.rows || [];
      const stMap = { draft: ["草稿", ""], sent: ["已发出", "warn"], confirmed: ["已回签", "ok"], cancelled: ["已作废", ""] };
      const pending = srows.filter((s) => s.status === "sent").length;
      $("#stm-total").textContent = srows.length ? `共 ${srows.length} 张${pending ? `，${pending} 张待回签` : ""}` : "";
      $("#stm-list").innerHTML = srows.length
        ? `<table class="grid"><thead><tr><th>单号</th><th>类型</th><th>客商</th><th>期间</th><th class="num">期初</th><th class="num">本期增</th><th class="num">本期减</th><th class="num">期末</th><th>状态</th><th>明细</th><th></th></tr></thead><tbody>${srows.map((s) => {
            const st = stMap[s.status] || [s.status, ""];
            const acts = can("voucher_new") ? [
              s.status === "draft" ? `<button class="btn ghost sm" data-stm-st="${s.id}" data-to="sent">发出</button>` : "",
              s.status === "sent" ? `<button class="btn ghost sm" data-stm-st="${s.id}" data-to="confirmed">客户回签</button>` : "",
              (s.status === "draft" || s.status === "sent") ? `<button class="btn ghost sm" data-stm-st="${s.id}" data-to="cancelled">作废</button>` : "",
            ].filter(Boolean).join(" ") : "";
            return `<tr><td>${esc(s.no)}</td><td>${s.kind === "ar" ? '<span class="tag">应收</span>' : '<span class="tag warn">应付</span>'}</td>
              <td>${esc(s.party_code)} ${esc(s.party_name || "")}</td>
              <td class="muted">${esc(s.from)} ~ ${esc(s.to)}</td>
              <td class="num">${fmt(s.begin_balance)}</td><td class="num">${fmt(s.period_increase)}</td><td class="num">${fmt(s.period_decrease)}</td>
              <td class="num"><b>${fmt(s.end_balance)}</b></td>
              <td><span class="tag ${st[1]}">${st[0]}</span></td>
              <td><button class="btn ghost sm" data-stm-detail="${s.id}">查看</button></td><td class="row-actions">${acts}</td></tr>`;
          }).join("")}</tbody></table>`
        : `<div class="muted">暂无对账单（点「新建对账单」按客商 + 期间生成）</div>`;
      window._stmCache = srows;
      $all("[data-stm-st]", main).forEach((b) => b.onclick = async () => {
        const to = b.dataset.to;
        let body = { status: to };
        if (to === "confirmed") {
          const who = prompt("回签确认人（客户签字人或代签人）", "");
          if (who === null) return;
          if (!who.trim()) { toast("回签确认必须填写确认人", "err"); return; }
          body.confirmed_by = who.trim();
        }
        try { await post(`/settle/statements/${b.dataset.stmSt}/status`, body); toast("状态已更新", "ok"); loadStatements(); }
        catch (e) { toast(e.message, "err"); }
      });
      $all("[data-stm-detail]", main).forEach((b) => b.onclick = () => {
        const s = (window._stmCache || []).find((x) => x.id === parseInt(b.dataset.stmDetail, 10));
        if (s) openStatementDetail(s);
      });
    } catch (e) { $("#stm-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  function openStatementDetail(s) {
    const m = modal(`<h3>${esc(s.no)} · ${esc(s.kind_label)} <span class="muted" style="font-size:12px">${esc(s.party_code)} ${esc(s.party_name || "")} · ${esc(s.from)} ~ ${esc(s.to)}</span></h3>
      <table class="grid" id="stm-table"><thead><tr><th>日期</th><th>凭证号</th><th>摘要</th><th class="num">本期增</th><th class="num">本期减</th><th class="num">余额</th></tr></thead><tbody>
      ${(s.lines || []).map((l) => `<tr><td>${esc(l.date)}</td><td>${esc(l.doc_no)}</td><td>${esc(l.summary)}</td><td class="num">${fmt(l.increase)}</td><td class="num">${fmt(l.decrease)}</td><td class="num">${fmt(l.balance)}</td></tr>`).join("")}
      <tr><td colspan="3"><b>本期合计</b></td><td class="num"><b>${fmt(s.period_increase)}</b></td><td class="num"><b>${fmt(s.period_decrease)}</b></td><td class="num"><b>${fmt(s.end_balance)}</b></td></tr>
      </tbody></table>
      <div class="muted" style="font-size:12px;margin-top:6px">期初 ${fmt(s.begin_balance)}　制单 ${esc(s.created_by || "")}　${s.confirmed_by ? `回签确认 ${esc(s.confirmed_by)}` : ""}${s.memo ? `　${esc(s.memo)}` : ""}</div>
      <div class="foot"><button class="btn ghost" id="stm-print">打印预览</button><button class="btn ghost" id="stm-close">关闭</button></div>`);
    $("#stm-close", m).onclick = closeModal;
    $("#stm-print", m).onclick = () => printPreview(`往来对账单 ${s.no}`, $("#stm-table", m));
  }
  if ($("#stm-new", main)) $("#stm-new").onclick = async () => {
    const kind = prompt("类型：ar=应收对账单 / ap=应付对账单", "ar");
    if (kind === null) return;
    if (!["ar", "ap"].includes(kind.trim())) { toast("类型只能是 ar 或 ap", "err"); return; }
    const party = prompt("客商编码（如 C01）", "");
    if (party === null) return;
    if (!party.trim()) { toast("客商编码不能为空", "err"); return; }
    const pname = prompt("客商名称（可留空）", "");
    if (pname === null) return;
    const from = prompt("起始期间（YYYY-MM）", upto().slice(0, 7));
    if (from === null) return;
    const to = prompt("截止期间（YYYY-MM）", upto().slice(0, 7));
    if (to === null) return;
    if (!ymmOf(from) || !ymmOf(to)) { toast("期间格式应为 YYYY-MM", "err"); return; }
    try {
      const r = await post("/settle/statements", {
        kind: kind.trim(), account: acct(), party_code: party.trim(), party_name: (pname || "").trim(),
        from: ymmOf(from), to: ymmOf(to), memo: "",
      });
      toast(`已生成 ${r.no}，期末余额 ${fmt(r.end_balance)}`, "ok");
      loadStatements();
    } catch (e) { toast(e.message, "err"); }
  };
  loadStatements();
  if ($("#stm-reload", main)) $("#stm-reload", main).onclick = loadStatements;

  $("#st-load", main).onclick = load;
  if ($("#st-draft", main)) $("#st-draft", main).onchange = load;
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
  if ($("#st-manual-diff", main)) $("#st-manual-diff", main).onclick = async () => {
    if (!selFrom || !selTo) { toast("请分别选择「原单」与「收/付」两行", "err"); return; }
    const amt = prompt("本次核销金额（实收/实付）", "");
    if (amt === null) return;
    const diff = prompt("差异（抹零）金额；无差异直接确定", "");
    if (diff === null) return;
    let diffAcct = "";
    if ((diff || "").trim() !== "") {
      diffAcct = prompt("差异科目（默认 660305 财务费用-其他；也可填 6711 营业外支出）", "660305");
      if (diffAcct === null) return;
    }
    try {
      const r = await post("/settle/run", { from_entry: selFrom, to_entry: selTo, amount: amt, diff: (diff || "").trim(), diff_account: (diffAcct || "").trim() });
      toast(r.diff_voucher_id ? `已核销，差异凭证 #${r.diff_voucher_id}` : "已核销", "ok");
      load();
    } catch (e) { toast(e.message, "err"); }
  };
  if ($("#st-manual-cross", main)) $("#st-manual-cross").onclick = async () => {
    if (!selFrom || !selTo) { toast("请分别选择「原单」(借方) 与「收/付」(贷方) 两行", "err"); return; }
    const amt = prompt("互抵金额", "");
    if (amt === null) return;
    const dt = prompt("过渡凭证日期（YYYY-MM-DD）", new Date().toISOString().slice(0, 10));
    if (dt === null) return;
    try {
      const r = await post("/settle/cross", { debit_entry: selFrom, credit_entry: selTo, amount: amt, date: (dt || "").trim() });
      toast(r.bridge_voucher_id ? `已互抵，过渡凭证 #${r.bridge_voucher_id}（到凭证列表记账后即入账）` : "已互抵", "ok");
      load();
    } catch (e) { toast(e.message, "err"); }
  };
  if ($("#st-aging-xlsx", main)) $("#st-aging-xlsx", main).onclick = () => {
    downloadUrl(`/api/settle/aging/xlsx?account=${encodeURIComponent(acct())}&upto=${encodeURIComponent(upto())}`);
  };
  if ($("#st-aging", main)) $("#st-aging", main).onclick = async () => {
    try {
      const d = await api(`/settle/aging?account=${encodeURIComponent(acct())}&upto=${encodeURIComponent(upto())}`);
      $("#st-extra", main).innerHTML = `<div class="panel"><b>账龄分析（${esc(d.as_of)}）</b><table class="grid" style="margin-top:6px"><thead><tr><th>往来对象</th>${d.buckets.map((b) => `<th class="num">${esc(b)}</th>`).join("")}<th class="num">借方合计</th><th class="num">贷方合计</th><th class="num">净额</th><th class="num">最老天数</th><th class="num">逾期</th></tr></thead><tbody>${(d.rows || []).length ? d.rows.map((r) => `<tr><td><a href="#" class="drill" data-drill="${esc(r.key || "")}" title="看这个对象还没核销的单据">${esc(r.name || r.key || "—")}</a></td>${r.amounts.map((x) => `<td class="num">${esc(x)}</td>`).join("")}<td class="num">${esc(r.total)}</td><td class="num">${esc(r.credit_total)}</td><td class="num">${esc(r.net)}</td><td class="num">${r.max_days}</td><td class="num" style="${Number(r.overdue_days) > 0 ? "color:var(--warn-t,#b46a00)" : ""}">${r.pay_days == null ? '<span class="muted">未设账期</span>' : (Number(r.overdue_days) > 0 ? `逾期 ${r.overdue_days} 天` : "未到期")}</td></tr>`).join("") : `<tr><td colspan="${d.buckets.length + 6}" class="muted">无数据</td></tr>`}</tbody></table>
      <div id="st-drill" style="margin-top:10px"></div></div>`;
      // 账龄下钻：会计每天问的第一句是「华达这 5 万是哪几张单」。
      // 以前要回明细账手翻，现在点名字就看到——取数走 `/settle/open` 的
      // party 过滤，未核销口径与核销页完全一致，不会出现「账龄有、核销页没有」。
      $all("[data-drill]", main).forEach((a) => a.onclick = async (ev) => {
        ev.preventDefault();
        const key = a.dataset.drill || "";
        const box = $("#st-drill", main);
        box.innerHTML = `<div class="muted">加载中…</div>`;
        try {
          const u = encodeURIComponent(upto());
          const d2 = await api(`/settle/open?account=${encodeURIComponent(acct())}&upto=${u}&limit=0${key ? `&party=${encodeURIComponent(key)}` : ""}`);
          const rs = d2.rows || [];
          box.innerHTML = `<b>${esc(a.textContent)}　未核销单据 ${esc(d2.total)} 笔，合计 ${esc(d2.sum_debit)}</b>` +
            `<table class="grid" style="margin-top:6px"><thead><tr><th>日期</th><th>凭证</th><th>摘要</th><th class="num">借方</th><th class="num">贷方</th><th class="num">已核销</th><th class="num">账龄(天)</th></tr></thead><tbody>` +
            (rs.length ? rs.map((r) => `<tr><td>${esc(r.date)}</td><td>${esc(r.voucher_no || (r.word + "-" + r.no))}</td><td>${esc(r.summary)}</td><td class="num">${esc(r.debit)}</td><td class="num">${esc(r.credit)}</td><td class="num">${esc(r.settled)}</td><td class="num">${r.days != null ? r.days : ""}</td></tr>`).join("")
              : `<tr><td colspan="7" class="muted">这笔没有未核销单据（可能已核销完，或期初挂账）</td></tr>`) +
            `</tbody></table>`;
        } catch (e) { box.innerHTML = `<div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; }
      });
    } catch (e) { toast(e.message, "err"); }
  };
  if ($("#st-records", main)) $("#st-records", main).onclick = async () => {
    try {
      const d = await api(`/settle/records?account=${encodeURIComponent(acct())}`);
      const rows2 = d.rows || [];
      $("#st-extra", main).innerHTML = `<div class="panel"><b>核销记录</b><table class="grid" style="margin-top:6px"><thead><tr><th>期间</th><th>往来对象</th><th class="num">金额</th><th>操作人</th><th>时间</th><th></th></tr></thead><tbody>${rows2.length ? rows2.map((r) => `<tr><td>${esc(r.period)}</td><td>${esc(r.aux_key || "—")}</td><td class="num">${esc(r.amount)}</td><td>${esc(r.settled_by)}</td><td>${esc(r.settled_at)}</td><td>${can("voucher_new") ? `<button class="btn sm ghost" data-unsettle="${r.id}" data-amount="${esc(r.amount)}">取消核销</button>` : ""}</td></tr>`).join("") : `<tr><td colspan="6" class="muted">暂无记录</td></tr>`}</tbody></table></div>`;
      $all("[data-unsettle]", main).forEach((b) => b.onclick = () => {
        // 取消核销可以只撤一部分：核销三张单共 5 万、事后发现其中一张该是 3 万时，
        // 整条撤销的瞬间三张单全变「未核销」，账龄立刻虚高。
        const id = parseInt(b.dataset.unsettle, 10);
        const amt = b.dataset.amount || "";
        modal(`<h3>取消核销</h3>
          <p class="muted">这条核销记录金额 <b>${esc(amt)}</b>。填写要撤销的金额；填全额（或留空）就是整条撤销。</p>
          <label>撤销金额 <input id="us-amt" value="${esc(amt)}" style="width:140px"></label>
          <div class="row" style="margin-top:12px"><button class="btn" id="us-ok">确定</button><button class="btn ghost" id="us-cancel">取消</button></div>`);
        $("#us-cancel").onclick = closeModal;
        $("#us-ok").onclick = async () => {
          const v = ($("#us-amt").value || "").trim();
          try {
            await post("/settle/unsettle", { id, amount: v === "" ? null : v });
            closeModal();
            toast("已撤销", "ok");
            $("#st-records", main).click();
          } catch (e) { toast(e.message, "err"); }
        };
      });
    } catch (e) { toast(e.message, "err"); }
  };
  // ---- 往来对冲单（应收 ↔ 应付相互抵销）----
  async function loadOffsets() {
    const el = $("#off-list", main);
    if (!el) return;
    try {
      const r = await api("/settle/offsets");
      const rows = r.rows || [];
      if (!rows.length) { el.innerHTML = `<div class="muted">暂无对冲单</div>`; return; }
      el.innerHTML = `<table class="grid"><thead><tr><th>日期</th><th>单号</th><th>客户(应收)</th><th>供应商(应付)</th><th class="num">金额</th><th>状态</th><th></th></tr></thead><tbody>${rows.map((o) => `<tr><td>${esc(o.date)}</td><td>${esc(o.no)}</td><td>${esc(o.customer)}</td><td>${esc(o.supplier)}</td><td class="num">${fmt(o.amount)}</td><td>${o.status === "audited" ? "已审核" : "待审核"}</td><td>${can("voucher_new") && o.status === "draft" ? `<button class="btn sm ghost" data-off-act="audit" data-off-id="${o.id}">审核</button> ` : ""}${(can("voucher_audit") || can("voucher_post")) && o.status === "audited" ? `<button class="btn sm ghost" data-off-act="unaudit" data-off-id="${o.id}">撤审</button> ` : ""}${can("voucher_new") && o.status !== "audited" ? `<button class="btn sm ghost" data-off-act="delete" data-off-id="${o.id}">删除</button>` : ""}</td></tr>`).join("")}</tbody></table>`;
      $all("[data-off-act]", el).forEach((b) => b.onclick = async () => {
        const act = b.dataset.offAct, id = b.dataset.offId;
        const label = act === "audit" ? "审核（出凭证并两端核销）" : act === "unaudit" ? "撤审（回退凭证与核销）" : "删除";
        if (!(await confirmDialog(`确定${label}该对冲单？`, false))) return;
        try { await post(`/settle/offsets/${id}/${act}`, {}); toast("已处理", "ok"); loadOffsets(); load(); } catch (e) { toast(e.message, "err"); }
      });
    } catch (e) { el.innerHTML = `<div class="muted">${esc(e.message)}</div>`; }
  }
  if ($("#off-reload", main)) $("#off-reload", main).onclick = loadOffsets;
  if ($("#off-date", main)) $("#off-date", main).value = new Date().toISOString().slice(0, 10);
  if ($("#off-new", main)) $("#off-new", main).onclick = async () => {
    const body = {
      date: $("#off-date", main).value.trim(),
      customer: $("#off-cust", main).value.trim(),
      supplier: $("#off-sup", main).value.trim(),
      amount: $("#off-amt", main).value.trim(),
      memo: $("#off-memo", main).value.trim(),
    };
    if (!body.customer || !body.supplier || !body.amount) { toast("客户、供应商、对冲金额都要填", "err"); return; }
    try {
      await post("/settle/offsets", body);
      toast("已新增对冲单；到列表点「审核」出凭证并两端核销", "ok");
      $("#off-cust", main).value = ""; $("#off-sup", main).value = "";
      $("#off-amt", main).value = ""; $("#off-memo", main).value = "";
      loadOffsets();
    } catch (e) { toast(e.message, "err"); }
  };
  loadOffsets();
  load();
  // 收款认领：刷新 / 按建议勾 / 落库
  if ($("#cl-reload", main)) $("#cl-reload", main).onclick = clLoad;
  if ($("#cl-acct", main)) $("#cl-acct", main).onchange = clLoad;
  if ($("#cl-period", main)) $("#cl-period", main).onchange = clLoad;
  if ($("#cl-suggest", main)) $("#cl-suggest", main).onclick = () => {
    const sel = $("#cl-inflow", main);
    const curId = sel && sel.value ? parseInt(sel.value, 10) : 0;
    const picks = (clSugg.find((x) => x.inflow === curId) || {}).picks || [];
    $all("#cl-list [data-cl-pick]", main).forEach((b) => { b.checked = false; });
    picks.forEach((p) => {
      const box = $(`#cl-list [data-cl-pick="${p.entry}"]`, main);
      const amt = $(`#cl-list [data-cl-amt="${p.entry}"]`, main);
      if (box) box.checked = true;
      if (amt) amt.value = p.amount;
    });
    toast(`按建议勾了 ${picks.length} 笔；金额可以自己改`, "ok");
  };
  if ($("#cl-apply", main)) $("#cl-apply", main).onclick = async () => {
    const sel = $("#cl-inflow", main);
    const inflow = sel && sel.value ? parseInt(sel.value, 10) : 0;
    if (!inflow) { toast("先选一笔收款", "err"); return; }
    const picks = [];
    $all("#cl-list [data-cl-pick]", main).forEach((b) => {
      if (!b.checked) return;
      const amt = $(`#cl-list [data-cl-amt="${b.dataset.clPick}"]`, main);
      const v = amt ? amt.value.trim() : "";
      if (v) picks.push({ entry: parseInt(b.dataset.clPick, 10), amount: v });
    });
    if (!picks.length) { toast("先勾选要认领的挂账并填金额", "err"); return; }
    try {
      const r = await post("/claim", { account: clAcct(), inflow, picks, date: "" });
      toast(`认领 ${r.amount}；跨客户过渡凭证 ${r.bridges.length} 张，这笔收款还剩 ${r.leftover}`, "ok");
      clLoad(); load();
    } catch (e) { toast(e.message, "err"); }
  };
  clLoad();
}
