// FinBook Web 前端 — 多栏账、汇总表、利润表（多维）、月度简报、维度利润、财务比率、生产计划/物料需求/工艺、审批、票据、归档、预算版本
//
// 这一份是 `crates/finweb/static/app.js` 拆分出来的（原单文件 14k 行、295 个顶层函数）。
// 拆分方式：**经典 script，按原始行序切分，不重排任何代码**——顶层 function/const 的
// 提升与声明顺序完全保持原样，所以跨文件不存在 TDZ/未定义问题；只有 `VIEWS` 路由表
// 因为引用全部视图函数，被挪到了最后加载的 app-hr.js。
// 加载顺序由 `index.html` 的 <script> 顺序决定（也是 `main.rs` 里 asset_version 的清单顺序）。
// 新增文件必须同步三处：index.html、main.rs 的 asset_version 清单、tools/lib-app-src.js 的解析（自动）。
// ===========================================================================
// 多栏账
// ===========================================================================
async function viewMultiColumn(main) {
  main.innerHTML = `<h2>多栏账</h2>
    <div class="toolbar">
      <label>主科目 <input id="mc-main" value="6602" style="width:90px" /></label>
      <label>栏目(逗号分隔) <input id="mc-cols" value="660201,660202,660203" style="width:220px" /></label>
      <label>期间 <input id="mc-period" value="${esc(state.current)}" style="width:90px" /></label>
      <button class="btn primary" id="mc-run">查询</button>
      <button class="btn ghost sm" id="mc-run-print">打印预览</button>
    </div>
    <div id="mc-result" class="muted">填写条件后点击查询</div>`;
  $("#mc-run-print").addEventListener("click", () => { const el = $("#mc-result").querySelector("table"); printPreview("多栏账", el); });
  $("#mc-run").addEventListener("click", async () => {
    const mainCode = $("#mc-main").value.trim();
    const cols = $("#mc-cols").value.split(",").map((s) => s.trim()).filter(Boolean);
    const p = $("#mc-period").value.trim();
    if (!mainCode || !cols.length) { toast("请填写主科目与栏目", "err"); return; }
    try {
      const r = await api(`/reports/multi-column?main=${encodeURIComponent(mainCode)}&cols=${encodeURIComponent(cols.join(","))}&from=${encodeURIComponent(p)}&to=${encodeURIComponent(p)}`);
      const rows = r.rows || [];
      const head = ["日期", "凭证号", "摘要", "发生额", ...cols, "余额"];
      $("#mc-result").innerHTML = `<table><thead><tr>${head.map((h) => `<th>${esc(h)}</th>`).join("")}</tr></thead>
        <tbody>${rows.map((x) => `<tr><td>${esc(x.date)}</td><td>${esc(x.voucher_no)}</td><td>${esc(x.summary)}</td>
          <td class="r">${fmt(x.amount)}</td>${cols.map((_, i) => `<td class="r">${fmt(x.cols[i])}</td>`).join("")}
          <td class="r">${fmt(x.balance)}</td></tr>`).join("")}</tbody></table>`;
    } catch (e) { toast(e.message, "err"); }
  });
}

// ===========================================================================
// 摘要汇总表
// ===========================================================================
async function viewSummaryTable(main) {
  main.innerHTML = `<h2>摘要汇总表</h2>
    <div class="toolbar">
      <label>期间 <input id="st-period" value="${esc(state.current)}" style="width:90px" /></label>
      <button class="btn primary" id="st-run">查询</button>
      <button class="btn ghost sm" id="st-run-print">打印预览</button>
    </div>
    <div id="st-result" class="muted">填写期间后点击查询</div>`;
  $("#st-run-print").addEventListener("click", () => { const el = $("#st-result").querySelector("table"); printPreview("摘要汇总表", el); });
  $("#st-run").addEventListener("click", async () => {
    const p = $("#st-period").value.trim();
    try {
      const r = await api(`/reports/summary-table?from=${encodeURIComponent(p)}&to=${encodeURIComponent(p)}`);
      const rows = r.rows || [];
      $("#st-result").innerHTML = `<table><thead><tr><th>摘要</th><th>凭证张数</th><th>借方发生额</th><th>贷方发生额</th></tr></thead>
        <tbody>${rows.map((x) => `<tr><td>${esc(x.summary)}</td><td class="r">${x.voucher_count}</td><td class="r">${fmt(x.debit)}</td><td class="r">${fmt(x.credit)}</td></tr>`).join("")}</tbody></table>`;
    } catch (e) { toast(e.message, "err"); }
  });
}

