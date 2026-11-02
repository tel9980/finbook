// FinBook Web 前端 — 发票（销项/进项）、税务申报、到岸成本、成本差异
//
// 这一份是 `crates/finweb/static/app.js` 拆分出来的（原单文件 14k 行、295 个顶层函数）。
// 拆分方式：**经典 script，按原始行序切分，不重排任何代码**——顶层 function/const 的
// 提升与声明顺序完全保持原样，所以跨文件不存在 TDZ/未定义问题；只有 `VIEWS` 路由表
// 因为引用全部视图函数，被挪到了最后加载的 app-hr.js。
// 加载顺序由 `index.html` 的 <script> 顺序决定（也是 `main.rs` 里 asset_version 的清单顺序）。
// 新增文件必须同步三处：index.html、main.rs 的 asset_version 清单、tools/lib-app-src.js 的解析（自动）。
function today() { const d = new Date(); const p = (n) => String(n).padStart(2, "0"); return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`; }

// ===========================================================================
// 发票管理
// ===========================================================================
let invoiceCache = null;

async function loadInvoices(filter) {
  const q = new URLSearchParams();
  if (filter && filter.kind) q.set("kind", filter.kind);
  if (filter && filter.status) q.set("status", filter.status);
  if (filter && filter.keyword) q.set("keyword", filter.keyword);
  const s = q.toString();
  const data = await api(`/invoices${s ? "?" + s : ""}`);
  // 汇总卡片：单独拉取（始终全量）
  let summary = {};
  try { summary = await api("/invoices/summary"); } catch (e) {}
  data.summary = (summary && summary.by_kind) || {};
  invoiceCache = data;
  return data;
}

// 申报前体检 → 界面块
//
// 体检为什么值得单独占一块界面：申报表上每一个数都「看起来没问题」，
// 唯一能看出错的线索是「票和账对不上」。把它做成一句「可以申报」或
// 「先处理 2 个错误」，会计才敢点导出。三级配色固定：红=错了别报、
// 橙=看一眼再决定、灰=知道就行。
function taxPrecheckPanel(pre) {
  if (!pre) return "";
  const items = pre.items || [];
  const color = (lv) => (lv === "error" ? "var(--err)" : lv === "warn" ? "var(--warn)" : "var(--muted)");
  const tag = (lv) => (lv === "error" ? "错误" : lv === "warn" ? "待确认" : "提醒");
  const rows = items.map((i) => `
    <li style="margin:4px 0;line-height:1.7">
      <b style="color:${color(i.level)}">[${tag(i.level)}]</b>
      <b>${esc(i.title)}</b>${i.amount ? ` <span class="num" style="color:${color(i.level)}">${esc(i.amount)}</span>` : ""}
      <div class="muted" style="font-size:12px">${esc(i.detail)}</div>
    </li>`).join("");
  const head = pre.can_file
    ? `<span style="color:var(--ok)">可以申报</span>`
    : `<span style="color:var(--err)">有 ${pre.errors} 个错误，先改再报</span>`;
  return `<div class="panel" id="tx-precheck">
    <h3 style="margin:0 0 6px">申报前体检 · ${head}</h3>
    <div class="muted" style="font-size:12px;margin-bottom:8px">
      体检只核对<b>票和账是否互相印证</b>（票录了凭证有没有记、进项有没有认证、跨期、票面自己勾不勾稽），
      不改任何数。错误项清了才能报；待确认项要人看一眼再决定。
    </div>
    ${items.length ? `<ul style="margin:0 0 0 18px;padding:0">${rows}</ul>`
      : `<div class="muted">票与账一致，没有发现问题。</div>`}
  </div>`;
}

// ---------------- 税务申报（增值税一般纳税人） ----------------
//
// 边界写进界面里而不是只写在文档里：用户第一眼就该知道「不连网、手工申报」，
// 以及哪些数系统算了、哪些没算。只在导出 CSV 里写口径说明是不够的——
// 到那一步时人已经把表填完了。
async function viewTaxDecl(main) {
  const period = state.current;
  main.innerHTML = `<h2>税务申报 · 增值税一般纳税人</h2><div class="muted">加载中…</div>`;
  // 体检与申报表分开取：报税前那几天会计会反复改票再体检，
  // 而申报表只在最后看一次，绑在一起会每次都重取整张表。
  let pre = null;
  try { pre = await api(`/tax/precheck?period=${encodeURIComponent(period)}`); } catch (e) {}
  let d;
  try {
    d = await api(`/tax/vat?period=${encodeURIComponent(period)}`);
  } catch (e) {
    main.innerHTML = `<h2>税务申报 · 增值税一般纳税人</h2><div style="color:var(--err)">${esc(e.message)}</div>`;
    return;
  }
  const f = d.form || {};
  const sur = f.surcharge || {};
  const sign = (v) => (Number(v) < 0 ? "var(--err)" : "inherit");
  const bucketRows = (t) => (t.buckets || []).map((b) => `
    <tr data-tx-kind="${t === f.details_out ? "out" : "in"}" data-tx-rate="${esc(b.rate)}" style="cursor:pointer" title="点击查看构成发票">
      <td>${esc(b.rate)}%</td>
      <td>${b.general_method ? "一般计税" : (b.rate === "0" ? "免税" : "简易计税")}</td>
      <td class="num">${b.count}</td>
      <td class="num">${esc(b.net)}</td>
      <td class="num">${esc(b.tax)}</td>
      <td class="num">${esc(b.gross)}</td>
    </tr>`).join("");

  main.innerHTML = `
    <h2>税务申报 · 增值税一般纳税人</h2>
    <div class="muted" style="margin-bottom:10px;line-height:1.7">
      税款所属期 <b>${esc(f.period || period)}</b>${d.tax_no ? ` · 纳税人识别号 ${esc(d.tax_no)}` : ""}。
      <b>本模块不连网</b>：不做发票查验、不做电子税务局网报、不碰税控设备。系统只负责把数算对、
      把数给全、把来源说得清；申报表导出为 CSV，由会计手工填报。
    </div>
    ${(f.warnings || []).length ? `<div class="banner unset" id="tx-warn" style="margin-bottom:10px">
      <b>口径与待办</b>
      <ul style="margin:6px 0 0 18px;line-height:1.7">${f.warnings.map((w) => `<li>${esc(w)}</li>`).join("")}</ul>
    </div>` : ""}
    <div class="toolbar" style="box-shadow:none;border:none;padding:0;margin:0 0 12px">
      <button class="btn ghost sm" id="tx-opts">申报参数</button>
      <button class="btn primary sm" id="tx-export">导出申报表 CSV</button>
    </div>
    ${taxPrecheckPanel(pre)}
    <div class="cards" id="tx-cards" style="margin-bottom:12px">
      <div class="card"><div class="k">销项税额</div><div class="v">${esc(f.output_tax)}</div></div>
      <div class="card"><div class="k">进项税额（仅已认证）</div><div class="v">${esc(f.input_tax)}</div></div>
      <div class="card"><div class="k">应纳税额</div><div class="v" style="color:${sign(f.payable)}">${esc(f.payable)}</div></div>
      <div class="card"><div class="k">附加税费合计</div><div class="v">${esc(sur.total)}</div></div>
    </div>
    <div class="panel">
      <h3 style="margin:0 0 10px">一、销项税额</h3>
      <table class="grid"><tbody>
        <tr><td>一般计税方法计税销售额</td><td class="num">${esc(f.sales_general)}</td></tr>
        <tr><td>简易计税方法计税销售额</td><td class="num">${esc(f.sales_simple)}</td></tr>
        <tr><td>免征增值税销售额</td><td class="num">${esc(f.sales_exempt)}</td></tr>
        ${Number(f.sales_unknown) ? `<tr><td class="muted">税率未填/不可识别（<b>未计入上面三行</b>）</td><td class="num" style="color:var(--err)">${esc(f.sales_unknown)}</td></tr>` : ""}
        <tr><td><b>销项税额</b></td><td class="num"><b>${esc(f.output_tax)}</b></td></tr>
      </tbody></table>
    </div>
    <div class="panel">
      <h3 style="margin:0 0 10px">二、进项税额</h3>
      <table class="grid"><tbody>
        <tr><td>本期认证相符的进项税额</td><td class="num">${esc(f.input_tax)}</td></tr>
        <tr><td>上期留抵税额</td><td class="num muted">人工填报</td></tr>
        <tr><td>进项税额转出额（附列资料三）</td><td class="num muted">人工填报</td></tr>
        <tr><td>未开票销售额 / 销项税额</td><td class="num muted">人工填报（视同销售等需逐笔认定）</td></tr>
        <tr><td><b>应纳税额</b></td><td class="num"><b>${esc(f.payable)}</b></td></tr>
      </tbody></table>
    </div>
    <div class="panel">
      <h3 style="margin:0 0 10px">三、附加税费</h3>
      <table class="grid"><tbody>
        <tr><td>城市维护建设税（${esc(sur.city_rate)}）</td><td class="num">${esc(sur.city_tax)}</td></tr>
        <tr><td>教育费附加（${esc(sur.edu_rate)}）</td><td class="num">${esc(sur.edu)}</td></tr>
        <tr><td>地方教育附加（${esc(sur.local_edu_rate)}）</td><td class="num">${esc(sur.local_edu)}</td></tr>
        <tr><td><b>合计</b></td><td class="num"><b>${esc(sur.total)}</b></td></tr>
      </tbody></table>
    </div>
    ${(f.details_out.buckets || []).length ? `<div class="panel">
      <h3 style="margin:0 0 10px">附列资料一：应税货物和劳务销项明细 <span class="muted" style="font-weight:400;font-size:12px">（点行看构成发票）</span></h3>
      <table class="grid"><thead><tr><th>税率</th><th>计税方式</th><th>张数</th><th>销售额</th><th>销项税额</th><th>价税合计</th></tr></thead>
      <tbody>${bucketRows(f.details_out)}
        <tr style="font-weight:600"><td>合计</td><td></td><td class="num">${f.details_out.count}</td><td class="num">${esc(f.details_out.net_total)}</td><td class="num">${esc(f.details_out.tax_total)}</td><td class="num">${esc(f.details_out.gross_total)}</td></tr>
      </tbody></table>
    </div>` : ""}
    ${(f.details_in.buckets || []).length ? `<div class="panel">
      <h3 style="margin:0 0 10px">附列资料二：进项税额明细 <span class="muted" style="font-weight:400;font-size:12px">（仅已认证，点行看构成发票）</span></h3>
      <table class="grid"><thead><tr><th>税率</th><th>计税方式</th><th>张数</th><th>金额</th><th>进项税额</th><th>价税合计</th></tr></thead>
      <tbody>${bucketRows(f.details_in)}
        <tr style="font-weight:600"><td>合计</td><td></td><td class="num">${f.details_in.count}</td><td class="num">${esc(f.details_in.net_total)}</td><td class="num">${esc(f.details_in.tax_total)}</td><td class="num">${esc(f.details_in.gross_total)}</td></tr>
      </tbody></table>
    </div>` : ""}
    <div class="panel">
      <h3 style="margin:0 0 8px">系统不代算的项（须人工填报）</h3>
      <ul style="margin:0 0 0 18px;line-height:1.8">${(d.manual_fields || []).map((x) => `<li>${esc(x)}</li>`).join("")}</ul>
      <p class="muted" style="margin:8px 0 0;font-size:12px">
        附列资料三（进项税额转出）要按每张发票的<b>实际用途</b>逐张判定（集体福利 / 个人消费 /
        免税项目…），靠科目和摘要自动推断必然出错，而出错方向是多缴税或被追缴，所以留手工填。
      </p>
    </div>
    <div id="tx-detail" class="panel" style="display:none"></div>`;

  $("#tx-export").onclick = () => {
    downloadUrl(`/api/tax/vat/export?period=${encodeURIComponent(f.period || period)}`);
  };
  $("#tx-opts").onclick = () => openTaxOptions(d.options || {});
  $all("tr[data-tx-kind]", main).forEach((tr) => {
    tr.onclick = async () => {
      const box = $("#tx-detail");
      box.style.display = "";
      box.innerHTML = `<h3>构成发票（${tr.dataset.txKind === "out" ? "销项" : "进项"} · ${esc(tr.dataset.txRate)}%）</h3><div class="muted">加载中…</div>`;
      try {
        const r = await api(`/tax/vat/detail?period=${encodeURIComponent(f.period || period)}&kind=${tr.dataset.txKind}&rate=${encodeURIComponent(tr.dataset.txRate)}`);
        const rows = r.rows || [];
        box.innerHTML = `<h3>构成发票（${tr.dataset.txKind === "out" ? "销项" : "进项"} · ${esc(tr.dataset.txRate)}%，共 ${rows.length} 张）</h3>
          <table class="grid"><thead><tr><th>开票日</th><th>发票号</th><th>${tr.dataset.txKind === "out" ? "购买方" : "销售方"}</th><th>金额</th><th>税额</th><th>价税合计</th><th>备注</th></tr></thead>
          <tbody>${rows.map((x) => `<tr><td>${esc(x.date)}</td><td>${esc(x.number)}</td><td>${esc(x.party)}</td><td class="num">${esc(x.net)}</td><td class="num">${esc(x.tax)}</td><td class="num">${esc(x.gross)}</td><td>${esc(x.memo)}</td></tr>`).join("")}</tbody></table>`;
      } catch (e) {
        box.innerHTML = `<h3>构成发票</h3><div style="color:var(--err)">${esc(e.message)}</div>`;
      }
    };
  });
}

function openTaxOptions(o) {
  const mask = modal(`
    <h3>税务申报参数</h3>
    <div class="field"><label>纳税人身份</label>
      <select id="to-taxpayer">
        <option value="general" ${o.taxpayer === "general" ? "selected" : ""}>一般纳税人</option>
        <option value="small" ${o.taxpayer === "small" ? "selected" : ""}>小规模纳税人</option>
      </select></div>
    <div class="field"><label>城建税适用地区</label>
      <select id="to-zone">
        <option value="city" ${o.city_tax_zone === "city" ? "selected" : ""}>市区（7%）</option>
        <option value="county" ${o.city_tax_zone === "county" ? "selected" : ""}>县城、镇（5%）</option>
        <option value="other" ${o.city_tax_zone === "other" ? "selected" : ""}>其他地区（1%）</option>
      </select></div>
    <div class="field"><label>教育费附加（小数，如 0.03）</label>
      <input id="to-edu" value="${esc(o.edu_rate || "0.03")}" /></div>
    <div class="field"><label>地方教育附加（小数，如 0.02）</label>
      <input id="to-local" value="${esc(o.local_edu_rate || "0.02")}" /></div>
    <p class="muted" style="font-size:12px;line-height:1.7">
      城建税是法定税率，<b>只能按适用地区选</b>、不能自选，所以这里没有自由输入。
      附加税率上限 10%：法定值远低于此，再高基本是「3% 误录成 30%」这类手误。
    </p>
    <div style="margin-top:10px;display:flex;gap:8px">
      <button class="btn" id="to-cancel">取消</button>
      <button class="btn primary" id="to-ok">保存</button>
    </div>`);
  $("#to-cancel", mask).onclick = () => closeModal();
  $("#to-ok", mask).onclick = async () => {
    try {
      await postJson("/tax/options", {
        taxpayer: $("#to-taxpayer", mask).value,
        city_tax_zone: $("#to-zone", mask).value,
        edu_rate: $("#to-edu", mask).value.trim(),
        local_edu_rate: $("#to-local", mask).value.trim(),
      });
      toast("已保存", "ok");
      closeModal();
      goView("tax-decl", { force: true });
    } catch (e) { toast(e.message, "err"); }
  };
}

// ---------------- 到岸成本（Landed Cost） ----------------
//
// 界面必须说清三件事，否则用户会把它当成「多录一笔费用」：
//  1. **不改原入库单** —— 追加成本调整流水，原单据与已开票数据一个字节都不动
//  2. **入存货 vs 转费用** 是过账时按**当时的结存**算的，草稿阶段算不出来
//  3. 货已全部出清时，运费是**当期费用**而不是存货 —— 挂在存货上是幽灵资产
async function viewLandedCost(main) {
  const period = state.current;
  main.innerHTML = `<h2>到岸成本</h2><div class="muted">加载中…</div>`;
  let d;
  try {
    d = await api(`/landed-cost?period=${encodeURIComponent(period)}`);
  } catch (e) {
    main.innerHTML = `<h2>到岸成本</h2><div style="color:var(--err)">${esc(e.message)}</div>`;
    return;
  }
  const rows = d.rows || [];
  main.innerHTML = `
    <h2>到岸成本 · ${esc(period)}</h2>
    <div class="muted" style="margin-bottom:10px;line-height:1.7">
      把运费、关税、保险、清关费摊进存货成本。<b>不改原采购入库单</b>（改了就得连带重算税额与
      已对账金额），而是追加成本调整流水。<br>
      <b>入存货 / 转费用是过账时按当时的结存算的</b>：货已经卖掉的部分摊不进去，会自动转当期费用
      —— 挂在已清空的存货上是幽灵资产，资产负债表凭空多一笔且不报错。
    </div>
    <div class="cards" style="margin-bottom:12px">
      <div class="card"><div class="k">附加成本合计</div><div class="v">${esc(d.total_charges)}</div></div>
      <div class="card"><div class="k">摊入存货</div><div class="v">${esc(d.total_to_stock)}</div></div>
      <div class="card"><div class="k">转当期费用</div><div class="v" style="color:var(--err)">${esc(d.total_to_expense)}</div></div>
      <div class="card"><div class="k">单据数</div><div class="v">${rows.length}</div></div>
    </div>
    <div class="toolbar" style="box-shadow:none;border:none;padding:0;margin:0 0 12px">
      <button class="btn primary sm" id="lc-new">新增到岸成本单</button>
    </div>
    <div class="panel" style="padding:0;overflow:hidden">
      ${rows.length ? `<table class="grid"><thead><tr>
        <th>单号</th><th>日期</th><th>供应商</th><th>基准</th>
        <th class="num">附加成本</th><th class="num">摊入存货</th><th class="num">转费用</th>
        <th>状态</th><th></th>
      </tr></thead><tbody>
      ${rows.map((r) => `<tr>
        <td>${esc(r.no)}</td><td>${esc(r.date)}</td><td>${esc(r.supplier_name || r.supplier_code)}</td>
        <td>${esc(r.basis_label)}</td>
        <td class="num"><b>${esc(r.total_charges)}</b></td>
        <td class="num">${esc(r.total_to_stock)}</td>
        <td class="num" style="color:${Number(r.total_to_expense) > 0 ? "var(--err)" : "inherit"}">${esc(r.total_to_expense)}</td>
        <td><span class="tag ${r.status === "posted" ? "ok" : ""}">${esc(r.status_label)}</span></td>
        <td class="row-actions">${r.status === "draft" ? `<button class="btn ghost sm" data-lc-post="${r.id}">过账</button>` : ""}${
          r.voucher_id ? `<a class="btn ghost sm" href="#" data-voucher="${r.voucher_id}">凭证</a>` : ""
        }<button class="btn ghost sm" data-lc-view="${r.id}">明细</button></td>
      </tr>`).join("")}
      </tbody></table>` : `<div class="muted" style="padding:14px;text-align:center">
        本期没有到岸成本单。一批货从下单到入库，运费/关税常常是货款的百分之几到几十 ——
        不摊进存货成本，存货估值偏低、销售成本低估、**采购价格差异也会算歪**。
      </div>`}
    </div>
    <div id="lc-detail" class="panel" style="display:none"></div>`;

  $("#lc-new").onclick = () => openLcEditor(period, loadLc);
  $all("[data-lc-post]").forEach((b) => b.onclick = async () => {
    if (!(await confirmDialog("过账后将追加存货成本并生成凭证，且不可撤销。确认？", true))) return;
    try {
      const r = await postJson(`/landed-cost/${b.dataset.lcPost}/post`, {});
      const warn = Number(String(r.total_to_expense).replace(/,/g, "")) > 0
        ? `，其中 ${r.total_to_expense} 因货已出清转入当期费用`
        : "";
      toast(`已过账，凭证 #${r.voucher_id}，摊入存货 ${r.total_to_stock}${warn}`, "ok");
      loadLc();
    } catch (e) {
      toast(e.message, "err");
    }
  });
  $all("[data-lc-view]").forEach((b) => b.onclick = async () => {
    const box = $("#lc-detail");
    box.style.display = "";
    box.innerHTML = `<h3>明细</h3><div class="muted">加载中…</div>`;
    try {
      const r = await api(`/landed-cost?period=${encodeURIComponent(period)}`);
      const lc = (r.rows || []).find((x) => String(x.id) === b.dataset.lcView);
      if (!lc) throw new Error("找不到该单");
      box.innerHTML = `<h3>${esc(lc.no)} 明细</h3>
        <p class="muted" style="font-size:12px;line-height:1.7;margin:0 0 8px">
          运费 ${esc(lc.freight)} · 关税 ${esc(lc.duty)} · 保险 ${esc(lc.insurance)} ·
          清关 ${esc(lc.clearing)} · 其他 ${esc(lc.other)}
        </p>
        <table class="grid"><thead><tr>
          <th>存货</th><th>名称</th><th>来源采购单</th>
          <th class="num">收货量</th><th class="num">货值</th>
          <th class="num">分摊额</th><th class="num">入存货</th><th class="num">转费用</th><th class="num">当前结存</th>
        </tr></thead><tbody>
        ${(lc.lines || []).map((l) => `<tr>
          <td>${esc(l.item_code)}</td><td>${esc(l.item_name)}</td><td>${esc(l.po_no || l.po_id)}</td>
          <td class="num">${esc(l.qty)}</td><td class="num">${esc(l.amount)}</td>
          <td class="num"><b>${esc(l.allocated)}</b></td>
          <td class="num">${esc(l.to_stock)}</td>
          <td class="num" style="color:${Number(String(l.to_expense).replace(/,/g, "")) > 0 ? "var(--err)" : "inherit"}">${esc(l.to_expense)}</td>
          <td class="num muted">${esc(l.on_hand_qty)}</td>
        </tr>`).join("")}
        </tbody></table>`;
    } catch (e) {
      box.innerHTML = `<h3>明细</h3><div style="color:var(--err)">${esc(e.message)}</div>`;
    }
  });

  function loadLc() {
    viewLandedCost(main);
  }
}

