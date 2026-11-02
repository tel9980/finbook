// FinBook Web 前端 — 资金：预算、预收预付、收款、转账、票据、出纳班次、日记账、盘点、借款
//
// 这一份是 `crates/finweb/static/app.js` 拆分出来的（原单文件 14k 行、295 个顶层函数）。
// 拆分方式：**经典 script，按原始行序切分，不重排任何代码**——顶层 function/const 的
// 提升与声明顺序完全保持原样，所以跨文件不存在 TDZ/未定义问题；只有 `VIEWS` 路由表
// 因为引用全部视图函数，被挪到了最后加载的 app-hr.js。
// 加载顺序由 `index.html` 的 <script> 顺序决定（也是 `main.rs` 里 asset_version 的清单顺序）。
// 新增文件必须同步三处：index.html、main.rs 的 asset_version 清单、tools/lib-app-src.js 的解析（自动）。
// ===========================================================================
// 资金管理：资金日报 / 票据 / 融资 / 资金预测
// ===========================================================================
// ===========================================================================
// 电费台账（资金管理页的一个页签）：充值 → 抄表 → 结转
//
// 加工厂的电费多是「先往电卡充钱、按用量扣」，三件事必须分开：
// ①充值只是把钱从银行挪到预付卡（借 112301 / 贷 银行存款），不是费用；
// ②月底抄表只是登记用量（本页表格），还不动账；
// ③结转才把用量转成费用并拆进项（借 510103 不含税 / 借 22210101 / 贷 112301）。
// 本页只做②③；充值走收款付款或凭证录入。为什么这样分，见 findb/src/meter.rs 文件头。
async function drawMeterTab(body) {
  if (!state.meterPeriod) state.meterPeriod = state.period || today().slice(0, 7);
  if (!state.meterKind) state.meterKind = "electric";
  const load = async () => {
    const q = `period=${encodeURIComponent(state.meterPeriod)}&kind=${encodeURIComponent(state.meterKind)}`;
    let d;
    try {
      d = await api(`/meter?${q}`);
    } catch (e) {
      body.innerHTML = `<div class="muted">${esc(e.message || String(e))}</div>`;
      return;
    }
    const s = d.summary || {};
    const rows = (d.items || []).map((r) => `<tr>
        <td>${esc(r.read_date)}</td>
        <td>${esc(r.meter_no)}</td>
        <td class="num">${esc(r.qty)}</td>
        <td class="num">${esc(r.unit_price)}</td>
        <td class="num">${esc(r.amount)}</td>
        <td class="num">${esc(r.net)}</td>
        <td class="num">${esc(r.tax)}</td>
        <td>${r.closed ? `已结转 #${r.voucher_id}` : '<b style="color:var(--warn)">待结转</b>'}</td>
        <td>${r.closed ? "" : `<button class="btn sm ghost" data-del="${r.id}">删除</button>`}</td>
      </tr>`).join("");
    body.innerHTML = `
      <div class="toolbar">
        <label>月份 <input type="month" id="mt-period" value="${esc(state.meterPeriod)}" /></label>
        <label>类别
          <select id="mt-kind">
            <option value="electric"${state.meterKind === "electric" ? " selected" : ""}>电费</option>
            <option value="water"${state.meterKind === "water" ? " selected" : ""}>水费</option>
          </select>
        </label>
        <button class="btn sm primary" id="mt-close">结转本期（出凭证）</button>
      </div>
      <div class="cards">
        <div class="card"><div class="muted">本期充值</div><div class="big">${esc(s.recharged || "0.00")}</div></div>
        <div class="card"><div class="muted">本期已结转</div><div class="big">${esc(s.consumed || "0.00")}</div></div>
        <div class="card"><div class="muted">卡里余额</div><div class="big">${esc(s.balance || "0.00")}</div></div>
        <div class="card"><div class="muted">待结转 ${s.unclosed_count || 0} 笔</div><div class="big" style="color:var(--warn)">${esc(s.unclosed || "0.00")}</div></div>
      </div>
      <p class="muted" style="font-size:12px">「卡里余额」是 112301 预付电费的期末余额 —— 电卡里还剩多少，账上就该显示多少。待结转的不含税 ${esc(s.unclosed_net || "0.00")}、可抵进项 ${esc(s.unclosed_tax || "0.00")}。</p>
      <table class="grid"><thead><tr>
        <th>抄表日</th><th>表号/户号</th><th>用量</th><th>含税单价</th><th>金额(含税)</th><th>不含税</th><th>进项</th><th>状态</th><th></th>
      </tr></thead><tbody>${rows || '<tr><td colspan="9" class="muted">本期还没有抄表记录</td></tr>'}</tbody></table>
      <h3 style="margin-top:14px">登记抄表</h3>
      <div class="toolbar">
        <label>抄表日 <input type="date" id="mr-date" value="${esc(today())}" /></label>
        <label>表号 <input id="mr-no" placeholder="如 DB-01" style="width:90px" /></label>
        <label>用量 <input id="mr-qty" placeholder="度/吨" style="width:80px" /></label>
        <label>含税单价 <input id="mr-price" placeholder="1.20" style="width:80px" /></label>
        <label>金额(含税) <input id="mr-amt" placeholder="留空=用量×单价" style="width:120px" /></label>
        <label>税率
          <select id="mr-rate">
            <option value="0.13">13%（专票）</option>
            <option value="0.09">9%</option>
            <option value="0">无票 / 不能抵</option>
          </select>
        </label>
        <button class="btn sm primary" id="mr-save">登记</button>
      </div>
      <p class="muted" style="font-size:12px">只有拿到票才能填 13%（或 9%）：进项能不能抵，看的是电力公司开的票，不是「电费天生能抵」。</p>`;
    $("#mt-period", body).onchange = (e) => { state.meterPeriod = e.target.value; load(); };
    $("#mt-kind", body).onchange = (e) => { state.meterKind = e.target.value; load(); };
    $("#mr-save", body).onclick = async () => {
      const payload = {
        kind: state.meterKind,
        meter_no: $("#mr-no", body).value.trim(),
        date: $("#mr-date", body).value.trim(),
        qty: $("#mr-qty", body).value.trim(),
        unit_price: $("#mr-price", body).value.trim(),
        amount: $("#mr-amt", body).value.trim(),
        rate: $("#mr-rate", body).value,
        period: state.meterPeriod,
      };
      if (!payload.meter_no) { toast("请填写表号/户号", "err"); return; }
      try {
        await postJson("/meter", payload);
        toast("已登记");
        load();
      } catch (e) { toast(e.message || String(e), "err"); }
    };
    $("#mt-close", body).onclick = async () => {
      try {
        const r = await postJson("/meter/close", { period: state.meterPeriod, kind: state.meterKind });
        if (r.voucher_id) toast(`已生成结转凭证 #${r.voucher_id}`);
        else toast(r.message || "本期没有待结转的抄表记录");
        load();
      } catch (e) { toast(e.message || String(e), "err"); }
    };
    body.querySelectorAll("[data-del]").forEach((b) => {
      b.onclick = async () => {
        if (!confirm("删除这条抄表记录？")) return;
        try {
          await api(`/meter/${b.getAttribute("data-del")}`, { method: "DELETE" });
          load();
        } catch (e) { toast(e.message || String(e), "err"); }
      };
    });
  };
  await load();
}