// ===========================================================================
// 财务指标
// ===========================================================================
// 多维损益：按辅助核算维度（部门 / 项目 / 客户…）看利润
//
// 桌面端一直有这张表，Web 端到 v1.8.62 才补上。取数在后端
// `findb::mgmt::dim_profit_report`，与桌面端是同一个函数，口径不会分叉。
// 只做「直接归属」：分录挂了哪个部门就算哪个部门，没挂的进「未分配」——
// 分摊率每家公司都不一样，编一个出来只会把账算歪。
// 月份利润分析表：照抄厂里那张表（序号/科目/行次/金额/备注），按行取数。
//
// 厂里那张表的名目在标准科目表里对不上（「化工」不是一个科目），所以行是数据：
// 每行挂若干科目，金额 = 这些科目本期发生额之和。没挂科目的行一键建同名科目。
async function viewProfitSheet(main) {
  main.innerHTML = `<h2>利润分析表</h2>
    <div class="toolbar">
      <label>期间 <input id="ps-period" value="${esc(state.current)}" style="width:90px" /></label>
      <button class="btn primary" id="ps-run">出表</button>
      <button class="btn ghost sm" id="ps-accounts" title="给还没挂科目的行，在 5101 制造费用下建一个同名末级科目">按这张表生成科目</button>
      <button class="btn ghost sm" id="ps-auto" title="按你现有科目的名字，把这张表的行挂上去（先给你看，再决定）">自动挂科目</button>
      <button class="btn ghost sm" id="ps-edit">编辑行</button>
      <button class="btn ghost sm" id="ps-print">打印预览</button>
    </div>
    <div class="muted" style="margin:6px 0">这张表照抄厂里在用的格式。金额从挂的科目本期发生额取，不用手工填。</div>
    <div id="ps-warn" class="muted" style="margin:6px 0"></div>
    <div id="ps-result" class="muted">加载中…</div>
    <div id="ps-rows" style="margin-top:14px"></div>`;
  let last = null;

  const render = () => {
    if (!last) return;
    const rows = last.rows || [];
    // 行次：收入行从 1 起、支出行也从 1 起（厂里那张表就是这么编的）
    let revNo = 0;
    let expNo = 0;
    const body = rows.map((r) => {
      const isRev = (r.label || "").startsWith("收入");
      const no = isRev ? ++revNo : ++expNo;
      const qty = r.qty ? `<span class="muted" style="margin-left:6px">产量 ${esc(r.qty)}</span>` : "";
      const unm = r.unmapped ? ` <span class="neg" title="这一行还没挂科目，金额取不到">未挂科目</span>` : "";
      return `<tr><td class="r">${r.seq}</td><td>${esc(r.label)}${unm}${qty}</td><td class="r">${no}</td>
        <td class="r">${esc(r.amount)}</td><td class="muted">${esc(r.hint || "")}</td></tr>`;
    }).join("");
    const profit = Number(String(last.profit ?? "").replace(/,/g, "")) || 0;
    $("#ps-result").innerHTML = `<table class="grid">
      <thead><tr><th class="r" style="width:60px">序号</th><th>科目</th><th class="r" style="width:60px">行次</th>
        <th class="r" style="width:130px">金额（元）</th><th>备注</th></tr></thead>
      <tbody>${body}</tbody>
      <tfoot>
        <tr><th colspan="3">收入合计</th><th class="r">${esc(last.revenue)}</th><th></th></tr>
        <tr><th colspan="3">支出合计</th><th class="r">${esc(last.expense)}</th><th></th></tr>
        <tr><th colspan="3">净利润（亏损以「-」号填写）</th><th class="r ${profit < 0 ? "neg" : "pos"}">${esc(last.profit)}</th><th></th></tr>
      </tfoot></table>`;
    const n = last.unmapped || 0;
    $("#ps-warn").innerHTML = n
      ? `<span class="neg">有 ${n} 行还没挂科目</span>：你的账里这些费用记在哪个科目？点「自动挂科目」按现有科目名对一遍，或「按这张表生成科目」新建，也可以点「编辑行」手工填。`
      : "";
  };

  const run = async () => {
    const p = $("#ps-period").value.trim();
    try {
      last = await api(`/profit-sheet/report?period=${encodeURIComponent(p)}`);
      render();
    } catch (e) { toast(e.message, "err"); }
  };

  const loadRows = async () => {
    try {
      const r = await api("/profit-sheet");
      const rows = r.rows || [];
      $("#ps-rows").innerHTML = `<div class="panel"><div class="muted" style="margin-bottom:6px">
        行的口径：每行挂的科目决定它取哪些发生额。同一科目只能挂一行，否则合计会翻倍。</div>
        <table class="grid"><thead><tr><th class="r" style="width:60px">序号</th><th>科目</th><th>类型</th>
          <th>挂的科目</th><th>备注</th><th style="width:120px">操作</th></tr></thead><tbody>
        ${rows.map((x) => `<tr><td class="r">${x.seq}</td><td>${esc(x.label)}</td>
          <td>${x.kind === "revenue" ? "收入" : "支出"}</td>
          <td class="mono">${esc(x.accounts || "—")}</td>
          <td class="muted">${esc(x.hint || "")}</td>
          <td><button class="btn ghost sm" data-ps-edit="${x.id}">编辑</button>
            <button class="btn ghost sm" data-ps-del="${x.id}">删</button></td></tr>`).join("")}
        </tbody></table>
        <div class="toolbar" style="margin-top:8px"><button class="btn ghost sm" id="ps-add">新增一行</button></div></div>`;
      $("#ps-add").addEventListener("click", () => openEditor(null));
      $all("[data-ps-edit]").forEach((b) => b.addEventListener("click", () => {
        openEditor(rows.find((x) => String(x.id) === b.dataset.psEdit));
      }));
      $all("[data-ps-del]").forEach((b) => b.addEventListener("click", async () => {
        if (!(await confirmDialog("删掉这一行？出表时就不再显示。", true))) return;
        try {
          await api(`/profit-sheet/${b.dataset.psDel}`, { method: "DELETE" });
          toast("已删除");
          await loadRows();
          await run();
        } catch (e) { toast(e.message, "err"); }
      }));
    } catch (e) { toast(e.message, "err"); }
  };

  const openEditor = (row) => {
    const r = row || { id: 0, seq: 0, label: "", hint: "", kind: "expense", accounts: "" };
    modal(`<h3>${row ? "编辑" : "新增"}一行</h3>
      <div class="form">
        <label>序号 <input id="pe-seq" value="${r.seq || ""}" style="width:70px" /></label>
        <label>科目（行名） <input id="pe-label" value="${esc(r.label)}" style="width:180px" /></label>
        <label>类型 <select id="pe-kind">
          <option value="expense"${r.kind === "expense" ? " selected" : ""}>支出</option>
          <option value="revenue"${r.kind === "revenue" ? " selected" : ""}>收入</option>
        </select></label>
        <label>挂的科目 <input id="pe-accounts" value="${esc(r.accounts || "")}" style="width:220px" placeholder="510106,660209" /></label>
        <label>备注 <input id="pe-hint" value="${esc(r.hint || "")}" style="width:260px" /></label>
      </div>
      <div class="toolbar" style="margin-top:10px">
        <button class="btn primary" id="pe-save">保存</button>
        <button class="btn ghost" id="pe-cancel">取消</button>
      </div>`);
    $("#pe-cancel").addEventListener("click", () => closeModal());
    $("#pe-save").addEventListener("click", async () => {
      const body = {
        id: r.id || 0,
        seq: Number($("#pe-seq").value) || 0,
        label: $("#pe-label").value.trim(),
        kind: $("#pe-kind").value,
        accounts: $("#pe-accounts").value.trim(),
        hint: $("#pe-hint").value.trim(),
        disabled: false,
      };
      try {
        await postJson("/profit-sheet", body);
        closeModal();
        toast("已保存");
        await loadRows();
        await run();
      } catch (e) { toast(e.message, "err"); }
    });
  };

  $("#ps-run").addEventListener("click", run);
  $("#ps-print").addEventListener("click", () => {
    const el = $("#ps-result").querySelector("table");
    if (!el) return toast("先出表再打印", "err");
    printPreview("利润分析表", el);
  });
  $("#ps-accounts").addEventListener("click", async () => {
    if (!(await confirmDialog("给还没挂科目的行，在 5101 制造费用下各建一个同名末级科目？建完这张表就自动出数。", false))) return;
    try {
      const r = await postJson("/profit-sheet/create-accounts", {});
      toast(`新建 ${r.created} 个科目，回写 ${r.touched} 行`);
      await loadRows();
      await run();
    } catch (e) { toast(e.message, "err"); }
  });
  // 自动挂科目：先预览（不写库），人看过再落地。
  // 猜错了不会报错，只会让某一行金额偏大、另一行偏小，合计还是对的——这种错最难发现。
  $("#ps-auto").addEventListener("click", async () => {
    let preview;
    try {
      preview = await postJson("/profit-sheet/auto-map", { apply: false });
    } catch (e) { toast(e.message, "err"); return; }
    const rows = preview.rows || [];
    const added = rows.filter((r) => (r.added || []).length);
    const body = rows.map((r) => {
      const add = (r.added || []).map((a) => `${esc(a[0])} ${esc(a[1])}`).join("<br>");
      return `<tr><td class="r">${r.seq}</td><td>${esc(r.label)}</td>
        <td class="mono">${add || "—"}</td>
        <td class="muted">${esc(r.note || "")}</td></tr>`;
    }).join("");
    modal(`<h3>自动挂科目（先看一眼）</h3>
      <div class="muted" style="margin-bottom:8px">按你现有科目的名字去对这张表的行。同一科目只挂一行；
        标准科目表里没有的名字（化工、染料封闭剂…）会留空，等你自己建。</div>
      <table class="grid"><thead><tr><th class="r" style="width:60px">序号</th><th>科目</th>
        <th style="width:220px">将挂的科目</th><th>说明</th></tr></thead><tbody>${body}</tbody></table>
      <div class="toolbar" style="margin-top:12px;justify-content:flex-end">
        <button class="btn ghost" id="ps-auto-cancel">取消</button>
        <button class="btn primary" id="ps-auto-ok" ${added.length ? "" : "disabled"}>应用这 ${added.length} 行</button>
      </div>`, true);
    $("#ps-auto-cancel").addEventListener("click", () => closeModal());
    $("#ps-auto-ok").addEventListener("click", async () => {
      try {
        const r = await postJson("/profit-sheet/auto-map", { apply: true });
        closeModal();
        toast(`挂好 ${r.touched} 行${(r.unmatched || []).length ? `，还有 ${r.unmatched.length} 行没找到科目` : ""}`);
        await loadRows();
        await run();
      } catch (e) { toast(e.message, "err"); }
    });
  });
  $("#ps-edit").addEventListener("click", () => {
    const box = $("#ps-rows");
    if (box.innerHTML) { box.innerHTML = ""; return; }
    loadRows();
  });
  run();
}