/// 新增/编辑到岸成本单。取行必须走「按采购订单取」：只列**已收货**的存货行，
/// 没收到的货没有附加成本可摊（运费到了货还没到是常事）。
async function openLcEditor(period, after) {
  const mask = modal(`
    <h3>新增到岸成本单</h3>
    <div class="field"><label>来源采购订单 *</label>
      <input id="le-po" placeholder="采购订单 ID（发货/收货页可见）" /></div>
    <div class="toolbar" style="box-shadow:none;border:none;padding:6px 0">
      <button class="btn sm" id="le-load">取已收货存货行</button>
      <span class="muted" id="le-hint" style="font-size:12px"></span>
    </div>
    <div id="le-lines" class="panel" style="padding:0;overflow:auto;max-height:34vh"></div>
    <div class="field"><label>分摊基准</label>
      <select id="le-basis">
        <option value="amount">按货值金额（最常用）</option>
        <option value="qty">按数量</option>
        <option value="weight">按重量</option>
        <option value="manual">手工指定（各行自行填写）</option>
      </select></div>
    <div style="display:flex;gap:10px;flex-wrap:wrap">
      <div><label>运费</label><input id="le-freight" value="0" style="width:110px" /></div>
      <div><label>关税</label><input id="le-duty" value="0" style="width:110px" /></div>
      <div><label>保险</label><input id="le-insurance" value="0" style="width:110px" /></div>
      <div><label>清关/杂费</label><input id="le-clearing" value="0" style="width:110px" /></div>
      <div><label>其他</label><input id="le-other" value="0" style="width:110px" /></div>
    </div>
    <div class="field"><label>付款账户 <span class="muted" style="font-weight:400">（贷方是银行存款时必填）</span></label>
      <input id="le-bank" placeholder="如：100201 工商银行基本户" /></div>
    <div id="le-err" style="display:none;margin-top:8px;color:var(--err);font-size:12px"></div>
    <p class="muted" style="font-size:12px;line-height:1.7;margin-top:8px">
      借贷科目留空则取账套配置（借：存货科目；贷：资金账户）。<b>贷方默认是银行存款，
      而银行存款核算银行账户，付款账户必填</b> —— 运费是付给货代的，从哪个账户付是必填信息。
      <b>入存货 / 转费用要到过账时才算</b> —— 那时才知道货还剩多少。
    </p>
    <div style="margin-top:10px;display:flex;gap:8px">
      <button class="btn" id="le-cancel">取消</button>
      <button class="btn primary" id="le-save">保存草稿</button>
    </div>`);
  const errBox = $("#le-err", mask);
  let lines = [];

  $("#le-load", mask).onclick = async () => {
    const poId = $("#le-po", mask).value.trim();
    if (!/^\d+$/.test(poId)) {
      errBox.textContent = "请填采购订单 ID（纯数字）";
      errBox.style.display = "";
      return;
    }
    try {
      const r = await api(`/landed-cost/from-po?po_id=${poId}`);
      lines = r.rows || [];
      $("#le-hint", mask).textContent = `取到 ${lines.length} 行`;
      if (!lines.length) {
        $("#le-lines", mask).innerHTML = `<div class="muted" style="padding:12px">${
          r.note || "该采购订单还没有已收货的存货行 —— 未收到的货没有附加成本可摊"
        }</div>`;
        return;
      }
      $("#le-lines", mask).innerHTML = `<table class="grid"><thead><tr>
        <th>存货</th><th>名称</th><th class="num">收货量</th><th class="num">货值</th><th class="num">手工分摊额</th>
      </tr></thead><tbody>
      ${lines.map((l, i) => `<tr>
        <td>${esc(l.item_code)}</td><td>${esc(l.item_name)}</td>
        <td class="num">${esc(l.qty)}</td><td class="num">${esc(l.amount)}</td>
        <td class="num"><input data-lc-m="${i}" value="0" style="width:100px;text-align:right" /></td>
      </tr>`).join("")}
      </tbody></table>`;
      errBox.style.display = "none";
    } catch (e) {
      errBox.textContent = e.message;
      errBox.style.display = "";
    }
  };
  $("#le-cancel", mask).onclick = () => closeModal();
  $("#le-save", mask).onclick = async () => {
    errBox.style.display = "none";
    if (!lines.length) {
      errBox.textContent = "请先「取已收货存货行」";
      errBox.style.display = "";
      return;
    }
    const poId = Number($("#le-po", mask).value.trim());
    try {
      await postJson("/landed-cost", {
        period: Number(String(period).replace(/-/g, "")),
        date: period + "-15",
        basis: $("#le-basis", mask).value,
        bank_account: $("#le-bank", mask).value.trim(),
        freight: $("#le-freight", mask).value.trim() || "0",
        duty: $("#le-duty", mask).value.trim() || "0",
        insurance: $("#le-insurance", mask).value.trim() || "0",
        clearing: $("#le-clearing", mask).value.trim() || "0",
        other: $("#le-other", mask).value.trim() || "0",
        lines: lines.map((l, i) => {
          const mi = $(`[data-lc-m="${i}"]`, mask);
          return {
            po_id: poId,
            item_code: l.item_code,
            item_name: l.item_name,
            qty: l.qty,
            weight: l.weight,
            amount: l.amount,
            manual_amount: mi ? mi.value.trim() || "0" : "0",
          };
        }),
      });
      toast("已保存草稿，运费等已登记；过账后才会摊入存货", "ok");
      closeModal();
      if (after) after();
    } catch (e) {
      errBox.textContent = e.message;
      errBox.style.display = "";
    }
  };
}

