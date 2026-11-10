// FinBook Web 前端 — 单据链、采购/销售订单文档、工作流设计器、打印配置、订单编辑与变更日志
//
// 这一份是 `crates/finweb/static/app.js` 拆分出来的（原单文件 14k 行、295 个顶层函数）。
// 拆分方式：**经典 script，按原始行序切分，不重排任何代码**——顶层 function/const 的
// 提升与声明顺序完全保持原样，所以跨文件不存在 TDZ/未定义问题；只有 `VIEWS` 路由表
// 因为引用全部视图函数，被挪到了最后加载的 app-hr.js。
// 加载顺序由 `index.html` 的 <script> 顺序决定（也是 `main.rs` 里 asset_version 的清单顺序）。
// 新增文件必须同步三处：index.html、main.rs 的 asset_version 清单、tools/lib-app-src.js 的解析（自动）。
// 待发通知面板：pending 列表 → 「去出库」预填执行坞（发货成功后通知自动完成）
// 报价单存完之后的「和价目表对不对得上」那一行。
//
// 抽成顶层是为了让 tools/check-app-render.js 能真调用：这行字里既有颜色也有
// 数字（价目表折合价、差多少），写错了没人看得出来，只能靠门禁守。
// 只提示、不拦单 —— 手工价有时是对的（老客户、一次性让价），但 1.5 写成 15
// 这种错误得让人当场看见。
// 「按价目表算」算完之后那一行绿字：用的是哪条价、单价多少、合计多少、折算过程。
// 抽成顶层是为了让 check-app-render 能真调用它（不抽就只能靠手点）。
function linePriceHtml(p) {
  if (!p) return "";
  const unit = esc(p.stock_unit || "");
  const price = esc(fmt(p.unit_price));
  const total = esc(fmt(p.total));
  const note = p.note ? `（${esc(p.note)}）` : "";
  return `<span style="color:var(--ok)">✔ ${esc(p.rule_name || "价目表")} → 单价 ${price} / ${unit}，合计 ${total}${note}</span>`;
}

function priceCheckHtml(r) {
  if (!r) return "";
  const c = r.price_check || null;
  if (r.price_warn) {
    return `<span style="color:var(--warn)">⚠ ${esc(r.price_warn)}</span>`;
  }
  if (c && c.kind === "ok") {
    return `<span style="color:var(--ok)">✔ 和价目表一致：${esc(c.rule_price)} 元/${esc(c.rule_unit || c.stock_unit)}，折合每${esc(c.stock_unit)} ${esc(c.per_unit)} 元</span>`;
  }
  return "";
}