async function viewMonthBrief(main) {
  main.innerHTML = `<h2>月度简报</h2>
    <div class="toolbar">
      <label>期间 <input id="mb-period" value="${esc(state.current)}" style="width:90px" /></label>
      <button class="btn primary" id="mb-run">查询</button>
      <button class="btn ghost sm" id="mb-print">打印预览</button>
    </div>
    <div class="muted" style="margin:6px 0">一页纸看三件事：赚了多少、钱在哪、干了多少。所有数字都从既有单据算出来，不用另外录。</div>
    <div id="mb-result" class="muted">加载中…</div>`;
  $("#mb-print").addEventListener("click", () => {
    const el = $("#mb-result").querySelector("table");
    if (!el) return toast("先查询出数据再打印", "err");
    printPreview("月度简报", el);
  });
  const run = async () => {
    const p = $("#mb-period").value.trim();
    try {
      const r = await api(`/reports/brief?period=${encodeURIComponent(p)}`);
      const rows = r.rows || [];
      if (!rows.length) {
        $("#mb-result").innerHTML = `<div class="muted">这一期还没有数据。</div>`;
        return;
      }
      // 按 group 分组，保持后端给的顺序（后端顺序就是给人读的顺序）
      const groups = [];
      for (const x of rows) {
        let g = groups.find((y) => y.name === x.group);
        if (!g) { g = { name: x.group, rows: [] }; groups.push(g); }
        g.rows.push(x);
      }
      const fmtNum = (x) => {
        const v = Number(String(x ?? "").replace(/,/g, ""));
        return Number.isFinite(v) ? v : 0;
      };
      const deltaCell = (x) => {
        if (x.delta === null || x.delta === undefined) return `<td class="r muted">—</td><td class="r muted">—</td>`;
        const d = fmtNum(x.delta);
        if (d === 0) return `<td class="r">0.00</td><td class="r muted">—</td>`;
        // 变好标绿、变差标红：方向由后端给的 better_high 决定，
        // 不判断方向会把「应收暴涨」标成绿色。
        const good = x.better_high ? d > 0 : d < 0;
        const cls = good ? "pos" : "neg";
        const sign = d > 0 ? "+" : "";
        const pct = x.delta_pct ? ` (${sign}${x.delta_pct}%)` : "";
        return `<td class="r ${cls}">${sign}${esc(x.delta)}</td><td class="r ${cls}">${esc(pct ? pct.trim() : "")}</td>`;
      };
      const body = groups
        .map((g) => `<tr><th colspan="5" class="mb-group">${esc(g.name)}</th></tr>` +
          g.rows.map((x) => `<tr><td>${esc(x.label)}</td><td class="r">${esc(x.value)}</td>
            <td class="r muted">${x.prev === null || x.prev === undefined ? "—" : esc(x.prev)}</td>${deltaCell(x)}</tr>`).join(""))
        .join("");
      $("#mb-result").innerHTML = `<table class="grid">
        <thead><tr><th>项目</th><th class="r">本期</th><th class="r">上月</th><th class="r">增减</th><th class="r">增减%</th></tr></thead>
        <tbody>${body}</tbody></table>
        <div class="muted" style="margin-top:6px">${r.has_prev ? `对比期间：${esc(r.prev_period)}` : `上月（${esc(r.prev_period)}）没有账，同比一栏显示「—」`}</div>`;
    } catch (e) { toast(e.message, "err"); }
  };
  $("#mb-run").addEventListener("click", run);
  run();
}

async function viewDimProfit(main) {
  const DIMS = [
    { code: "dept", label: "部门" },
    { code: "project", label: "项目" },
    { code: "customer", label: "客户" },
    { code: "supplier", label: "供应商" },
    { code: "employee", label: "职员" },
    { code: "item", label: "存货" },
  ];
  main.innerHTML = `<h2>多维损益</h2>
    <div class="toolbar">
      <label>期间 <input id="dp-period" value="${esc(state.current)}" style="width:90px" /></label>
      <label>维度 <select id="dp-dim">${DIMS.map((d) => `<option value="${d.code}">${d.label}</option>`).join("")}</select></label>
      <button class="btn primary" id="dp-run">查询</button>
      <button class="btn ghost sm" id="dp-print">打印预览</button>
    </div>
    <div class="muted" style="margin:6px 0">只做「直接归属」：分录上挂了哪个维度就算哪个维度的，没挂的汇总成「未分配」行，不编分摊率。</div>
    <div id="dp-result" class="muted">选择期间与维度后点击查询。</div>`;
  $("#dp-print").addEventListener("click", () => {
    const el = $("#dp-result").querySelector("table");
    if (!el) return toast("先查询出数据再打印", "err");
    printPreview("多维损益", el);
  });
  const cell = (v) => `<td class="r">${esc(v ?? "")}</td>`;
  const run = async () => {
    const p = $("#dp-period").value.trim();
    const dim = $("#dp-dim").value;
    try {
      const r = await api(`/reports/dim-profit?period=${encodeURIComponent(p)}&dim=${encodeURIComponent(dim)}`);
      const rows = r.rows || [];
      const lines = rows.slice();
      if (r.unallocated) lines.push({ ...r.unallocated, unassigned: true });
      const head = `<thead><tr><th>${esc(r.dim_label || "维度")}</th><th>名称</th>
        <th class="r">收入</th><th class="r">成本</th><th class="r">费用</th><th class="r">税金</th><th class="r">利润</th><th class="r">利润率</th></tr></thead>`;
      const body = lines.length
        ? lines.map((x) => `<tr${x.unassigned ? ' class="muted"' : ""}><td>${x.unassigned ? "—" : esc(x.key)}</td><td>${esc(x.name)}</td>${cell(x.revenue)}${cell(x.cost)}${cell(x.expense)}${cell(x.tax)}${cell(x.profit)}${cell(x.profit_rate ? x.profit_rate + "%" : "")}</tr>`).join("")
        : `<tr><td colspan="8" class="muted">这一期没有损益发生额。</td></tr>`;
      const t = r.total || {};
      const foot = `<tfoot><tr><th colspan="2">合计</th><th class="r">${esc(t.revenue)}</th><th class="r">${esc(t.cost)}</th><th class="r">${esc(t.expense)}</th><th class="r">${esc(t.tax)}</th><th class="r">${esc(t.profit)}</th><th class="r">${esc(t.profit_rate ? t.profit_rate + "%" : "")}</th></tr></tfoot>`;
      $("#dp-result").innerHTML = `<table class="grid">${head}<tbody>${body}</tbody>${foot}</table>`;
    } catch (e) { toast(e.message, "err"); }
  };
  $("#dp-run").addEventListener("click", run);
  $("#dp-dim").addEventListener("change", run);
  run();
}