// ---------------- 存货成本差异（标准成本） ----------------
//
// 与「生产 → 成本差异」（实际 vs BOM，订单级）是**两个不同口径**，数字本就不该相等：
// 前者看物料采购与耗用，后者看单张生产订单的成本达成。差异每行带构成它的流水 id，
// 点开能看明细 —— 「超支 3 万」不是可行动的信息，「集中在 3 个物料」才是。
async function viewCostVariance(main) {
  const period = state.current;
  main.innerHTML = `<h2>存货成本差异</h2><div class="muted">加载中…</div>`;
  let d;
  try {
    d = await api(`/cost/material-variance?period=${encodeURIComponent(period)}`);
  } catch (e) {
    main.innerHTML = `<h2>存货成本差异</h2><div style="color:var(--err)">${esc(e.message)}</div>`;
    return;
  }
  const rows = d.rows || [];
  const sign = (v) => (Number(v) > 0 ? "var(--err)" : Number(v) < 0 ? "var(--ok)" : "inherit");

  main.innerHTML = `
    <h2>存货成本差异 · ${esc(period)}</h2>
    <div class="muted" style="margin-bottom:10px;line-height:1.7">
      口径：<b>存货级</b>——按收发存流水算「买贵了 / 耗用超标准」。<br>
      与「生产 → 成本差异」（实际 vs BOM，订单级）是<b>两个不同口径</b>，数字本就不该相等。
      只统计<b>已设标准成本单价</b>的存货；逐存货配置的计价方式优先于全局方法。
      实际单价已含<b>到岸成本</b>（运费/关税等）—— 不含的话，「货价便宜」会被
      运费吃掉，而差异表照样显示「节约」。正差异=超支，负差异=节约。
    </div>
    ${(d.warnings || []).length ? `<div class="banner unset" id="cv-warn" style="margin-bottom:10px">
      <b>需先补数据</b>
      <ul style="margin:6px 0 0 18px;line-height:1.7">${d.warnings.map((w) => `<li>${esc(w)}</li>`).join("")}</ul>
    </div>` : ""}
    <div class="cards" style="margin-bottom:12px">
      <div class="card"><div class="k">采购价格差异合计</div><div class="v" style="color:${sign(d.total_price)}">${esc(d.total_price)}</div></div>
      <div class="card"><div class="k">成本超支差异合计</div><div class="v" style="color:${sign(d.total_spend)}">${esc(d.total_spend)}</div></div>
      <div class="card"><div class="k">参与统计存货</div><div class="v">${rows.length}</div></div>
    </div>
    <div class="panel">
      <h3 style="margin:0 0 10px">差异明细 <span class="muted" style="font-weight:400;font-size:12px">（按价格差异绝对值排序，点行看构成流水）</span></h3>
      ${rows.length ? `<table class="grid"><thead><tr>
        <th>存货</th><th>名称</th><th class="num">标准成本</th><th class="num">参考成本</th>
        <th class="num">实际入库单价</th><th class="num">入库数量</th><th class="num">到岸成本</th><th class="num">价格差异</th>
        <th class="num">出库数量</th><th class="num">出库标准金额</th><th class="num">出库实际成本</th><th class="num">超支差异</th>
      </tr></thead><tbody>
      ${rows.map((r) => `<tr data-cv-item="${esc(r.item)}" style="cursor:pointer" title="点击查看构成流水">
        <td>${esc(r.item)}</td><td>${esc(r.item_name)}</td>
        <td class="num">${esc(r.standard_cost)}</td>
        <td class="num muted">${esc(r.ref_cost)}</td>
        <td class="num" style="color:${r.actual_unit != r.standard_cost ? sign(Number(r.actual_unit) - Number(r.standard_cost)) : "inherit"}">${esc(r.actual_unit)}</td>
        <td class="num">${esc(r.in_qty)}</td>
        <td class="num" title="运费/关税等，已计入实际单价">${esc(r.landed_cost)}${Number(r.other_adjust) ? `<span class="muted">+${esc(r.other_adjust)}</span>` : ""}</td>
        <td class="num" style="color:${sign(r.price_variance)}"><b>${esc(r.price_variance)}</b></td>
        <td class="num">${esc(r.out_qty)}</td>
        <td class="num">${esc(r.out_standard_amount)}</td>
        <td class="num">${esc(r.out_amount)}</td>
        <td class="num" style="color:${sign(r.spend_variance)}"><b>${esc(r.spend_variance)}</b></td>
      </tr>`).join("")}
      </tbody></table>` : `<div class="muted" style="padding:10px 0">
        本期没有参与差异统计的存货。差异分析<b>只统计已设标准成本单价</b>的存货——
        移动加权/先进先出是另一套口径，混进来没有可比性。
      </div>`}
    </div>
    ${(d.missing || []).length ? `<div class="panel">
      <h3 style="margin:0 0 8px">本表<b>不</b>包含的差异项</h3>
      <ul style="margin:0 0 0 18px;line-height:1.8">${d.missing.map((x) => `<li>${esc(x)}</li>`).join("")}</ul>
      <p class="muted" style="margin:8px 0 0;font-size:12px">
        这三档的输入（标准 BOM / 标准工时与工资率 / 产能与固定费用预算）目前都没有维护入口。
        缺输入时硬算等于编数，编出来的数比没有更危险 —— 所以这里点名说明，而不是给一个数。
      </p>
    </div>` : ""}
    <div id="cv-detail" class="panel" style="display:none"></div>`;

  $all("tr[data-cv-item]", main).forEach((tr) => {
    tr.onclick = async () => {
      const r = rows.find((x) => x.item === tr.dataset.cvItem);
      if (!r) return;
      const box = $("#cv-detail");
      box.style.display = "";
      box.innerHTML = `<h3>构成流水 · ${esc(r.item)} ${esc(r.item_name)}</h3><div class="muted">加载中…</div>`;
      try {
        // 用差异行自带的 move_ids 取构成流水：差异表已经把这几张单据算清楚了，
        // 这里只列出来给人看，不重新推导一遍，避免两处口径打架。
        const ids = r.move_ids || [];
        if (!ids.length) {
          box.innerHTML = `<h3>构成流水 · ${esc(r.item)}</h3><div class="muted">本期该存货没有金额发生额。</div>`;
          return;
        }
        const list = (await Promise.all(ids.map((id) => api(`/inventory/move/${id}`).catch(() => null)))).filter(Boolean);
        box.innerHTML = `<h3>构成流水 · ${esc(r.item)} ${esc(r.item_name)}（${list.length} 张）</h3>
          <table class="grid"><thead><tr><th>日期</th><th>类型</th><th class="num">数量</th><th class="num">单价</th><th class="num">金额</th><th>备注</th></tr></thead>
          <tbody>${list.map((m) => `<tr>
            <td>${esc(m.biz_date)}</td><td>${esc(m.kind_label)}</td>
            <td class="num">${esc(m.qty)}</td><td class="num">${esc(m.price)}</td>
            <td class="num">${esc(m.amount)}</td><td>${esc(m.memo)}</td></tr>`).join("")}</tbody></table>`;
      } catch (e) {
        box.innerHTML = `<h3>构成流水</h3><div style="color:var(--err)">${esc(e.message)}</div>`;
      }
    };
  });
}