function quoReconcileHtml(rows) {
  if (!rows || !rows.length) return `<div class="muted">当期还没有报价单</div>`;
  const drifted = rows.filter((r) => r.so_id && Number(r.diff) !== 0);
  const tr = (r) => {
    const none = !r.so_id;
    const diff = none ? "" : fmtMoney(r.diff);
    const sign = !none && Number(r.diff) > 0 ? "+" : "";
    const cls = !none && Number(r.diff) !== 0 ? "warn" : "";
    return `<tr${cls ? ` class="${cls}"` : ""}>
      <td>${esc(r.quo_no)}</td>
      <td>${esc(r.customer_name)}</td>
      <td>${esc(r.item_name)}</td>
      <td class="num">${esc(fmt(r.quo_qty))}</td>
      <td class="num">${esc(fmtMoney(r.quo_price))}</td>
      <td class="num">${none ? `<span class="muted">未转订单</span>` : esc(fmtMoney(r.so_price))}</td>
      <td class="num">${none ? `<span class="muted">—</span>` : `<b>${sign}${esc(diff)}</b>`}</td>
      <td class="num">${none ? `<span class="muted">—</span>` : esc(fmtMoney(r.diff_amount))}</td>
      <td>${none ? `<span class="muted">—</span>` : esc(r.so_status)}</td>
    </tr>`;
  };
  const summary = drifted.length
    ? `<div class="muted" style="margin:4px 0">共 ${rows.length} 张，其中 <span style="color:var(--warn)">${drifted.length} 张</span>实际单价和报价不一致</div>`
    : `<div class="muted" style="margin:4px 0">共 ${rows.length} 张，实际单价都与报价一致</div>`;
  return `${summary}<table class="grid"><thead><tr><th>单号</th><th>客户</th><th>存货</th><th class="num">数量</th><th class="num">报价单价</th><th class="num">实际单价</th><th class="num">单价差</th><th class="num">差额</th><th>订单状态</th></tr></thead><tbody>${rows.map(tr).join("")}</tbody></table>`;
}

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
      <label>收货行 <select id="pd-line" style="width:190px"></select></label>
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
  const lineCode = () => $("#pd-line").value || "";
  // 收货行下拉：跟着订单ID刷新，列出该 PO 的物料行。
  // 多行订单必须选行 —— 不选时金额只能按订购量占比折算，对部分收货是错的
  // （两张行各 100 件、含税单价 11，实收 100 件全是第一行会折算成 550）。
  // 复用 /procure/estimate/status：它本来就是按行给出物料与订购量，
  // 不必为下拉单开一个只读 PO 端点。
  const refreshLines = async () => {
    const sel = $("#pd-line");
    if (!poid()) { sel.innerHTML = `<option value="">（先填订单ID）</option>`; return; }
    try {
      const r = await api(`/procure/estimate/status?po_id=${poid()}`);
      const rows = (r.rows || []).filter((x) => x.item_code);
      sel.innerHTML = rows.length
        ? rows.map((x) => `<option value="${esc(x.item_code)}">${esc(x.item_code)} ${esc(x.item_name || "")}（订 ${fmt(x.qty_ordered)}／已收 ${fmt(x.qty_received)}）</option>`).join("")
        : `<option value="">（该单没有物料行）</option>`;
    } catch (e) { sel.innerHTML = `<option value="">（读取订单失败）</option>`; }
  };
  $("#pd-poid").addEventListener("change", refreshLines);
  $("#pd-receipt").addEventListener("click", async () => { if (!poid()) { toast("请填写采购订单ID", "err"); return; } try { const rr = await postJson("/procure/receipt", { po_id: poid(), period: ymm(state.current || ""), date: today(), qty: amt(), warehouse: $("#pd-wh").value.trim(), item_code: lineCode(), memo: memo() }); toast(rr && rr.qc_pending ? "已到货（待检入库，待质检转正后可用）" : "已到货", "ok"); $("#pd-amt").value=""; $("#pd-memo2").value=""; load(); refreshLines(); } catch (e) { toast(e.message, "err"); } });
  $("#pd-return").addEventListener("click", async () => { if (!poid()) { toast("请填写采购订单ID", "err"); return; } try { await postJson("/procure/return", { po_id: poid(), period: ymm(state.current || ""), date: today(), qty: amt(), warehouse: $("#pd-wh").value.trim(), item_code: lineCode(), memo: memo() }); toast("已退货", "ok"); $("#pd-amt").value=""; $("#pd-memo2").value=""; load(); refreshLines(); } catch (e) { toast(e.message, "err"); } });
  $("#pd-pay").addEventListener("click", async () => { if (!poid()) { toast("请填写采购订单ID", "err"); return; } try { const r = await postJson("/procure/payment", { po_id: poid(), period: ymm(state.current || ""), date: today(), amount: amt(), memo: memo() }); toast(r && r.doc_id ? `已付款，付款单 #${r.doc_id}（待审核，审核后出凭证并自动核销）` : "已付款", "ok"); $("#pd-amt").value=""; $("#pd-memo2").value=""; load(); } catch (e) { toast(e.message, "err"); } });
  load();
}