async function viewRatios(main) {
  main.innerHTML = `<h2>财务指标分析</h2>
    <div class="toolbar">
      <label>期间 <input id="rt-period" value="${esc(state.current)}" style="width:90px" /></label>
      <button class="btn primary" id="rt-run">查询</button>
      <button class="btn ghost sm" id="rt-run-print">打印预览</button>
    </div>
    <div id="rt-result" class="muted">填写期间后点击查询</div>`;
  $("#rt-run-print").addEventListener("click", () => { const el = $("#rt-result").querySelector("table"); printPreview("财务指标分析", el); });
  $("#rt-run").addEventListener("click", async () => {
    const p = $("#rt-period").value.trim();
    try {
      const r = await api(`/reports/ratios?period=${encodeURIComponent(p)}`);
      const rows = r.ratios || [];
      $("#rt-result").innerHTML = `<table><thead><tr><th>指标</th><th>数值</th><th>计算公式</th></tr></thead>
        <tbody>${rows.map((x) => `<tr><td>${esc(x.name)}</td><td class="r">${esc(x.display)}</td><td class="muted">${esc(x.formula)}</td></tr>`).join("")}</tbody></table>`;
    } catch (e) { toast(e.message, "err"); }
  });
}

// ===========================================================================
// MPS 主生产计划 + 粗排 + 细排（链6：清单⑥）
async function viewMps(main) {
  main.innerHTML = `<h2>MPS 主生产计划 · 粗细排产</h2>
    <div class="panel">
      <div style="display:flex;align-items:center;gap:8px;flex-wrap:wrap"><h4 style="margin:0">① MPS 运行（需求 − 现有 − 在制 = 计划）</h4><span class="grow"></span>
        <label style="font-size:12px"><input type="checkbox" id="mps-sales" checked /> 从销售订单收集未发量</label>
        <label style="font-size:12px">手工 <input id="mps-item" placeholder="存货编码" style="width:110px" /> × <input id="mps-qty" placeholder="数量" style="width:70px" /></label>
        <label style="font-size:12px">建议交期 <input type="date" id="mps-due" /></label>
        <button class="btn primary" id="mps-run">运行 MPS</button>
        <button class="btn ghost" id="mps-latest">最近一次</button>
        <button class="btn ghost sm" id="mps-tomrp">计划量转 MRP</button>
      </div>
      <div id="mps-table" class="muted" style="margin-top:8px">运行后显示净算建议；「下达」一键生成已下达生产订单。</div>
    </div>
    <div class="panel" style="margin-top:12px">
      <div style="display:flex;align-items:center;gap:8px;flex-wrap:wrap"><h4 style="margin:0">② 粗排（件/日产能顺排）</h4><span class="grow"></span>
        <label style="font-size:12px">日产能 <input id="rq-daily" value="10" style="width:60px" /> 件/日</label>
        <button class="btn" id="rq-run">计算粗排</button>
        <button class="btn ghost sm" id="rq-apply-all">全部应用到订单</button>
      </div>
      <div id="rq-table" class="muted" style="margin-top:8px">计算后显示排期建议与按日负荷（超载标红）。</div>
    </div>
    <div class="panel" style="margin-top:12px">
      <div style="display:flex;align-items:center;gap:8px"><h4 style="margin:0">③ 细排结果（已写回订单的计划日期）</h4><span class="grow"></span><button class="btn ghost sm" id="sch-reload">刷新</button></div>
      <div id="sch-table" class="muted" style="margin-top:8px">「粗排-应用」写回后在此显示。</div>
    </div>`;
  // 建议交期默认今天 +7（内联计算——shift 是别处的局部函数）
  $("#mps-due").value = (() => {
    const d = new Date();
    d.setDate(d.getDate() + 7);
    return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
  })();
  let mpsRows = [];
  let roughRows = [];

  const renderMps = (rows) => {
    mpsRows = rows || [];
    $("#mps-table").innerHTML = mpsRows.length
      ? `<table class="grid"><thead><tr><th>存货</th><th class="num">需求</th><th class="num">现有</th><th class="num">在制</th><th class="num">计划量</th><th>来源</th><th>建议交期</th><th>状态</th><th></th></tr></thead><tbody>${mpsRows.map((r) => `<tr><td>${esc(r.item_code)}</td><td class="num">${fmt(r.demand)}</td><td class="num">${fmt(r.on_hand)}</td><td class="num">${fmt(r.wip)}</td><td class="num"><b>${fmt(r.planned)}</b></td><td>${esc(r.source)}</td><td>${esc(r.due_date)}</td><td>${r.status === "open" ? '<span class="tag">待下达</span>' : '<span class="tag ok">已下达</span>'}</td>
        <td class="row-actions">${r.status === "open" && Number(r.planned) > 0 ? `<button class="btn primary sm" data-mps-go="${r.id}">下达</button>` : ""}</td></tr>`).join("")}</tbody></table>`
      : `<div class="muted">暂无结果（先运行 MPS）</div>`;
    $all("[data-mps-go]").forEach((b) => b.onclick = async () => {
      try {
        const r = await postJson(`/mps/${b.dataset.mpsGo}/convert`, {});
        toast(`已下达 → 生产订单 ${r.order_no}（到「工序报工」开工/领料）`, "ok");
        loadMps();
      } catch (e) { toast(e.message, "err"); }
    });
  };
  const loadMps = async () => {
    try { const r = await api("/mps/latest"); renderMps(r.rows || []); } catch (e) { $("#mps-table").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  $("#mps-run").onclick = async () => {
    try {
      const demands = [];
      const it = $("#mps-item").value.trim();
      const q = $("#mps-qty").value.trim();
      if (it && q) demands.push({ item_code: it, qty: q, source: "手工" });
      const r = await postJson("/mps/run", { demands, from_sales: $("#mps-sales").checked, due_date: $("#mps-due").value });
      renderMps(r.rows || []);
      $("#mps-item").value = ""; $("#mps-qty").value = "";
      toast(`MPS 运算完成（${(r.rows || []).length} 行）`, "ok");
    } catch (e) { toast(e.message, "err"); }
  };
  $("#mps-latest").onclick = loadMps;
  $("#mps-tomrp").onclick = async () => {
    const demands = mpsRows
      .filter((r) => Number(r.planned) > 0)
      .map((r) => ({ item_code: r.item_code, qty: String(r.planned), source: "MPS" }));
    if (!demands.length) { toast("无正计划量行可转", "err"); return; }
    try {
      await postJson("/mrp/run", { demands });
      toast("已按 MPS 计划量运行 MRP（到「MRP 运算」页点「最近一次结果」）", "ok");
    } catch (e) { toast(e.message, "err"); }
  };

  const applySch = async (list) => {
    const use = list.filter((o) => o && o.sug_start && o.sug_end);
    if (!use.length) { toast("无可用排期", "err"); return; }
    try {
      const r = await postJson("/prod/schedule", { items: use.map((o) => ({ id: o.id, start: o.sug_start, end: o.sug_end })) });
      toast(`已写回 ${r.updated} 单计划日期`, "ok");
      loadSch();
    } catch (e) { toast(e.message, "err"); }
  };
  const renderRough = (orders, load) => {
    roughRows = orders || [];
    $("#rq-table").innerHTML = (roughRows.length
      ? `<table class="grid"><thead><tr><th>订单</th><th>存货</th><th class="num">未完工</th><th class="num">需天数</th><th>建议开工</th><th>建议完工</th><th></th></tr></thead><tbody>${roughRows.map((o) => `<tr><td>${esc(o.no)}</td><td>${esc(o.item_code)}</td><td class="num">${fmt(o.open_qty)}</td><td class="num">${o.need_days}</td><td>${esc(o.sug_start)}</td><td>${esc(o.sug_end)}</td>
        <td class="row-actions"><button class="btn ghost sm" data-sch="${o.id}">应用</button></td></tr>`).join("")}</tbody></table>`
      : `<div class="muted">无未完工自制订单</div>`)
      + ((load || []).length
        ? `<div style="margin-top:10px"><b style="font-size:12.5px">按日负荷</b><table class="grid" style="margin-top:4px"><thead><tr><th>日期</th><th class="num">负荷</th><th class="num">产能</th><th>状态</th></tr></thead><tbody>${load.map((l) => `<tr><td>${esc(l.date)}</td><td class="num">${fmt(l.qty)}</td><td class="num">${fmt(l.capacity)}</td><td>${l.over ? '<span class="tag err">超载</span>' : '<span class="tag ok">正常</span>'}</td></tr>`).join("")}</tbody></table></div>`
        : "");
    $all("[data-sch]").forEach((b) => b.onclick = () => applySch([roughRows.find((o) => String(o.id) === b.dataset.sch)]));
  };
  $("#rq-run").onclick = async () => {
    try {
      const r = await postJson("/mps/rough", { daily_qty: $("#rq-daily").value.trim() || "10" });
      renderRough(r.orders || [], r.load || []);
    } catch (e) { toast(e.message, "err"); }
  };
  $("#rq-apply-all").onclick = () => applySch(roughRows);

  const loadSch = async () => {
    try {
      const r = await api(`/prod?period=${ymm(state.current || "")}`);
      const rows = (r.orders || []).filter((o) => o.plan_start);
      $("#sch-table").innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>订单</th><th>存货</th><th class="num">计划量</th><th>计划开工</th><th>计划完工</th><th>状态</th></tr></thead><tbody>${rows.map((o) => `<tr><td>${esc(o.no)}</td><td>${esc(o.item_code)}</td><td class="num">${fmt(o.planned_qty)}</td><td>${esc(o.plan_start)}</td><td>${esc(o.plan_end)}</td><td>${esc(o.status)}</td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">暂无已排产订单（粗排后点「应用」）</div>`;
    } catch (e) { $("#sch-table").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  $("#sch-reload").onclick = loadSch;
  await Promise.all([loadMps(), loadSch()]);
}

// MRP 运算
// ===========================================================================
async function viewMrp(main) {
  main.innerHTML = `<h2>MRP 运算</h2>
    <div class="toolbar">
      <label>需求产品 <input id="mrp-item" style="width:120px" /></label>
      <label>数量 <input id="mrp-qty" value="10" style="width:80px" /></label>
      <button class="btn primary" id="mrp-run">运行</button>
      <button class="btn ghost" id="mrp-latest">最近一次结果</button>
    </div>
    <div id="mrp-result" class="muted">输入需求产品与数量后运行</div>`;
  const load = (rows) => {
    $("#mrp-result").innerHTML = `<table><thead><tr><th>层级</th><th>物料</th><th>毛需求</th><th>现有库存</th><th>净需求</th><th>计划量</th><th>行动</th><th>来源</th><th></th></tr></thead>
      <tbody>${rows.map((x) => `<tr><td>${x.level}</td><td>${esc(x.item_code)}</td><td class="r">${fmt(x.gross_req)}</td><td class="r">${fmt(x.on_hand)}</td><td class="r">${fmt(x.net_req)}</td><td class="r">${fmt(x.planned_qty)}</td><td>${x.action === "produce" ? "生产" : x.action === "purchase" ? "采购" : "无"}</td><td>${esc(x.source)}</td><td>${x.action === "produce" ? `<button class="btn ghost sm" data-mrp-go-item="${esc(x.item_code)}" data-mrp-go-qty="${esc(String(x.planned_qty))}">下达</button>` : x.action === "purchase" ? `<button class="btn ghost sm" data-mrp-req="${x.id}">下推请购</button>` : ""}</td></tr>`).join("")}</tbody></table>`;
    $all("[data-mrp-go-item]").forEach((b) => b.onclick = async () => {
      try {
        const r = await postJson("/prod", { item_code: b.dataset.mrpGoItem, qty: b.dataset.mrpGoQty });
        toast(`已下达生产订单 ${r.no}（到「工序报工」开工/领料）`, "ok");
      } catch (e) { toast(e.message, "err"); }
    });
    $all("[data-mrp-req]").forEach((b) => b.onclick = async () => {
      try {
        const r = await postJson(`/mrp/${b.dataset.mrpReq}/to-req`, {});
        toast(`已下推请购单 ${r.no}（到「采购单据」审批后下推采购订单）`, "ok");
        b.disabled = true; b.textContent = "已下推";
      } catch (e) { toast(e.message, "err"); }
    });
  };
  $("#mrp-run").addEventListener("click", async () => {
    const item = $("#mrp-item").value.trim();
    const qty = $("#mrp-qty").value.trim();
    if (!item || !qty) { toast("请填写需求产品与数量", "err"); return; }
    try {
      const r = await api("/mrp/run", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ demands: [{ item_code: item, qty, source: "手工" }] }) });
      load(r.rows || []);
      toast("MRP 运算完成", "ok");
    } catch (e) { toast(e.message, "err"); }
  });
  $("#mrp-latest").addEventListener("click", async () => {
    try { const r = await api("/mrp/latest"); load(r.rows || []); } catch (e) { toast(e.message, "err"); }
  });
}

// ===========================================================================
// 工艺路线
// ===========================================================================
async function viewRouting(main) {
  main.innerHTML = `<h2>工艺路线</h2>
    <div class="toolbar">
      <label>产品代码 <input id="rt-item" style="width:120px" /></label>
      <button class="btn primary" id="rt-load">加载</button>
    </div>
    <div id="rt-result" class="muted">输入产品代码后加载</div>`;
  $("#rt-load").addEventListener("click", async () => {
    const item = $("#rt-item").value.trim();
    if (!item) { toast("请输入产品代码", "err"); return; }
    try {
      const r = await api(`/routing/${encodeURIComponent(item)}`);
      const ops = r.ops || [];
      $("#rt-result").innerHTML = `<table><thead><tr><th>序号</th><th>工序编码</th><th>工序名称</th><th>工作中心</th><th>标准工时</th><th>小时费率</th><th>检验点</th></tr></thead>
        <tbody>${ops.map((o) => `<tr><td>${o.seq}</td><td>${esc(o.op_code)}</td><td>${esc(o.op_name)}</td><td>${esc(o.work_center)}</td><td class="r">${fmt(o.std_hours)}</td><td class="r">${fmt(o.rate)}</td><td>${o.qc_required ? '<span class="tag warn">需检验</span>' : '<span class="muted">—</span>'}</td></tr>`).join("")}</tbody></table>`;
      if (!ops.length) $("#rt-result").innerHTML = `<div class="muted">该产品暂无工艺路线。维护请调用 POST /api/routing/:item。</div>`;
    } catch (e) { toast(e.message, "err"); }
  });
}

// ===========================================================================
// 审批中心
// ===========================================================================
async function viewApproval(main) {
  main.innerHTML = `<h2>审批中心</h2><div id="ap-list" class="muted">加载中…</div>`;
  const load = async () => {
    try {
      const r = await api("/approvals/todo");
      const rows = r.rows || [];
      $("#ap-list").innerHTML = rows.length ? rows.map((a) => `
        <div class="card"><div class="row"><b>${esc(a.title)}</b> <span class="tag">${esc(a.biz_kind)}/${a.biz_id}</span></div>
          <div class="muted">申请人 ${esc(a.applicant)} · 当前节点 ${a.current_node}/${a.steps.length}</div>
          <div class="row" style="margin-top:8px">
            <button class="btn sm" data-ap="${a.id}" data-act="1">通过</button>
            <button class="btn danger sm" data-ap="${a.id}" data-act="0">驳回</button>
          </div>
        </div>`).join("") : `<div class="muted">没有待审批的单据</div>`;
      $all("[data-ap]").forEach((b) => b.onclick = async () => {
        const approve = b.dataset.act === "1";
        try { await api(`/approvals/${b.dataset.ap}/act`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ approve, comment: approve ? "同意" : "驳回" }) }); toast(approve ? "已通过" : "已驳回", "ok"); load(); } catch (e) { toast(e.message, "err"); }
      });
    } catch (e) { toast(e.message, "err"); }
  };
  load();
}