async function viewInvoices(main) {
  main.innerHTML = `<h2>发票管理</h2><div class="muted">加载中…</div>`;
  let d;
  try { d = await loadInvoices(); } catch (e) { main.innerHTML = `<h2>发票管理</h2><div class="muted" style="color:var(--err)">${esc(e.message)}</div>`; return; }
  renderInvoices(main, d);
}

function renderInvoices(main, d) {
  const rows = (d && d.rows) || [];
  const sum = (d && d.summary) || {};
  const statusBadge = (s) => {
    if (s === "verified") return `<span class="tag ok">已认证</span>`;
    if (s === "rejected") return `<span class="tag err">已作废</span>`;
    return `<span class="tag warn">待认证</span>`;
  };
  const kindLabel = (k) => (k === "out" ? "销项" : "进项");
  main.innerHTML = `
    <h2>发票管理</h2>
    <div class="cards" style="margin-bottom:14px">
      <div class="card"><div class="k">发票总数</div><div class="v">${rows.length}</div></div>
      <div class="card"><div class="k">进项价税合计</div><div class="v" style="font-size:18px">${esc((sum.in && sum.in.amount_tax) || "0.00")}</div></div>
      <div class="card"><div class="k">销项价税合计</div><div class="v" style="font-size:18px">${esc((sum.out && sum.out.amount_tax) || "0.00")}</div></div>
    </div>
    <div class="toolbar">
      <input id="inv-kw" placeholder="号码 / 代码 / 购销方" style="width:180px" />
      <select id="inv-kind">
        <option value="">全部类型</option>
        <option value="in">进项</option>
        <option value="out">销项</option>
      </select>
      <select id="inv-status">
        <option value="">全部状态</option>
        <option value="pending">待认证</option>
        <option value="verified">已认证</option>
        <option value="rejected">已作废</option>
      </select>
      <button class="btn" id="inv-query">查询</button>
      <div class="spacer"></div>
      ${can("voucher_new") ? `<button class="btn primary" id="inv-new">新增发票</button>` : ""}
    </div>
    <div class="panel" style="padding:0;overflow:hidden">
      <table class="grid" id="inv-list">
        <thead><tr>
          <th>类型</th><th>发票号码</th><th>开票日期</th><th>购买方</th><th>销售方</th>
          <th class="num">不含税</th><th class="num">税额</th><th class="num">价税合计</th><th>状态</th><th></th>
        </tr></thead>
        <tbody>
          ${rows.length ? rows.map((r) => `
            <tr>
              <td>${kindLabel(r.kind)}</td>
              <td>${esc(r.number)}</td>
              <td>${esc(r.date)}</td>
              <td>${esc(r.buyer)}</td>
              <td>${esc(r.seller)}</td>
              <td class="num">${esc(r.amount)}</td>
              <td class="num">${esc(r.tax)}</td>
              <td class="num">${esc(r.amount_tax)}</td>
              <td>${statusBadge(r.status)}</td>
              <td class="row-actions">
                <button class="btn sm ghost" data-inv-chain="${r.id}">链</button>
                ${can("voucher_edit") ? `<button class="btn sm ghost" data-act="edit" data-id="${r.id}">编辑</button>` : ""}
                ${r.status === "pending" && can("voucher_edit") ? `<button class="btn sm ghost" data-act="verify" data-id="${r.id}">认证</button>` : ""}
                ${r.status !== "rejected" && can("voucher_edit") ? `<button class="btn sm ghost" data-act="reject" data-id="${r.id}">作废</button>` : ""}
                ${can("voucher_delete") ? `<button class="btn sm ghost" data-act="del" data-id="${r.id}">删除</button>` : ""}
              </td>
            </tr>`).join("") : `<tr><td colspan="10" class="muted" style="text-align:center;padding:18px">暂无发票</td></tr>`}
        </tbody>
      </table>
    </div>`;
  $("#inv-query").addEventListener("click", async () => {
    const d2 = await loadInvoices({ kind: $("#inv-kind").value, status: $("#inv-status").value, keyword: $("#inv-kw").value });
    renderInvoices(main, d2);
  });
  if ($("#inv-new")) $("#inv-new").addEventListener("click", () => openInvoiceEditor(main, null));
  $all("[data-inv-chain]", main).forEach((b) => b.addEventListener("click", () => openDocChain("invoice", parseInt(b.dataset.invChain, 10), "发票")));
  $all("[data-act]", main).forEach((b) => b.addEventListener("click", async () => {
    const id = parseInt(b.dataset.id, 10);
    const act = b.dataset.act;
    try {
      if (act === "edit") {
        const list = (await loadInvoices()).rows || [];
        const inv = list.find((x) => x.id === id);
        if (inv) openInvoiceEditor(main, inv);
      } else if (act === "verify") {
        const up = await api(`/invoices/${id}/status`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ status: "verified" }) });
        if (up) { toast("已认证", "ok"); if (state.view === "invoices") renderInvoices(main, await loadInvoices()); }
      } else if (act === "reject") {
        if (!(await confirmDialog("确定作废该发票？", true))) return;
        const up = await api(`/invoices/${id}/status`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ status: "rejected" }) });
        if (up) { toast("已作废", "ok"); if (state.view === "invoices") renderInvoices(main, await loadInvoices()); }
      } else if (act === "del") {
        if (!(await confirmDialog("确定删除该发票？", true))) return;
        await api(`/invoices/${id}`, { method: "DELETE" });
        toast("已删除", "ok");
        if (state.view === "invoices") renderInvoices(main, await loadInvoices());
      }
    } catch (e) { toast(e.message, "err"); }
  }));
}

