// FinBook Web 前端 — 数据导入（其他软件 / CSV / Excel）、明细账、期初批量粘贴
//
// 这一份是 `crates/finweb/static/app.js` 拆分出来的（原单文件 14k 行、295 个顶层函数）。
// 拆分方式：**经典 script，按原始行序切分，不重排任何代码**——顶层 function/const 的
// 提升与声明顺序完全保持原样，所以跨文件不存在 TDZ/未定义问题；只有 `VIEWS` 路由表
// 因为引用全部视图函数，被挪到了最后加载的 app-hr.js。
// 加载顺序由 `index.html` 的 <script> 顺序决定（也是 `main.rs` 里 asset_version 的清单顺序）。
// 新增文件必须同步三处：index.html、main.rs 的 asset_version 清单、tools/lib-app-src.js 的解析（自动）。
// ===========================================================================
// 数据导入（其他软件 / CSV / Excel）
// ===========================================================================
async function viewImports(main) {
  main.innerHTML = `
    <h2>数据导入</h2>
    <div class="panel">
      <p class="muted" style="margin:0 0 12px;line-height:1.6">
        从其他财务软件（金蝶 / 用友）或 Excel 导入数据。支持 <b>基础资料</b>（辅助核算档案 / 存货 / 科目）、
        <b>期初余额</b>、<b>期初库存</b> 与 <b>记账凭证</b>；遇到账套里没有的科目可手动映射，
        重复编码自动跳过（幂等可重导）。
      </p>
      <div class="toolbar" style="box-shadow:none;border:none;padding:0;margin:0">
        <label>导入类型</label>
        <select id="imp-kind">
          <optgroup label="基础资料（会计/各岗位）">
            <option value="aux">辅助核算档案（客户/供应商/部门/存货…）</option>
            <option value="item">存货档案</option>
            <option value="account">会计科目</option>
          </optgroup>
          <optgroup label="期初（会计）">
            <option value="begin">期初余额表</option>
            <option value="opening_stock">期初库存（数量/批次）</option>
            <option value="arap_opening">往来期初明细（应收/应付按单据）</option>
          </optgroup>
          <optgroup label="凭证（会计）">
            <option value="voucher">记账凭证</option>
          </optgroup>
          <optgroup label="出纳（银行对账）">
            <!-- 出纳的数据不走这张表：银行对账单的对账/勾对是另一套逻辑
                 （要按金额+日期容差去勾账内分录，CSV 结构也不同）。所以这里
                 给出的是**入口跳转**而不是多一个导入类型 —— 早先出纳在这张
                 表里一个选项都找不到，得自己知道去「银行对账」页。 -->
            <option value="__bank">银行对账单 → 导入并自动勾对（转到银行对账）</option>
          </optgroup>
        </select>
        <label>来源模板</label>
        <select id="imp-template">
          <option value="generic">通用</option>
          <option value="kingdee">金蝶</option>
          <option value="yonyou">用友</option>
        </select>
        <label>期间（凭证用，YYYYMM）</label>
        <input id="imp-period" placeholder="202601" value="${state.current ? String(state.current).replace('-','') : ""}" style="width:90px" />
        <div class="spacer"></div>
        <button class="btn ghost sm" id="imp-tpl">下载模板</button>
        <button class="btn primary" id="imp-analyze">预检</button>
        <button class="btn" id="imp-run">执行导入</button>
      </div>
      <div style="margin-top:10px">
        <label style="font-weight:600">选择 Excel 文件（.xlsx / .xls / .ods，可选）</label>
        <input type="file" id="imp-file" accept=".xlsx,.xls,.ods" style="display:block;margin:4px 0 8px" />
        <textarea id="imp-text" rows="8"></textarea>
      </div>
      <div id="imp-result" class="muted" style="margin-top:10px;min-height:20px;white-space:pre-wrap;font-size:13px"></div>
      <div id="imp-goto" style="margin-top:10px;display:none"></div>
      <div id="imp-issues" style="margin-top:8px;display:none"></div>
    </div>
    <div class="panel" id="imp-mapping-wrap" style="display:none">
      <h3 style="margin:0 0 10px">缺失科目映射</h3>
      <p class="muted" style="margin:0 0 10px">以下科目在当前账套中不存在，请为每个选择目标科目（留空 = 忽略该科目对应行）。</p>
      <div id="imp-mapping"></div>
    </div>
    <div class="panel" style="margin-top:12px">
      <div style="display:flex;align-items:center;gap:8px;flex-wrap:wrap"><h3 style="margin:0">导出计划任务</h3><span class="grow"></span>
        <span class="muted" style="font-size:12px">每日定时写 CSV 到服务器 books/exports/（需「导出」权限）</span>
        ${can("export") ? `<button class="btn primary sm" id="ex-new">新增任务</button>` : ""}
        <button class="btn ghost sm" id="ex-reload">刷新</button>
      </div>
      <div id="ex-list" class="muted" style="margin-top:8px">加载中…</div>
    </div>`;
  let mapping = {};
  let fileB64 = "";
  // 加载科目列表供映射下拉
  let accounts = [];
  try { accounts = await api("/accounts"); } catch (e) {}
  // 异步竞态守卫：本次 await 期间用户可能已切走（renderMain 会换掉整个 #main 节点）。
  // 此时本次调用若继续往下绑监听器，`$("#imp-xxx")` 走的是 document 全局查询，会绑到
  // **新视图的同名元素**上；而新视图自己也会绑一遍。轻则事件触发两次，重则两边都绑到
  // 已卸载的游离节点、页面按钮彻底失灵（点「预检」毫无反应）。
  // 判据：本次视图的 #main 仍在文档里，才说明我们仍是当前视图。
  if (!main.isConnected) return;
  const acctOpts = (sel) => accounts.map((a) => `<option value="${esc(a.code)}" ${sel === a.code ? "selected" : ""}>${esc(a.code)} ${esc(a.name)}</option>`).join("");

  // Excel 文件 → base64
  $("#imp-file").addEventListener("change", (ev) => {
    const f = ev.target.files && ev.target.files[0];
    if (!f) { fileB64 = ""; return; }
    const reader = new FileReader();
    reader.onload = () => {
      const dataUrl = String(reader.result || "");
      fileB64 = dataUrl.split(",").slice(1).join(","); // 去掉 data:...;base64, 前缀
      $("#imp-result").textContent = `已选择文件：${f.name}（${(f.size / 1024).toFixed(1)} KB），点击「预检科目」或「执行导入」。`;
    };
    reader.readAsDataURL(f);
  });

  // 模板下载（当前类型）+ 列头提示随类型切换
  const TPL_COLS = {
    aux: "类型,编码,名称,备注（客户/供应商/部门/职员/项目/银行/存货）",
    item: "编码,名称,保质期天,安全库存",
    account: "编码,名称,类别,方向,备注（类别空按编码首位推）",
    opening_stock: "存货编码,仓库,数量,单价,批次号,生产日期,备注（只入数量，不生成凭证）",
    arap_opening: "类型(应收/应付),客商编码,单据号,单据日期,金额,客商名称,备注（影子挂账进账龄，不生成凭证）",
    begin: "科目编码, 方向(借/贷), 金额（金蝶/用友用完整列）",
    voucher: "日期, 凭证字, 摘要, 科目编码, 借方, 贷方",
  };
  const kindSel = $("#imp-kind");
  const syncKind = () => {
    const k = kindSel.value;
    // 出纳那一项是**入口跳转**，不是导入类型：银行对账单要对账内分录做
    // 金额+日期容差勾对，CSV 结构与这里的档案/凭证导入完全不同，
    // 硬塞进同一个 kind 只会让人以为「导进来就等于对上了」。
    if (k === "__bank") {
      toast("银行对账单在「期末 → 银行对账」里导入并自动勾对，正在跳转…");
      // 复位必须在 click **之前**：bc.click() 同步走 goView → saveViewState，那一刻
      // 它读的正是 #imp-kind 的值；放在 click 后面就晚了，快照里存下 __bank。
      kindSel.value = "aux";
      const bc = $all(".nav-item").find((b) => b.dataset.view === "bank");
      if (bc) bc.click();
      else toast("没有「银行对账」入口，可能当前账号缺「凭证录入」权限", "err");
      return;
    }
    $("#imp-text").placeholder = `或直接粘贴 CSV 内容…\n列：${TPL_COLS[k] || ""}\n（或点「下载模板」拿标准模板填）`;
    $("#imp-analyze").textContent = k === "begin" || k === "voucher" ? "预检科目" : "预检";
    // 切换导入类型时重置已选文件、粘贴内容、科目映射，避免用「上一种类型的文件」
    // 去预检/导入「这个类型」——科目编码体系不同，映射也会错位。
    fileB64 = "";
    mapping = {};
    $("#imp-file").value = "";
    $("#imp-text").value = "";
    $("#imp-result").textContent = "";
    $("#imp-issues").style.display = "none";
    $("#imp-issues").innerHTML = "";
    $("#imp-mapping-wrap").style.display = "none";
    $("#imp-run").disabled = false;
    $("#imp-run").title = "";
  };
  kindSel.addEventListener("change", syncKind);
  syncKind();
  $("#imp-tpl").addEventListener("click", () => {
    window.location = `/api/import/template?kind=${encodeURIComponent(kindSel.value)}`;
  });

  $("#imp-analyze").addEventListener("click", async () => {
    const text = $("#imp-text").value;
    const kind = $("#imp-kind").value;
    const template = $("#imp-template").value;
    if (!text.trim() && !fileB64) { toast("请粘贴 CSV 内容或选择 Excel 文件", "err"); return; }
    try {
      const period = parseInt(($("#imp-period").value || "0").replace(/[^0-9]/g, ""), 10) || 0;
      const r = await api("/import/analyze", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ kind, text, template, file: fileB64 || null, period, mapping }) });
      const missing = (r && r.missing) || [];
      const issues = (r && r.issues) || [];
      const wrap = $("#imp-mapping-wrap");
      const box = $("#imp-mapping");
      const runBtn = $("#imp-run");

      // 逐条问题：按类别分组，error 排前面，带源文件行号。
      // 原来预检只回答「有没有不存在的科目」，期间已结账 / 科目停用 / 借贷不平
      // 这类真正会让导入失败的原因要点完「执行导入」才暴露。行号是刚需 ——
      // 几百行里错 3 行时，「借贷不平 ×3」用户无从下手。
      const issueBox = $("#imp-issues");
      if (issues.length) {
        const byCat = {};
        for (const i of issues) (byCat[i.category] = byCat[i.category] || []).push(i);
        // 同一类别内按行号排序，方便从头往下改
        issueBox.innerHTML = Object.keys(byCat).sort((a, b) => {
          const ea = byCat[a].some((x) => x.level === "error") ? 0 : 1;
          const eb = byCat[b].some((x) => x.level === "error") ? 0 : 1;
          return ea - eb;
        }).map((cat) => {
          const list = byCat[cat].slice().sort((x, y) => (x.row || 0) - (y.row || 0));
          return `
          <div style="margin-bottom:8px">
            <b style="color:${list.some((x) => x.level === "error") ? "var(--err)" : "var(--muted)"}">${esc(cat)}（${list.length}）</b>
            <ul style="margin:4px 0 0 18px;padding:0">
              ${list.map((i) => {
                const tag = i.level === "error" ? '<span style="color:var(--err)">✕</span>' : '<span class="muted">!</span>';
                const at = i.row ? `<span class="muted">[行 ${esc(i.row)}]</span> ` : "";
                return `<li style="font-size:13px">${tag} ${at}${esc(i.detail)}</li>`;
              }).join("")}
            </ul>
          </div>`;
        }).join("");
        issueBox.style.display = "";
      } else {
        issueBox.style.display = "none";
        issueBox.innerHTML = "";
      }

      // 有 error 就别让用户点执行 —— 但这只是前端提示，真正把关仍在写入路径
      if (r && r.blocking) {
        runBtn.disabled = true;
        runBtn.title = "预检发现会导致导入失败的问题，请先处理";
      } else {
        runBtn.disabled = false;
        runBtn.title = "";
      }

      if (missing.length === 0) {
        wrap.style.display = "none";
        const extra = (r && r.errors) ? `⚠ ${r.errors} 个问题、${r.warns || 0} 个提醒` : "✅ 预检通过";
        $("#imp-result").textContent = `${extra}${r && r.total_rows ? `（共 ${r.total_rows} 行数据）` : ""}：所有科目在账套中均存在。`;
        return;
      }
      box.innerHTML = missing.map((m) => `
        <div style="display:flex;gap:8px;align-items:center;margin-bottom:6px">
          <span style="min-width:140px;font-family:monospace">${esc(m.code)} <span class="muted">×${m.count}</span></span>
          <select class="imp-map" data-code="${esc(m.code)}" style="flex:1">
            <option value="">— 忽略 —</option>
            ${acctOpts("")}
          </select>
        </div>`).join("");
      wrap.style.display = "";
      $("#imp-result").textContent = `找到 ${missing.length} 个缺失科目，请选择映射或忽略。缺失科目可映射，不算阻断。`;
      $all(".imp-map", box).forEach((s) => s.addEventListener("change", async () => {
        mapping[s.dataset.code] = s.value;
        // 改完映射立刻重跑预检：映射能消掉「科目缺失」，也可能暴露新的
        // 问题（映射到停用科目上）。不刷新的话用户看到的是上一轮的结果。
        $("#imp-analyze").click();
      }));
    } catch (e) { $("#imp-result").textContent = "预检失败：" + e.message; }
  });

  $("#imp-run").addEventListener("click", async () => {
    const text = $("#imp-text").value;
    const kind = $("#imp-kind").value;
    const template = $("#imp-template").value;
    const period = parseInt(($("#imp-period").value || "0").replace(/[^0-9]/g, ""), 10) || 0;
    if (!text.trim() && !fileB64) { toast("请粘贴 CSV 内容或选择 Excel 文件", "err"); return; }
    try {
      const r = await api("/import/run", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ kind, text, template, file: fileB64 || null, period, mapping }) });
      const lines = [`✅ 成功导入 ${r.ok} 条`, r.skipped ? `⚠ 跳过 ${r.skipped} 条` : ""].filter(Boolean);
      if (r.warnings && r.warnings.length) {
        lines.push("", "警告：");
        r.warnings.slice(0, 20).forEach((w) => lines.push("  · " + w));
        if (r.warnings.length > 20) lines.push(`  …共 ${r.warnings.length} 条警告`);
      }
      // 明确说清「导完之后该干什么」—— 不说的话「导入成功」很容易被理解成
      // 「已经进账了」，而凭证其实是草稿，还要审核 → 记账。
      if (r.next_steps && r.next_steps.length) {
        lines.push("", "接下来：");
        r.next_steps.forEach((s) => lines.push("  → " + s));
      }
      $("#imp-result").textContent = lines.join("\n");
      toast(`已导入 ${r.ok} 条`, "ok");
      // 导入凭证后给一个直达入口：凭证是草稿，还要审核→记账，提醒用户去看
      if (kind === "voucher" && r.ok > 0) {
        const goto = $("#imp-goto");
        goto.style.display = "";
        goto.innerHTML = `<button class="btn primary sm" id="imp-goto-vouchers">查看导入的凭证</button>`;
        $("#imp-goto-vouchers").onclick = () => { location.hash = "#/vouchers"; };
      } else {
        $("#imp-goto").style.display = "none";
      }
      // 不要自动跳走：结果和「接下来」都在这一屏上，跳走等于把刚说的话藏起来
    } catch (e) { $("#imp-result").textContent = "导入失败：" + e.message; }
  });

  // 导出计划任务：列表 / 新增 / 立即执行 / 删除
  const EX_KINDS = { vouchers: "记账凭证", trial: "科目余额表", payroll: "工资表", claims: "报销单" };
  const loadEx = async () => {
    try {
      const r = await api("/export/schedules");
      const rows = r.rows || [];
      $("#ex-list").innerHTML = rows.length
        ? `<table class="grid"><thead><tr><th>类型</th><th>期间</th><th>执行时刻</th><th>状态</th><th>最近执行</th><th>备注</th><th></th></tr></thead><tbody>${rows.map((x) => `<tr>
            <td>${esc(EX_KINDS[x.kind] || x.kind)}</td><td>${x.period_mode === "last" ? "上一期间" : "当前期间"}</td><td>${esc(x.at_time)}</td>
            <td>${x.enabled ? '<span class="tag ok">启用</span>' : '<span class="tag">停用</span>'}</td>
            <td>${esc(x.last_run || "—")}</td><td>${esc(x.memo || "")}</td>
            <td class="row-actions">${can("export") ? `<button class="btn ghost sm" data-ex-run="${x.id}">立即执行</button><button class="btn danger sm" data-ex-del="${x.id}">删</button>` : ""}</td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">暂无导出计划任务</div>`;
      $all("[data-ex-run]").forEach((b) => b.onclick = async () => {
        try { const r = await postJson(`/export/schedules/${b.dataset.exRun}/run`, {}); toast(`已导出：${r.path}`, "ok"); loadEx(); } catch (e) { toast(e.message, "err"); }
      });
      $all("[data-ex-del]").forEach((b) => b.onclick = async () => {
        if (!(await confirmDialog("删除该导出计划任务？", true))) return;
        try { await api(`/export/schedules/${b.dataset.exDel}/delete`, { method: "POST" }); toast("已删除", "ok"); loadEx(); } catch (e) { toast(e.message, "err"); }
      });
    } catch (e) { $("#ex-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  if ($("#ex-new")) $("#ex-new").onclick = () => {
    const m = modal(`<h3>新增导出计划任务</h3>
      <div class="field"><label>导出内容</label><select id="ex-kind"><option value="vouchers">记账凭证</option><option value="trial">科目余额表</option><option value="payroll">工资表</option><option value="claims">报销单</option></select></div>
      <div class="field"><label>期间口径</label><select id="ex-mode"><option value="current">当前期间</option><option value="last">上一期间</option></select></div>
      <div class="field"><label>每日执行时刻（HH:MM）</label><input id="ex-time" value="08:00" style="width:90px" /></div>
      <div class="field"><label style="display:inline-flex;gap:4px;align-items:center"><input type="checkbox" id="ex-enabled" checked /> 启用</label></div>
      <div class="field"><label>备注</label><input id="ex-memo" /></div>
      <div class="foot"><button class="btn primary" id="ex-save">保存</button><button class="btn ghost" id="ex-cancel">取消</button></div>`);
    $("#ex-cancel", m).onclick = closeModal;
    $("#ex-save", m).onclick = async () => {
      try {
        await postJson("/export/schedules", { kind: $("#ex-kind", m).value, period_mode: $("#ex-mode", m).value, at_time: $("#ex-time", m).value.trim(), enabled: $("#ex-enabled", m).checked, memo: $("#ex-memo", m).value.trim() });
        toast("已保存计划任务", "ok"); closeModal(); loadEx();
      } catch (e) { toast(e.message, "err"); }
    };
  };
  $("#ex-reload").onclick = loadEx;
  loadEx();
}

// ===========================================================================
// 明细账
// ===========================================================================
let ledgerTab = "detail";
// 最近一次查询的完整结果（打印用）。虚拟滚动只把可视行放进 DOM，
// 直接打印 #l-table 会只印出屏幕上的十几行，所以按这份全量数据重拼。
let lastLedger = null;
async function viewLedger(main) {
  await ensureAccounts();
  main.innerHTML = `
    <h2>账簿查询</h2>
    <div class="muted" style="font-size:12px;margin:2px 0 8px;line-height:1.7">
      三个页签的区别：<b>明细账</b> 把每一笔都列出来（对账用）；
      <b>总账</b> 一个月只出一行合计数（看走势用）；
      <b>日记账</b> 按日期排的流水（银行、现金对账用）。
    </div>
    <div class="toolbar">
      <label>科目 <input id="l-code" list="acct-list" placeholder="科目编码，如 1002" style="width:160px" /></label>
      <datalist id="acct-list">${(state.accounts || []).map((a) => `<option value="${esc(a.code)}">${esc(a.name)}</option>`).join("")}</datalist>
      <label>从 <input id="l-from" type="month" value="${esc(state.current || "")}" style="width:90px" /></label>
      <label>至 <input id="l-to" type="month" value="${esc(state.current || "")}" style="width:90px" /></label>
      <label><input type="checkbox" id="l-children" checked /> 含下级</label>
      <label><input type="checkbox" id="l-posted" checked /> 仅已记账</label>
      <button class="btn sm" id="l-go">查询</button>
      <div class="spacer"></div>
      <button class="btn ghost sm" data-ltab="detail">明细账</button>
      <button class="btn ghost sm" data-ltab="general">总账</button>
      <button class="btn ghost sm" data-ltab="journal">日记账</button>
      <button class="btn ghost sm" id="l-print">打印预览</button>
      <button class="btn ghost sm" id="l-printform">套打</button>
      ${can("export") ? `<button class="btn ghost sm" id="l-export">导出 CSV</button>` : ""}
    </div>
    <div class="panel"><table class="grid" id="l-table"><thead></thead><tbody><tr><td class="muted">请输入科目后查询</td></tr></tbody></table></div>`;
  const setTab = (t) => {
    ledgerTab = t;
    $all("[data-ltab]", main).forEach((b) => b.classList.toggle("primary", b.dataset.ltab === t));
  };
  setTab("detail");
  $all("[data-ltab]", main).forEach((b) => b.onclick = () => { setTab(b.dataset.ltab); loadLedger(); });
  $("#l-go").addEventListener("click", loadLedger);
  $("#l-print").addEventListener("click", () => {
    printPreview(({ detail: "明细账", general: "总账", journal: "日记账" })[ledgerTab] || "账簿", ledgerPrintTable());
  });
  $("#l-printform").addEventListener("click", () => {
    const code = $("#l-code").value.trim();
    if (!code) { toast("请先输入科目编码", "err"); return; }
    const q = `code=${encodeURIComponent(code)}&from=${encodeURIComponent($("#l-from").value)}&to=${encodeURIComponent($("#l-to").value)}&include_children=${$("#l-children").checked ? 1 : 0}&posted_only=${$("#l-posted").checked ? 1 : 0}&type=${ledgerTab}`;
    window.open(`/api/ledger/print-form?${q}`, "_blank");
  });
  if ($("#l-export")) $("#l-export").onclick = () => {
    const code = $("#l-code").value.trim();
    if (!code) { toast("请先输入科目编码", "err"); return; }
    const q = `code=${encodeURIComponent(code)}&from=${encodeURIComponent($("#l-from").value)}&to=${encodeURIComponent($("#l-to").value)}&include_children=${$("#l-children").checked ? 1 : 0}&posted_only=${$("#l-posted").checked ? 1 : 0}`;
    downloadUrl(`/api/export/ledger?${q}`);
  };
}
// 明细账虚拟滚动容器：只建一次，之后复用；切到总账/日记账或空态时拆掉。
//
// 顺序不能反：必须先把 wrap 放到表格原位，再把表格搬进 wrap。
// 先 `content.appendChild(tbl)` 再 `parent.replaceChild(wrap, tbl)` 时，
// 表格已经不在原父节点下，replaceChild 抛 NotFoundError —— 查询中断、
// 表格从页面消失（E2E 的「账簿查询」就是被这个卡红的）。
function ensureLedgerVScroll(tbl) {
  let wrap = tbl.closest(".ledger-vscroll");
  if (!wrap) {
    wrap = document.createElement("div");
    wrap.className = "ledger-vscroll";
    wrap.style.maxHeight = "56vh";
    wrap.style.overflow = "auto";
    wrap.style.position = "relative";
    tbl.parentNode.replaceChild(wrap, tbl);
    const placeholder = document.createElement("div");
    placeholder.dataset.vsRole = "placeholder";
    wrap.appendChild(placeholder);
    const content = document.createElement("div");
    content.dataset.vsRole = "content";
    content.style.position = "absolute";
    content.style.top = "0";
    content.style.left = "0";
    content.style.right = "0";
    wrap.appendChild(content);
    content.appendChild(tbl);
    tbl.style.width = "100%";
  }
  return {
    wrap,
    placeholder: wrap.querySelector('[data-vs-role="placeholder"]'),
    content: wrap.querySelector('[data-vs-role="content"]'),
  };
}

// 拆掉虚拟滚动容器，把表格放回原位（总账/日记账/空态用普通渲染）
function unwrapLedgerVScroll(tbl) {
  const wrap = tbl.closest(".ledger-vscroll");
  if (!wrap || !wrap.parentNode) return;
  wrap.parentNode.replaceChild(tbl, wrap);
  tbl.style.width = "";
}

async function loadLedger() {  const code = $("#l-code").value.trim();
  if (!code) { toast("请先输入科目编码", "err"); return; }
  const qs = `code=${encodeURIComponent(code)}&from=${encodeURIComponent($("#l-from").value)}&to=${encodeURIComponent($("#l-to").value)}&include_children=${$("#l-children").checked ? 1 : 0}&posted_only=${$("#l-posted").checked ? 1 : 0}`;
  const url = ledgerTab === "general" ? `/ledger/general?${qs}` : ledgerTab === "journal" ? `/ledger/journal?${qs}` : `/ledger?${qs}`;
  const tbl = $("#l-table");
  const message = (text, isErr) => {
    unwrapLedgerVScroll(tbl);
    tbl.innerHTML = `<tbody><tr><td colspan="8" ${isErr ? 'style="color:var(--err)"' : 'class="muted"'}>${esc(text)}</td></tr></tbody>`;
  };
  let rows;
  try { rows = await api(url); } catch (e) { message(e.message, true); return; }
  if (!rows.length) { message("该科目在所选期间无记录", false); return; }
  lastLedger = { tab: ledgerTab, rows, hasQty: false };
  if (ledgerTab === "general") {
    unwrapLedgerVScroll(tbl);
    tbl.innerHTML = `<thead><tr><th>期间</th><th>摘要</th><th class="num">借方</th><th class="num">贷方</th><th>方向</th><th class="num">余额</th></tr></thead><tbody>${rows.map((r) => `<tr><td>${esc(r.period)}</td><td>${esc(r.summary)}</td><td class="num">${esc(r.debit)}</td><td class="num">${esc(r.credit)}</td><td>${esc(r.dir === "debit" ? "借" : "贷")}</td><td class="num">${esc(r.balance)}</td></tr>`).join("")}</tbody>`;
    return;
  }
  if (ledgerTab === "journal") {
    unwrapLedgerVScroll(tbl);
    tbl.innerHTML = `<thead><tr><th>日期</th><th>凭证号</th><th>摘要</th><th>对方科目</th><th class="num">借方</th><th class="num">贷方</th><th>方向</th><th>出纳</th><th class="num">余额</th></tr></thead><tbody>${rows.map((r) => `<tr><td>${esc(r.date)}</td><td>${esc(r.voucher_no)}</td><td>${esc(r.summary)}</td><td>${esc(r.opposite_accounts || "")}</td><td class="num">${esc(r.debit)}</td><td class="num">${esc(r.credit)}</td><td>${esc(r.dir === "debit" ? "借" : "贷")}</td><td>${esc(r.cashier || "")}</td><td class="num">${esc(r.balance)}</td></tr>`).join("")}</tbody>`;
    return;
  }
  const hasQty = rows.some((r) => r.qty_balance != null);
  lastLedger.hasQty = hasQty;
  const head = `<tr><th>日期</th><th>凭证号</th><th>摘要</th><th class="num">借方</th><th class="num">贷方</th><th>方向</th><th class="num">余额</th>${hasQty ? '<th class="num">数量余额</th>' : ""}</tr>`;

  // --- 虚拟滚动（明细账）：只渲染可视行，滚动时增量更新 ---
  // 大账套单科目明细可达数万行，全量 innerHTML 会卡死界面；这里只把
  // 可视区域（＋1 行缓冲）渲染进 DOM，滚动时用 transform 位移内容块。
  const ROW_HEIGHT = 44;
  const vs = ensureLedgerVScroll(tbl);
  const { wrap, placeholder, content } = vs;
  placeholder.style.height = rows.length * ROW_HEIGHT + "px";
  tbl.innerHTML = "";

  const thead = tbl.createTHead();
  thead.innerHTML = `<tr>${head}</tr>`;
  $all("th", thead).forEach((th) => {
    th.style.position = "sticky";
    th.style.top = "0";
    th.style.background = "var(--panel)";
    th.style.zIndex = "4";
  });

  const tbody = tbl.createTBody();
  let vsRange = { s: -1, e: -1 };

  function renderVisible() {
    const st = wrap.scrollTop;
    // 刚插入 DOM 时 clientHeight 可能还是 0；至少按 8 行渲染，避免只显示一行。
    const vh = Math.max(wrap.clientHeight, ROW_HEIGHT * 8);
    const s = Math.max(0, Math.floor(st / ROW_HEIGHT));
    const e = Math.min(rows.length, Math.ceil((st + vh) / ROW_HEIGHT) + 1);
    if (s === vsRange.s && e === vsRange.e) return;
    vsRange = { s, e };
    content.style.transform = `translateY(${s * ROW_HEIGHT}px)`;
    tbody.innerHTML = rows.slice(s, e).map((r, i) => `
      <tr>
        <td>${esc(r.date)}</td><td>${esc(r.voucher_no)}</td><td>${esc(r.summary)}</td>
        <td class="num">${esc(r.debit)}</td><td class="num">${esc(r.credit)}</td>
        <td>${esc(r.dir === "debit" ? "借" : "贷")}</td><td class="num">${esc(r.balance)}</td>
        ${hasQty ? `<td class="num" style="color:var(--err,#c62828)">${r.qty_balance != null ? esc(fmt(r.qty_balance)) : ""}</td>` : ""}
      </tr>
    `).join("");
  }

  // 每次查询都换掉 onscroll（而不是 addEventListener），否则上一次查询的
  // 闭包会留着旧 rows，滚动时把新数据覆盖回旧内容。
  wrap.onscroll = renderVisible;
  renderVisible();
  // 布局稳定后再算一次可视区，修正首帧 clientHeight 为 0 的情况。
  requestAnimationFrame(renderVisible);
}

// 用全量 rows 拼一张打印用的表：虚拟滚动只渲染可视行，
// 直接打印 #l-table 会把屏幕外的行全丢掉。
function ledgerPrintTable() {
  const t = lastLedger;
  if (!t) return $("#l-table");
  const tbl = document.createElement("table");
  if (t.tab === "general") {
    tbl.innerHTML = `<thead><tr><th>期间</th><th>摘要</th><th class="num">借方</th><th class="num">贷方</th><th>方向</th><th class="num">余额</th></tr></thead><tbody>${t.rows.map((r) => `<tr><td>${esc(r.period)}</td><td>${esc(r.summary)}</td><td class="num">${esc(r.debit)}</td><td class="num">${esc(r.credit)}</td><td>${esc(r.dir === "debit" ? "借" : "贷")}</td><td class="num">${esc(r.balance)}</td></tr>`).join("")}</tbody>`;
  } else if (t.tab === "journal") {
    tbl.innerHTML = `<thead><tr><th>日期</th><th>凭证号</th><th>摘要</th><th>对方科目</th><th class="num">借方</th><th class="num">贷方</th><th>方向</th><th>出纳</th><th class="num">余额</th></tr></thead><tbody>${t.rows.map((r) => `<tr><td>${esc(r.date)}</td><td>${esc(r.voucher_no)}</td><td>${esc(r.summary)}</td><td>${esc(r.opposite_accounts || "")}</td><td class="num">${esc(r.debit)}</td><td class="num">${esc(r.credit)}</td><td>${esc(r.dir === "debit" ? "借" : "贷")}</td><td>${esc(r.cashier || "")}</td><td class="num">${esc(r.balance)}</td></tr>`).join("")}</tbody>`;
  } else {
    tbl.innerHTML = `<thead><tr><th>日期</th><th>凭证号</th><th>摘要</th><th class="num">借方</th><th class="num">贷方</th><th>方向</th><th class="num">余额</th>${t.hasQty ? '<th class="num">数量余额</th>' : ""}</tr></thead><tbody>${t.rows.map((r) => `<tr><td>${esc(r.date)}</td><td>${esc(r.voucher_no)}</td><td>${esc(r.summary)}</td><td class="num">${esc(r.debit)}</td><td class="num">${esc(r.credit)}</td><td>${esc(r.dir === "debit" ? "借" : "贷")}</td><td class="num">${esc(r.balance)}</td>${t.hasQty ? `<td class="num">${r.qty_balance != null ? esc(fmt(r.qty_balance)) : ""}</td>` : ""}</tr>`).join("")}</tbody>`;
  }
  return tbl;
}

// ===========================================================================
// 报表中心（打印 vs 导出 权限区分）
// ===========================================================================
/// 期初余额的批量粘贴窗口。
///
/// 与桌面端 `views/begin_batch.rs` 同一套解析（后端 `fincore::batch::begin`），
/// **方向不用填**：只给金额时按科目表的余额方向自动归位（资产记借、负债权益记贷），
/// 负数/红字括号自动转成反方向。新手最不懂的就是这一步，而它恰恰是期初不平的
/// 头号原因 —— 让用户自己判方向，等于把最容易错的一步交出去了。
function openBeginBatch(main) {
  batchPasteDialog({
    title: "批量粘贴期初余额",
    hint: [
      "把上期期末的科目余额表复制过来，每行一条。<b>先预览再导入</b> —— 粘错位（多带表头、带上合计行）会让你先看见。",
      "<b>方向不用你填</b>：只给金额时按科目自动归位（资产记借、负债权益记贷）；负数或红字括号自动转成反方向。",
      "科目编码抄错时预览会给出「像不像 XXX」的提示；已经填过该科目期初数的会跳过，<b>不会覆盖</b>。",
    ],
    example:
      "科目编码, 金额\n" +
      "1001,58000\n" +
      "1002,125000\n" +
      "2001,80000\n" +
      "也可以带方向或借贷两列：\n" +
      "1001,借,58000        ← 编码,方向,金额\n" +
      "1001,58000,0         ← 编码,借方,贷方（和表格同一布局）",
    placeholder: "科目编码, 金额\n1001,58000\n1002,125000\n2001,80000",
    trial: true,
    parse: (text) => api("/begin-batch", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ text }),
    }),
    importRows: (rows) => api("/begin-batch", {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        rows: rows.map((r) => ({ code: r.code, dir: r.dir, amount: r.amount })),
        // 草稿里已有的编码要避开：解析时看到的是库里的期初数，
        // 看不到用户**正在编辑还没保存**的那些行
        existing: (state.beginDraft || []).map((d) => d.account_code).filter(Boolean),
      }),
    }),
    cols: [
      { key: "code", label: "科目编码" },
      { key: "dir_label", label: "方向" },
      { key: "amount", label: "金额", cls: "num" },
      { key: "skip", label: "状态" },
    ],
    onDone: () => { state.beginDraft = null; viewBegin(main); },
  });
}