// ===========================================================================
// 报表附注
// ===========================================================================
async function viewNotes(main) {
  main.innerHTML = `<h2>报表附注</h2>
    <div class="toolbar">
      <label>报表 <select id="nt-key"><option value="balance_sheet">资产负债表</option><option value="income_statement">利润表</option><option value="cash_flow">现金流量表</option></select></label>
      <label>期间 <input id="nt-period" value="${esc(state.current)}" style="width:90px" /></label>
      <button class="btn primary" id="nt-load">加载</button>
    </div>
    <div id="nt-list"></div>
    <div class="card" style="margin-top:12px"><h3>新增附注</h3>
      <div class="field"><label>标题</label><input id="nt-title" /></div>
      <div class="field"><label>内容</label><textarea id="nt-content" rows="3"></textarea></div>
      <button class="btn primary" id="nt-save">保存</button>
    </div>`;
  const load = async () => {
    const key = $("#nt-key").value, p = $("#nt-period").value.trim();
    try {
      const r = await api(`/reports/notes?report_key=${key}&period=${encodeURIComponent(p)}`);
      const rows = r.rows || [];
      $("#nt-list").innerHTML = rows.map((n) => `<div class="card"><div class="row"><b>${n.seq} · ${esc(n.title)}</b><button class="btn danger sm" data-nt-del="${n.id}">删除</button></div><div>${esc(n.content)}</div><div class="muted">${esc(n.updated_by)} ${esc(n.updated_at)}</div></div>`).join("") || `<div class="muted">暂无附注</div>`;
      $all("[data-nt-del]").forEach((b) => b.onclick = async () => { if (!(await confirmDialog("删除该附注？", true))) return; try { await api(`/reports/notes/${b.dataset.ntDel}/delete`, { method: "POST" }); toast("已删除", "ok"); load(); } catch (e) { toast(e.message, "err"); } });
    } catch (e) { toast(e.message, "err"); }
  };
  $("#nt-load").addEventListener("click", load);
  $("#nt-save").addEventListener("click", async () => {
    const key = $("#nt-key").value, p = $("#nt-period").value.trim();
    const title = $("#nt-title").value.trim(), content = $("#nt-content").value.trim();
    if (!title) { toast("标题不能为空", "err"); return; }
    try {
      await api("/reports/notes", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ report_key: key, period: ymm(p), title, content }) });
      toast("已保存附注", "ok");
      $("#nt-title").value = ""; $("#nt-content").value = "";
      load();
    } catch (e) { toast(e.message, "err"); }
  });
  load();
}

