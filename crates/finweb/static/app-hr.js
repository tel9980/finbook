// FinBook Web 前端 — 工资、费用报销、寄售、价格规则、计件工资、工装台账、返工成本 + 启动自举
//
// 这一份是 `crates/finweb/static/app.js` 拆分出来的（原单文件 14k 行、295 个顶层函数）。
// 拆分方式：**经典 script，按原始行序切分，不重排任何代码**——顶层 function/const 的
// 提升与声明顺序完全保持原样，所以跨文件不存在 TDZ/未定义问题；只有 `VIEWS` 路由表
// 因为引用全部视图函数，被挪到了最后加载的 app-hr.js。
// 加载顺序由 `index.html` 的 <script> 顺序决定（也是 `main.rs` 里 asset_version 的清单顺序）。
// 新增文件必须同步三处：index.html、main.rs 的 asset_version 清单、tools/lib-app-src.js 的解析（自动）。
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
  if ($("#py-export")) $("#py-export").onclick = () => downloadUrl(`/api/export/payroll?period=${encodeURIComponent(period)}`);
  if ($("#py-bank")) $("#py-bank").onclick = () => downloadUrl(`/api/payroll/bank-file?period=${encodeURIComponent(ymm(period))}`);
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
          <button class="btn" id="pv-social-btn">生成社保缴纳凭证</button>
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
    if ($("#pv-social-btn")) $("#pv-social-btn").onclick = runVoucher("/payroll/social-pay", { date: $("#pv-date").value, social_payable: $("#pv-social").value.trim(), housing_payable: $("#pv-housing").value.trim(), personal_payable: $("#pv-personal").value.trim(), bank_account: $("#pv-bank").value.trim() }, "社保缴纳凭证");
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
    downloadUrl(`/api/export/claims?${qs.toString()}`);
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
// ===========================================================================
// 客供料台账（受托加工：客户来料只记数量，不进存货、不出凭证）
// ===========================================================================
async function viewConsign(main) {
  main.innerHTML = `<h2>客供料台账</h2>
    <div class="toolbar">
      <label>客户 <input id="cs-cust" style="width:120px" placeholder="留空 = 全部" /></label>
      <button class="btn primary sm" id="cs-load">查询单据</button>
      <button class="btn ghost sm" id="cs-bal">查结存</button>
      <button class="btn ghost sm" id="cs-stmt">客户对账单</button>
      <button class="btn ghost sm" id="cs-sop">交货进度</button>
      ${can("order_ops") ? `<button class="btn primary sm" id="cs-new">登记单据</button>` : ""}
    </div>
    <div class="panel"><h3 style="margin-top:0;font-size:14px">单据</h3><div id="cs-docs" class="muted">加载中…</div></div>
    <div class="panel"><h3 style="margin-top:0;font-size:14px">结存（客户 + 物料 + 批次）</h3><div id="cs-bals" class="muted">点「查结存」查看</div></div>
    <div class="muted" style="font-size:12px">客户来料不进自己的存货、不产生金额、不出凭证；交货 / 退料 / 损耗都扣减结存，超交会被拦下</div>`;

  const cust = () => $("#cs-cust", main).value.trim();
  const qs = () => (cust() ? `?customer=${encodeURIComponent(cust())}` : "");
  let kinds = [];
  let docs = [];
  const kindOpts = (sel) =>
    kinds.map((k) => `<option value="${esc(k.code)}" ${k.code === sel ? "selected" : ""}>${esc(k.label)}</option>`).join("");

  const loadDocs = async () => {
    try {
      const r = await api(`/consign/docs${qs()}`);
      kinds = r.kinds || [];
      docs = r.docs || [];
      $("#cs-docs", main).innerHTML = docs.length
        ? `<table class="grid"><thead><tr><th>单号</th><th>日期</th><th>客户</th><th>类型</th><th>销售订单</th><th>行数</th><th>合计数量</th><th>备注</th><th></th></tr></thead><tbody>${docs.map((d) => {
            const total = (d.lines || []).reduce((s, l) => s + (parseFloat(String(l.qty).replace(/,/g, "")) || 0), 0);
            return `<tr><td>${esc(d.no)}</td><td>${esc(d.date)}</td><td>${esc(d.customer)}</td>
              <td><span class="tag">${esc(d.kind_label || d.kind)}</span></td>
              <td>${esc(d.so_no || "—")}</td>
              <td class="num">${(d.lines || []).length}</td><td class="num">${fmt(total)}</td>
              <td>${esc(d.memo || "")}</td>
              <td class="row-actions">${can("order_ops") ? `<button class="btn ghost sm" data-cs-del="${d.id}">删</button>` : ""}</td></tr>`;
          }).join("")}</tbody></table>`
        : `<div class="muted">暂无客供料单据</div>`;
      $all("[data-cs-del]", main).forEach((b) => (b.onclick = async () => {
        if (!(await confirmDialog(`删除客供料单 ${b.dataset.csDel}？删掉后结存会跟着变。`, true))) return;
        try { await api(`/consign/docs/${b.dataset.csDel}`, { method: "DELETE" }); toast("已删除", "ok"); loadDocs(); }
        catch (e) { toast(e.message, "err"); }
      }));
    } catch (e) { $("#cs-docs", main).innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };

  const loadBal = async () => {
    try {
      const r = await api(`/consign/balances${qs()}`);
      const rows = r.rows || [];
      $("#cs-bals", main).innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>客户</th><th>物料</th><th>名称</th><th>批次</th><th>单位</th><th>结存数量</th></tr></thead><tbody>${rows.map((x) => `<tr>
            <td>${esc(x.customer)}</td><td>${esc(x.item)}</td><td>${esc(x.item_name || "")}</td>
            <td>${esc(x.batch_no || "—")}</td><td>${esc(x.unit || "")}</td><td class="num">${fmt(x.qty)}</td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">没有结存</div>`;
    } catch (e) { $("#cs-bals", main).innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };

  const openEditor = () => {
    let rows = [{ item: "", item_name: "", unit: "件", qty: "", batch_no: "" }];
    const m = modal(`<h3>登记客供料单据</h3>
      <div class="field"><label>客户 *</label><input id="cs-customer" value="${esc(cust())}" /></div>
      <div class="field"><label>单据类型 *</label><select id="cs-kind">${kindOpts("in")}</select></div>
      <div class="field"><label>日期 *</label><input id="cs-date" type="date" value="${esc(state.today || "")}" /></div>
      <div class="field"><label>销售订单</label><select id="cs-so"><option value="0">不挂订单</option></select>
        <span class="muted" style="font-size:12px;margin-left:6px">交货挂上订单后，「交货进度」能看每单还差多少</span></div>
      <div class="field"><label>备注</label><input id="cs-memo" /></div>
      <div id="cs-lines"></div>
      <div class="foot"><button class="btn ghost sm" id="cs-add">加一行</button><button class="btn primary" id="cs-save">保存</button><button class="btn ghost" id="cs-cancel">取消</button></div>`, true);
    const drawLines = () => {
      $("#cs-lines", m).innerHTML = `<table class="grid"><thead><tr><th>物料 *</th><th>名称</th><th>单位</th><th>数量 *</th><th>批次</th><th></th></tr></thead><tbody>${rows.map((l, i) => `<tr>
        <td><input data-f="item" data-i="${i}" value="${esc(l.item)}" style="width:110px" /></td>
        <td><input data-f="item_name" data-i="${i}" value="${esc(l.item_name)}" style="width:110px" /></td>
        <td><input data-f="unit" data-i="${i}" value="${esc(l.unit)}" style="width:60px" /></td>
        <td><input data-f="qty" data-i="${i}" value="${esc(l.qty)}" style="width:80px" /></td>
        <td><input data-f="batch_no" data-i="${i}" value="${esc(l.batch_no)}" style="width:80px" /></td>
        <td>${rows.length > 1 ? `<button class="btn danger sm" data-i="${i}" data-del="1">删</button>` : ""}</td></tr>`).join("")}</tbody></table>`;
      $all("[data-f]", m).forEach((el) => (el.oninput = () => { rows[+el.dataset.i][el.dataset.f] = el.value; }));
      $all("[data-del]", m).forEach((b) => (b.onclick = () => { rows.splice(+b.dataset.i, 1); drawLines(); }));
    };
    // 拉当期销售订单填下拉（客户填了就只留该客户的）
    api(`/sales/so?period=${encodeURIComponent(state.current || "")}`)
      .then((r) => {
        const all = (r.rows || []).filter((so) => !cust() || String(so.customer_code || "").trim() === cust());
        $("#cs-so", m).innerHTML =
          `<option value="0">不挂订单</option>` +
          all.map((so) => `<option value="${so.id}">${esc(so.no)} · ${esc(so.customer_code || "")} · ${esc(so.date || "")}</option>`).join("");
      })
      .catch(() => {});
    drawLines();
    $("#cs-add", m).onclick = () => { rows.push({ item: "", item_name: "", unit: "件", qty: "", batch_no: "" }); drawLines(); };
    $("#cs-cancel", m).onclick = closeModal;
    $("#cs-save", m).onclick = async () => {
      const body = {
        customer: $("#cs-customer", m).value.trim(),
        kind: $("#cs-kind", m).value,
        date: $("#cs-date", m).value.trim(),
        memo: $("#cs-memo", m).value.trim(),
        so_id: Number($("#cs-so", m).value || 0),
        lines: rows.filter((l) => l.item.trim() || l.qty.trim()),
      };
      if (!body.customer || !body.date) { toast("客户与日期必填", "err"); return; }
      if (!body.lines.length) { toast("至少要一行", "err"); return; }
      try {
        await api("/consign/docs", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
        toast("已保存", "ok"); closeModal(); loadDocs(); loadBal();
      } catch (e) { toast(e.message, "err"); }
    };
  };

  const openStatement = () => {
    const m = modal(`<h3>客户对账单</h3>
      <div class="field"><label>客户 *</label><input id="cs-st-cust" value="${esc(cust())}" /></div>
      <div class="field"><label>起 <input id="cs-from" style="width:110px" placeholder="2026-01" value="${esc(state.current || "")}" /></label>
        <label style="margin-left:8px">止 <input id="cs-to" style="width:110px" placeholder="2026-01" value="${esc(state.current || "")}" /></label></div>
      <div id="cs-st-out" class="muted">点「出对账单」</div>
      <div class="foot"><button class="btn primary" id="cs-st-run">出对账单</button><button class="btn ghost" id="cs-st-close">关闭</button></div>`, true);
    $("#cs-st-close", m).onclick = closeModal;
    $("#cs-st-run", m).onclick = async () => {
      const c = $("#cs-st-cust", m).value.trim();
      if (!c) { toast("对账单必须指定客户", "err"); return; }
      try {
        const r = await api(`/consign/statement?customer=${encodeURIComponent(c)}&from=${encodeURIComponent($("#cs-from", m).value.trim())}&to=${encodeURIComponent($("#cs-to", m).value.trim())}`);
        const rows = r.rows || [];
        $("#cs-st-out", m).innerHTML = rows.length
          ? `<table class="grid"><thead><tr><th>物料</th><th>名称</th><th>批次</th><th>单位</th><th>期初</th><th>来料</th><th>交货</th><th>退料</th><th>损耗</th><th>期末</th></tr></thead><tbody>${rows.map((x) => `<tr>
              <td>${esc(x.item)}</td><td>${esc(x.item_name || "")}</td><td>${esc(x.batch_no || "—")}</td><td>${esc(x.unit || "")}</td>
              <td class="num">${fmt(x.begin)}</td><td class="num">${fmt(x.inbound)}</td><td class="num">${fmt(x.delivered)}</td>
              <td class="num">${fmt(x.returned)}</td><td class="num">${fmt(x.loss)}</td><td class="num">${fmt(x.end)}</td></tr>`).join("")}</tbody></table>`
          : `<div class="muted">该区间没有客供料发生额</div>`;
      } catch (e) { toast(e.message, "err"); }
    };
  };

  const openProgress = () => {
    const m = modal(`<h3>销售订单交货进度</h3>
      <div class="field"><label>客户 *</label><input id="cs-sop-cust" value="${esc(cust())}" /></div>
      <div id="cs-sop-out" class="muted">点「算进度」</div>
      <div class="foot"><button class="btn primary" id="cs-sop-run">算进度</button><button class="btn ghost" id="cs-sop-close">关闭</button></div>`, true);
    $("#cs-sop-close", m).onclick = closeModal;
    $("#cs-sop-run", m).onclick = async () => {
      const c = $("#cs-sop-cust", m).value.trim();
      if (!c) { toast("交货进度必须指定客户", "err"); return; }
      try {
        const r = await api(`/consign/so-progress?customer=${encodeURIComponent(c)}`);
        const rows = r.rows || [];
        $("#cs-sop-out", m).innerHTML = rows.length
          ? `<table class="grid"><thead><tr><th>订单号</th><th>日期</th><th>物料</th><th>名称</th><th>订购</th><th>已交</th><th>未交</th></tr></thead><tbody>${rows.map((x) => `<tr>
              <td>${esc(x.so_no)}</td><td>${esc(x.date || "")}</td><td>${esc(x.item)}</td><td>${esc(x.item_name || "")}</td>
              <td class="num">${fmt(x.ordered)}</td><td class="num">${fmt(x.delivered)}</td>
              <td class="num"${num(x.remain) > 0 ? ' style="color:var(--err)"' : ""}>${fmt(x.remain)}</td></tr>`).join("")}</tbody></table>
              <div class="muted" style="font-size:12px">已交只算交货单，退料 / 损耗不抵成品交付</div>`
          : `<div class="muted">该客户没有销售订单</div>`;
      } catch (e) { toast(e.message, "err"); }
    };
  };

  $("#cs-sop", main).onclick = openProgress;
  $("#cs-load", main).onclick = loadDocs;
  $("#cs-bal", main).onclick = loadBal;
  $("#cs-stmt", main).onclick = openStatement;
  if ($("#cs-new", main)) $("#cs-new").onclick = openEditor;
  loadDocs();
}

// ===========================================================================
// 加工费单价（单价矩阵 + 折算系数）
// ===========================================================================
// 金额进输入框：去掉多余的小数零，7.0200 → 7.02，0.00 → 0
function priceIn(v) {
  return String(v == null ? "" : v)
    .replace(/(\.\d*?)0+$/, "$1")
    .replace(/\.$/, "");
}

/// 单价矩阵里的一格
///
/// 两条边界，错了就会报出一个不存在的价：
/// ① 没有折算系数 → 这一格根本不能报价（不是「0 元」），只给一句「没设系数」；
/// ② 有系数但没定价 → 输入框空着写「未定价」，不能预填 0，否则一路确下去就是白送。
function priceCellHtml(row, c, canEdit) {
  // 通配行（「任意物料（通配价）」）与具体物料两回事：
  // 通配行本来就没有折算系数 —— 它说的是「这个客户按 ㎡ 一律 12 元」，
  // 跟「某个物料 1 只折合多少 ㎡」无关。
  const wild = !row.item_code;
  // 具体物料没设系数：价照旧要显示（客户发来的价目表导进来就是这种情况），
  // 只是下面多一行小字提醒「按这个口径开单前还要先录系数」。
  // 早先这里是「没设系数」的纯文字格，把已录好的价一起藏了 —— 导入一张价表
  // 之后满屏「没设系数」，用户会以为没导进去。
  const noFactor = !wild && c.factor == null;
  return `<td style="white-space:nowrap">
      <input class="pr-cell" style="width:64px;text-align:right" data-item="${esc(row.item_code)}" data-unit="${esc(c.unit)}"
        value="${c.rule_id ? priceIn(c.unit_price) : ""}" placeholder="未定价" ${canEdit ? "" : "disabled"} />
      ${canEdit ? `<button class="btn ghost sm" style="padding:0 4px" data-cell-edit="${esc(row.item_code)}|${esc(c.unit)}" title="改起步价 / 提前录下一版价">调</button>` : ""}
      ${c.future ? `<div style="font-size:10px;color:var(--warn)">${esc(c.effective_from || "")} 起</div>` : ""}
      ${num(c.min_charge) > 0 ? `<div style="font-size:10px" class="muted">起步 ${fmt(c.min_charge)}</div>` : ""}
      ${noFactor ? `<div class="muted" style="font-size:10px${canEdit ? ";cursor:pointer" : ""}"${canEdit ? ` data-cell-none="${esc(row.item_code)}|${esc(c.unit)}"` : ""} title="按${esc(c.unit)}开单前，要先告诉系统 1 个库存单位折合多少 ${esc(c.unit)}">未设系数 · 点这里录</div>` : ""}
    </td>`;
}

// 导入结果说人话。抽成顶层函数是为了让 tools/check-app-render.js 能真调用它 ——
// 这段文案是用户判断「到底导进去没有」的唯一依据，写错了没人看得见。
function importResultHtml(r, hint) {
  const created = Number(r.created) || 0;
  const updated = Number(r.updated) || 0;
  const skipped = Number(r.skipped) || 0;
  const warns = Array.isArray(r.warnings) ? r.warnings : [];
  if (created + updated + skipped === 0) {
    return `<div style="color:var(--warn)">一行都没读到 —— 是不是粘错了地方？点「下载模板」照着填最省事。</div>`;
  }
  const parts = [];
  if (created) parts.push(`新增 <b>${created}</b> 条`);
  if (updated) parts.push(`覆盖 <b>${updated}</b> 条`);
  if (skipped) parts.push(`跳过 <b>${skipped}</b> 行`);
  const head = `<div style="color:${skipped ? "var(--warn)" : "var(--ok)"}">${parts.join("、")}</div>`;
  // 导进来的价可能挂在客户名下（客户发来的价目表就是按客户报的）。
  // 此时矩阵还停在「全部客户」，格子会显示「未定价」—— 用户会以为没导进去，
  // 所以在结果里提一句「右上角填客户编码」。
  const hintHtml = hint ? `<div class="muted" style="font-size:12px;margin-top:4px">${esc(hint)}</div>` : "";
  if (!warns.length) return head + hintHtml;
  // 只列前 10 条：一次导 500 行时把每一行都摊出来，用户反而找不到重点
  const shown = warns.slice(0, 10).map((w) => `<li>${esc(w)}</li>`).join("");
  const more = warns.length > 10 ? `<li>…… 还有 ${warns.length - 10} 行没列出来</li>` : "";
  return `${head}<ul style="margin:6px 0 0 18px;padding:0;font-size:12px;line-height:1.6">${shown}${more}</ul>${hintHtml}`;
}

async function viewPriceRules(main) {
  main.innerHTML = `<h2>加工费规则</h2>
    <div class="toolbar">
      <label>客户 <input id="pr-cust" style="width:120px" placeholder="留空 = 通用价" /></label>
      <button class="btn primary sm" id="pr-load">看这个客户的价</button>
      ${can("price_edit") ? `<button class="btn ghost sm" id="pr-import">批量导入</button>
      <button class="btn ghost sm" id="pr-new-unit">录折算系数</button>
      <button class="btn ghost sm" id="pr-new">新增规则（高级）</button>` : ""}
      <button class="btn ghost sm" id="pr-clear">清空客户</button>
    </div>
    <div class="muted" style="font-size:12px;margin:-4px 0 10px">
      一行一个工件，一列一种算法 —— 按只、按米、按 kg、按 ㎡ 各自一个价。格子里直接改单价，改完即生效。
      「调」里能改起步价，也能把下一版价提前录进去（写 2026-03-01，到那天自动换过来，旧单仍按旧价）。
      格子里写「没设系数」的，是还没告诉系统「1 只折合多少 ㎡」—— 先录折算系数，开单时填只数就能自动折算。
    </div>
    <div class="panel"><h3 style="margin-top:0;font-size:14px">单价矩阵</h3>
      <div id="pr-matrix" class="muted">加载中…</div></div>
    <div class="panel"><h3 style="margin-top:0;font-size:14px">折算系数（1 个库存单位 = 多少计价单位）</h3>
      <div id="pr-units" class="muted">加载中…</div></div>
    <div class="panel"><h3 style="margin-top:0;font-size:14px">试算报价</h3>
      <div class="toolbar">
        <label>物料 <input id="pr-q-item" style="width:110px" /></label>
        <label>数量 <input id="pr-q-qty" style="width:80px" value="1" /></label>
        <label>口径 <select id="pr-q-unit" style="width:130px"></select></label>
        <label style="display:inline-flex;gap:4px;align-items:center"><input type="checkbox" id="pr-q-rush" /> 加急</label>
        <button class="btn primary sm" id="pr-q-run">试算</button>
      </div>
      <div class="muted" style="font-size:12px">口径留「（不折算）」时，数量按计价单位直接算（老算法）。</div>
      <div id="pr-q-out" class="muted">填好物料和数量后试算</div>
    </div>`;

  let mx = { customer: "", units: [], rows: [] };
  let unitsAll = [];

  const loadMatrix = async () => {
    const cust = $("#pr-cust", main).value.trim();
    try {
      const [m, u] = await Promise.all([
        api(`/price-matrix?customer=${encodeURIComponent(cust)}`),
        api("/calc-units"),
      ]);
      mx = m || { customer: cust, units: [], rows: [] };
      unitsAll = u.units || [];
      renderMatrix();
      renderUnits();
      renderUnitSelect();
    } catch (e) {
      $("#pr-matrix", main).innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`;
    }
  };

  const renderMatrix = () => {
    const box = $("#pr-matrix", main);
    if (!mx.rows || !mx.rows.length) {
      box.innerHTML = `<div class="muted">这个客户名下还没有任何单价 —— 点右上「录折算系数」给物料挂一个口径，矩阵里就会出现它那一行</div>`;
      return;
    }
    const head = mx.units.map((u) => `<th class="num">按${esc(u)}</th>`).join("");
    const body = mx.rows
      .map((row) => {
        const label = row.item_code
          ? `${esc(row.item_code)} <span class="muted" style="font-size:11px">${esc(row.item_name || "")}${row.stock_unit ? " · 库存单位 " + esc(row.stock_unit) : ""}</span>`
          : `<span class="muted">${esc(row.item_name)}</span>`;
        return `<tr><td>${label}</td>${row.cells.map((c) => priceCellHtml(row, c, can("price_edit"))).join("")}</tr>`;
      })
      .join("");
    box.innerHTML = `<table class="grid"><thead><tr><th>物料</th>${head}</tr></thead><tbody>${body}</tbody></table>`;

    $all(".pr-cell", main).forEach((el) => {
      el.onchange = async () => {
        const v = el.value.trim();
        if (!v) return;
        await putCell(el.dataset.item, el.dataset.unit, { unit_price: v });
      };
    });
    $all("[data-cell-edit]", main).forEach((b) => {
      b.onclick = () => {
        const [item, unit] = b.dataset.cellEdit.split("|");
        const row = mx.rows.find((r) => r.item_code === item);
        const c = row && row.cells.find((x) => x.unit === unit);
        if (c) openCell(row, c);
      };
    });
    $all("[data-cell-none]", main).forEach((td) => {
      td.onclick = () => {
        const [item, unit] = td.dataset.cellNone.split("|");
        openUnit({ item_code: item, calc_unit: unit, scale: 2 });
      };
    });
  };

  const putCell = async (item, unit, extra) => {
    const row = mx.rows.find((r) => r.item_code === item);
    const c = row && row.cells.find((x) => x.unit === unit);
    const body = Object.assign(
      { customer_code: mx.customer || "", item_code: item, calc_unit: unit, unit_price: "", min_charge: "0", effective_from: "" },
      c ? { unit_price: c.unit_price, min_charge: c.min_charge, effective_from: c.effective_from || "" } : {},
      extra,
    );
    try {
      await api("/price-matrix", { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
      toast("已保存", "ok");
      loadMatrix();
    } catch (e) {
      toast(e.message, "err");
    }
  };

  const openCell = (row, c) => {
    const m = modal(`<h3>${esc(row.item_code || "任意物料")} 按${esc(c.unit)}</h3>
      <div class="field"><label>单价（${esc(c.unit)}）</label><input id="pc-price" value="${c.rule_id ? priceIn(c.unit_price) : ""}" placeholder="未定价" /></div>
      <div class="field"><label>起步价（不足补到）</label><input id="pc-min" value="${priceIn(c.min_charge)}" /></div>
      <div class="field"><label>生效日期（留空 = 立刻；提前录下一版价写 2026-03-01）</label><input id="pc-from" value="${esc(c.effective_from || "")}" placeholder="YYYY-MM-DD" /></div>
      <div class="muted" style="font-size:12px">1 ${esc(row.stock_unit || "库存单位")} = ${priceIn(c.factor)} ${esc(c.unit)}（在下面的折算系数里改）</div>
      <div class="foot"><button class="btn primary" id="pc-save">保存</button><button class="btn ghost" id="pc-cancel">取消</button></div>`);
    $("#pc-cancel", m).onclick = closeModal;
    $("#pc-save", m).onclick = async () => {
      await putCell(row.item_code, c.unit, {
        unit_price: $("#pc-price", m).value.trim(),
        min_charge: $("#pc-min", m).value.trim() || "0",
        effective_from: $("#pc-from", m).value.trim(),
      });
      closeModal();
    };
  };

  const renderUnits = () => {
    const box = $("#pr-units", main);
    box.innerHTML = unitsAll.length
      ? `<table class="grid"><thead><tr><th>物料</th><th>口径</th><th>折算</th><th>保留位</th><th>状态</th><th></th></tr></thead><tbody>${unitsAll
          .map(
            (u) => `<tr><td>${esc(u.item_code)}</td><td>${esc(u.calc_unit)}</td>
            <td class="num">1 ${esc(u.item_code)} = ${priceIn(u.factor)} ${esc(u.calc_unit)}</td>
            <td class="num">${u.scale}</td>
            <td>${u.disabled ? '<span class="tag err">停用</span>' : '<span class="tag ok">启用</span>'}</td>
            <td class="row-actions">${can("price_edit") ? `<button class="btn ghost sm" data-unit-edit="${u.id}">改</button><button class="btn danger sm" data-unit-del="${u.id}">删</button>` : ""}</td></tr>`,
          )
          .join("")}</tbody></table>`
      : `<div class="muted">还没有折算系数。一个物料可以挂好几个口径（只 / 米 / kg / ㎡），每个口径一条。</div>`;
    $all("[data-unit-edit]", main).forEach((b) => {
      b.onclick = () => openUnit(unitsAll.find((u) => String(u.id) === b.dataset.unitEdit));
    });
    $all("[data-unit-del]", main).forEach((b) => {
      b.onclick = async () => {
        if (!(await confirmDialog("删除这条折算系数？删掉后这个口径就不能报价了", true))) return;
        try {
          await api(`/calc-units/${b.dataset.unitDel}`, { method: "DELETE" });
          toast("已删除", "ok");
          loadMatrix();
        } catch (e) {
          toast(e.message, "err");
        }
      };
    });
  };

  const openUnit = (u) => {
    const c = u || { id: 0, item_code: "", calc_unit: "", factor: "0", scale: 2, disabled: false, memo: "" };
    const m = modal(`<h3>${u && u.id ? "修改" : "新增"}折算系数</h3>
      <div class="field"><label>物料编码 *</label><input id="pu-item" value="${esc(c.item_code)}" placeholder="MB01" /></div>
      <div class="field"><label>计价口径 *</label><input id="pu-unit" value="${esc(c.calc_unit)}" placeholder="㎡ / 米 / kg / 只" /></div>
      <div class="field"><label>1 个库存单位 = 多少计价单位 *</label><input id="pu-factor" value="${priceIn(c.factor)}" placeholder="0.0234" /></div>
      <div class="field"><label>折算后保留几位小数（0-4）</label><input id="pu-scale" value="${c.scale}" /></div>
      <div class="field"><label>备注</label><input id="pu-memo" value="${esc(c.memo || "")}" /></div>
      <div class="muted" style="font-size:12px">例：300 只工件每只 0.0234 ㎡，就填「㎡ / 0.0234 / 2」—— 开单填 300 只，自动算成 7.02 ㎡。</div>
      <div class="foot"><button class="btn primary" id="pu-save">保存</button><button class="btn ghost" id="pu-cancel">取消</button></div>`);
    $("#pu-cancel", m).onclick = closeModal;
    $("#pu-save", m).onclick = async () => {
      const body = {
        id: c.id || 0,
        item_code: $("#pu-item", m).value.trim(),
        calc_unit: $("#pu-unit", m).value.trim(),
        factor: $("#pu-factor", m).value.trim(),
        scale: Number($("#pu-scale", m).value.trim() || 2),
        disabled: !!c.disabled,
        memo: $("#pu-memo", m).value.trim(),
      };
      if (!body.item_code || !body.calc_unit) {
        toast("物料和口径都要填", "err");
        return;
      }
      try {
        await api("/calc-units", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
        toast("已保存", "ok");
        closeModal();
        loadMatrix();
      } catch (e) {
        toast(e.message, "err");
      }
    };
  };

  const renderUnitSelect = () => {
    const sel = $("#pr-q-unit", main);
    const keep = sel.value;
    sel.innerHTML = `<option value="">（不折算，按计价数量）</option>${mx.units.map((u) => `<option value="${esc(u)}">${esc(u)}</option>`).join("")}`;
    if (keep) sel.value = keep;
  };

  const openRule = (r) => {
    const c = r || { id: 0, name: "", item_code: "", customer_code: "", unit: "件", unit_price: "0", min_charge: "0", rush_fee: "0", rush_rate: "0", disabled: false, memo: "", effective_from: "" };
    const m = modal(`<h3>${r ? "编辑" : "新增"}加工费规则（高级）</h3>
      <div class="muted" style="font-size:12px">日常改价直接用矩阵就行；这里是给「一条规则管很多物料」或者设加急费的场合。</div>
      <div class="field"><label>名称 *</label><input id="pr-name" value="${esc(c.name)}" /></div>
      <div class="field"><label>物料（留空=任意）</label><input id="pr-item" value="${esc(c.item_code)}" /></div>
      <div class="field"><label>客户（留空=任意）</label><input id="pr-cust2" value="${esc(c.customer_code)}" /></div>
      <div class="field"><label>计价口径</label><input id="pr-unit" value="${esc(c.unit)}" placeholder="㎡ / 件 / kg / 只" /></div>
      <div class="field"><label>单价 *</label><input id="pr-price" value="${esc(c.unit_price)}" /></div>
      <div class="field"><label>起步价（最低收费）</label><input id="pr-min" value="${esc(c.min_charge)}" /></div>
      <div class="field"><label>加急固定费</label><input id="pr-rfee" value="${esc(c.rush_fee)}" /></div>
      <div class="field"><label>加急加价率 %</label><input id="pr-rrate" value="${esc(c.rush_rate)}" /></div>
      <div class="field"><label>生效日期（留空 = 立刻）</label><input id="pr-from" value="${esc(c.effective_from || "")}" placeholder="YYYY-MM-DD" /></div>
      <div class="field"><label style="display:inline-flex;gap:4px;align-items:center"><input type="checkbox" id="pr-dis" ${c.disabled ? "checked" : ""}/> 停用</label></div>
      <div class="field"><label>备注</label><input id="pr-memo" value="${esc(c.memo || "")}" /></div>
      <div class="foot"><button class="btn primary" id="pr-save">保存</button><button class="btn ghost" id="pr-cancel2">取消</button></div>`);
    $("#pr-cancel2", m).onclick = closeModal;
    $("#pr-save", m).onclick = async () => {
      const body = {
        id: c.id || 0,
        name: $("#pr-name", m).value.trim(),
        item_code: $("#pr-item", m).value.trim(),
        customer_code: $("#pr-cust2", m).value.trim(),
        unit: $("#pr-unit", m).value.trim(),
        unit_price: $("#pr-price", m).value.trim(),
        min_charge: $("#pr-min", m).value.trim(),
        rush_fee: $("#pr-rfee", m).value.trim(),
        rush_rate: $("#pr-rrate", m).value.trim(),
        disabled: $("#pr-dis", m).checked,
        memo: $("#pr-memo", m).value.trim(),
        effective_from: $("#pr-from", m).value.trim(),
      };
      if (!body.name) {
        toast("名称必填", "err");
        return;
      }
      try {
        await api("/price-rules", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
        toast("已保存", "ok");
        closeModal();
        loadMatrix();
      } catch (e) {
        toast(e.message, "err");
      }
    };
  };

  $("#pr-load", main).onclick = loadMatrix;
  $("#pr-clear", main).onclick = () => {
    $("#pr-cust", main).value = "";
    loadMatrix();
  };
  // 批量导入：客户发来一张价目表，整张导进来 —— 这是「高效管理单价」真正的省力点，
  // 一格一格点四十个客户 × 十几个工件，没人干得下去。
  const openImport = () => {
    const m = modal(`<h3>批量导入</h3>
      <div class="muted" style="font-size:12px;line-height:1.7">
        从 Excel 里直接复制整张表粘进来，或者选文件。表头行会自动跳过，<code>#</code> 开头的行当注释。
        同一「物料 + 客户 + 口径 + 生效日」会<b>覆盖</b>原来那一格，不会变成两条；换了生效日就是另一版价。
      </div>
      <div class="field"><label>导入什么</label>
        <select id="pi-kind">
          <option value="rules">单价（物料 × 客户 × 口径 → 元）</option>
          <option value="units">折算系数（1 只 = 多少 ㎡ / 1 米 = 多少 kg）</option>
        </select>
        <a id="pi-tmpl" class="btn ghost sm" style="margin-left:8px" href="${API}/price-import/template?kind=rules" download="加工费单价-模板.csv">下载模板</a>
      </div>
      <div class="field"><label>粘贴内容</label>
        <textarea id="pi-text" rows="9" style="width:100%;font-family:ui-monospace,Menlo,monospace;font-size:12px"
          placeholder="物料编码,客户编码,计价口径,单价,起步价,加急固定费,加急率%,生效日期,名称,备注"></textarea>
      </div>
      <div class="field"><label>或者选文件（.xlsx / .csv，会覆盖上面粘的内容）</label><input id="pi-file" type="file" accept=".xlsx,.csv,.txt" /></div>
      <div id="pi-out" style="margin:6px 0"></div>
      <div class="foot"><button class="btn primary" id="pi-run">开始导入</button><button class="btn ghost" id="pi-cancel">关闭</button></div>`, true);
    const kindSel = $("#pi-kind", m);
    kindSel.onchange = () => {
      $("#pi-tmpl", m).href = `${API}/price-import/template?kind=${kindSel.value}`;
      $("#pi-tmpl", m).download = kindSel.value === "units" ? "折算系数-模板.csv" : "加工费单价-模板.csv";
      $("#pi-text", m).placeholder = kindSel.value === "units"
        ? "物料编码,计价口径,系数,保留位数"
        : "物料编码,客户编码,计价口径,单价,起步价,加急固定费,加急率%,生效日期,名称,备注";
    };
    $("#pi-cancel", m).onclick = closeModal;
    // Excel 文件 → base64（后端用 rust_xlsxwriter 那边的读法解）
    const asB64 = (file) => new Promise((res, rej) => {
      const rd = new FileReader();
      rd.onload = () => res(String(rd.result || "").split(",").slice(1).join(","));
      rd.onerror = () => rej(new Error("读不了这个文件，换一个试试"));
      rd.readAsDataURL(file);
    });
    $("#pi-run", m).onclick = async () => {
      const kind = kindSel.value;
      const out = $("#pi-out", m);
      const btn = $("#pi-run", m);
      const text = $("#pi-text", m).value;
      const fileEl = $("#pi-file", m);
      const f = fileEl.files && fileEl.files[0];
      if (!text.trim() && !f) {
        out.innerHTML = importResultHtml({ created: 0, updated: 0, skipped: 0, warnings: [] });
        return;
      }
      btn.disabled = true;
      btn.textContent = "导入中…";
      try {
        let body;
        if (f && /\.xlsx$/i.test(f.name)) {
          body = { file: await asB64(f) };
        } else if (f) {
          body = { text: await f.text() };
        } else {
          body = { text };
        }
        const r = await api(kind === "units" ? "/calc-units/import" : "/price-rules/import", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(body),
        });
        const noCust = !$("#pr-cust", main).value.trim();
        out.innerHTML = importResultHtml(
          r,
          noCust && r.created + r.updated > 0
            ? "上面的价如果挂在客户名下，右上角「客户编码」填上客户再看，才知道这个客户卖多少。"
            : "",
        );
        if (r.created + r.updated > 0) {
          toast(`已导入 ${r.created + r.updated} 条`, "ok");
          loadMatrix();
        }
      } catch (e) {
        out.innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`;
      } finally {
        btn.disabled = false;
        btn.textContent = "开始导入";
      }
    };
  };

  if ($("#pr-import", main)) $("#pr-import", main).onclick = openImport;
  if ($("#pr-new", main)) $("#pr-new", main).onclick = () => openRule(null);
  if ($("#pr-new-unit", main)) $("#pr-new-unit", main).onclick = () => openUnit(null);

  $("#pr-q-run", main).onclick = async () => {
    const item = $("#pr-q-item", main).value.trim();
    const qty = $("#pr-q-qty", main).value.trim();
    const unit = $("#pr-q-unit", main).value.trim();
    const rush = $("#pr-q-rush", main).checked ? "1" : "0";
    const cust = $("#pr-cust", main).value.trim();
    const q = unit
      ? `/price-rules/quote?item=${encodeURIComponent(item)}&customer=${encodeURIComponent(cust)}&stock_qty=${encodeURIComponent(qty)}&calc_unit=${encodeURIComponent(unit)}&rush=${rush}`
      : `/price-rules/quote?item=${encodeURIComponent(item)}&customer=${encodeURIComponent(cust)}&qty=${encodeURIComponent(qty)}&rush=${rush}`;
    try {
      const r = await api(q);
      const x = r.quote;
      const conv = r.conv
        ? `<tr><td>折算</td><td class="num">${fmt(r.conv.stock_qty)} ${esc(r.conv.stock_unit || "")} × ${priceIn(r.conv.factor)} = ${fmt(r.conv.calc_qty)} ${esc(r.conv.calc_unit)}</td></tr>`
        : "";
      const fut = x.future
        ? `<tr><td colspan="2" style="color:var(--warn)">注意：这条价 ${esc(x.effective_from)} 才生效，现在报的是提前录的下一版价。</td></tr>`
        : "";
      $("#pr-q-out", main).innerHTML = `<table class="grid"><tbody>${conv}
        <tr><td>规则</td><td>${esc(x.rule_name)}</td></tr>
        <tr><td>数量 × 单价</td><td class="num">${fmt(x.qty)} ${esc(x.unit)} × ${fmt(x.unit_price)} = ${fmt(x.base)}</td></tr>
        <tr><td>起步价补差</td><td class="num">${fmt(x.floor_extra)}（起步 ${fmt(x.min_charge)}）</td></tr>
        <tr><td>加急加价</td><td class="num">${fmt(x.rush_extra)}（固定 ${fmt(x.rush_fee)} + ${fmt(x.rush_rate)}%）</td></tr>
        ${fut}<tr><td><b>合计</b></td><td class="num"><b>${fmt(x.total)}</b></td></tr></tbody></table>`;
    } catch (e) {
      $("#pr-q-out", main).innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`;
    }
  };
  loadMatrix();
}

// ===========================================================================
// 计件工资（工序报工完工量 × 计件单价，按期间汇总后回填工资表）
// ===========================================================================
async function viewPiecePay(main) {
  main.innerHTML = `<h2>计件工资</h2>
    <div class="toolbar">
      <label>期间 <input id="pp-period" style="width:110px" placeholder="2026-11" value="${esc(state.current || "")}" /></label>
      <button class="btn primary sm" id="pp-run">算计件</button>
      ${can("voucher_new") ? `<button class="btn primary sm" id="pp-fill">回填工资表</button>` : ""}
      <button class="btn ghost sm" id="pp-print">打印预览</button>
      <span class="muted" style="font-size:12px">数量来自工序报工流水（须先在「工序报工」里派工并报工）；回填只把应发提到不低于计件额，不重复累加</span>
    </div>
    <div class="panel"><h3 style="margin-top:0;font-size:14px">本期计件</h3><div id="pp-out" class="muted">点「算计件」</div></div>
    <div class="panel"><h3 style="margin-top:0;font-size:14px">计件单价</h3>
      <div class="toolbar">${can("production_ops") ? `<button class="btn primary sm" id="pp-new">新增单价</button>` : ""}</div>
      <div id="pp-rates" class="muted">加载中…</div></div>`;
  let rates = [];
  const period = () => $("#pp-period", main).value.trim();
  const loadRates = async () => {
    try {
      const r = await api("/piece-rates");
      rates = r.rates || [];
      $("#pp-rates", main).innerHTML = rates.length
        ? `<table class="grid"><thead><tr><th>版本</th><th>物料</th><th>工序</th><th>单价</th><th>状态</th><th></th></tr></thead><tbody>${rates.map((x) => `<tr>
            <td>${esc(x.version || "不限")}</td><td>${esc(x.item_code || "任意")}</td><td>${esc(x.op_name || "任意")}</td><td class="num">${fmt(x.rate)}</td>
            <td>${x.disabled ? '<span class="tag err">停用</span>' : '<span class="tag ok">启用</span>'}</td>
            <td class="row-actions">${can("production_ops") ? `<button class="btn danger sm" data-pp-del="${x.id}">删</button>` : ""}</td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">还没有计件单价</div>`;
      $all("[data-pp-del]", main).forEach((b) => (b.onclick = async () => {
        if (!(await confirmDialog("删除这条计件单价？已发生的人工不会自动重算，重算后再回填即可。", true))) return;
        try { await api(`/piece-rates/${b.dataset.ppDel}`, { method: "DELETE" }); toast("已删除", "ok"); loadRates(); }
        catch (e) { toast(e.message, "err"); }
      }));
    } catch (e) { $("#pp-rates", main).innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  const run = async () => {
    try {
      const r = await api(`/payroll/piece?period=${encodeURIComponent(period())}`);
      const rows = r.rows || [];
      $("#pp-out", main).innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>员工</th><th>数量</th><th>计件金额</th><th>明细</th></tr></thead><tbody>${rows.map((p) => `<tr>
            <td>${esc(p.employee)}</td><td class="num">${fmt(p.qty)}</td><td class="num">${fmt(p.amount)}</td>
            <td class="muted" style="font-size:12px">${(p.lines || []).map((l) => `${esc(l.item_code)}/${esc(l.op_name)}${l.version ? "@" + esc(l.version) : ""} ${fmt(l.qty)}×${fmt(l.rate)}${l.matched ? "" : "（无单价）"}`).join("；")}</td></tr>`).join("")}
          </tbody><tfoot><tr><td><b>合计</b></td><td></td><td class="num"><b>${fmt(r.total)}</b></td><td></td></tr></tfoot></table>`
        : `<div class="muted">本期没有报工流水（先派工、再报工）</div>`;
    } catch (e) { $("#pp-out", main).innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  $("#pp-run", main).onclick = run;
  $("#pp-print", main).onclick = () => { const el = $("#pp-out", main).querySelector("table"); if (!el) { toast("先算出数据再打印", "err"); return; } printPreview("计件工资", el); };
  if ($("#pp-new", main)) $("#pp-new", main).onclick = () => {
    const m = modal(`<h3>新增计件单价</h3>
      <div class="field"><label>工艺版本（留空=不限）</label><input id="ppr-ver" placeholder="V1 / V2" /></div>
      <div class="field"><label>物料（留空=任意）</label><input id="ppr-item" /></div>
      <div class="field"><label>工序（留空=任意）</label><input id="ppr-op" placeholder="氧化 / 包装" /></div>
      <div class="field"><label>单价 *</label><input id="ppr-rate" value="0" /></div>
      <div class="field"><label>备注</label><input id="ppr-memo" /></div>
      <div class="foot"><button class="btn primary" id="ppr-save">保存</button><button class="btn ghost" id="ppr-cancel">取消</button></div>`);
    $("#ppr-cancel", m).onclick = closeModal;
    $("#ppr-save", m).onclick = async () => {
      try {
        await api("/piece-rates", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({
          version: $("#ppr-ver", m).value.trim(),
          item_code: $("#ppr-item", m).value.trim(), op_name: $("#ppr-op", m).value.trim(),
          rate: $("#ppr-rate", m).value.trim(), memo: $("#ppr-memo", m).value.trim(),
        }) });
        toast("已保存", "ok"); closeModal(); loadRates();
      } catch (e) { toast(e.message, "err"); }
    };
  };
  if ($("#pp-fill", main)) $("#pp-fill", main).onclick = async () => {
    if (!(await confirmDialog(`把 ${period()} 的计件金额回填进工资表（应发取「不低于计件额」）？`, false))) return;
    try {
      const r = await api(`/payroll/piece-fill?period=${encodeURIComponent(period())}`, { method: "POST", headers: { "Content-Type": "application/json" }, body: "{}" });
      const f = r.filled || [];
      const s = r.skipped || [];
      toast(f.length ? `已回填 ${f.length} 人，跳过 ${s.length} 人` : `没有需要回填的人（跳过 ${s.length}）`, f.length ? "ok" : "warn");
      run();
    } catch (e) { toast(e.message, "err"); }
  };
  loadRates();
  run();
}

// ===========================================================================
// 挂具台账（氧化厂的挂具 / 挂篮 / 夹具：只记数量与使用次数，不进存货、不出凭证）
// ===========================================================================
async function viewToolLedger(main) {
  main.innerHTML = `<h2>挂具台账</h2>
    <div class="toolbar">
      <label>挂具 <input id="tl-item" style="width:120px" placeholder="编号，留空 = 全部" /></label>
      <button class="btn primary sm" id="tl-reload">查询</button>
      ${can("production_ops") ? `<button class="btn primary sm" id="tl-new-move">登记收发</button>
      <button class="btn ghost sm" id="tl-new-item">新增档案</button>` : ""}
      <button class="btn ghost sm" id="tl-print">打印</button>
    </div>
    <div class="panel"><h3 style="margin-top:0;font-size:14px">该换新了（额定次数已用完）</h3><div id="tl-warn" class="muted">加载中…</div></div>
    <div class="panel"><h3 style="margin-top:0;font-size:14px">结存（在库 / 在车间 / 累计报废 / 使用次数）</h3><div id="tl-table" class="muted">加载中…</div></div>
    <div class="panel"><h3 style="margin-top:0;font-size:14px">收发流水</h3><div id="tl-moves" class="muted">加载中…</div></div>
    <div class="muted" style="font-size:12px">领用 = 出库进车间；归还 = 回库并记使用次数（按套数折算，可改）；报废只从在库扣 —— 坏挂具先「归还」入库再报废</div>`;

  let kinds = [];
  let items = [];
  const code = () => $("#tl-item", main).value.trim();
  const kindOpts = (sel) =>
    kinds.map((k) => `<option value="${esc(k.code)}" ${k.code === sel ? "selected" : ""}>${esc(k.label)}</option>`).join("");
  const itemOpts = () =>
    items.map((i) => `<option value="${esc(i.code)}">${esc(i.code)} ${esc(i.name || "")}</option>`).join("");

  const loadBal = async () => {
    try {
      const r = await api("/tools");
      items = r.items || [];
      kinds = r.kinds || [];
      const rows = r.rows || [];
      const warn = rows.filter((x) => x.life_times > 0 && (x.used_times || 0) >= x.life_times);
      $("#tl-warn", main).innerHTML = warn.length
        ? `<table class="grid"><thead><tr><th>挂具</th><th>名称</th><th>规格</th><th>已用次数</th><th>额次数</th><th>在库</th></tr></thead><tbody>${warn.map((x) => `<tr>
            <td>${esc(x.tool_code)}</td><td>${esc(x.name || "")}</td><td>${esc(x.spec || "")}</td>
            <td class="num">${x.used_times || 0}</td><td class="num">${x.life_times}</td><td class="num">${fmt(x.in_stock)}</td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">没有需要换新的挂具</div>`;
      $("#tl-table", main).innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>挂具</th><th>名称</th><th>规格</th><th>单位</th><th>在库</th><th>在车间</th><th>累计报废</th><th>使用次数</th><th>额定</th><th>状态</th><th></th></tr></thead><tbody>${rows.map((x) => {
            const over = x.life_times > 0 && (x.used_times || 0) >= x.life_times;
            const life = x.life_times > 0 ? String(x.life_times) : "不限";
            return `<tr><td>${esc(x.tool_code)}</td><td>${esc(x.name || "")}</td><td>${esc(x.spec || "")}</td>
              <td>${esc(x.unit || "")}</td><td class="num">${fmt(x.in_stock)}</td><td class="num">${fmt(x.in_use)}</td>
              <td class="num">${fmt(x.scraped)}</td><td class="num">${x.used_times || 0}</td><td class="num">${life}</td>
              <td>${x.disabled ? `<span class="tag">停用</span>` : over ? `<span class="tag" style="color:var(--warn)">该换新</span>` : ""}</td>
              <td class="row-actions">${can("production_ops") ? `<button class="btn ghost sm" data-tl-del="${esc(x.tool_code)}">删档</button>` : ""}</td></tr>`;
          }).join("")}</tbody></table>`
        : `<div class="muted">还没有挂具档案，先「新增档案」</div>`;
      $all("[data-tl-del]", main).forEach((b) => (b.onclick = async () => {
        const c = b.dataset.tlDel;
        if (!(await confirmDialog(`删除挂具档案 ${c}？有收发记录的会被拦下。`, true))) return;
        try { await api(`/tools/items/${encodeURIComponent(c)}`, { method: "DELETE" }); toast("已删除", "ok"); loadBal(); }
        catch (e) { toast(e.message, "err"); }
      }));
    } catch (e) { $("#tl-table", main).innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };

  const loadMoves = async () => {
    try {
      const r = await api(`/tools/moves${code() ? `?tool=${encodeURIComponent(code())}` : ""}`);
      kinds = r.kinds || kinds;
      const rows = r.moves || [];
      $("#tl-moves", main).innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>单号</th><th>日期</th><th>类型</th><th>挂具</th><th>客户</th><th>数量</th><th>次数</th><th>备注</th><th></th></tr></thead><tbody>${rows.map((x) => `<tr>
            <td>${esc(x.no)}</td><td>${esc(x.date)}</td><td><span class="tag">${esc(x.kind_label || x.kind)}</span></td>
            <td>${esc(x.tool_code)}</td><td>${esc(x.customer || "—")}</td><td class="num">${fmt(x.qty)}</td>
            <td class="num">${x.times || 0}</td><td>${esc(x.memo || "")}</td>
            <td class="row-actions">${can("production_ops") ? `<button class="btn ghost sm" data-tl-mdel="${x.id}">删</button>` : ""}</td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">没有收发记录</div>`;
      $all("[data-tl-mdel]", main).forEach((b) => (b.onclick = async () => {
        if (!(await confirmDialog("删除这条收发记录？结存会跟着变。", true))) return;
        try { await api(`/tools/moves/${b.dataset.tlMdel}`, { method: "DELETE" }); toast("已删除", "ok"); loadBal(); loadMoves(); }
        catch (e) { toast(e.message, "err"); }
      }));
    } catch (e) { $("#tl-moves", main).innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };

  const openMove = () => {
    const m = modal(`<h3>登记挂具收发</h3>
      <div class="field"><label>挂具 *</label><select id="tlm-code">${itemOpts()}</select></div>
      <div class="field"><label>类型 *</label><select id="tlm-kind">${kindOpts("issue")}</select></div>
      <div class="field"><label>日期 *</label><input id="tlm-date" type="date" value="${esc(state.today || "")}" /></div>
      <div class="field"><label>客户（专用挂具才填）</label><input id="tlm-cust" /></div>
      <div class="field"><label>数量 *</label><input id="tlm-qty" style="width:100px" /></div>
      <div class="field"><label>使用次数（归还留空按套数算）</label><input id="tlm-times" style="width:100px" /></div>
      <div class="field"><label>备注</label><input id="tlm-memo" /></div>
      <div class="foot"><button class="btn primary" id="tlm-save">保存</button><button class="btn ghost" id="tlm-cancel">取消</button></div>`);
    $("#tlm-cancel", m).onclick = closeModal;
    $("#tlm-save", m).onclick = async () => {
      const body = {
        tool_code: $("#tlm-code", m).value,
        kind: $("#tlm-kind", m).value,
        date: $("#tlm-date", m).value.trim(),
        customer: $("#tlm-cust", m).value.trim(),
        qty: $("#tlm-qty", m).value.trim(),
        times: parseInt($("#tlm-times", m).value.trim() || "0", 10) || 0,
        memo: $("#tlm-memo", m).value.trim(),
      };
      if (!body.tool_code) { toast("先建挂具档案", "err"); return; }
      if (!body.qty) { toast("数量必填", "err"); return; }
      try {
        const r = await api("/tools/moves", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
        toast(`已保存 ${r.move.no}：在库 ${fmt(r.balance.in_stock)}，在车间 ${fmt(r.balance.in_use)}`, "ok");
        closeModal(); loadBal(); loadMoves();
      } catch (e) { toast(e.message, "err"); }
    };
  };

  const openItem = () => {
    const m = modal(`<h3>新增挂具档案</h3>
      <div class="field"><label>编号 *</label><input id="tli-code" placeholder="如 GJ-001" /></div>
      <div class="field"><label>名称</label><input id="tli-name" /></div>
      <div class="field"><label>类型</label><input id="tli-kind" value="挂具" /></div>
      <div class="field"><label>规格</label><input id="tli-spec" placeholder="如 600×400" /></div>
      <div class="field"><label>单位</label><input id="tli-unit" value="套" style="width:80px" /></div>
      <div class="field"><label>额定使用次数（0 = 不限）</label><input id="tli-life" value="0" style="width:100px" /></div>
      <div class="field"><label>备注</label><input id="tli-memo" /></div>
      <div class="foot"><button class="btn primary" id="tli-save">保存</button><button class="btn ghost" id="tli-cancel">取消</button></div>`);
    $("#tli-cancel", m).onclick = closeModal;
    $("#tli-save", m).onclick = async () => {
      const body = {
        code: $("#tli-code", m).value.trim(),
        name: $("#tli-name", m).value.trim(),
        tool_kind: $("#tli-kind", m).value.trim(),
        spec: $("#tli-spec", m).value.trim(),
        unit: $("#tli-unit", m).value.trim(),
        life_times: parseInt($("#tli-life", m).value.trim() || "0", 10) || 0,
        memo: $("#tli-memo", m).value.trim(),
      };
      if (!body.code) { toast("编号必填", "err"); return; }
      try {
        await api("/tools/items", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
        toast("已保存", "ok"); closeModal(); loadBal();
      } catch (e) { toast(e.message, "err"); }
    };
  };

  $("#tl-reload", main).onclick = () => { loadBal(); loadMoves(); };
  $("#tl-print", main).onclick = () => printPreview("挂具台账", main);
  const bNew = $("#tl-new-move", main);
  if (bNew) bNew.onclick = openMove;
  const bItem = $("#tl-new-item", main);
  if (bItem) bItem.onclick = openItem;
  await loadBal();
  await loadMoves();
}


// ===========================================================================
// 返工成本单：返工的材料/人工/制造费用单独归集，看「这批返工亏了多少」
// ===========================================================================
async function viewRework(main) {
  main.innerHTML = `<h2>返工成本单</h2>
    <div class="toolbar">
      <label>期间 <input id="rw-period" style="width:100px" placeholder="202601" /></label>
      <button class="btn primary sm" id="rw-reload">查询</button>
      ${can("production_ops") ? `<button class="btn ghost sm" id="rw-new">新建返工单</button>` : ""}
      <button class="btn ghost sm" id="rw-print">打印预览</button>
      <button class="btn ghost sm" id="rw-reasons">原因分类</button>
    </div>
    <div class="panel"><div id="rw-list" class="muted">加载中…</div></div>
    <div class="panel"><h3 style="margin-top:0;font-size:14px">返工成本分析</h3>
      <div id="rw-analysis" class="muted">加载中…</div></div>
    <div class="muted" style="font-size:12px">返工单挂在原生产订单上，成本单独归集 —— 返工的钱不进原单，原单单位成本才不会虚高；「占原单」= 返工成本 ÷ 原单已归集成本</div>`;

  const periodInput = $("#rw-period");
  if (periodInput && !periodInput.value) periodInput.value = state.current || "";

  let rows = [];

  const load = async () => {
    const el = $("#rw-list");
    el.className = "muted";
    el.textContent = "加载中…";
    const q = periodInput && periodInput.value ? `?period=${encodeURIComponent(periodInput.value)}` : "";
    const d = await api(`/rework${q}`);
    rows = d.orders || [];
    if (!rows.length) {
      el.textContent = "这一期没有返工单。";
      return;
    }
    const total = rows.reduce((a, r) => a + num(r.total), 0);
    el.className = "";
    el.innerHTML = `<table class="grid">
      <thead><tr><th>单号</th><th>日期</th><th>来源订单</th><th>物料</th><th class="r">返工数量</th>
        <th>分类</th><th>原因</th><th>状态</th><th class="r">材料</th><th class="r">人工</th><th class="r">制造费用</th>
        <th class="r">合计</th><th>操作</th></tr></thead>
      <tbody>${rows
        .map(
          (r) => `<tr>
            <td>${esc(r.no)}</td>
            <td>${esc(r.date)}</td>
            <td>${esc(r.po_no)}</td>
            <td>${esc(r.item_code)} ${esc(r.item_name)}</td>
            <td class="r">${fmt(r.qty)}</td>
            <td>${esc(r.reason_label || "未分类")}</td>
            <td>${esc(r.reason || "")}</td>
            <td>${esc(r.status === "closed" ? "已结案" : "进行中")}</td>
            <td class="r">${fmt(r.material)}</td>
            <td class="r">${fmt(r.labor)}</td>
            <td class="r">${fmt(r.overhead)}</td>
            <td class="r">${fmt(r.total)}</td>
            <td><button class="btn ghost sm" data-rw-open="${r.id}">成本</button>
                <button class="btn ghost sm" data-rw-del="${r.id}">删除</button></td>
          </tr>`
        )
        .join("")}</tbody>
      <tfoot><tr><td colspan="11" class="r">合计</td><td class="r">${fmt(String(total))}</td><td></td></tr></tfoot>
    </table>`;
    for (const b of $all("[data-rw-open]", el)) {
      b.addEventListener("click", () => openCosts(Number(b.dataset.rwOpen)));
    }
    for (const b of $all("[data-rw-del]", el)) {
      b.addEventListener("click", async () => {
        if (!(await confirmDialog("删除这张返工单？成本明细会一起删掉。", true))) return;
        try {
          await api(`/rework/${b.dataset.rwDel}`, { method: "DELETE" });
          toast("已删除");
          load();
        } catch (e) {
          toast(e.message, "err");
        }
      });
    }
    loadAnalysis();
  };

  // 按原因 / 按部门汇总：回答「哪种原因最费钱、哪个班组返工最多」
  const loadAnalysis = async () => {
    const el = $("#rw-analysis");
    if (!el) return;
    const q = periodInput && periodInput.value ? `?period=${encodeURIComponent(periodInput.value)}` : "";
    let d;
    try {
      d = await api(`/rework/analysis${q}`);
    } catch (e) {
      el.className = "muted";
      el.textContent = e.message;
      return;
    }
    const block = (title, list, emptyLabel) => {
      const arr = list || [];
      if (!arr.length) return `<div class="muted">${emptyLabel}</div>`;
      return `<table class="grid"><thead><tr><th>${title}</th><th class="r">张数</th><th class="r">返工件数</th>
          <th class="r">材料</th><th class="r">人工</th><th class="r">制造费用</th>
          <th class="r">合计</th><th class="r">占比</th></tr></thead>
        <tbody>${arr
          .map(
            (b) => `<tr>
              <td>${esc(b.label)}</td>
              <td class="r">${b.count}</td>
              <td class="r">${fmt(b.qty)}</td>
              <td class="r">${fmt(b.material)}</td>
              <td class="r">${fmt(b.labor)}</td>
              <td class="r">${fmt(b.overhead)}</td>
              <td class="r">${fmt(b.total)}</td>
              <td class="r">${esc(b.share || "—")}${b.share ? "%" : ""}</td>
            </tr>`
          )
          .join("")}</tbody></table>`;
    };
    el.className = "";
    el.innerHTML = `<div class="muted" style="margin-bottom:6px">本期 ${d.count} 张返工单，返工成本合计 ${fmt(d.total)}</div>
      <h4 style="margin:8px 0 4px;font-size:13px">按原因</h4>${block("原因", d.by_reason, "这一期没有返工成本")}
      <h4 style="margin:12px 0 4px;font-size:13px">按责任部门</h4>${block("部门", d.by_dept, "这一期没有返工成本")}`;
  };

  // 原因分类维护：没有字典，同一件事会写成三行，按原因汇总就等于没汇总
  $("#rw-reasons")?.addEventListener("click", async () => {
    let list = [];
    try {
      const d = await api("/rework/reasons");
      list = d.rows || [];
    } catch (e) {
      toast(e.message, "err");
      return;
    }
    modal(`<h3>返工原因分类</h3>
      <div class="muted" style="font-size:12px;margin-bottom:8px">编码是历史单据的锚：改名可以，换码会让旧单变成「未分类」。删除只能删没被用过的，用过的改成停用。</div>
      <div id="rr-list"></div>
      <div class="form" style="margin-top:8px">
        <label>编码 <input id="rr-code" style="width:90px" placeholder="SLUG" /></label>
        <label>名称 <input id="rr-name" style="width:140px" placeholder="槽液" /></label>
        <label>备注 <input id="rr-memo" style="width:180px" /></label>
      </div>
      <div class="toolbar"><button class="btn primary sm" id="rr-add">新增分类</button>
        <button class="btn ghost sm" id="rr-close">关闭</button></div>`);
    const draw = () => {
      const box = $("#rr-list");
      if (!list.length) {
        box.innerHTML = `<div class="muted">还没有原因分类</div>`;
        return;
      }
      box.innerHTML = `<table class="grid"><thead><tr><th>编码</th><th>名称</th><th>备注</th><th>状态</th><th>操作</th></tr></thead>
        <tbody>${list
          .map(
            (r) => `<tr>
              <td>${esc(r.code)}</td><td>${esc(r.name)}</td><td>${esc(r.memo || "")}</td>
              <td>${r.disabled ? "停用" : "启用"}</td>
              <td><button class="btn ghost sm" data-rr-toggle="${r.id}" data-dis="${r.disabled ? 0 : 1}">${r.disabled ? "启用" : "停用"}</button>
                  <button class="btn ghost sm" data-rr-del="${r.id}">删除</button></td>
            </tr>`
          )
          .join("")}</tbody></table>`;
      for (const b of $all("[data-rr-toggle]", box)) {
        b.addEventListener("click", async () => {
          const r = list.find((x) => String(x.id) === b.dataset.rrToggle);
          try {
            await postJson("/rework/reasons", {
              id: r.id,
              code: r.code,
              name: r.name,
              memo: r.memo || "",
              disabled: b.dataset.dis === "1",
            });
            await refresh();
          } catch (e) {
            toast(e.message, "err");
          }
        });
      }
      for (const b of $all("[data-rr-del]", box)) {
        b.addEventListener("click", async () => {
          if (!(await confirmDialog("删除这个原因分类？被返工单用过的不许删。", true))) return;
          try {
            await api(`/rework/reasons/${b.dataset.rrDel}`, { method: "DELETE" });
            await refresh();
          } catch (e) {
            toast(e.message, "err");
          }
        });
      }
    };
    const refresh = async () => {
      const d = await api("/rework/reasons");
      list = d.rows || [];
      draw();
    };
    draw();
    $("#rr-close").addEventListener("click", closeModal);
    $("#rr-add").addEventListener("click", async () => {
      try {
        await postJson("/rework/reasons", {
          code: $("#rr-code").value,
          name: $("#rr-name").value,
          memo: $("#rr-memo").value,
        });
        $("#rr-code").value = "";
        $("#rr-name").value = "";
        $("#rr-memo").value = "";
        await refresh();
        toast("已保存");
      } catch (e) {
        toast(e.message, "err");
      }
    });
  });

  // 新建：选来源生产订单（带出物料/计划量），数量不能超过它
  $("#rw-new")?.addEventListener("click", async () => {
    let orders = [];
    let reasons = [];
    try {
      const d = await api("/prod");
      orders = d.orders || [];
      const rd = await api("/rework/reasons");
      reasons = (rd.rows || []).filter((r) => !r.disabled);
    } catch (e) {
      toast(e.message, "err");
      return;
    }
    if (!orders.length) {
      toast("本期还没有生产订单，先下生产订单再返工", "err");
      return;
    }
    modal(`<h3>新建返工单</h3>
      <div class="form">
        <label>来源生产订单 <select id="rwn-po">${orders
          .map(
            (o) =>
              `<option value="${o.id}" data-planned="${esc(o.planned_qty)}">${esc(o.no)} ${esc(o.item_code)} ×${esc(o.planned_qty)}</option>`
          )
          .join("")}</select></label>
        <label>返工数量 <input id="rwn-qty" style="width:110px" placeholder="不超过原单计划量" /></label>
        <label>日期 <input id="rwn-date" type="date" value="${state.today || ""}" /></label>
        <label>原因分类 <select id="rwn-code"><option value="">未分类</option>${reasons
          .map((r) => `<option value="${esc(r.code)}">${esc(r.name)}</option>`)
          .join("")}</select></label>
        <label>责任部门 <input id="rwn-dept" style="width:140px" placeholder="如 氧化一班" /></label>
        <label>原因备注 <input id="rwn-reason" style="width:220px" placeholder="如 挂具印痕 / 氧化膜厚不足" /></label>
        <label>备注 <input id="rwn-memo" style="width:260px" /></label>
      </div>
      <div class="toolbar"><button class="btn primary sm" id="rwn-save">保存</button>
        <button class="btn ghost sm" id="rwn-cancel">取消</button></div>`);
    $("#rwn-cancel").addEventListener("click", closeModal);
    $("#rwn-save").addEventListener("click", async () => {
      const sel = $("#rwn-po");
      const opt = sel.options[sel.selectedIndex];
      const qty = num($("#rwn-qty").value);
      const planned = num(opt?.dataset.planned || "0");
      if (!(qty > 0)) return toast("返工数量必须大于 0", "err");
      if (qty > planned) return toast(`返工数量不能超过原单计划量 ${fmt(opt?.dataset.planned || "0")}`, "err");
      try {
        const r = await postJson("/rework", {
          po_id: Number(sel.value),
          qty: $("#rwn-qty").value,
          date: $("#rwn-date").value,
          reason: $("#rwn-reason").value,
          reason_code: $("#rwn-code").value,
          dept: $("#rwn-dept").value,
          memo: $("#rwn-memo").value,
        });
        closeModal();
        toast(`已建返工单 ${r.no}`);
        load();
      } catch (e) {
        toast(e.message, "err");
      }
    });
  });

  // 成本明细：三段汇总 + 追加/删除
  const openCosts = async (id) => {
    const d = await api(`/rework/${id}`);
    const o = d.order;
    const list = (d.costs || [])
      .map(
        (c) => `<tr><td>${esc(kindLabel(d.kinds, c.cost_type))}</td><td class="r">${fmt(c.amount)}</td>
          <td>${esc(c.memo || "")}</td>
          <td><button class="btn ghost sm" data-cost-del="${c.id}">删</button></td></tr>`
      )
      .join("");
    modal(`<h3>${esc(o.no)} · ${esc(o.po_no)} ${esc(o.item_code)}（返工 ${fmt(o.qty)}）</h3>
      <div class="muted">原因：${esc(o.reason || "—")}　状态：${esc(o.status === "closed" ? "已结案" : "进行中")}</div>
      <table class="grid">
        <thead><tr><th>成本项</th><th class="r">金额</th><th>备注</th><th>操作</th></tr></thead>
        <tbody>${list || `<tr><td colspan="4" class="muted">还没有成本记录</td></tr>`}</tbody>
        <tfoot><tr><td class="r">材料</td><td class="r">${fmt(d.material)}</td><td></td><td></td></tr>
          <tr><td class="r">人工</td><td class="r">${fmt(d.labor)}</td><td></td><td></td></tr>
          <tr><td class="r">制造费用</td><td class="r">${fmt(d.overhead)}</td><td></td><td></td></tr>
          <tr><td class="r">合计</td><td class="r">${fmt(d.total)}</td><td></td><td></td></tr></tfoot>
      </table>
      <div class="toolbar">
        <select id="rwc-kind">${(d.kinds || [])
          .map((k) => `<option value="${k.code}">${esc(k.label)}</option>`)
          .join("")}</select>
        <input id="rwc-amt" style="width:110px" placeholder="金额" />
        <input id="rwc-memo" style="width:180px" placeholder="备注" />
        <button class="btn primary sm" id="rwc-add">追加成本</button>
        <button class="btn ghost sm" id="rwc-close">关闭</button>
      </div>
      <div class="muted" style="font-size:12px">已结案的单不许再改成本：结案意味着这批返工的钱已经认下来了</div>`);
    $("#rwc-close").addEventListener("click", closeModal);
    $("#rwc-add").addEventListener("click", async () => {
      const amt = num($("#rwc-amt").value);
      if (!(amt > 0)) return toast("金额必须大于 0", "err");
      try {
        await postJson(`/rework/${id}/costs`, {
          cost_type: $("#rwc-kind").value,
          amount: $("#rwc-amt").value,
          memo: $("#rwc-memo").value,
        });
        closeModal();
        toast("已追加");
        load();
      } catch (e) {
        toast(e.message, "err");
      }
    });
    for (const b of $all("[data-cost-del]")) {
      b.addEventListener("click", async () => {
        try {
          await api(`/rework/costs/${b.dataset.costDel}`, { method: "DELETE" });
          closeModal();
          toast("已删除");
          load();
        } catch (e) {
          toast(e.message, "err");
        }
      });
    }
  };

  $("#rw-reload")?.addEventListener("click", load);
  $("#rw-print")?.addEventListener("click", () => {
    const el = $("#rw-list");
    if (!el.querySelector("table")) return toast("先查询出数据再打印");
    printPreview("返工成本单", el);
  });
  load();
}

// 带千分位的金额字符串 → 数字（列表里求和、比较都用它）
function num(v) {
  if (v === null || v === undefined) return 0;
  const n = Number(String(v).replace(/,/g, ""));
  return Number.isFinite(n) ? n : 0;
}

function kindLabel(kinds, code) {
  const k = (kinds || []).find((x) => x.code === code);
  return k ? k.label : code;
}
// ---- 视图路由表 ---------------------------------------------------------
// 为什么这张表放在**最后一个**脚本里：它的值是对每个视图函数的引用，而函数声明只在
// 自己那一份脚本里提升。放在前面加载的文件里，加载那一刻这些标识符还不存在 → ReferenceError，
// 整个前端白屏。放在最后加载的文件里，等它执行时全部视图函数都已就位。


// 视图注册表：id → 渲染函数（函数声明已提升，可在顶层引用）
const VIEWS = {
  "overview": viewOverview,
  "platform-users": viewPlatformUsers,
  "platform-books": viewPlatformBooks,
  "audit-log": viewAuditLog,
  "backup": viewBackup,
  "consolidate": viewConsolidate,
  "dashboard": viewDashboard,
  "vouchers": viewVouchers,
  "invoices": viewInvoices,
  "tax-decl": viewTaxDecl,
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
  "dim-profit": viewDimProfit,
  "month-brief": viewMonthBrief,
  "profit-sheet": viewProfitSheet,
  "arap-recon": viewArapRecon,
  "consign": viewConsign,
  "tool-ledger": viewToolLedger,
  "rework": viewRework,
  "piece-pay": viewPiecePay,
  "price-rules": viewPriceRules,
  "period-end": viewPeriodEnd,
  "assets": viewAssets,
  "bank": viewBank,
  "customers": viewCustomers,
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
  "cost-variance": viewCostVariance,
  "landed-cost": viewLandedCost,
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


(async function boot() {
  try {
    const me = await api("/me");
    session.user = me;
    // `platformAdmin` 必须在这里补一次。
    //
    // 侧栏可见性判据 `isNavVisible` 里有 `if (n.platform && !pa) return false`，
    // 而这个变量此前**只在登录响应与 showBookPicker 里被写过** —— 刷新页面
    // 走的是 boot()，两处都不经过，于是「合并报表」这类 platform 项
    // 一按 F5 就从侧栏消失（登录后第一次进来是好的，刷新后才坏）。
    //
    // 这里刻意**不**用 `/api/me` 的 `is_admin`：那是**账套内**管理员
    // （`PublicUser`，来自 Role::Admin），与平台管理员不是一回事 ——
    // 一个被拉进账套的普通用户若在那套里恰好是 Role::Admin，直接用
    // 会让他看见合并报表，越权。
    //
    // 走 `/api/books`：它的 `user` 是 `RealmUser`（平台层身份），
    // `is_admin` 就是平台管理员，语义对得上。该接口对普通账号也返回 200
    // （`visible_books` 只是按归属过滤账套列表），所以这里不是靠 403
    // fail-closed，而是**字段本身可信**；真取不到（网络/会话失效）才落回 false。
    try {
      const b = await api("/books");
      session.platformAdmin = !!(b.user && b.user.is_admin);
    } catch (e) {
      session.platformAdmin = false;
    }
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