async function viewFunds(main) {
  main.innerHTML = `<h2>资金管理</h2>
    <div class="toolbar">
      <button class="btn sm ${state.fundsTab === "daily" ? "primary" : "ghost"}" id="ft-daily">资金日报</button>
      <button class="btn sm ${state.fundsTab === "bill" ? "primary" : "ghost"}" id="ft-bill">票据</button>
      <button class="btn sm ${state.fundsTab === "loan" ? "primary" : "ghost"}" id="ft-loan">融资</button>
      <button class="btn sm ${state.fundsTab === "count" ? "primary" : "ghost"}" id="ft-count">现金盘点</button>
      <button class="btn sm ${state.fundsTab === "check" ? "primary" : "ghost"}" id="ft-check">支票簿</button>
      <button class="btn sm ${state.fundsTab === "journal" ? "primary" : "ghost"}" id="ft-journal">日记账</button>
      <button class="btn sm ${state.fundsTab === "advance" ? "primary" : "ghost"}" id="ft-advance">借支</button>
      <button class="btn sm ${state.fundsTab === "shift" ? "primary" : "ghost"}" id="ft-shift">交接班</button>
      <button class="btn sm ${state.fundsTab === "budget" ? "primary" : "ghost"}" id="ft-budget">资金预算</button>
      <button class="btn sm ${state.fundsTab === "receipt" ? "primary" : "ghost"}" id="ft-receipt">收付款</button>
      <button class="btn sm ${state.fundsTab === "transfer" ? "primary" : "ghost"}" id="ft-transfer">资金调拨</button>
      <button class="btn sm ${state.fundsTab === "forecast" ? "primary" : "ghost"}" id="ft-forecast">资金预测</button>
      <button class="btn sm ${state.fundsTab === "meter" ? "primary" : "ghost"}" id="ft-meter">电费台账</button>
    </div>
    <div id="funds-body" class="muted">加载中…</div>`;
  const tab = state.fundsTab || "daily";
  // 资金管理页写按钮按 voucher_new 显隐：导航放行的是 fin_report（只读/成本会计可进页），
  // 而页内写操作服务端要求 voucher_new——显隐与权限一致，避免"看得见点不了"
  const FUNDS_WRITE_IDS = ["#rc-new", "#cc-new", "#ck-new", "#ad-new", "#bill-new", "#loan-new",
    "#jn-clear", "#jn-unclear", "#jn-receipt", "#jn-payment", "#tf-new"];
  const gateFundsWrites = () => {
    if (can("voucher_new")) return;
    FUNDS_WRITE_IDS.forEach((s) => { const el = $(s); if (el) el.remove(); });
  };
  const switchTab = (t) => { state.fundsTab = t; viewFunds(main); gateFundsWrites(); };
  $("#ft-daily").onclick = () => switchTab("daily");
  $("#ft-bill").onclick = () => switchTab("bill");
  $("#ft-loan").onclick = () => switchTab("loan");
  $("#ft-count").onclick = () => switchTab("count");
  $("#ft-check").onclick = () => switchTab("check");
  $("#ft-journal").onclick = () => switchTab("journal");
  $("#ft-advance").onclick = () => switchTab("advance");
  $("#ft-shift").onclick = () => switchTab("shift");
  $("#ft-budget").onclick = () => switchTab("budget");
  $("#ft-receipt").onclick = () => switchTab("receipt");
  $("#ft-transfer").onclick = () => switchTab("transfer");
  $("#ft-forecast").onclick = () => switchTab("forecast");
  $("#ft-meter").onclick = () => switchTab("meter");

  const body = $("#funds-body");
  if (tab === "meter") {
    body.className = "";
    await drawMeterTab(body);
    gateFundsWrites();
    return;
  }
  if (tab === "daily") {
    body.className = "";
    const shift = (ds, n) => { const d = new Date(ds + "T00:00:00"); d.setDate(d.getDate() + n); return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`; };
    if (!state.fundsDate) state.fundsDate = today();
    body.innerHTML = `<div class="toolbar">
        <label>日期 <input type="date" id="fd-date" value="${esc(state.fundsDate)}" /></label>
        <button class="btn sm ghost" id="fd-prev">« 前一日</button>
        <button class="btn sm ghost" id="fd-next">后一日 »</button>
        <button class="btn sm ghost" id="fd-today">今天</button>
      </div><div id="fd-table" class="muted">加载中…</div>`;
    const load = async () => {
      try {
        const r = await api(`/funds/daily-by-date?date=${encodeURIComponent(state.fundsDate)}`);
        const rows = r.rows || [];
        $("#fd-table").innerHTML = rows.length
          ? `<table class="grid"><thead><tr><th>科目</th><th>科目名称</th><th class="num">上日结余</th><th class="num">本日收入</th><th class="num">本日支出</th><th class="num">日末结存</th></tr></thead><tbody>${rows.map((x) => `<tr><td>${esc(x.account_code)}</td><td>${esc(x.account_name)}</td><td class="num">${fmt(x.begin)}</td><td class="num">${fmt(x.income)}</td><td class="num">${fmt(x.expense)}</td><td class="num"><b>${fmt(x.end)}</b></td></tr>`).join("")}</tbody></table>`
          : `<div class="muted">无现金/银行科目</div>`;
      } catch (e) { $("#fd-table").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
    };
    const gotoDate = (ds) => { state.fundsDate = ds; $("#fd-date").value = ds; load(); };
    $("#fd-date").onchange = (e) => { state.fundsDate = e.target.value; load(); };
    $("#fd-prev").onclick = () => gotoDate(shift(state.fundsDate, -1));
    $("#fd-next").onclick = () => gotoDate(shift(state.fundsDate, 1));
    $("#fd-today").onclick = () => gotoDate(today());
    load();
  } else if (tab === "bill") {
    renderBills(body);
  } else if (tab === "loan") {
    renderLoans(body);
  } else if (tab === "count") {
    renderCashCount(body);
  } else if (tab === "check") {
    renderChecks(body);
  } else if (tab === "journal") {
    renderJournal(body);
  } else if (tab === "advance") {
    renderAdvances(body);
  } else if (tab === "shift") {
    renderCashShifts(body);
  } else if (tab === "budget") {
    renderBudget(body);
  } else if (tab === "receipt") {
    renderReceipts(body);
  } else if (tab === "transfer") {
    renderTransfers(body);
  } else {
    body.className = "";
    try {
      const r = await api("/funds/forecast");
      const f = r.forecast || {};
      body.innerHTML = `<div class="cards">
        ${["现金/银行结存", "在库应收票据", "应付票据", "放款可收回", "借款需偿还", "预计资金头寸"].map((t, i) => {
          const k = ["cash_balance", "receivable_bills", "payable_bills", "lend", "borrow", "position"][i];
          const v = f[k] || "0";
          return `<div class="card"><div class="k">${t}</div><div class="v">${fmt(v)}</div></div>`;
        }).join("")}
      </div>`;
    } catch (e) { body.innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
    body.innerHTML += `<div class="panel" style="margin-top:12px">
      <div style="display:flex;align-items:center;gap:8px;flex-wrap:wrap"><b>滚动预测（票据到期 + 融资起止按期间展开）</b><span class="grow"></span>
        <label style="font-size:12px">起 <input id="fr-from" value="${esc((state.current || "").replace("-", ""))}" style="width:90px" /></label>
        <label style="font-size:12px">期数 <input id="fr-n" value="6" style="width:50px" /></label>
        <button class="btn ghost sm" id="fr-run">滚动预测</button>
      </div>
      <div id="fr-out" class="muted" style="margin-top:6px">口径：期初结存=该期现金/银行期末（已记账）；不含未到期未核销往来</div>
    </div>`;
    $("#fr-run").onclick = async () => {
      try {
        const r = await api(`/funds/forecast-rolling?from=${encodeURIComponent($("#fr-from").value.trim())}&periods=${encodeURIComponent($("#fr-n").value.trim())}`);
        const rows = r.rows || [];
        $("#fr-out").innerHTML = rows.length
          ? `<table class="grid"><thead><tr><th>期间</th><th class="num">票据到期(收)</th><th class="num">票据到期(付)</th><th class="num">融资到账</th><th class="num">融资偿还</th><th class="num">净流</th><th class="num">期末结存</th></tr></thead><tbody>${rows.map((x) => `<tr><td>${esc(x.period)}</td><td class="num">${fmt(x.bill_in)}</td><td class="num">${fmt(x.bill_out)}</td><td class="num">${fmt(x.loan_in)}</td><td class="num">${fmt(x.loan_out)}</td><td class="num"><b>${fmt(x.net)}</b></td><td class="num"><b>${fmt(x.balance)}</b></td></tr>`).join("")}</tbody></table>`
          : `<div class="muted">无数据</div>`;
      } catch (e) { $("#fr-out").innerHTML = `<span style="color:var(--err)">${esc(e.message)}</span>`; }
    };
  }
  // 页内写按钮按 voucher_new 显隐（初始进入与切页签都会经过）
  gateFundsWrites();
}

// 资金预算 vs 执行（现金/银行科目；实际=当期已记账净额 H-3）
async function renderBudget(body) {
  body.className = "";
  body.innerHTML = `<div id="fb-table" class="muted">加载中…</div>`;
  try {
    const r = await api(`/funds/budget?period=${ymm(state.current || "")}`);
    const rows = r.rows || [];
    $("#fb-table").innerHTML = rows.length
      ? `<table class="grid"><thead><tr><th>科目</th><th>科目名称</th><th class="num">预算</th><th class="num">实际(净额)</th><th class="num">差异</th><th class="num">执行率</th><th>状态</th></tr></thead><tbody>${rows.map((x) => `<tr>
          <td>${esc(x.account_code)}</td><td>${esc(x.account_name)}</td>
          <td class="num">${fmt(x.budget)}</td><td class="num">${fmt(x.actual)}</td>
          <td class="num">${fmt(x.diff)}</td><td class="num">${esc(String(x.rate))}%</td>
          <td>${x.over ? '<span class="tag err">超预算</span>' : '<span class="tag ok">正常</span>'}</td></tr>`).join("")}</tbody></table>
        <div class="muted" style="font-size:12px;margin-top:6px">口径：预算取当前版本的现金/银行科目预算行；实际为当期已记账发生净额（H-3）。预算金额在预算管理维护。</div>`
      : `<div class="muted">本期未编制现金/银行科目预算：到【预算】为 1001/1002 等科目新增预算行后，此处显示执行情况。</div>`;
  } catch (e) { $("#fb-table").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
}

// 员工借支（出纳）：建单 → 支付（出凭证）→ 核销冲账（出凭证）
async function renderAdvances(body) {
  body.className = "";
  body.innerHTML = `<div class="toolbar">
      <label>日期 <input type="date" id="ad-date" value="${today()}" /></label>
      <label>借支人 <input id="ad-emp" style="width:90px" /></label>
      <label>事由 <input id="ad-purpose" style="width:150px" /></label>
      <label>金额 <input id="ad-amt" style="width:100px" /></label>
      <label>支付账户 <input id="ad-acct" value="1001" style="width:90px" /></label>
      <label>备注 <input id="ad-memo" style="width:110px" /></label>
      <button class="btn sm" id="ad-new">新增借支</button>
    </div>
    <div class="toolbar">
      <span class="muted" style="font-size:12px">核销参数（支付/核销按今天记账）：</span>
      <label>冲账费用科目 <input id="ad-exp-acct" value="660201" style="width:90px" /></label>
      <label>冲账金额 <input id="ad-exp-amt" style="width:100px" placeholder="全额退回填0" /></label>
    </div><div id="ad-list" class="muted">加载中…</div>`;
  $("#ad-new").onclick = async () => {
    if (!$("#ad-emp").value.trim()) { toast("借支人必填", "err"); return; }
    try {
      await api("/funds/advances", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({
        date: $("#ad-date").value, employee: $("#ad-emp").value.trim(), purpose: $("#ad-purpose").value.trim(),
        amount: $("#ad-amt").value.trim(), pay_account: $("#ad-acct").value.trim(),
        expense_account: $("#ad-exp-acct").value.trim(), memo: $("#ad-memo").value.trim(),
      }) });
      toast("已新增借支单", "ok");
      for (const k of ["ad-emp", "ad-purpose", "ad-amt", "ad-memo"]) $("#" + k).value = "";
      load();
    } catch (e) { toast(e.message, "err"); }
  };
  await load();
  async function load() {
    try {
      const r = await api("/funds/advances");
      const rows = r.rows || [];
      const stMap = { approved: ["待支付", "warn"], paid: ["已支付", "tag"], settled: ["已核销", "ok"] };
      $("#ad-list").innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>编号</th><th>日期</th><th>借支人</th><th>事由</th><th class="num">金额</th><th>支付账户</th><th>状态</th><th></th></tr></thead><tbody>${rows.map((a) => {
            const s = stMap[a.status] || [a.status, ""];
            return `<tr><td>${esc(a.no)}</td><td>${esc(a.date)}</td><td>${esc(a.employee)}</td><td>${esc(a.purpose || "—")}</td>
              <td class="num">${fmt(a.amount)}</td><td>${esc(a.pay_account)}</td>
              <td><span class="tag ${s[1]}">${esc(s[0])}</span></td>
              <td class="row-actions">
                ${a.status === "approved" ? `<button class="btn ghost sm" data-ad-p="${a.id}">支付</button><button class="btn ghost sm" data-ad-d="${a.id}">删除</button>` : ""}
                ${a.status === "paid" ? `<button class="btn ghost sm" data-ad-s="${a.id}">核销</button>` : ""}
              </td></tr>`;
          }).join("")}</tbody></table>`
        : `<div class="muted">暂无借支单</div>`;
      $all("[data-ad-p]").forEach((b) => b.onclick = async () => {
        try { const x = await api(`/funds/advances/${b.dataset.adP}/pay`, { method: "POST" }); toast(x.voucher_id ? `已支付，生成凭证 #${x.voucher_id}` : "已支付", "ok"); load(); } catch (e) { toast(e.message, "err"); }
      });
      $all("[data-ad-s]").forEach((b) => b.onclick = async () => {
        const expAmt = $("#ad-exp-amt").value.trim();
        if (expAmt === "") { toast("请先填写冲账金额（全额退回填0）", "err"); return; }
        try {
          const x = await api(`/funds/advances/${b.dataset.adS}/settle`, { method: "POST", headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ expense_account: $("#ad-exp-acct").value.trim() || "660201", expense_amount: expAmt }) });
          toast(x.voucher_id ? `已核销，生成凭证 #${x.voucher_id}` : "已核销", "ok");
          $("#ad-exp-amt").value = "";
          load();
        } catch (e) { toast(e.message, "err"); }
      });
      $all("[data-ad-d]").forEach((b) => b.onclick = async () => { if (!(await confirmDialog("删除该借支单？", true))) return; try { await api(`/funds/advances/${b.dataset.adD}/delete`, { method: "POST" }); toast("已删除", "ok"); load(); } catch (e) { toast(e.message, "err"); } });
    } catch (e) { $("#ad-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  }
}

// 收付款单（对标金蝶收款单/付款单）：保存即出凭证并按往来单位 FIFO 自动核销
async function renderReceipts(body) {
  body.className = "";
  const kindSel = () => $("#rc-kind").value;
  const loadParties = async () => {
    let parties = [];
    try { parties = await api(`/aux?kind=${kindSel() === "receipt" ? "customer" : "supplier"}`); } catch (e) { parties = []; }
    const sel = $("#rc-party");
    if (!sel) return;
    sel.innerHTML = parties.length
      ? parties.map((p) => `<option value="${esc(p.code)}">${esc(p.code)} ${esc(p.name)}</option>`).join("")
      : `<option value="">（先到辅助档案新增${kindSel() === "receipt" ? "客户" : "供应商"}）</option>`;
    sel.disabled = !parties.length;
  };
  body.innerHTML = `<div class="toolbar">
      <label>日期 <input type="date" id="rc-date" value="${today()}" /></label>
      <label>类型 <select id="rc-kind"><option value="receipt">收款</option><option value="payment">付款</option></select></label>
      <label>资金账户 <input id="rc-fund" value="" placeholder="默认100201" style="width:100px" /></label>
      <label>往来单位 <select id="rc-party" style="width:170px"></select></label>
      <label>金额 <input id="rc-amt" style="width:110px" /></label>
      <label>备注 <input id="rc-memo" style="width:130px" /></label>
      <button class="btn primary" id="rc-new">新增收付款</button>
    </div>
    <div class="muted" style="font-size:12px;margin-bottom:6px">保存即生成记账凭证草稿（会计记账后入账，H-3 草稿不入余额），并按往来单位对未清挂账 FIFO 自动核销；往来单位选凭证辅助编码（C01/S01…）。</div>
    <div class="panel"><div style="display:flex;align-items:center;gap:8px"><h4 style="margin:0">收付款单</h4><span class="grow"></span>${sizeSel("rc-size")}<button class="btn ghost sm" id="rc-print">批量打印</button><button class="btn ghost sm" id="rc-printcfg">打印设置</button></div><div id="rc-list" class="muted" style="margin-top:8px">加载中…</div></div>`;
  $("#rc-kind").onchange = () => loadParties();
  $("#rc-print").onclick = () => runPrint("/api/funds/receipts/print-form", ".rc-chk", "receipt", PRINT_RECEIPT_FIELDS);
  $("#rc-printcfg").onclick = () => openPrintConfig("receipt", PRINT_RECEIPT_FIELDS);
  bindSize($("#rc-size"), "receipt", PRINT_RECEIPT_FIELDS);
  $("#rc-new").onclick = async () => {
    if (!$("#rc-party").value) { toast("请选择往来单位", "err"); return; }
    try {
      const r = await postJson("/funds/receipts", {
        date: $("#rc-date").value, kind: kindSel(), fund_account: $("#rc-fund").value.trim(),
        party: $("#rc-party").value, amount: $("#rc-amt").value.trim(), memo: $("#rc-memo").value.trim(),
      });
      toast(`已保存为待审核单（审核后生成凭证并自动核销）`, "ok");
      $("#rc-amt").value = ""; $("#rc-memo").value = "";
      load();
    } catch (e) { toast(e.message, "err"); }
  };
  await loadParties();
  await load();
  async function load() {
    try {
      const r = await api("/funds/receipts");
      const rows = r.rows || [];
      $("#rc-list").innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th style="width:26px"><input type="checkbox" id="rc-chkall" title="全选" /></th><th>单号</th><th>日期</th><th>类型</th><th>资金账户</th><th>往来单位</th><th class="num">金额</th><th>凭证</th><th>状态</th><th>备注</th><th></th></tr></thead><tbody>${rows.map((d) => `<tr>
            <td><input type="checkbox" class="rc-chk" value="${d.id}" /></td><td>${esc(d.no)}</td><td>${esc(d.date)}</td><td>${d.kind === "receipt" ? "收款" : "付款"}</td>
            <td>${esc(d.fund_account)}</td><td>${esc(d.party)}</td><td class="num">${fmt(d.amount)}</td>
            <td>${d.voucher_id ? `<a href="#" data-rc-v="${d.voucher_id}">凭证 #${d.voucher_id}</a>` : "—"}</td>
            <td>${d.status === "audited" ? '<span class="tag ok">已审核</span>' : '<span class="tag warn">待审核</span>'}<span data-wftag="receipt:${d.id}"></span></td>
            <td>${esc(d.memo || "")}</td>
            <td class="row-actions">${can("voucher_audit") || can("voucher_post") ? (d.status === "draft" ? `<button class="btn ghost sm" data-rc-audit="${d.id}">审核</button>` : `<button class="btn ghost sm" data-rc-unaudit="${d.id}">撤审</button>`) : ""}<button class="btn ghost sm" data-rc-d="${d.id}">删除</button></td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">暂无收付款单</div>`;
      fillWfTags($("#rc-list"));
      $all("[data-rc-v]").forEach((a) => a.onclick = (e) => { e.preventDefault(); openVoucherEditor(parseInt(a.dataset.rcV, 10)); });
      $all("[data-rc-d]").forEach((b) => b.onclick = async () => { if (!(await confirmDialog("删除该收付款单？其凭证需先作废或删除", true))) return; try { await api(`/funds/receipts/${b.dataset.rcD}/delete`, { method: "POST" }); toast("已删除", "ok"); load(); } catch (e) { toast(e.message, "err"); } });
      if ($("#rc-chkall")) $("#rc-chkall").onclick = (e) => { $all(".rc-chk").forEach((c) => { c.checked = e.target.checked; }); };
      $all("[data-rc-audit]").forEach((b) => b.onclick = async () => {
        try {
          const r = await postJson(`/funds/receipts/${b.dataset.rcAudit}/audit`, {});
          if (r.pending) { toast(`已审批 → 下一节点：${r.pending}`, "ok"); load(); return; }
          toast(`已审核，凭证 #${r.voucher_id}${r.settled ? `（自动核销 ${r.settled} 笔）` : ""}`, "ok");
          load();
        } catch (e) { toast(e.message, "err"); }
      });
      $all("[data-rc-unaudit]").forEach((b) => b.onclick = async () => {
        if (!(await confirmDialog("撤销审核？将删除其未记账凭证并清理核销配对，单据回到草稿。", true))) return;
        try { await postJson(`/funds/receipts/${b.dataset.rcUnaudit}/unaudit`, {}); toast("已撤销审核", "ok"); load(); } catch (e) { toast(e.message, "err"); }
      });
    } catch (e) { $("#rc-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  }
}

// 资金调拨（账户间转账）：建单（草稿）→ 审核出凭证（借 转入 / 贷 转出）→ 撤审/删除。
// 两端都是现金/银行科目，不涉及往来、不核销。
async function renderTransfers(body) {
  body.className = "";
  body.innerHTML = `<div class="toolbar">
      <label>日期 <input type="date" id="tf-date" value="${today()}" /></label>
      <label>转出 <select id="tf-from" style="width:190px"></select></label>
      <label>转入 <select id="tf-to" style="width:190px"></select></label>
      <label>金额 <input id="tf-amt" style="width:110px" /></label>
      <label>备注 <input id="tf-memo" style="width:150px" /></label>
      <button class="btn primary" id="tf-new">新增调拨</button>
    </div>
    <div class="muted" style="font-size:12px;margin-bottom:6px">保存为待审核单；审核后生成凭证：<b>借 转入账户 / 贷 转出账户</b>（账户间转账，不改往来账）。两端须为现金/银行科目且不能相同。</div>
    <div class="panel"><h4 style="margin:0">资金调拨单</h4><div id="tf-list" class="muted" style="margin-top:8px">加载中…</div></div>`;

  const accts = (await ensureAccounts()).filter((a) => a.is_cash || a.is_bank);
  const fill = (sel, def) => {
    const el = $(sel);
    if (!el) return;
    el.innerHTML = accts.length
      ? accts.map((a) => `<option value="${esc(a.code)}">${esc(a.code)} ${esc(a.name)}</option>`).join("")
      : `<option value="">（先到会计科目设现金/银行科目）</option>`;
    if (def && accts.some((a) => a.code === def)) el.value = def;
  };
  fill("#tf-from", "100201");
  fill("#tf-to", "1001");

  $("#tf-new").onclick = async () => {
    const from = $("#tf-from").value, to = $("#tf-to").value;
    if (!from || !to) { toast("请选择转出 / 转入账户", "err"); return; }
    if (from === to) { toast("转出与转入不能相同", "err"); return; }
    if (!($("#tf-amt").value || "").trim()) { toast("请填写金额", "err"); return; }
    try {
      await postJson("/funds/transfers", {
        date: $("#tf-date").value, from_account: from, to_account: to,
        amount: $("#tf-amt").value.trim(), memo: $("#tf-memo").value.trim(),
      });
      toast("已保存为待审核单（审核后生成凭证）", "ok");
      $("#tf-amt").value = ""; $("#tf-memo").value = "";
      load();
    } catch (e) { toast(e.message, "err"); }
  };
  await load();

  async function load() {
    try {
      const r = await api("/funds/transfers");
      const rows = r.rows || [];
      $("#tf-list").innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>单号</th><th>日期</th><th>转出</th><th>转入</th><th class="num">金额</th><th>凭证</th><th>状态</th><th>备注</th><th></th></tr></thead><tbody>${rows.map((d) => `<tr>
            <td>${esc(d.no)}</td><td>${esc(d.date)}</td>
            <td>${esc(d.from_account)}</td><td>${esc(d.to_account)}</td>
            <td class="num">${fmt(d.amount)}</td>
            <td>${d.voucher_id ? `<a href="#" data-tf-v="${d.voucher_id}">凭证 #${d.voucher_id}</a>` : "—"}</td>
            <td>${d.status === "audited" ? '<span class="tag ok">已审核</span>' : '<span class="tag warn">待审核</span>'}</td>
            <td>${esc(d.memo || "")}</td>
            <td class="row-actions">${can("voucher_audit") || can("voucher_post") ? (d.status === "draft" ? `<button class="btn ghost sm" data-tf-audit="${d.id}">审核</button>` : `<button class="btn ghost sm" data-tf-unaudit="${d.id}">撤审</button>`) : ""}<button class="btn ghost sm" data-tf-d="${d.id}">删除</button></td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">暂无资金调拨单</div>`;
      $all("[data-tf-v]").forEach((a) => a.onclick = (e) => { e.preventDefault(); openVoucherEditor(parseInt(a.dataset.tfV, 10)); });
      $all("[data-tf-d]").forEach((b) => b.onclick = async () => { if (!(await confirmDialog("删除该资金调拨单？其凭证需先作废或删除", true))) return; try { await api(`/funds/transfers/${b.dataset.tfD}/delete`, { method: "POST" }); toast("已删除", "ok"); load(); } catch (e) { toast(e.message, "err"); } });
      $all("[data-tf-audit]").forEach((b) => b.onclick = async () => {
        try {
          const r = await postJson(`/funds/transfers/${b.dataset.tfAudit}/audit`, {});
          toast(`已审核，凭证 #${r.voucher_id}`, "ok"); load();
        } catch (e) { toast(e.message, "err"); }
      });
      $all("[data-tf-unaudit]").forEach((b) => b.onclick = async () => {
        if (!(await confirmDialog("撤销审核？将删除其未记账凭证，单据回到草稿。", true))) return;
        try { await postJson(`/funds/transfers/${b.dataset.tfUnaudit}/unaudit`, {}); toast("已撤销审核", "ok"); load(); } catch (e) { toast(e.message, "err"); }
      });
    } catch (e) { $("#tf-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  }
}

// 支票登记簿（出纳备查簿，不入账；账务由凭证体现）
async function renderChecks(body) {
  body.className = "";
  body.innerHTML = `<div class="toolbar">
      <label>票号 <input id="ck-no" style="width:110px" /></label>
      <label>类型 <select id="ck-kind"><option value="transfer">转账支票</option><option value="cash">现金支票</option></select></label>
      <label>付款科目 <input id="ck-bank" value="100201" style="width:90px" /></label>
      <label>收款人 <input id="ck-payee" style="width:120px" /></label>
      <label>金额 <input id="ck-amt" style="width:100px" /></label>
      <label>开出日 <input type="date" id="ck-date" value="${today()}" /></label>
      <label>备注 <input id="ck-memo" style="width:110px" /></label>
      <button class="btn sm" id="ck-new">新增支票</button>
    </div><div id="ck-list" class="muted">加载中…</div>`;
  $("#ck-new").onclick = async () => {
    if (!$("#ck-no").value.trim()) { toast("支票号必填", "err"); return; }
    try {
      await api("/funds/checks", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({
        no: $("#ck-no").value.trim(), kind: $("#ck-kind").value, bank_account: $("#ck-bank").value.trim(),
        payee: $("#ck-payee").value.trim(), amount: $("#ck-amt").value.trim(),
        issued_date: $("#ck-date").value, memo: $("#ck-memo").value.trim(),
      }) });
      toast("已登记支票", "ok");
      for (const k of ["ck-no", "ck-payee", "ck-amt", "ck-memo"]) $("#" + k).value = "";
      load();
    } catch (e) { toast(e.message, "err"); }
  };
  await load();
  async function load() {
    try {
      const r = await api("/funds/checks");
      const rows = r.rows || [];
      $("#ck-list").innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>票号</th><th>类型</th><th>付款科目</th><th>收款人</th><th class="num">金额</th><th>开出日</th><th>状态</th><th></th></tr></thead><tbody>${rows.map((c) => `<tr>
            <td>${esc(c.no)}</td><td>${c.kind === "cash" ? "现金支票" : "转账支票"}</td>
            <td>${esc(c.bank_account)}</td><td>${esc(c.payee || "—")}</td>
            <td class="num">${fmt(c.amount)}</td><td>${esc(c.issued_date)}</td>
            <td><span class="tag ${c.status === "void" ? "warn" : "ok"}">${c.status === "void" ? "已作废" : "已开出"}</span></td>
            <td class="row-actions">
              <button class="btn ghost sm" data-ck-s="${c.id}" data-to="${c.status === "void" ? "issued" : "void"}">${c.status === "void" ? "恢复" : "作废"}</button>
              <button class="btn ghost sm" data-ck-d="${c.id}">删除</button>
            </td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">暂无支票记录</div>`;
      $all("[data-ck-s]").forEach((b) => b.onclick = async () => { try { await api(`/funds/checks/${b.dataset.ckS}/status`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ status: b.dataset.to }) }); toast("已更新", "ok"); load(); } catch (e) { toast(e.message, "err"); } });
      $all("[data-ck-d]").forEach((b) => b.onclick = async () => { if (!(await confirmDialog("删除该支票记录？", true))) return; try { await api(`/funds/checks/${b.dataset.ckD}/delete`, { method: "POST" }); toast("已删除", "ok"); load(); } catch (e) { toast(e.message, "err"); } });
    } catch (e) { $("#ck-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  }
}

// 出纳交接班：交班快照（现金/银行结存、在库票据、未日清）+ 接班人确认
async function renderCashShifts(body) {
  body.className = "";
  const canWrite = can("cashier_sign");
  body.innerHTML = `<div class="toolbar">
      <label>日期 <input type="date" id="cs-date" value="${today()}" /></label>
      <label>接班人 <input id="cs-to" style="width:120px" placeholder="留空 = 待定" /></label>
      <label>备注 <input id="cs-memo" style="width:180px" /></label>
      <button class="btn sm primary" id="cs-new">新建交班单</button>
    </div>
    <div class="muted" style="font-size:12px;margin-bottom:8px">交班单快照当日现金/银行结存（仅已记账口径）、在库票据与未日清账户数；由接班人确认，交班人不能自确认。</div>
    <div id="cs-table" class="muted">加载中…</div>`;
  const load = async () => {
    try {
      const r = await api("/funds/shifts");
      const rows = r.rows || [];
      const stMap = { open: ["待确认", "warn"], confirmed: ["已确认", "ok"], cancelled: ["已取消", ""] };
      $("#cs-table").innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>日期</th><th>交班人</th><th>接班人</th><th class="num">现金结存</th><th class="num">银行结存</th><th class="num">在库票据</th><th class="num">未日清</th><th>状态</th><th>确认</th><th>备注</th><th></th></tr></thead><tbody>${rows.map((s) => {
            const st = stMap[s.status] || [s.status, ""];
            const acts = s.status === "open" && canWrite
              ? `<button class="btn ghost sm" data-cs-confirm="${s.id}">确认接班</button> <button class="btn ghost sm" data-cs-cancel="${s.id}">取消</button>` : "";
            return `<tr>
              <td>${esc(s.date)}</td><td>${esc(s.from_user)}</td><td>${esc(s.to_user || "—")}</td>
              <td class="num">${fmt(s.cash_balance)}</td><td class="num">${fmt(s.bank_balance)}</td>
              <td class="num">${s.bill_count} 张 / ${fmt(s.bill_amount)}</td>
              <td class="num">${s.uncleared}</td>
              <td><span class="tag ${st[1]}">${st[0]}</span></td>
              <td>${s.confirmed_by ? `${esc(s.confirmed_by)}<div class="muted" style="font-size:11px">${esc(s.confirmed_at || "")}</div>` : `<span class="muted">—</span>`}</td>
              <td>${esc(s.memo)}</td><td class="row-actions">${acts}</td></tr>`;
          }).join("")}</tbody></table>`
        : `<div class="muted">暂无交班记录</div>`;
      $all("[data-cs-confirm]").forEach((b) => b.onclick = async () => {
        if (!(await confirmDialog("确认接班？确认后该交班单不可再修改。"))) return;
        try {
          await api(`/funds/shifts/${b.dataset.csConfirm}/confirm`, { method: "POST", headers: { "Content-Type": "application/json" }, body: "{}" });
          toast("已确认接班", "ok");
          load();
        } catch (e) { toast(e.message, "err"); }
      });
      $all("[data-cs-cancel]").forEach((b) => b.onclick = async () => {
        if (!(await confirmDialog("取消该交班单？"))) return;
        try {
          await api(`/funds/shifts/${b.dataset.csCancel}/cancel`, { method: "POST", headers: { "Content-Type": "application/json" }, body: "{}" });
          toast("已取消", "ok");
          load();
        } catch (e) { toast(e.message, "err"); }
      });
    } catch (e) { $("#cs-table").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  if (canWrite) {
    $("#cs-new").onclick = async () => {
      try {
        await api("/funds/shifts", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ date: $("#cs-date").value, to_user: $("#cs-to").value.trim(), memo: $("#cs-memo").value.trim() }) });
        toast("交班单已创建，等待接班人确认", "ok");
        $("#cs-to").value = ""; $("#cs-memo").value = "";
        load();
      } catch (e) { toast(e.message, "err"); }
    };
  } else {
    const b = $("#cs-new"); if (b) b.remove();
  }
  load();
}

// 出纳日记账：本期已记账逐笔滚动 + 日清标记 + 收付登记（跳凭证录入预填科目）
async function renderJournal(body) {
  body.className = "";
  if (!state.journalAcct) state.journalAcct = "1001";
  body.innerHTML = `<div class="toolbar">
      <label>科目 <input id="jn-acct" value="${esc(state.journalAcct)}" style="width:90px" /></label>
      <label>日清日期 <input type="date" id="jn-date" value="${today()}" /></label>
      <button class="btn sm" id="jn-clear">标记日清</button>
      <button class="btn sm ghost" id="jn-unclear">取消日清</button>
      <button class="btn sm" id="jn-receipt">收款登记</button>
      <button class="btn sm" id="jn-payment">付款登记</button>
      <button class="btn sm ghost" id="jn-print">打印日记账</button>
    </div><div id="jn-table" class="muted">加载中…</div>`;
  const period = ymm(state.current || "");
  const y = parseInt(period.slice(0, 4), 10), m = parseInt(period.slice(4, 6), 10);
  const lastDay = new Date(y, m, 0).getDate();
  const pFrom = `${period.slice(0, 4)}-${period.slice(4, 6)}-01`;
  const pTo = `${period.slice(0, 4)}-${period.slice(4, 6)}-${String(lastDay).padStart(2, "0")}`;
  const acct = () => $("#jn-acct").value.trim() || "1001";
  const load = async () => {
    const a = acct();
    state.journalAcct = a;
    try {
      const [rows, cleared] = await Promise.all([
        api(`/ledger/journal?code=${encodeURIComponent(a)}&from=${period}&to=${period}`),
        api(`/funds/day-clear?account=${encodeURIComponent(a)}&from=${pFrom}&to=${pTo}`),
      ]);
      const cset = new Set(cleared.dates || []);
      $("#jn-table").innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>日期</th><th>凭证号</th><th>摘要</th><th>对方科目</th><th class="num">借方</th><th class="num">贷方</th><th class="num">余额</th><th>日清</th></tr></thead><tbody>${rows.map((r) => {
            const ds = String(r.date).slice(0, 10);
            return `<tr><td>${esc(ds)}</td><td>${esc(r.voucher_no)}</td><td>${esc(r.summary)}</td><td>${esc(r.opposite_accounts || "")}</td><td class="num">${fmt(r.debit)}</td><td class="num">${fmt(r.credit)}</td><td class="num">${fmt(r.balance)}</td><td>${cset.has(ds) ? '<span class="tag ok">已日清</span>' : ""}</td></tr>`;
          }).join("")}</tbody></table>`
        : `<div class="muted">该科目本期无已记账记录</div>`;
    } catch (e) { $("#jn-table").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  $("#jn-acct").onchange = () => load();
  const setClear = async (clear) => {
    try {
      await api("/funds/day-clear", { method: "POST", headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ account_code: acct(), date: $("#jn-date").value, clear }) });
      toast(clear ? "已标记日清" : "已取消日清", "ok");
      load();
    } catch (e) { toast(e.message, "err"); }
  };
  $("#jn-clear").onclick = () => setClear(true);
  $("#jn-unclear").onclick = () => setClear(false);
  $("#jn-print").onclick = () => printPreview(`出纳日记账 ${acct()} ${period}`, $("#jn-table").querySelector("table"));
  const jump = (dir) => {
    state.pendingCash = { account: acct(), dir };
    document.querySelector('.nav-item[data-view="vouchers"]').click();
    setTimeout(() => { const b = $("#new-v"); if (b) b.click(); }, 150);
  };
  $("#jn-receipt").onclick = () => jump("debit");
  $("#jn-payment").onclick = () => jump("credit");
  await load();
}

// 现金盘点（出纳）：账面按资金日报（按日）口径快照，差异一键出盘盈盘亏凭证
async function renderCashCount(body) {
  body.className = "";
  body.innerHTML = `<div class="toolbar">
      <label>日期 <input type="date" id="cc-date" value="${today()}" /></label>
      <label>科目 <input id="cc-acct" value="1001" style="width:90px" /></label>
      <label>实盘金额 <input id="cc-counted" style="width:110px" /></label>
      <label>备注 <input id="cc-memo" style="width:150px" /></label>
      <button class="btn sm" id="cc-new">新增盘点</button>
    </div><div id="cc-list" class="muted">加载中…</div>`;
  $("#cc-new").onclick = async () => {
    try {
      const r = await api("/funds/cash-counts", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({
        date: $("#cc-date").value, account_code: $("#cc-acct").value.trim(),
        counted: $("#cc-counted").value.trim(), memo: $("#cc-memo").value.trim(),
      }) });
      toast(`账面 ${fmt(r.book_amount)}，差异 ${fmt(r.diff)}`, "ok");
      $("#cc-counted").value = ""; $("#cc-memo").value = "";
      load();
    } catch (e) { toast(e.message, "err"); }
  };
  await load();
  async function load() {
    try {
      const r = await api("/funds/cash-counts");
      const rows = r.rows || [];
      $("#cc-list").innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>日期</th><th>科目</th><th class="num">账面余额</th><th class="num">实盘金额</th><th class="num">差异</th><th>备注</th><th></th></tr></thead><tbody>${rows.map((c) => `<tr>
            <td>${esc(c.date)}</td><td>${esc(c.account_code)}</td>
            <td class="num">${fmt(c.book_amount)}</td><td class="num">${fmt(c.counted)}</td>
            <td class="num">${Number(c.diff) === 0 ? fmt(c.diff) : `<b style="color:${Number(c.diff) > 0 ? "var(--ok)" : "var(--err)"}">${Number(c.diff) > 0 ? "+" : ""}${fmt(c.diff)}</b>`}</td>
            <td>${esc(c.memo || "")}</td>
            <td class="row-actions">
              ${!c.voucher_id && Number(c.diff) !== 0 ? `<button class="btn ghost sm" data-cc-v="${c.id}">生成凭证</button>` : ""}
              ${c.voucher_id ? `<span class="tag ok">已出凭证</span>` : ""}
              <button class="btn ghost sm" data-cc-d="${c.id}">删除</button>
            </td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">暂无盘点记录</div>`;
      $all("[data-cc-v]").forEach((b) => b.onclick = async () => { try { const x = await api(`/funds/cash-counts/${b.dataset.ccV}/voucher`, { method: "POST" }); toast(`已生成盘盈盘亏凭证 #${x.id}`, "ok"); load(); } catch (e) { toast(e.message, "err"); } });
      $all("[data-cc-d]").forEach((b) => b.onclick = async () => { if (!(await confirmDialog("删除该盘点记录？", true))) return; try { await api(`/funds/cash-counts/${b.dataset.ccD}/delete`, { method: "POST" }); toast("已删除", "ok"); load(); } catch (e) { toast(e.message, "err"); } });
    } catch (e) { $("#cc-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  }
}

async function renderBills(body) {
  body.className = "";
  const toolbar = `<div class="toolbar">
      <button class="btn sm" id="bill-new">新增票据</button>
      <label>类型 <select id="bill-kind"><option value="">全部</option><option value="receivable">应收</option><option value="payable">应付</option></select></label>
    </div><div id="bill-list" class="muted">加载中…</div>`;
  body.innerHTML = toolbar;
  $("#bill-new").onclick = () => openBillEditor(null);
  $("#bill-kind").onchange = loadBills;
  await loadBills();
  async function loadBills() {
    const kind = $("#bill-kind").value;
    try {
      const r = await api(`/funds/bills?kind=${encodeURIComponent(kind)}`);
      const rows = r.rows || [];
      const stMap = { in_hand: ["在库", "ok"], endorsed: ["已背书", "warn"], discounted: ["已贴现", "warn"], matured: ["已到期", "err"], settled: ["已兑付", "ok"] };
      $("#bill-list").innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>类型</th><th>票据号</th><th>出票日</th><th>到期日</th><th>对方单位</th><th>承兑银行</th><th class="num">金额</th><th>状态</th><th></th></tr></thead>
          <tbody>${rows.map((b) => {
            const s = stMap[b.status] || [b.status, ""];
            return `<tr><td>${b.kind === "receivable" ? "应收" : "应付"}</td><td>${esc(b.no)}</td>
              <td>${esc(b.issue_date)}</td><td>${esc(b.due_date)}</td><td>${esc(b.counterpart || "—")}</td>
              <td>${esc(b.bank || "—")}</td><td class="num">${fmt(b.amount)}</td>
              <td><span class="tag ${s[1]}">${esc(s[0])}</span></td>
              <td class="row-actions">
                <button class="btn ghost sm" data-bill="${b.id}">打开</button>
                ${b.status === "in_hand" ? `<button class="btn ghost sm" data-bill-act="${b.id}" data-to="endorsed">背书</button>
                <button class="btn ghost sm" data-bill-act="${b.id}" data-to="discounted">贴现</button>
                <button class="btn ghost sm" data-bill-act="${b.id}" data-to="settled">兑付</button>` : ""}
                ${["discounted", "endorsed", "settled"].includes(b.status) && !b.voucher_id ? `<button class="btn ghost sm" data-bill-v="${b.id}">生成凭证</button>` : ""}
              </td></tr>`;
          }).join("")}</tbody></table>`
        : `<div class="muted">暂无票据</div>`;
      $all("[data-bill]").forEach((b) => b.onclick = () => openBillEditor(parseInt(b.dataset.bill, 10)));
      $all("[data-bill-act]").forEach((b) => b.onclick = async () => {
        try {
          const r = await api(`/funds/bills/${b.dataset.billAct}/status`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ status: b.dataset.to, date: today() }) });
          toast(r.voucher_id ? `已更新，生成凭证 #${r.voucher_id}` : "已更新", "ok"); loadBills();
        } catch (e) { toast(e.message, "err"); }
      });
      $all("[data-bill-v]").forEach((b) => b.onclick = async () => {
        try { const r = await api(`/funds/bills/${b.dataset.billV}/voucher`, { method: "POST" }); toast(`已生成凭证 #${r.id}`, "ok"); loadBills(); } catch (e) { toast(e.message, "err"); }
      });
    } catch (e) { $("#bill-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  }
}

async function openBillEditor(id) {
  let b = { id: 0, kind: "receivable", no: "", issue_date: today(), due_date: today(), counterpart: "", bank: "", amount: "0", status: "in_hand", memo: "" };
  if (id) { try { const r = await api("/funds/bills"); b = (r.rows || []).find((x) => x.id === id) || b; } catch (e) { toast(e.message, "err"); return; } }
  const mask = modal(`
    <h3>${id ? "编辑票据" : "新增票据"}</h3>
    <div class="field"><label>类型</label><select id="b-kind">
      <option value="receivable" ${b.kind === "receivable" ? "selected" : ""}>应收票据</option>
      <option value="payable" ${b.kind === "payable" ? "selected" : ""}>应付票据</option></select></div>
    <div class="field"><label>票据号</label><input id="b-no" value="${esc(b.no)}" /></div>
    <div class="field"><label>出票日</label><input id="b-issue" type="date" value="${esc(b.issue_date)}" /></div>
    <div class="field"><label>到期日</label><input id="b-due" type="date" value="${esc(b.due_date)}" /></div>
    <div class="field"><label>对方单位</label><input id="b-cp" value="${esc(b.counterpart)}" /></div>
    <div class="field"><label>承兑银行</label><input id="b-bank" value="${esc(b.bank)}" /></div>
    <div class="field"><label>金额</label><input id="b-amt" value="${esc(b.amount)}" /></div>
    <div class="field"><label>备注</label><input id="b-memo" value="${esc(b.memo)}" /></div>
    <div class="foot"><button class="btn" id="b-save">保存</button>${id ? `<button class="btn danger ghost" id="b-del">删除</button>` : ""}<button class="btn ghost" id="b-cancel">取消</button></div>`);
  $("#b-cancel", mask).onclick = closeModal;
  $("#b-save", mask).onclick = async () => {
    const body2 = {
      id: b.id, kind: $("#b-kind", mask).value, no: $("#b-no", mask).value.trim(),
      period: ymm(state.current || ""), issue_date: $("#b-issue", mask).value,
      due_date: $("#b-due", mask).value, counterpart: $("#b-cp", mask).value.trim(),
      bank: $("#b-bank", mask).value.trim(), amount: $("#b-amt", mask).value.trim(),
      status: "in_hand", memo: $("#b-memo", mask).value.trim(),
    };
    if (!body2.no || !body2.issue_date || !body2.due_date) { toast("票据号与日期必填", "err"); return; }
    try { await api("/funds/bills", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body2) }); toast("已保存", "ok"); closeModal(); renderBills($("#funds-body")); } catch (e) { toast(e.message, "err"); }
  };
  if (id) $("#b-del", mask).onclick = async () => { if (!(await confirmDialog("删除该票据？", true))) return; try { await api(`/funds/bills/${id}/delete`, { method: "POST" }); toast("已删除", "ok"); closeModal(); renderBills($("#funds-body")); } catch (e) { toast(e.message, "err"); } };
}

async function renderLoans(body) {
  body.className = "";
  body.innerHTML = `<div class="toolbar"><button class="btn sm" id="loan-new">新增融资</button>
      <label>类型 <select id="loan-kind"><option value="">全部</option><option value="borrow">借款</option><option value="lend">放款</option></select></label>
    </div><div id="loan-list" class="muted">加载中…</div>`;
  $("#loan-new").onclick = () => openLoanEditor(null);
  $("#loan-kind").onchange = loadLoans;
  await loadLoans();
  async function loadLoans() {
    const kind = $("#loan-kind").value;
    try {
      const r = await api(`/funds/loans?kind=${encodeURIComponent(kind)}`);
      const rows = r.rows || [];
      $("#loan-list").innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>类型</th><th>编号</th><th>机构</th><th class="num">本金</th><th class="num">年利率%</th><th>起息日</th><th>到期日</th><th>状态</th><th></th></tr></thead>
          <tbody>${rows.map((l) => `<tr>
            <td>${l.kind === "borrow" ? "借款" : "放款"}</td><td>${esc(l.no)}</td><td>${esc(l.bank || "—")}</td>
            <td class="num">${fmt(l.principal)}</td><td class="num">${fmt(l.rate_pct)}</td>
            <td>${esc(l.start_date)}</td><td>${esc(l.end_date)}</td>
            <td><span class="tag ${l.status === "active" ? "warn" : "ok"}">${l.status === "active" ? "存续" : "已结清"}</span></td>
            <td class="row-actions">
              <button class="btn ghost sm" data-loan="${l.id}">打开</button>
              ${l.status === "active" ? `<button class="btn ghost sm" data-loan-settle="${l.id}">结清</button>` : ""}
              ${l.status === "active" && !l.voucher_id ? `<button class="btn ghost sm" data-loan-v="${l.id}">到账凭证</button>` : ""}
              ${l.status !== "active" && !l.settle_voucher_id ? `<button class="btn ghost sm" data-loan-v="${l.id}">还本凭证</button>` : ""}
            </td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">暂无融资记录</div>`;
      $all("[data-loan]").forEach((b) => b.onclick = () => openLoanEditor(parseInt(b.dataset.loan, 10)));
      $all("[data-loan-settle]").forEach((b) => b.onclick = async () => {
        if (!(await confirmDialog("结清该笔融资？", true))) return;
        try {
          const r = await api(`/funds/loans/${b.dataset.loanSettle}/settle`, { method: "POST" });
          toast(r.voucher_id ? `已结清，生成还本凭证 #${r.voucher_id}` : "已结清", "ok"); loadLoans();
        } catch (e) { toast(e.message, "err"); }
      });
      $all("[data-loan-v]").forEach((b) => b.onclick = async () => {
        try { const r = await api(`/funds/loans/${b.dataset.loanV}/voucher`, { method: "POST" }); toast(`已生成凭证 #${r.id}`, "ok"); loadLoans(); } catch (e) { toast(e.message, "err"); }
      });
    } catch (e) { $("#loan-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  }
}

async function openLoanEditor(id) {
  let l = { id: 0, kind: "borrow", no: "", bank: "", principal: "0", rate_pct: "0", start_date: today(), end_date: today(), status: "active", memo: "" };
  if (id) { try { const r = await api("/funds/loans"); l = (r.rows || []).find((x) => x.id === id) || l; } catch (e) { toast(e.message, "err"); return; } }
  const mask = modal(`
    <h3>${id ? "编辑融资" : "新增融资"}</h3>
    <div class="field"><label>类型</label><select id="l-kind">
      <option value="borrow" ${l.kind === "borrow" ? "selected" : ""}>借款</option>
      <option value="lend" ${l.kind === "lend" ? "selected" : ""}>放款</option></select></div>
    <div class="field"><label>编号</label><input id="l-no" value="${esc(l.no)}" /></div>
    <div class="field"><label>机构</label><input id="l-bank" value="${esc(l.bank)}" /></div>
    <div class="field"><label>本金</label><input id="l-pr" value="${esc(l.principal)}" /></div>
    <div class="field"><label>年利率(%)</label><input id="l-rate" value="${esc(l.rate_pct)}" /></div>
    <div class="field"><label>起息日</label><input id="l-start" type="date" value="${esc(l.start_date)}" /></div>
    <div class="field"><label>到期日</label><input id="l-end" type="date" value="${esc(l.end_date)}" /></div>
    <div class="field"><label>备注</label><input id="l-memo" value="${esc(l.memo)}" /></div>
    <div class="foot"><button class="btn" id="l-save">保存</button>${id ? `<button class="btn danger ghost" id="l-del">删除</button>` : ""}<button class="btn ghost" id="l-cancel">取消</button></div>`);
  $("#l-cancel", mask).onclick = closeModal;
  $("#l-save", mask).onclick = async () => {
    const body2 = {
      id: l.id, kind: $("#l-kind", mask).value, no: $("#l-no", mask).value.trim(),
      bank: $("#l-bank", mask).value.trim(), principal: $("#l-pr", mask).value.trim(),
      rate_pct: $("#l-rate", mask).value.trim(), start_date: $("#l-start", mask).value,
      end_date: $("#l-end", mask).value, status: "active", memo: $("#l-memo", mask).value.trim(),
    };
    if (!body2.no || !body2.start_date || !body2.end_date) { toast("编号与日期必填", "err"); return; }
    try { await api("/funds/loans", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body2) }); toast("已保存", "ok"); closeModal(); renderLoans($("#funds-body")); } catch (e) { toast(e.message, "err"); }
  };
  if (id) $("#l-del", mask).onclick = async () => { if (!(await confirmDialog("删除该笔融资？", true))) return; try { await api(`/funds/loans/${id}/delete`, { method: "POST" }); toast("已删除", "ok"); closeModal(); renderLoans($("#funds-body")); } catch (e) { toast(e.message, "err"); } };
}