// ===========================================================================
// 电子档案
// ===========================================================================
async function viewArchive(main) {
  main.innerHTML = `<h2>会计电子档案</h2>
    <div class="toolbar">
      <label>期间 <input id="ar-period" value="${esc(state.current)}" style="width:90px" /></label>
      <label>类型(空=全部) <input id="ar-kind" placeholder="voucher/ledger/report/balance" style="width:180px" /></label>
      <button class="btn primary" id="ar-load">加载</button>
    </div>
    <div id="ar-list"></div>
    <div class="card" style="margin-top:12px"><h3>新增归档</h3>
      <div class="field"><label>类型</label><input id="ar-new-kind" value="voucher" style="width:120px" /></div>
      <div class="field"><label>标题</label><input id="ar-new-title" /></div>
      <div class="field"><label>内容(JSON)</label><textarea id="ar-new-payload" rows="3"></textarea></div>
      <button class="btn primary" id="ar-save">归档</button>
    </div>`;
  const load = async () => {
    const p = $("#ar-period").value.trim(), k = $("#ar-kind").value.trim();
    try {
      const r = await api(`/archives?period=${encodeURIComponent(p)}${k ? "&kind=" + encodeURIComponent(k) : ""}`);
      const rows = r.rows || [];
      $("#ar-list").innerHTML = rows.map((a) => `<div class="card"><div class="row"><b>${esc(a.file_no)}</b> <span class="tag">${esc(a.kind)}</span> <span class="tag ok">${a.sealed ? "封存" : "未封存"}</span></div><div>${esc(a.title)}</div><div class="muted">${esc(a.archived_by)} ${esc(a.archived_at)} · ${esc(a.content_hash).slice(0, 16)}</div></div>`).join("") || `<div class="muted">暂无档案</div>`;
    } catch (e) { toast(e.message, "err"); }
  };
  $("#ar-load").addEventListener("click", load);
  $("#ar-save").addEventListener("click", async () => {
    const p = $("#ar-period").value.trim(), kind = $("#ar-new-kind").value.trim();
    const title = $("#ar-new-title").value.trim(), payload = $("#ar-new-payload").value.trim();
    if (!title || !payload) { toast("标题与内容不能为空", "err"); return; }
    try {
      await api("/archives", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ period: ymm(p), kind, title, payload }) });
      toast("已归档", "ok");
      $("#ar-new-title").value = ""; $("#ar-new-payload").value = "";
      load();
    } catch (e) { toast(e.message, "err"); }
  });
  load();
}