function openInvoiceEditor(main, inv) {
  const isEdit = !!inv;
  const mask = modal(`
    <h3>${isEdit ? "编辑发票" : "新增发票"}</h3>
    <div class="field"><label>类型</label>
      <select id="inv-kind2">
        <option value="in" ${!isEdit || inv.kind === "in" ? "selected" : ""}>进项</option>
        <option value="out" ${isEdit && inv.kind === "out" ? "selected" : ""}>销项</option>
      </select>
    </div>
    <div class="field"><label>发票代码</label><input id="inv-code" value="${esc(inv ? inv.code : "")}" /></div>
    <div class="field"><label>发票号码 *</label><input id="inv-number" value="${esc(inv ? inv.number : "")}" /></div>
    <div class="field"><label>开票日期</label><input id="inv-date" value="${esc(inv ? inv.date : today())}" /></div>
    <div class="field"><label>购买方</label><input id="inv-buyer" value="${esc(inv ? inv.buyer : "")}" /></div>
    <div class="field"><label>销售方</label><input id="inv-seller" value="${esc(inv ? inv.seller : "")}" /></div>
    <div class="field"><label>价税合计</label><input id="inv-amt" value="${esc(inv ? inv.amount_tax : "0")}" /></div>
    <div class="field"><label>不含税金额</label><input id="inv-amount" value="${esc(inv ? inv.amount : "0")}" /></div>
    <div class="field"><label>税额</label><input id="inv-tax" value="${esc(inv ? inv.tax : "0")}" /></div>
    <div class="field"><label>税率（如 0.13）</label><input id="inv-rate" value="${esc(inv ? inv.tax_rate : "0")}" /></div>
    <div class="field"><label>备注</label><input id="inv-memo" value="${esc(inv ? inv.memo : "")}" /></div>
    <div class="foot">
      <button class="btn ghost" id="inv-close">取消</button>
      <button class="btn primary" id="inv-save">保存</button>
    </div>
  `, true);
  $("#inv-close").addEventListener("click", closeModal);
  $("#inv-save").addEventListener("click", async () => {
    const number = $("#inv-number").value.trim();
    if (!number) { toast("发票号码必填", "err"); return; }
    const body = {
      id: isEdit ? inv.id : 0,
      kind: $("#inv-kind2").value,
      code: $("#inv-code").value.trim(),
      number,
      date: $("#inv-date").value.trim(),
      buyer: $("#inv-buyer").value.trim(),
      seller: $("#inv-seller").value.trim(),
      amount_tax: $("#inv-amt").value.trim() || "0",
      amount: $("#inv-amount").value.trim() || "0",
      tax: $("#inv-tax").value.trim() || "0",
      tax_rate: $("#inv-rate").value.trim() || "0",
      status: isEdit ? inv.status : "pending",
      memo: $("#inv-memo").value.trim(),
    };
    try {
      if (isEdit) {
        await api(`/invoices/${inv.id}`, { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
        toast("已保存", "ok");
      } else {
        await api("/invoices", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
        toast("已新增", "ok");
      }
      closeModal();
      if (state.view === "invoices") renderInvoices(main, await loadInvoices());
    } catch (e) { toast(e.message, "err"); }
  });
}
