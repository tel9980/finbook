// FinBook Web 前端 — 自定义报表、采购/销售对账、库存账龄/ABC、序列号、单位、组装、仓库、调拨、盘点、物料主档、批次、暂估
//
// 这一份是 `crates/finweb/static/app.js` 拆分出来的（原单文件 14k 行、295 个顶层函数）。
// 拆分方式：**经典 script，按原始行序切分，不重排任何代码**——顶层 function/const 的
// 提升与声明顺序完全保持原样，所以跨文件不存在 TDZ/未定义问题；只有 `VIEWS` 路由表
// 因为引用全部视图函数，被挪到了最后加载的 app-hr.js。
// 加载顺序由 `index.html` 的 <script> 顺序决定（也是 `main.rs` 里 asset_version 的清单顺序）。
// 新增文件必须同步三处：index.html、main.rs 的 asset_version 清单、tools/lib-app-src.js 的解析（自动）。
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
// ---------------- 销售订单下推生产订单 ----------------
//
// 产销在数据上必须连着：以前 production_order 没有 so_id，「这批货是哪张订单要的」
// 只能靠人肉记忆，缺货时更无法反查「哪些订单正等着这批料」。
//
// 弹窗第一件事是**先算 ATP**（现货 + 在途 − 已占用）并把三个分量都摆出来。
// 只给一个 ATP 数字，被算错了也不知道错在哪一项；而且下推前就该看到
// 「现货不够、但下推生产之后够不够」，而不是只能事后看库存。
async function openPushProd(soId, after) {
  const id = Number(soId);
  let so, a;
  try {
    so = (await api(`/sales/so?period=${state.current}`)).rows.find((x) => x.id === id);
    if (!so) throw new Error("订单不存在");
    a = await api(`/atp?item=${encodeURIComponent(((so.lines || [])[0] || {}).item_code || "")}`);
  } catch (e) {
    toast(e.message, "err");
    return;
  }
  const line = (so.lines || [])[0] || {};
  const num = (v) => Number(String(v).replace(/,/g, "")) || 0;
  const remain = num(line.qty_ordered) - num(line.qty_shipped);
  const sign = (v) => (num(v) < 0 ? "var(--err)" : num(v) > 0 ? "var(--ok)" : "inherit");
  const mask = modal(`
    <h3>下推生产订单 · ${esc(so.no)}</h3>
    <div class="muted" style="margin-bottom:8px;line-height:1.7">
      存货 <b>${esc(line.item_name || line.item_code || "")}</b>（${esc(line.item_code || "")}）·
      订购 ${esc(line.qty_ordered || 0)} / 已发 ${esc(line.qty_shipped || 0)}，
      <b>未发货 ${esc(remain)}</b>。下推数量按「订购 − 已发 − 已下推」封顶。
    </div>
    <div class="cards" style="margin-bottom:10px">
      <div class="card"><div class="k">现有可用库存</div><div class="v">${esc(a.on_hand)}</div></div>
      <div class="card"><div class="k">在途·已排期</div><div class="v">${esc(a.incoming_dated)}${
        a.earliest_ready ? `<div class="muted" style="font-size:11px">最早 ${esc(a.earliest_ready)}</div>` : ""
      }</div></div>
      <div class="card"><div class="k">在途·未排期</div><div class="v" style="color:${Number(a.incoming_undated) > 0 ? "var(--warn)" : "inherit"}">${esc(a.incoming_undated)}</div></div>
      <div class="card"><div class="k">已占用（已确认订单）</div><div class="v">${esc(a.committed)}</div></div>
      <div class="card"><div class="k">可承诺量 ATP</div><div class="v" style="color:${sign(a.atp)}">${esc(a.atp)}</div></div>
    </div>
    ${Number(a.incoming_undated) > 0 ? `<div class="banner unset" id="pp-warn" style="margin-bottom:10px">
      <b>${esc(a.incoming_undated)} 件在途没有排期</b>，给不出交期 —— 拿去承诺客户就是编的。
      系统不会把它算进「到某日为止可承诺量」；要让这些量能承诺，去「生产 → 细排」给产单排上计划完工日。
    </div>` : ""}
    <div class="field"><label>客户要货日期 <span class="muted" style="font-weight:400">（选填：算「到该日为止可承诺量」）</span></label>
      <input id="pp-date" placeholder="YYYY-MM-DD" /></div>
    <div id="pp-until" class="muted" style="font-size:12px;line-height:1.7"></div>
    <div class="field"><label>下推数量 <span class="muted" style="font-weight:400">（留空 = 认领全部 ${esc(remain)}）</span></label>
      <input id="pp-qty" placeholder="${esc(remain)}" /></div>
    <p class="muted" style="font-size:12px;line-height:1.7">
      ATP = ${esc(a.formula || "")}。<br>
      ${esc(a.note || "")}。待检库存不计入可用——来料检验未转正的货领不走，算进去就是对客户虚承诺交期。
    </p>
    <div id="pp-err" style="display:none;margin-top:8px;color:var(--err);font-size:12px;line-height:1.6"></div>
    <div style="margin-top:10px;display:flex;gap:8px">
      <button class="btn" id="pp-cancel">取消</button>
      <button class="btn primary" id="pp-ok">确认下推</button>
    </div>`);
  $("#pp-cancel", mask).onclick = () => closeModal();
  // 填了要货日期就现算「到该日为止可承诺量」：这是销售真正要的那个答案，
  // 而不是一个没有日期的总数。
  $("#pp-date", mask).onchange = async (ev) => {
    const box = $("#pp-until", mask);
    const d = ev.target.value.trim();
    if (!/^\d{4}-\d{2}-\d{2}$/.test(d)) {
      box.textContent = "";
      return;
    }
    try {
      const r = await api(`/atp?item=${encodeURIComponent(((so.lines || [])[0] || {}).item_code || "")}&date=${d}`);
      const q = (v) => Number(String(v).replace(/,/g, "")) || 0;
      const can = q(r.atp) > 0;
      box.innerHTML = can
        ? `<b>${esc(d)} 之前可承诺 ${esc(r.atp)}</b>（现货 ${esc(r.on_hand)} + 计划完工日 ≤ 该日的在途 ${esc(r.incoming)}）。`
        : `<b style="color:var(--err)">${esc(d)} 之前可承诺不了</b>（可承诺 ${esc(r.atp)}）。` +
          `缺口 ${esc(Math.abs(q(r.atp)))} —— 要么提前计划完工日，要么与客户改期，别先答应下来。`;
    } catch (e) {
      box.textContent = e.message;
    }
  };
  $("#pp-ok", mask).onclick = async () => {
    const errBox = $("#pp-err", mask);
    errBox.style.display = "none";
    try {
      const r = await postJson("/prod/from-so", {
        so_id: id,
        qty: $("#pp-qty", mask).value.trim(),
      });
      toast(`已下推生产订单 ${r.no}（${r.planned_qty}）`, "ok");
      closeModal();
      if (after) after();
    } catch (e) {
      // 错误**留在弹窗里**而不只弹 toast：弹窗不关闭，toast 却会自动消失。
      // 「已全部认领、无可再下推」这类拒绝如果没留在原地，用户只会看到
      // 按钮点了没反应，然后以为系统坏了。
      toast(e.message, "err");
      errBox.textContent = e.message;
      errBox.style.display = "";
    }
  };
}

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
      ${can("export") ? `<button class="btn ghost" id="im-export" title="导出存货编码/名称/保质期/安全库存，改完可以导回">导出 Excel</button>` : ""}
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
  if ($("#im-export")) $("#im-export").addEventListener("click", () => downloadUrl("/api/export/items"));
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
    <div id="pe-status" class="muted">填写订单ID后查询</div>
    <div class="toolbar">
      <label>物料 <select id="pe-item" style="width:210px"></select></label>
      <label>暂估金额 <input id="pe-amount" style="width:120px" /></label>
      <button class="btn ghost" id="pe-fill">带出还能登的额度</button>
      <button class="btn" id="pe-add">登记暂估</button>
    </div>
    <div id="pe-result" class="muted">填写订单ID后查询</div>`;
  let statusRows = [];
  // 带出该物料**还能登的**额度（cap − 已暂估），不是 cap ——
  // 带出 cap 会让用户照抄一个必然被拒的数。
  function fillAmount() {
    const row = statusRows.find((x) => x.item_code === $("#pe-item").value);
    $("#pe-amount").value = row ? String(row.remaining ?? "0") : "";
  }
  const load = async () => {
    const poId = $("#pe-poid").value.trim();
    if (!poId) { toast("请填写采购订单ID", "err"); return; }
    // 两个请求**各自容错**：订单不存在时 status 会报错，但暂估记录列表仍要能看。
    // 合成一个 try 的话，订单一填错整个视图就卡在旧数据上不动了。
    statusRows = [];
    try {
      // ① 逐行状态：可暂估额度 / 已暂估 / 还能登多少 —— 三笔数摆在面前，
      //    用户自己判断，而不是「手填金额 → 试错 → 被守卫拒」。
      const st = await api(`/procure/estimate/status?po_id=${encodeURIComponent(poId)}`);
      statusRows = st.rows || [];
      $("#pe-status").innerHTML = statusRows.length
        ? `<table class="grid"><thead><tr>
            <th>物料</th><th class="num">订购</th><th class="num">已收</th><th class="num">欠收</th>
            <th class="num">可暂估额度</th><th class="num">已暂估</th><th class="num">还能登</th></tr></thead>
          <tbody>${statusRows.map((x) => `<tr>
            <td>${esc(x.item_code)} ${esc(x.item_name || "")}</td>
            <td class="r">${fmt(x.qty_ordered)}</td>
            <td class="r">${fmt(x.qty_received)}</td>
            <td class="r">${fmt(x.qty_outstanding)}</td>
            <td class="r">${fmt(x.cap)}</td>
            <td class="r">${fmt(x.estimated)}</td>
            <td class="r">${Number(x.remaining) > 0 ? fmt(x.remaining) : `<span class="muted">已用满</span>`}</td>
          </tr>`).join("")}</tbody></table>`
        : `<span class="muted">该订单没有物料行</span>`;
    } catch (e) {
      $("#pe-status").innerHTML = `<span style="color:#b00">读取暂估状态失败：${esc(e.message)}</span>`;
    }
    // ② 物料改为**从订单行里选**：自由输入能敲订单里没有的物料，
    //    被守卫拒一次才发现，白跑一趟。
    const sel = $("#pe-item");
    const keep = sel.value;
    sel.innerHTML = statusRows.map((x) =>
      `<option value="${esc(x.item_code)}"${x.item_code === keep ? " selected" : ""}>${esc(x.item_code)} ${esc(x.item_name || "")}</option>`
    ).join("") || `<option value="">（先查订单）</option>`;
    fillAmount();

    // ③ 已有暂估记录
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
  $("#pe-item").addEventListener("change", fillAmount);
  $("#pe-fill").addEventListener("click", () => {
    fillAmount();
    if (!Number($("#pe-amount").value || 0)) toast("该物料的暂估额度已用满", "err");
  });
  $("#pe-add").addEventListener("click", async () => {
    const po_id = parseInt($("#pe-poid").value.trim(), 10);
    if (!po_id) { toast("请填写采购订单ID", "err"); return; }
    const item = $("#pe-item").value;
    if (!item) { toast("请选择物料（先查订单）", "err"); return; }
    try {
      const r = await postJson("/procure/estimate", { po_id, item, est_amount: $("#pe-amount").value.trim() });
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