// ===========================================================================
// 预算版本
// ===========================================================================
async function viewBudgetVersions(main) {
  main.innerHTML = `<h2>预算版本管理</h2>
    <div id="bv-list" class="muted">加载中…</div>
    <div class="card" style="margin-top:12px"><h3>新建版本</h3>
      <div class="field"><label>版本编码</label><input id="bv-key" placeholder="v2" style="width:140px" /></div>
      <div class="field"><label>版本名称</label><input id="bv-name" placeholder="2026 调整版" /></div>
      <div class="field"><label>备注</label><input id="bv-memo" /></div>
      <label class="muted"><input type="checkbox" id="bv-copy" checked /> 从当前版本复制数据</label>
      <div style="margin-top:8px"><button class="btn primary" id="bv-save">创建版本</button></div>
    </div>
    <div class="card" style="margin-top:12px"><h3>预算编制（当前版本）</h3>
      <div class="toolbar">
        <label>期间 <input id="be-per" value="${(state.current || "").replace("-", "")}" style="width:90px" placeholder="YYYYMM" /></label>
        <button class="btn sm" id="be-load">加载</button>
        <span class="grow"></span>
        <button class="btn primary sm" id="be-new">新增预算行</button>
      </div>
      <div id="be-list" class="muted">选择期间后加载</div>
      <p class="muted" style="font-size:12px;margin-top:6px">口径：执行额 = 会计年度 1 月至该期间已记账发生额；账套参数可设「超预算提醒 / 强控」（凭证保存时生效）。</p>
    </div>`;
  const load = async () => {
    try {
      const r = await api("/budget/versions");
      const versions = r.versions || [], current = r.current || "";
      $("#bv-list").innerHTML = versions.length ? versions.map((v) => `
        <div class="card"><div class="row">
          <b>${v.is_current ? "●" : "○"} ${esc(v.name)}</b> <span class="tag">${esc(v.key)}</span>
          <span class="grow"></span>
          ${v.is_current ? `<span class="tag ok">当前版本</span>` : `<button class="btn ghost sm" data-bv-act="${esc(v.key)}">设为当前</button>`}
          <button class="btn danger sm" data-bv-del="${esc(v.key)}">删除</button>
        </div><div class="muted">${esc(v.created_at)} ${esc(v.memo)}</div></div>`).join("")
        : `<div class="muted">暂无自定义版本，预算存于「默认」版本。当前版本：${current || "默认"}</div>`;
      $all("[data-bv-act]").forEach((b) => b.onclick = async () => { try { await api(`/budget/versions/${encodeURIComponent(b.dataset.bvAct)}/activate`, { method: "POST" }); toast("已设为当前版本", "ok"); load(); } catch (e) { toast(e.message, "err"); } });
      $all("[data-bv-del]").forEach((b) => b.onclick = async () => { if (!(await confirmDialog(`删除版本 ${b.dataset.bvDel} 及其全部预算数据？`, true))) return; try { await api(`/budget/versions/${encodeURIComponent(b.dataset.bvDel)}/delete`, { method: "POST" }); toast("已删除", "ok"); load(); } catch (e) { toast(e.message, "err"); } });
    } catch (e) { toast(e.message, "err"); }
  };
  $("#bv-save").addEventListener("click", async () => {
    const key = $("#bv-key").value.trim(), name = $("#bv-name").value.trim(), memo = $("#bv-memo").value.trim();
    if (!key || !name) { toast("版本编码与名称不能为空", "err"); return; }
    try {
      await api("/budget/versions", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ key, name, memo, is_current: false }) });
      if ($("#bv-copy").checked) {
        const r = await api("/budget/versions");
        const current = r.current || "";
        await api("/budget/versions/copy", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ from: current, to: key }) });
      }
      toast("版本已创建", "ok");
      $("#bv-key").value = ""; $("#bv-name").value = ""; $("#bv-memo").value = "";
      load();
    } catch (e) { toast(e.message, "err"); }
  });
  // 预算编制：当前版本的预算行 CRUD
  const loadRows = async () => {
    try {
      const r = await api(`/budget/rows?period=${encodeURIComponent($("#be-per").value.trim())}`);
      const rows = r.rows || [];
      $("#be-list").innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>科目</th><th>部门</th><th class="num">预算金额</th><th>备注</th><th></th></tr></thead><tbody>${rows.map((x) => `<tr><td>${esc(x.account_code)}</td><td>${esc(x.dept || "—")}</td><td class="num">${fmt(x.amount)}</td><td>${esc(x.memo || "")}</td><td class="row-actions"><button class="btn ghost sm" data-be-del="${x.id}">删</button></td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">该期间（版本：${esc(r.version || "默认")}）暂无预算行</div>`;
      $all("[data-be-del]").forEach((b) => b.onclick = async () => {
        if (!(await confirmDialog("删除该预算行？", true))) return;
        try { await api(`/budget/rows/${b.dataset.beDel}/delete`, { method: "POST" }); toast("已删除", "ok"); loadRows(); } catch (e) { toast(e.message, "err"); }
      });
    } catch (e) { $("#be-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  $("#be-load").onclick = loadRows;
  $("#be-new").onclick = () => {
    const m = modal(`<h3>新增预算行（当前版本）</h3>
      <div class="field"><label>期间 *</label><input id="be2-per" value="${esc($("#be-per").value.trim())}" style="width:100px" /></div>
      <div class="field"><label>科目编码 *</label><input id="be2-acct" placeholder="如 660201" /></div>
      <div class="field"><label>部门（可空 = 全公司口径）</label><input id="be2-dept" /></div>
      <div class="field"><label>预算金额 *</label><input id="be2-amt" /></div>
      <div class="field"><label>备注</label><input id="be2-memo" /></div>
      <div class="foot"><button class="btn primary" id="be2-save">保存</button><button class="btn ghost" id="be2-cancel">取消</button></div>`);
    $("#be2-cancel", m).onclick = closeModal;
    $("#be2-save", m).onclick = async () => {
      try {
        await postJson("/budget/rows", { period: parseInt($("#be2-per", m).value.trim(), 10) || 0, account_code: $("#be2-acct", m).value.trim(), dept: $("#be2-dept", m).value.trim(), amount: $("#be2-amt", m).value.trim(), memo: $("#be2-memo", m).value.trim() });
        toast("已保存预算行", "ok"); closeModal(); loadRows();
      } catch (e) { toast(e.message, "err"); }
    };
  };
  loadRows();
  load();
}

// 新建生产订单：选存货后带出该存货的工艺版本，按选中的版本生成工序。
// 版本不只是备注——报工流水会记下它，计件单价按版本算钱（见 findb::piecepay）。
function openProdOrderEditor(reload) {
  const mask = modal(`<h3>新建生产订单</h3>
    <div class="field"><label>产品编码 *</label><input id="pno-item" placeholder="如 140501" /></div>
    <div class="field"><label>数量 *</label><input id="pno-qty" value="1" /></div>
    <div class="field"><label>日期</label><input type="date" id="pno-date" value="${today()}" /></div>
    <div class="field"><label>工艺版本</label><select id="pno-ver" style="min-width:200px"><option value="">默认版本</option></select></div>
    <p class="muted" style="font-size:12px;margin:6px 0 0">填了产品编码会自动带出该产品的工艺版本；选哪一版，工序清单就按哪一版生成，报工与计件单价也按这一版算。没有工艺路线时订单照建，工序留空。</p>
    <div class="foot"><button class="btn primary" id="pno-save">创建</button><button class="btn ghost" id="pno-cancel">取消</button></div>`);
  $("#pno-cancel", mask).onclick = closeModal;
  const loadVersions = async () => {
    const item = $("#pno-item", mask).value.trim();
    if (!item) return;
    try {
      const r = await api(`/routing/${encodeURIComponent(item)}/versions`);
      const vs = r.versions || [];
      $("#pno-ver", mask).innerHTML = `<option value="">默认版本</option>` + vs.filter((v) => v).map((v) => `<option value="${esc(v)}">${esc(v)}</option>`).join("");
    } catch (e) { /* 没维护工艺路线很正常，保持默认版本 */ }
  };
  $("#pno-item", mask).onchange = loadVersions;
  $("#pno-item", mask).onblur = loadVersions;
  $("#pno-save", mask).onclick = async () => {
    const item = $("#pno-item", mask).value.trim();
    const qty = $("#pno-qty", mask).value.trim();
    if (!item || !qty) { toast("产品编码与数量必填", "err"); return; }
    try {
      const r = await postJson("/prod", {
        item_code: item, qty, date: $("#pno-date", mask).value,
        version: $("#pno-ver", mask).value,
      });
      const v = r.version ? ` @${r.version}` : "";
      toast(`已创建生产订单 ${r.no}${v}（工序 ${r.ops || 0} 道）`, "ok");
      closeModal();
      reload && reload();
    } catch (e) { toast(e.message, "err"); }
  };
}

// 新建委外订单（委外=生产的变体：BOM/领料/开工/完工全复用生产链，加工费独立确认）
function openOutsourceEditor(reload) {
  const mask = modal(`<h3>新建委外订单</h3>
    <div class="field"><label>加工产品编码 *</label><input id="os-item" placeholder="如 140501" /></div>
    <div class="field"><label>数量 *</label><input id="os-qty" value="1" /></div>
    <div class="field"><label>供应商编码 *</label><input id="os-sup" placeholder="如 S01" /></div>
    <div class="field"><label>供应商名称</label><input id="os-supn" /></div>
    <div class="field"><label>日期</label><input type="date" id="os-date" value="${today()}" /></div>
    <p class="muted" style="font-size:12px;margin:6px 0 0">委外订单复用生产链：维护 BOM → 领料出库（发材料）→ 开工 → 确认加工费 → 完工入库。</p>
    <div class="foot"><button class="btn primary" id="os-save">创建</button><button class="btn ghost" id="os-cancel">取消</button></div>`);
  $("#os-cancel", mask).onclick = closeModal;
  $("#os-save", mask).onclick = async () => {
    const item = $("#os-item", mask).value.trim();
    const qty = $("#os-qty", mask).value.trim();
    const sup = $("#os-sup", mask).value.trim();
    if (!item || !qty || !sup) { toast("产品编码、数量、供应商必填", "err"); return; }
    try {
      const r = await postJson("/prod", {
        item_code: item, qty, kind: "outsourcing",
        supplier_code: sup, supplier_name: $("#os-supn", mask).value.trim(), date: $("#os-date", mask).value,
      });
      toast(`已创建委外订单 ${r.no}（BOM → 领料 → 开工 → 加工费 → 完工）`, "ok");
      closeModal();
      reload && reload();
    } catch (e) { toast(e.message, "err"); }
  };
}

// 委外加工费确认：借 500102（不含税）/ 借 22210101 进项 / 贷应付(供应商辅助)
//
// 票种是这张表单的第二个必填项，因为「同样一笔加工费，三种票的实际成本差一截」：
// 含税 10,000 元，13% 专票成本 8,849.56、3% 票 9,708.74、无票 10,000 且不能税前扣除。
// 老版本只填一个金额、把含税数整笔记进成本，等于把可抵的进项扔了，成本还虚高。
function openFeeEditor(order, reload) {
  if (!order) { toast("请先选择生产订单", "err"); return; }
  if (order.order_kind !== "outsourcing") { toast("仅委外订单可确认加工费", "err"); return; }
  const mask = modal(`<h3>确认加工费 · 委外 ${esc(order.no || "")}</h3>
    <div class="field"><label>加工费金额（价税合计）*</label><input id="fe-amt" placeholder="发票上的价税合计 / 实付金额" /></div>
    <div class="field"><label>票种 *</label><select id="fe-ticket">
      <option value="13">13% 专用发票（可抵 13%）</option>
      <option value="3">3% 征收率发票（可抵 3%）</option>
      <option value="none">无票（不能抵，也不能税前扣除）</option>
    </select></div>
    <div class="field"><label>日期</label><input type="date" id="fe-date" value="${today()}" /></div>
    <div id="fe-cmp" class="muted" style="font-size:12px;line-height:1.7;margin:6px 0 0"></div>
    <p class="muted" style="font-size:12px;margin:6px 0 0">凭证：借 500102 生产成本-直接人工（<b>不含税</b>）/ 借 22210101 进项税额（有票才拆）/ 贷 应付账款（供应商辅助）；金额并入该订单人工要素，完工时随库存商品结转。</p>
    <div class="foot"><button class="btn primary" id="fe-save">确认</button><button class="btn ghost" id="fe-cancel">取消</button></div>`);
  // 三种票并排比：谈价时看的就是这张小表，选中的那一行加粗
  const refreshCmp = async () => {
    const box = $("#fe-cmp", mask);
    const g = $("#fe-amt", mask).value.trim();
    if (!g) { box.innerHTML = ""; return; }
    try {
      const r = await api(`/outsource/compare?gross=${encodeURIComponent(g)}`);
      const cur = $("#fe-ticket", mask).value;
      box.innerHTML = `<table class="grid" style="font-size:12px"><thead><tr><th>票种</th><th>不含税成本</th><th>可抵进项</th><th>现金成本</th></tr></thead><tbody>
        ${(r.rows || []).map((x) => `<tr${x.kind === cur ? ' style="font-weight:700"' : ""}>
          <td>${esc(x.label)}</td><td class="num">${esc(x.net)}</td><td class="num">${esc(x.tax)}</td><td class="num">${esc(x.real_cost)}</td></tr>`).join("")}
      </tbody></table><div style="margin-top:4px">${esc(((r.rows || []).find((x) => x.kind === cur) || {}).note || "")}</div>`;
    } catch (e) { box.textContent = ""; }
  };
  $("#fe-amt", mask).oninput = refreshCmp;
  $("#fe-ticket", mask).onchange = refreshCmp;
  $("#fe-cancel", mask).onclick = closeModal;
  $("#fe-save", mask).onclick = async () => {
    const amt = $("#fe-amt", mask).value.trim();
    if (!amt) { toast("请填写加工费金额", "err"); return; }
    try {
      const r = await postJson(`/prod/${order.id}/outsource-fee`, { amount: amt, date: $("#fe-date", mask).value, ticket: $("#fe-ticket", mask).value });
      toast(`已确认加工费，凭证 #${r.voucher_id}`, "ok");
      closeModal();
      reload && reload();
    } catch (e) { toast(e.message, "err"); }
  };
}

// BOM 维护（报工页「维护BOM」）：子件 / 用量 / 损耗率；领料与 MRP 按 BOM 展开
function openBomEditor(item, reload) {
  let rows = [{ child: "", qty: "1", loss: "0" }];
  const mask = modal(`<h3>维护BOM · ${esc(item)}</h3>
    <table class="grid" id="bm-t"><thead><tr><th>子件编码 *</th><th>用量/件</th><th>损耗率</th><th></th></tr></thead><tbody></tbody></table>
    <button class="btn ghost sm" id="bm-add">＋子件</button>
    <p class="muted" style="font-size:12px;margin:6px 0 0">用量 = 每 1 件父件所需子件数量；损耗率如 0.05。保存覆盖当前版本 BOM（领料按 BOM × 计划量展开）。</p>
    <div class="foot"><button class="btn primary" id="bm-save">保存</button><button class="btn ghost" id="bm-cancel">取消</button></div>`);
  const draw = () => {
    $("#bm-t tbody", mask).innerHTML = rows.map((r, i) => `<tr>
      <td><input data-f="child" data-i="${i}" value="${esc(r.child)}" style="width:120px" /></td>
      <td><input data-f="qty" data-i="${i}" value="${esc(r.qty)}" style="width:70px" /></td>
      <td><input data-f="loss" data-i="${i}" value="${esc(r.loss)}" style="width:60px" /></td>
      <td><button class="btn ghost sm" data-del="${i}">×</button></td></tr>`).join("");
    $all("[data-del]", mask).forEach((b) => b.onclick = () => { rows.splice(parseInt(b.dataset.del, 10), 1); if (!rows.length) rows.push({ child: "", qty: "1", loss: "0" }); draw(); });
    $all("[data-f]", mask).forEach((inp) => inp.oninput = () => { rows[parseInt(inp.dataset.i, 10)][inp.dataset.f] = inp.value; });
  };
  draw();
  api(`/bom?parent=${encodeURIComponent(item)}`).then((r) => {
    const list = r.rows || [];
    if (list.length) rows = list.map((x) => ({ child: x.child_code, qty: String(x.qty), loss: String(x.loss_rate) }));
    draw();
  }).catch((e) => toast(e.message, "err"));
  $("#bm-add", mask).onclick = () => { rows.push({ child: "", qty: "1", loss: "0" }); draw(); };
  $("#bm-cancel", mask).onclick = closeModal;
  $("#bm-save", mask).onclick = async () => {
    const clean = rows.filter((r) => r.child.trim());
    if (!clean.length) { toast("至少一个子件", "err"); return; }
    try {
      const r = await postJson("/bom", { parent: item, children: clean.map((c) => ({ child: c.child.trim(), qty: c.qty, loss: c.loss })) });
      toast(`BOM 已保存（${r.children} 个子件）`, "ok");
      closeModal();
      reload && reload();
    } catch (e) { toast(e.message, "err"); }
  };
}