async function viewSoDoc(main) {
  const period = encodeURIComponent(state.current || "");
  const table = (rows) => rows.length
    ? `<table class="grid"><thead><tr><th style="width:26px"><input type="checkbox" id="quo-chkall" title="全选" /></th><th>单号</th><th>客户</th><th>存货</th><th>数量</th><th>单价</th><th>状态</th><th></th></tr></thead>
      <tbody>${rows.map((r) => `<tr><td><input type="checkbox" class="quo-chk" value="${r.id}" /></td><td>${esc(r.no)}</td><td>${esc(r.customer_name)}</td><td>${esc(r.item_name)}</td><td class="num">${esc(r.qty)}</td><td class="num">${esc(r.unit_price)}</td><td>${esc(({ draft: "草稿", approved: "已审批", converted: "已转订单", cancelled: "已作废" })[r.status] || r.status)}<span data-wftag="quotation:${r.id}"></span></td><td class="row-actions"><button class="btn ghost sm" data-quo-print="${r.id}">打印</button>${r.status === "draft" ? `<button class="btn ghost sm" data-quo-approve="${r.id}">审批</button>` : ""}${r.status === "approved" ? `<button class="btn ghost sm" data-quo-toorder="${r.id}">转订单</button>` : ""}${r.status === "converted" ? `<span class="tag ok">已转订单</span>` : ""}</td></tr>`).join("")}</tbody></table>`
    : `<div class="muted">暂无报价单</div>`;
  const soTrack = (rows) => rows.length
    ? `<table class="grid"><thead><tr><th>订单号</th><th>客户</th><th>订单数量</th><th>发货数量</th><th>执行率</th></tr></thead>
      <tbody>${rows.map((t) => `<tr><td>${esc(t.no)}</td><td>${esc(t.customer_name)}</td><td class="num">${esc(t.ordered_qty)}</td><td class="num">${esc(t.shipped_qty)}</td><td class="num">${esc(t.rate)}%</td></tr>`).join("")}</tbody></table>`
    : `<div class="muted">暂无销售订单</div>`;

  main.innerHTML = `<h2>销售单据</h2>
    <div class="toolbar">
      <label>客户 <input id="sd-cust" style="width:110px" /></label>
      <label>存货 <input id="sd-item" style="width:130px" /></label>
      <label>规格 <input id="sd-spec" style="width:110px" placeholder="1250*250*3" /></label>
      <label>口径 <input id="sd-cunit" style="width:52px" placeholder="㎡" /></label>
      <label>工艺 <input id="sd-proc" style="width:88px" placeholder="硬质氧化" /></label>
      <label>数量 <input id="sd-qty" style="width:70px" /></label>
      <label>单价 <input id="sd-price" style="width:80px" /></label>
      <button class="btn ghost sm" id="sd-line" title="按价目表算这一行：规格折成计价数量、按数量挑档、不足起步价兜底">按价目表算</button>
      <button class="btn ghost sm" id="sd-quote" title="按「加工费规则」算价：面积×单价，不足最低收费兜底，加急另加">按规则报价</button>
      <button class="btn primary" id="sd-save">保存报价单</button>
    </div>
    <div id="sd-price-warn" class="muted" style="margin-top:6px"></div>
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
    <div class="panel" style="margin-top:12px"><div style="display:flex;align-items:center;gap:8px"><h4 style="margin:0">报价单</h4><span class="grow"></span><button class="btn ghost sm" id="quo-print" title="勾选若干报价单一次打一摞；没勾就打印当前列表全部">打印所选</button>${can("report") ? `<button class="btn ghost sm" id="sd-recon">报价对账</button><button class="btn ghost sm" id="sd-recon-export" title="把对账表导成 Excel 给老板/客户留档">导出</button>` : ""}</div><div id="sd-list">加载中…</div><div id="sd-recon-out" class="muted" style="margin-top:8px"></div></div>
    <div class="panel" style="margin-top:12px"><h4>销售订单执行跟踪</h4><div id="sd-track">加载中…</div></div>`;

  const load = async () => {
    try {
      const s = await api(`/sales/so?period=${period}`);
      const orows = s.rows || [];
      $("#so-list").innerHTML = orows.length
        ? `<table class="grid"><thead><tr><th style="width:26px"><input type="checkbox" id="so-chkall" title="全选" /></th><th>单号</th><th>客户</th><th class="num">不含税</th><th class="num">税额</th><th class="num">价税合计</th><th>状态</th><th></th></tr></thead><tbody>${orows.map((o) => `<tr><td><input type="checkbox" class="so-chk" value="${o.id}" /></td><td>${esc(o.no)}</td><td>${esc(o.customer_name)}</td><td class="num">${fmt(o.total_amount)}</td><td class="num">${fmt(o.total_tax)}</td><td class="num"><b>${fmt((Number(o.total_amount) || 0) + (Number(o.total_tax) || 0))}</b></td><td><span class="tag ${o.status === "Cancelled" ? "warn" : o.status === "Draft" ? "" : "ok"}">${esc(ORDER_STATUS_LABEL[o.status] || o.status)}</span></td><td class="row-actions"><button class="btn ghost sm" data-so-edit="${o.id}">编辑</button>${o.status === "Draft" ? `<button class="btn ghost sm" data-so-confirm="${o.id}">确认</button><button class="btn ghost sm" data-so-del="${o.id}">删除</button>` : ""}${o.status !== "Cancelled" ? `<button class="btn ghost sm" data-so-cancel="${o.id}">作废</button>` : ""}<button class="btn ghost sm" data-so-chain="${o.id}">链</button>${can("voucher_new") && can("order_ops") ? `<button class="btn ghost sm" data-so-inv="${o.id}">票</button>` : ""}${o.status !== "Draft" && o.status !== "Cancelled" ? `<button class="btn ghost sm" data-so-notice="${o.id}">通知</button><button class="btn ghost sm" data-so-prod="${o.id}">下推生产</button>` : ""}<button class="btn ghost sm" data-so-exec="${o.id}">执行</button></td></tr>`).join("")}</tbody></table>`
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
      $all("[data-so-prod]").forEach((b) => b.onclick = () => openPushProd(b.dataset.soProd, load));
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
      $all("[data-quo-print]").forEach((b) => b.onclick = () => window.open(`/api/sales/quote/print-form?ids=${b.dataset.quoPrint}`, "_blank"));
      if ($("#quo-chkall")) $("#quo-chkall").onclick = (e) => { $all(".quo-chk").forEach((c) => { c.checked = e.target.checked; }); };
      $("#quo-print").onclick = () => runPrint("/api/sales/quote/print-form", ".quo-chk", "order", PRINT_QUOTE_FIELDS);
      $all("[data-quo-toorder]").forEach((b) => b.onclick = async () => {
        try {
          const r = await postJson(`/sales/quote/${b.dataset.quoToorder}/to-order`, {});
          toast(`已生成销售订单 #${r.so_id}（草稿，可在上方订单列表确认执行）`, "ok");
          load();
        } catch (e) { toast(e.message, "err"); }
      });
    } catch (e) { $("#sd-list").innerHTML = `<div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; }
    if (can("report")) {
      try {
        const rc = await api(`/sales/quote/reconcile?period=${period}`);
        $("#sd-recon-out").innerHTML = quoReconcileHtml(rc.rows || []);
        $("#sd-recon").onclick = async () => {
          try { const rc2 = await api(`/sales/quote/reconcile?period=${period}`); $("#sd-recon-out").innerHTML = quoReconcileHtml(rc2.rows || []); toast("已刷新", "ok"); } catch (e) { toast(e.message, "err"); }
        };
        $("#sd-recon-export").onclick = () => window.open(`/api/sales/quote/reconcile/export?period=${period}`, "_blank");
      } catch (e) { $("#sd-recon-out").innerHTML = `<div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; }
    }
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
  // 加工费规则一键报价：计价口径在 findb::processfee（面积×单价 → 最低收费兜底 → 加急），
  // 界面只负责把结果填进单价框，不自己算——同一套规则在网页、桌面各算一遍早晚差一分钱。
  // 单价填「合计 / 数量」（保留 4 位），这样报价单金额正好等于规则算出来的合计；
  // 低于最低收费时，这个单价会高于规则单价，这是有意的（那正是「最低收费」的意思）。
  $("#sd-quote").addEventListener("click", async () => {
    const item = $("#sd-item").value.trim();
    const cust = $("#sd-cust").value.trim();
    const qty = $("#sd-qty").value.trim();
    if (!item) { toast("先填存货（物料）编码", "err"); return; }
    const n = Number(qty || "0");
    if (!(n > 0)) { toast("先填计价数量（面积/件数）", "err"); return; }
    try {
      const qs = new URLSearchParams({ item, qty, rush: $("#sd-rush") && $("#sd-rush").checked ? "1" : "0" });
      if (cust) qs.set("customer", cust);
      const r = await api(`/price-rules/quote?${qs.toString()}`);
      const q = r.quote || r;
      const num = (v) => Number(String(v == null ? 0 : v).replace(/,/g, ""));
      const unit = (num(q.total) / n).toFixed(4).replace(/0+$/, "").replace(/\.$/, "");
      $("#sd-price").value = unit;
      toast(`规则「${q.rule_name || "按规则"}」：${fmt(q.qty)} × ${fmt(q.unit_price)} = 基础价 ${fmt(q.base)}，最低收费补差 ${fmt(q.floor_extra)}，加急 ${fmt(q.rush_extra)}，合计 ${fmt(q.total)}`, "ok");
    } catch (e) { toast(e.message, "err"); }
  });
  // 按价目表算一行：规格 → 折算成计价数量 → 按数量挑档 → 摊回「每库存单位」的单价。
  // 后端 findb::processfee::price_for_line 算，界面不自己算（同一套规则两处实现早晚差一分钱）。
  $("#sd-line").addEventListener("click", async () => {
    const item = $("#sd-item").value.trim();
    const qty = $("#sd-qty").value.trim();
    if (!item) { toast("先填存货（物料）编码", "err"); return; }
    if (!(Number(qty || "0") > 0)) { toast("先填数量", "err"); return; }
    try {
      const qs = new URLSearchParams({ item, qty });
      const cust = $("#sd-cust").value.trim();
      const spec = $("#sd-spec").value.trim();
      const cunit = $("#sd-cunit").value.trim();
      const proc = $("#sd-proc").value.trim();
      if (cust) qs.set("customer", cust);
      if (spec) qs.set("spec", spec);
      if (cunit) qs.set("calc_unit", cunit);
      if (proc) qs.set("process", proc);
      const p = await api(`/sales/quote/price?${qs.toString()}`);
      $("#sd-price").value = String(p.unit_price == null ? "" : p.unit_price).replace(/,/g, "");
      const el = $("#sd-price-warn");
      if (el) el.innerHTML = linePriceHtml(p);
      toast(`按价目表算好：单价 ${fmt(p.unit_price)} / ${p.stock_unit}，合计 ${fmt(p.total)}`, "ok");
    } catch (e) { toast(e.message, "err"); }
  });
  // 存完把「这单价和价目表对不对得上」摆出来。只提示、不拦单 ——
  // 手工价有时是对的（老客户、一次性让价），系统不该替业务员拍板；
  // 但 1.5 写成 15 这种错误得让人当场看见。
  const showPriceCheck = (r) => {
    const el = $("#sd-price-warn");
    if (el) el.innerHTML = priceCheckHtml(r);
  };
  $("#sd-save").addEventListener("click", async () => {
    try {
      const r = await postJson("/sales/quote", { id: 0, period: ymm(state.current || ""), date: today(), customer_code: $("#sd-cust").value.trim(), customer_name: $("#sd-cust").value.trim(), item_code: $("#sd-item").value.trim(), item_name: $("#sd-item").value.trim(), qty: $("#sd-qty").value.trim() || "0", unit_price: $("#sd-price").value.trim() || "0", status: "draft", prepared_by: "", memo: "", spec: $("#sd-spec").value.trim(), calc_unit: $("#sd-cunit").value.trim(), process: $("#sd-proc").value.trim() });
      if (r && r.price_warn) toast("已保存，但这单价和价目表对不上（见下方提示）", "err"); else toast("已保存报价单", "ok");
      $("#sd-qty").value=""; $("#sd-price").value=""; $("#sd-spec").value=""; $("#sd-cunit").value=""; $("#sd-proc").value="";
      // 先等列表重画再写提示，否则 load() 把这块 DOM 换了、提示当场消失
      const saved = r; await load(); showPriceCheck(saved);
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
      ${can("sys_option") ? `<button class="btn ghost sm" id="wf-tpl">从模板创建</button><button class="btn primary sm" id="wf-new">新建流程</button>` : `<span class="muted" style="font-size:12px">仅账套管理员可新建/发布流程；审批人按节点参与人在实例页操作</span>`}
    </div>
    <div id="wf-body">${mode === "design" ? "加载中…" : ""}</div>`;
  $("#wf-d").onclick = () => { state.wfMode = "design"; viewWorkflow(main); };
  $("#wf-i").onclick = () => { state.wfMode = "instances"; viewWorkflow(main); };
  if (mode === "instances") return renderWfInstances($("#wf-body"));
  await renderWfDesign($("#wf-body"));
  if ($("#wf-new")) $("#wf-new").onclick = () => openWfEditor(null, () => renderWfDesign($("#wf-body")));
  if ($("#wf-tpl")) $("#wf-tpl").onclick = () => openWfTemplatePicker(() => renderWfDesign($("#wf-body")));
}

// 从预置模板创建流程
//
// 模板固化的是财务惯例里的常见审批链，**不是**这家企业的授权制度，所以落的是草稿：
// 管理员在画布上按自家授权制度改完、确认无误再发布。直接替企业发布等于替它做了
// 内控决策；且已有单据在流转时改流程，会让在途实例的 current_node 指向不存在的
// 节点（引擎报「流程可能被改」）。
function openWfTemplatePicker(onDone) {
  const m = modal(`<h3>从预置模板创建流程</h3>
    <div class="muted" style="font-size:12px;margin-bottom:10px">
      创建后为<b>草稿态</b>，请在画布上按本企业授权制度调整后再发布。
    </div>
    <div id="wt-list" class="muted">加载中…</div>
    <div class="foot"><button class="btn ghost" id="wt-close">关闭</button></div>`);
  $("#wt-close", m).onclick = closeModal;
  api("/workflows/templates").then((r) => {
    const rows = r.rows || [];
    if (!rows.length) { $("#wt-list", m).innerHTML = `<div class="muted">暂无预置模板</div>`; return; }
    $("#wt-list", m).innerHTML = rows.map((t) => {
      const apps = (t.applicable || []).map((a) => {
        const has = a.flow_id > 0;
        return `<label style="display:flex;align-items:center;gap:6px;font-size:13px;margin:3px 0;cursor:pointer">
          <input type="radio" name="wt-biz-${esc(t.key)}" value="${esc(a.biz_type)}" />
          <span>${esc(a.biz_label)}</span>
          ${has ? `<span class="muted" style="font-size:11px">（该单据已有${a.flow_status === "published" ? "已发布" : "草稿"}流程，将新增一条）</span>` : ""}
        </label>`;
      }).join("");
      return `<div class="panel" style="margin:0 0 10px;padding:10px 12px">
        <div style="display:flex;align-items:center;gap:8px;flex-wrap:wrap">
          <b>${esc(t.name)}</b>
          <span class="tag">${t.node_count} 节点</span>
          ${(t.requires_fields || []).length ? `<span class="tag warn">依赖字段：${esc((t.requires_fields || []).join("、"))}</span>` : ""}
        </div>
        <div class="muted" style="font-size:12px;margin:4px 0 6px">${esc(t.desc)}</div>
        <div style="font-size:12px;color:var(--muted);margin-bottom:2px">应用到：</div>
        ${apps}
        <div style="margin-top:8px"><button class="btn primary sm" data-wt-apply="${esc(t.key)}">用此模板创建</button></div>
      </div>`;
    }).join("");
    $all("[data-wt-apply]", m).forEach((b) => b.onclick = async () => {
      const key = b.dataset.wtApply;
      const sel = $(`input[name="wt-biz-${CSS.escape(key)}"]:checked`, m);
      if (!sel) { toast("请先选择要应用到哪种单据", "err"); return; }
      try {
        const r2 = await api("/workflows/templates/apply", {
          method: "POST", headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ key, biz_type: sel.value }),
        });
        toast(`已创建草稿「${r2.flow.name}」，请检查后发布`, "ok");
        closeModal();
        onDone && onDone();
      } catch (e) { toast(e.message, "err"); }
    });
  }).catch((e) => { $("#wt-list", m).innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; });
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
  // 当前单据类型可用的条件字段。**按单据类型不同**——生产订单没有 amount、
  // 报销单没有 planned_qty。以前这里是写死一句「amount/qty/customer_code 等」，
  // 用户在生产订单上照着写 amount > 5000 会一路保存成功，直到单据提交审批才报错。
  let condFields = [];
  const condFieldHint = () => {
    const fields = condFields.length ? condFields.join(" / ") : "加载中…";
    return `可用字段：${esc(fields)}；运算符 &gt; &gt;= &lt; &lt;= == !=；多条出线按序匹配，空条件为兜底；字符串值加引号`;
  };
  const loadCondFields = async () => {
    try {
      const r = await api(`/workflows/cond-fields?biz_type=${encodeURIComponent(biz)}`);
      condFields = r.fields || [];
    } catch (e) {
      condFields = [];
    }
    render();
  };
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
        <option value="purchase_order" ${biz === "purchase_order" ? "selected" : ""}>采购订单</option>
        <option value="production_order" ${biz === "production_order" ? "selected" : ""}>生产订单</option>
        <option value="sales_order" ${biz === "sales_order" ? "selected" : ""}>销售订单</option>
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
        <div class="field"><label>连线条件（审批时求值）</label><input id="pr-cond" value="${esc(e2.condition || "")}" placeholder="如：amount &gt; 5000" /><p class="muted" style="font-size:11px;margin:4px 0 0">${condFieldHint()}</p></div>
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
  // 换单据类型 → 换可用条件字段清单（字段集按单据不同）
  $("#wf-biz", mask).onchange = (ev) => { biz = ev.target.value; loadCondFields(); };
  loadRoles().then((rs) => { roles = rs; renderProps(); });
  loadCondFields();
}

// ---------------- 单据套打（订单/收付款单）：字段白名单 + 批量打印 ----------------
const PRINT_ORDER_FIELDS = ["no", "date", "status", "party", "memo", "prepared", "code", "name", "qty", "price", "rate", "amount", "tax", "linememo", "totals", "sign", "pack"];
// 报价单不含税、也没有行备注（折算说明在单据级「备注」里），字段白名单里去掉
// rate / tax / linememo —— 否则套打会打出三列空的「税率 / 税额 / 行备注」。
const PRINT_QUOTE_FIELDS = ["no", "date", "status", "party", "memo", "prepared", "code", "name", "qty", "price", "amount", "totals", "sign", "pack"];
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
