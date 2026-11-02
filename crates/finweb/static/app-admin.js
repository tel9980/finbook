// FinBook Web 前端 — 安全策略、角色与用户、平台用户与平台账本、审计日志、改密
//
// 这一份是 `crates/finweb/static/app.js` 拆分出来的（原单文件 14k 行、295 个顶层函数）。
// 拆分方式：**经典 script，按原始行序切分，不重排任何代码**——顶层 function/const 的
// 提升与声明顺序完全保持原样，所以跨文件不存在 TDZ/未定义问题；只有 `VIEWS` 路由表
// 因为引用全部视图函数，被挪到了最后加载的 app-hr.js。
// 加载顺序由 `index.html` 的 <script> 顺序决定（也是 `main.rs` 里 asset_version 的清单顺序）。
// 新增文件必须同步三处：index.html、main.rs 的 asset_version 清单、tools/lib-app-src.js 的解析（自动）。
// ===========================================================================
// 安全中心（用户管理）
// ===========================================================================
async function viewSecurity(main) {
  const roles = await loadRoles();
  main.innerHTML = `
    <h2>安全中心</h2>
    <div class="panel">
      <div style="display:flex;gap:14px;align-items:center;flex-wrap:wrap">
        <button class="btn sm" id="me-pwd">修改我的口令</button>
        ${can("user_manage") ? `<button class="btn sm" id="new-user">新建用户</button>` : ""}
        ${can("user_manage") ? `<span class="spacer"></span>
          <label>搜索 <input id="u-kw" style="width:150px" placeholder="账号 / 姓名" /></label>
          <label>角色 <select id="u-role"><option value="">全部</option>${roles.map((r) => `<option value="${r.role}">${esc(r.label)}</option>`).join("")}</select></label>` : ""}
      </div>
    </div>
    ${can("user_manage") ? `<div class="panel"><table class="grid" id="u-table"><thead><tr>
      <th data-sort="username" class="sortable" title="点击排序">账号</th><th>姓名</th><th>角色</th>
      <th data-sort="last_login_at" class="sortable" title="点击排序">最近登录</th><th>锁定</th><th>强制改密</th><th>绑定设备</th><th>状态</th><th></th>
    </tr></thead><tbody><tr><td colspan="9" class="muted">加载中…</td></tr></tbody></table></div>` : `<div class="panel muted">您没有用户管理权限，仅可修改自己的口令。</div>`}
    ${session.platformAdmin ? `
    <div class="panel">
      <h3 style="margin-top:0">口令策略（平台账号）</h3>
      <div class="muted" style="font-size:12px;margin-bottom:8px">对 Web 平台账号生效：登录锁定、改密校验、空闲登出。桌面端账套口令策略在各账套「安全中心」单独设置。</div>
      <div id="sec-pol-form" class="muted">加载中…</div>
    </div>
    <div class="panel">
      <h3 style="margin-top:0">平台登录审计</h3>
      <div style="display:flex;gap:10px;align-items:center;flex-wrap:wrap;margin-bottom:8px">
        <label>账号 <input id="sec-att-user" style="width:140px" placeholder="留空 = 全部" /></label>
        <label>条数 <select id="sec-att-limit"><option>50</option><option>200</option></select></label>
        <button class="btn sm" id="sec-att-load">刷新</button>
        <span class="spacer"></span>
        <label>解锁账号 <input id="sec-unlock-user" style="width:140px" placeholder="账号" /></label>
        <button class="btn sm" id="sec-unlock">解锁</button>
      </div>
      <div id="sec-locked-box"></div>
      <div id="sec-att-table" class="muted">加载中…</div>
    </div>` : ""}`;
  $("#me-pwd").addEventListener("click", () => openChangePwd(false));
  if (session.platformAdmin) {
    loadSecPolicy();
    loadSecAudit();
    $("#sec-att-load").addEventListener("click", loadSecAudit);
    $("#sec-att-user").addEventListener("keydown", (e) => { if (e.key === "Enter") loadSecAudit(); });
    $("#sec-unlock").addEventListener("click", doSecUnlock);
  }
  if (can("user_manage")) {
    $("#new-user").addEventListener("click", openNewUser);
    $("#u-kw").addEventListener("input", loadUsers);
    $("#u-role").addEventListener("change", loadUsers);
    $all(".sortable", main).forEach((th) => th.addEventListener("click", () => {
      const key = th.dataset.sort;
      // 排序字段与方向：再次点击同列翻转；切换列时重置为升序
      window._usersSort = { key, asc: window._usersSort && window._usersSort.key === key ? !window._usersSort.asc : true };
      $all(".sortable", main).forEach((h) => h.classList.remove("sort-asc", "sort-desc"));
      th.classList.add(window._usersSort.asc ? "sort-asc" : "sort-desc");
      loadUsers();
    }));
    loadUsers();
  }
}

// ===========================================================================
// 平台安全中心（口令策略 / 登录审计 / 解锁；仅平台管理员）
// ===========================================================================
async function loadSecPolicy() {
  const box = $("#sec-pol-form");
  if (!box) return;
  let p;
  try { p = await api("/security/policy"); }
  catch (e) { box.innerHTML = `<span style="color:var(--err)">${esc(e.message)}</span>`; return; }
  box.innerHTML = `
    <div style="display:flex;gap:12px;flex-wrap:wrap">
      <div><label>最小口令长度</label><input id="sp-len" type="number" min="1" max="64" value="${p.min_len}" style="width:90px" /></div>
      <div><label>连续失败次数</label><input id="sp-fail" type="number" min="1" max="20" value="${p.max_fail}" style="width:90px" /></div>
      <div><label>锁定时长（分钟）</label><input id="sp-lock" type="number" min="1" max="1440" value="${p.lock_minutes}" style="width:100px" /></div>
      <div><label>空闲登出（分钟，0=不登出）</label><input id="sp-idle" type="number" min="0" max="1440" value="${p.idle_minutes}" style="width:100px" /></div>
      <div><label>口令有效期（天，0=永不过期）</label><input id="sp-age" type="number" min="0" max="3650" value="${p.max_age_days}" style="width:100px" /></div>
    </div>
    <div style="display:flex;gap:14px;align-items:center;margin:10px 0;flex-wrap:wrap">
      <label style="display:inline-flex;gap:4px;align-items:center"><input type="checkbox" id="sp-letter" ${p.need_letter ? "checked" : ""}/> 必须含字母</label>
      <label style="display:inline-flex;gap:4px;align-items:center"><input type="checkbox" id="sp-digit" ${p.need_digit ? "checked" : ""}/> 必须含数字</label>
      <label style="display:inline-flex;gap:4px;align-items:center"><input type="checkbox" id="sp-symbol" ${p.need_symbol ? "checked" : ""}/> 必须含特殊字符</label>
      <span class="spacer"></span>
      <button class="btn primary sm" id="sp-save">保存策略</button>
    </div>
    <div class="muted" style="font-size:12px">锁定语义：连续失败达阈值后账号写入持久锁（跨重启生效），可在下方「解锁账号」解除；策略同时约束改密校验与空闲登出。</div>`;
  $("#sp-save").addEventListener("click", async () => {
    const numv = (id) => Number($(id).value);
    const body = {
      min_len: numv("#sp-len"), need_letter: $("#sp-letter").checked, need_digit: $("#sp-digit").checked,
      need_symbol: $("#sp-symbol").checked, max_age_days: numv("#sp-age"), max_fail: numv("#sp-fail"),
      lock_minutes: numv("#sp-lock"), idle_minutes: numv("#sp-idle"),
    };
    try {
      await api("/security/policy", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
      toast("口令策略已保存", "ok");
      loadSecPolicy();
    } catch (e) { toast(e.message, "err"); }
  });
}

async function loadSecAudit() {
  const box = $("#sec-att-table");
  if (!box) return;
  const u = ($("#sec-att-user").value || "").trim();
  const limit = $("#sec-att-limit").value || "50";
  let r;
  try { r = await api(`/security/login-attempts?limit=${limit}${u ? `&username=${encodeURIComponent(u)}` : ""}`); }
  catch (e) { box.innerHTML = `<span style="color:var(--err)">${esc(e.message)}</span>`; return; }
  // 锁定账号条（与审计分开接口，失败不阻塞审计展示）
  const lockBox = $("#sec-locked-box");
  if (lockBox) {
    try {
      const lk = await api("/security/locked-users");
      lockBox.innerHTML = (lk.items || []).length
        ? `<div style="margin-bottom:8px">🔒 当前锁定：${lk.items.map((x) => `<span class="tag warn">${esc(x.username)}（剩 ${x.remaining_min} 分钟）</span>`).join(" ")}</div>`
        : "";
    } catch (e) { lockBox.innerHTML = ""; }
  }
  const items = r.items || [];
  if (!items.length) { box.innerHTML = `<div class="muted">暂无登录记录</div>`; return; }
  box.innerHTML = `<table class="grid"><thead><tr><th>时间</th><th>账号</th><th>结果</th><th>来源 IP</th></tr></thead><tbody>${
    items.map((x) => `<tr>
      <td>${esc(x.ts)}</td><td>${esc(x.username)}</td>
      <td>${x.ok ? `<span class="tag ok">成功</span>` : `<span class="tag err">失败</span>`}</td>
      <td class="muted">${esc(x.ip || "—")}</td>
    </tr>`).join("")
  }</tbody></table>`;
}

async function doSecUnlock() {
  const u = ($("#sec-unlock-user").value || "").trim();
  if (!u) { toast("请输入要解锁的账号", "err"); return; }
  try {
    await api("/security/unlock", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ username: u }) });
    toast(`已解锁 ${u}`, "ok");
    $("#sec-unlock-user").value = "";
    loadSecAudit();
  } catch (e) { toast(e.message, "err"); }
}

// 角色 → 权限矩阵（/api/roles），缓存一次
let rolesCache = null;
async function loadRoles() {
  if (rolesCache) return rolesCache;
  try { rolesCache = await api("/roles"); } catch (e) { rolesCache = []; }
  return rolesCache;
}
function rolePermsHtml(perms) {
  if (!perms || !perms.length) return `<span class="muted">无权限</span>`;
  return perms.map((p) => `<span class="tag">${esc(p.label)}</span>`).join(" ");
}

async function loadUsers() {
  const tb = $("#u-table tbody");
  const kw = ($("#u-kw") ? $("#u-kw").value : "").trim().toLowerCase();
  const roleF = $("#u-role") ? $("#u-role").value : "";
  let users;
  try { users = await api("/users"); } catch (e) { tb.innerHTML = `<tr><td colspan="9" style="color:var(--err)">${esc(e.message)} <button class="btn ghost sm" id="u-retry">重试</button></td></tr>`; const rb = $("#u-retry", tb); if (rb) rb.addEventListener("click", loadUsers); return; }
  users = users.filter((u) => {
    if (kw && !((u.username || "").toLowerCase().includes(kw) || (u.display_name || "").toLowerCase().includes(kw))) return false;
    if (roleF && u.role !== roleF && !(u.roles || []).includes(roleF)) return false;
    return true;
  });
  // 列排序（账号 / 最近登录）
  const sort = window._usersSort;
  if (sort) {
    const dir = sort.asc ? 1 : -1;
    users.sort((a, b) => {
      const av = sort.key === "username" ? (a.username || "") : (a.last_login_at || "");
      const bv = sort.key === "username" ? (b.username || "") : (b.last_login_at || "");
      return av < bv ? -dir : av > bv ? dir : 0;
    });
  }
  if (!users.length) { tb.innerHTML = `<tr><td colspan="9" class="muted">暂无匹配用户</td></tr>`; return; }
  // 缓存当前页用户，供「编辑」弹窗按用户名取完整对象
  window._usersCache = users;
  tb.innerHTML = users.map((u) => {
    // role_label 由后端提供（Role::label），避免前端硬编码与角色扩展脱节
    const roleLabel = u.role_label || u.role;
    const dev = u.device_name ? `<span class="tag">${esc(u.device_name)}</span>` : `<span class="muted">未绑定</span>`;
    const dis = u.disabled ? `<span class="tag err">已停用</span>` : `<span class="tag ok">启用</span>`;
    const lock = u.locked_until ? `<span class="tag warn">已锁定</span>` : `<span class="muted">—</span>`;
    const must = u.must_change_pwd ? `<span class="tag warn">是</span>` : `<span class="muted">否</span>`;
    const last = u.last_login_at ? esc(u.last_login_at) : `<span class="muted">从未登录</span>`;
    const me = u.username === session.user.username;
    return `<tr>
      <td>${esc(u.username)}${u.memo ? `<div class="muted" style="font-size:11px">${esc(u.memo)}</div>` : ""}</td>
      <td>${esc(u.display_name)}</td><td>${esc(roleLabel)}</td>
      <td>${last}</td><td>${lock}</td><td>${must}</td>
      <td>${dev}</td><td>${dis}</td>
      <td class="row-actions">
        <button class="btn ghost sm" data-edit="${esc(u.username)}">编辑</button>
        <button class="btn ghost sm" data-reset-pwd="${esc(u.username)}">重置口令</button>
        <button class="btn ghost sm" data-reset-dev="${esc(u.username)}">重置设备</button>
        ${u.locked_until ? `<button class="btn ghost sm" data-unlock="${esc(u.username)}">解锁</button>` : ""}
        ${me ? "" : `<button class="btn ghost sm" data-toggle="${esc(u.username)}" data-next="${u.disabled ? "0" : "1"}">${u.disabled ? "启用" : "停用"}</button>
        <button class="btn danger sm" data-del="${esc(u.username)}">删除</button>`}
      </td>
    </tr>`;
  }).join("");
  $all("[data-edit]").forEach((b) => b.onclick = () => { const u = (window._usersCache || []).find((x) => x.username === b.dataset.edit); if (u) openEditUser(u); });
  $all("[data-reset-pwd]").forEach((b) => b.onclick = () => openAdminResetPwd(b.dataset.resetPwd));
  $all("[data-toggle]").forEach((b) => b.onclick = async () => {
    const dis = b.dataset.next === "1";
    if (dis && !(await confirmDialog(`停用 ${b.dataset.toggle}？其全部会话将被立即下线。`, true))) return;
    try {
      await api(`/users/${encodeURIComponent(b.dataset.toggle)}`, { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ disabled: dis }) });
      toast(dis ? "已停用，该账号全部会话已下线" : "已启用", "ok"); loadUsers();
    } catch (e) { toast(e.message, "err"); }
  });
  $all("[data-reset-dev]").forEach((b) => b.onclick = async () => { if (!(await confirmDialog(`重置 ${b.dataset.resetDev} 的设备绑定？将解绑该账号「全部」已绑定设备并强制下线，下次登录重新绑定。`))) return; try { await api(`/users/${encodeURIComponent(b.dataset.resetDev)}/reset-device`, { method: "POST" }); toast("已重置设备绑定，其会话已下线", "ok"); loadUsers(); } catch (e) { toast(e.message, "err"); } });
  $all("[data-unlock]").forEach((b) => b.onclick = async () => { if (!(await confirmDialog(`解锁 ${b.dataset.unlock}？解锁后可重新登录。`))) return; try { await api(`/users/${encodeURIComponent(b.dataset.unlock)}/unlock`, { method: "POST" }); toast("已解锁", "ok"); loadUsers(); } catch (e) { toast(e.message, "err"); } });
  $all("[data-del]").forEach((b) => b.onclick = async () => { if (!(await confirmDialog(`删除用户 ${b.dataset.del}？删除后该账号无法登录（历史操作日志保留）。`, true))) return; try { await api(`/users/${encodeURIComponent(b.dataset.del)}`, { method: "DELETE" }); toast("已删除", "ok"); loadUsers(); } catch (e) { toast(e.message, "err"); } });
}

async function openNewUser() {
  const roles = await loadRoles();
  const mask = modal(`
    <h3>新建用户（账套内成员）</h3>
    <div class="banner set" style="margin-bottom:12px">此账号用于本账套内的角色分工。对方需已存在同名<b>账号</b>才能登录本账套；没有的请先让管理员在「账号管理」中开通。</div>
    <div class="field"><label>账号（须与已开通账号同名）</label><input id="nu-u" /></div>
    <div class="field"><label>姓名</label><input id="nu-n" /></div>
    <div class="field"><label>角色（主岗位）</label><select id="nu-r">${roles.map((r) => `<option value="${r.role}">${esc(r.label)}</option>`).join("")}</select></div>
    <div class="field"><label>兼任岗位（可多选 = 身兼多职；权限取并集，出纳签字与会计核心仍互斥）</label>
      <span style="display:flex;flex-wrap:wrap;gap:6px 14px">${roles.map((r) => `<label style="display:inline-flex;align-items:center;gap:4px"><input type="checkbox" class="nu-rr" value="${r.role}" />${esc(r.label)}</label>`).join("")}</span>
    </div>
    <div class="field"><label>初始口令（至少 6 位）</label><input id="nu-p" type="password" /></div>
    <div class="field"><label>备注</label><input id="nu-memo" /></div>
    <div class="field"><label><input type="checkbox" id="nu-must" checked /> 首次登录强制改密</label></div>
    <div id="nu-perm-box">${permMatrixHtml(roles, { role: "accountant", extra_perms: [], deny_perms: [] })}</div>
    <div class="foot"><button class="btn" id="nu-save">创建</button><button class="btn ghost" id="nu-cancel">取消</button></div>`);
  // 切换角色时重新生成矩阵（跟随角色的默认勾选随角色变化）
  const refreshMatrix = () => {
    const checkedRoles = $all(".nu-rr", mask).filter((c) => c.checked).map((c) => c.value);
    $("#nu-perm-box", mask).innerHTML = permMatrixHtml(roles, { role: $("#nu-r", mask).value, roles: checkedRoles, extra_perms: [], deny_perms: [] });
  };
  $("#nu-r", mask).addEventListener("change", refreshMatrix);
  $all(".nu-rr", mask).forEach((cb) => cb.addEventListener("change", refreshMatrix));
  $("#nu-cancel", mask).onclick = closeModal;
  $("#nu-save", mask).onclick = async () => {
    const extra = [], deny = [];
    $all("[data-perm]", mask).forEach((sel) => {
      if (sel.value === "on") extra.push(sel.dataset.perm);
      else if (sel.value === "off") deny.push(sel.dataset.perm);
    });
    const body = { username: $("#nu-u", mask).value.trim(), display_name: $("#nu-n", mask).value.trim(), password: $("#nu-p", mask).value, role: $("#nu-r", mask).value, roles: $all(".nu-rr", mask).filter((c) => c.checked).map((c) => c.value), memo: $("#nu-memo", mask).value.trim(), must_change_pwd: $("#nu-must", mask).checked, extra_perms: extra, deny_perms: deny };
    if (!body.username || body.password.length < 6) { toast("账号必填且口令至少 6 位", "err"); return; }
    try { await api("/users", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) }); toast("已创建用户", "ok"); closeModal(); loadUsers(); } catch (e) { toast(e.message, "err"); }
  };
}

// 用户编辑弹窗：角色预设基础上的逐项权限开关矩阵
function permMatrixHtml(roles, u) {
  // 全部权限清单（取所有角色权限的并集）
  const allPerms = [];
  const seen = new Set();
  (roles || []).forEach((r) => (r.perms || []).forEach((p) => { if (!seen.has(p.code)) { seen.add(p.code); allPerms.push(p); } }));
  const extraSet = new Set(u.extra_perms || []);
  const denySet = new Set(u.deny_perms || []);
  // 角色预设权限：按 code 收集（perms 是 {code,label} 对象数组，需映射为 code，
  // 否则 new Set 存的是对象、.has(p.code) 永远为 false，导致"角色默认"全部显示为 ✖）
  // 主岗位 + 兼任岗位的权限并集（身兼多职）
  const roleCodes = new Set();
  [u.role, ...(u.roles || [])].forEach((rc) => {
    ((roles.find((x) => x.role === rc) || {}).perms || []).forEach((p) => roleCodes.add(p.code));
  });
  const stateOf = (code) => (denySet.has(code) ? "off" : extraSet.has(code) ? "on" : "role");
  const rows = allPerms.map((p) => {
    const granters = [u.role, ...(u.roles || [])].filter((rc) =>
      ((roles.find((x) => x.role === rc) || {}).perms || []).some((pp) => pp.code === p.code)
    ).map((rc) => ((roles.find((x) => x.role === rc) || {}).label) || rc);
    const base = granters.length ? `（来自：${granters.join(" + ")}）` : "（无权限）";
    const cur = stateOf(p.code);
    return `<tr><td>${esc(p.label)}</td><td><select data-perm="${esc(p.code)}">
      <option value="role" ${cur === "role" ? "selected" : ""}>跟随角色 ${base}</option>
      <option value="on" ${cur === "on" ? "selected" : ""}>强制开启</option>
      <option value="off" ${cur === "off" ? "selected" : ""}>强制关闭</option>
    </select></td></tr>`;
  }).join("");
  return `<div class="panel">
    <div class="muted" style="margin-bottom:6px">权限明细（在角色预设基础上逐项覆盖）</div>
    <table class="grid"><thead><tr><th>功能权限</th><th style="width:240px">授权方式</th></tr></thead>
    <tbody>${rows}</tbody></table>
  </div>`;
}

async function openEditUser(u) {
  const roles = await loadRoles();
  const ds = u.data_scope || {};
  const depts = (ds.depts || []).join(",");
  const me = u.username === session.user.username;
  const mask = modal(`
    <h3>编辑用户 · ${esc(u.username)}</h3>
    <div class="field"><label>姓名</label><input id="eu-n" value="${esc(u.display_name)}" /></div>
    <div class="field"><label>角色（主岗位）</label><select id="eu-r">${roles.map((r) => `<option value="${r.role}" ${r.role === u.role ? "selected" : ""}>${esc(r.label)}</option>`).join("")}</select></div>
    <div class="field"><label>兼任岗位（可多选 = 身兼多职；权限取并集）</label>
      <span style="display:flex;flex-wrap:wrap;gap:6px 14px">${roles.map((r) => `<label style="display:inline-flex;align-items:center;gap:4px"><input type="checkbox" class="eu-rr" value="${r.role}" ${(u.roles || []).includes(r.role) ? "checked" : ""} />${esc(r.label)}</label>`).join("")}</span>
    </div>
    <div class="field"><label>备注</label><input id="eu-memo" value="${esc(u.memo || "")}" /></div>
    <div class="field"><label><input type="checkbox" id="eu-must" ${u.must_change_pwd ? "checked" : ""} /> 强制下次登录改密</label></div>
    <div class="field"><label><input type="checkbox" id="eu-dis" ${u.disabled ? "checked" : ""} ${me ? "disabled" : ""} /> 停用该账号</label></div>
    <div class="panel">
      <div class="muted" style="margin-bottom:6px">数据范围（留空 / 不勾选 = 不限制）</div>
      <div class="field"><label>可见部门（逗号分隔）</label><input id="eu-depts" value="${esc(depts)}" /></div>
      <div class="field"><label>科目范围 从 <input id="eu-acct-from" value="${esc(ds.account_from || "")}" style="width:90px" /> 至 <input id="eu-acct-to" value="${esc(ds.account_to || "")}" style="width:90px" /></label></div>
      <div class="field"><label><input type="checkbox" id="eu-own-v" ${ds.own_voucher_only ? "checked" : ""} /> 仅看本人填制的凭证</label></div>
      <div class="field"><label><input type="checkbox" id="eu-own-d" ${ds.own_doc_only ? "checked" : ""} /> 仅看本人经手的业务单据</label></div>
    </div>
    ${u.is_admin ? `<div class="panel muted">系统管理员默认拥有全部权限，不受逐项开关限制。</div>` : permMatrixHtml(roles, u)}
    <div class="foot"><button class="btn" id="eu-save">保存</button><button class="btn ghost" id="eu-cancel">取消</button></div>`);
  $("#eu-cancel", mask).onclick = closeModal;
  $("#eu-save", mask).onclick = async () => {
    const deptsVal = $("#eu-depts", mask).value.split(",").map((s) => s.trim()).filter(Boolean);
    const extra = [], deny = [];
    if (!u.is_admin) {
      $all("[data-perm]", mask).forEach((sel) => {
        const code = sel.dataset.perm;
        if (sel.value === "on") extra.push(code);
        else if (sel.value === "off") deny.push(code);
      });
    }
    const body = {
      display_name: $("#eu-n", mask).value.trim(),
      role: $("#eu-r", mask).value,
      roles: $all(".eu-rr", mask).filter((c) => c.checked).map((c) => c.value),
      memo: $("#eu-memo", mask).value.trim(),
      must_change_pwd: $("#eu-must", mask).checked,
      disabled: $("#eu-dis", mask).checked,
      data_scope: { depts: deptsVal, account_from: $("#eu-acct-from", mask).value.trim(), account_to: $("#eu-acct-to", mask).value.trim(), own_voucher_only: $("#eu-own-v", mask).checked, own_doc_only: $("#eu-own-d", mask).checked },
      extra_perms: extra,
      deny_perms: deny,
    };
    try { await api(`/users/${encodeURIComponent(u.username)}`, { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) }); toast("已保存", "ok"); closeModal(); loadUsers(); } catch (e) { toast(e.message, "err"); }
  };
}
function openAdminResetPwd(username) {
  const mask = modal(`
    <h3>重置口令 · ${esc(username)}</h3>
    <div class="field"><label>新口令（至少 6 位）</label><input id="rp-p" type="password" /></div>
    <div class="foot"><button class="btn" id="rp-save">重置</button><button class="btn ghost" id="rp-cancel">取消</button></div>`);
  $("#rp-cancel", mask).onclick = closeModal;
  $("#rp-save", mask).onclick = async () => {
    const np = $("#rp-p", mask).value;
    if (np.length < 6) { toast("口令至少 6 位", "err"); return; }
    try { await api(`/users/${encodeURIComponent(username)}/reset-password`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ new: np }) }); toast("已重置口令", "ok"); closeModal(); } catch (e) { toast(e.message, "err"); }
  };
}
// ===========================================================================
// 账号管理（仅管理员，作用于全局账号库，与账套内子账号无关）
// ===========================================================================
async function viewPlatformUsers(main) {
  main.innerHTML = `<h2>账号管理</h2>
    <div class="toolbar">
      <button class="btn primary" id="pu-add">＋ 开通账号</button>
      <span class="muted">管理员创建与管理账套；普通账号由管理员开通并邀请进入账套工作。</span>
    </div>
    <div id="pu-list" class="muted">加载中…</div>`;
  async function load() {
    try {
      const rows = await api("/platform/users");
      $("#pu-list").innerHTML = rows.length ? `<table class="grid"><thead><tr>
        <th>账号</th><th>展示名</th><th>类型</th><th>状态</th><th>设备</th><th>创建时间</th><th>操作</th></tr></thead>
        <tbody>${rows.map((x) => `<tr>
          <td>${esc(x.username)}</td><td>${esc(x.display_name)}</td>
          <td>${x.is_admin ? "管理员" : "普通账号"}</td>
          <td>${x.disabled ? `<span style="color:var(--err)">已停用</span>` : "正常"}</td>
          <td>${x.device_bound ? "已绑定" : "未绑定"}</td>
          <td class="muted">${esc(x.created_at)}</td>
          <td>
            <button class="btn sm" data-act="edit" data-u="${esc(x.username)}">编辑</button>
            <button class="btn sm" data-act="reset" data-u="${esc(x.username)}">重置口令</button>
            <button class="btn sm" data-act="dev" data-u="${esc(x.username)}">重置设备</button>
            <button class="btn sm ghost" data-act="del" data-u="${esc(x.username)}">删除</button>
          </td></tr>`).join("")}</tbody></table>` : `<div class="muted">暂无账号</div>`;
    } catch (e) { $("#pu-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  }
  $("#pu-add").addEventListener("click", () => {
    const mask = modal(`
      <h3>开通账号</h3>
      <div class="field"><label>账号（登录名）</label><input id="nc-u" autocomplete="off" /></div>
      <div class="field"><label>展示名</label><input id="nc-d" /></div>
      <div class="field"><label>初始口令（至少 6 位，首次登录会要求改密）</label><input id="nc-p" type="text" /></div>
      <div class="field"><label><input type="checkbox" id="nc-a" /> 设为管理员</label></div>
      <div class="foot"><button class="btn" id="nc-cancel">取消</button><button class="btn primary" id="nc-save">创建</button></div>`);
    $("#nc-cancel", mask).onclick = closeModal;
    $("#nc-save", mask).onclick = async () => {
      const username = $("#nc-u", mask).value.trim();
      const password = $("#nc-p", mask).value;
      if (!username) { toast("请输入账号", "err"); return; }
      if (password.length < 6) { toast("口令至少 6 位", "err"); return; }
      try {
        await api("/platform/users", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ username, display_name: $("#nc-d", mask).value.trim(), password, is_admin: $("#nc-a", mask).checked }) });
        toast(`账号「${esc(username)}」已开通`, "ok");
        closeModal(); load();
      } catch (e) { toast(e.message, "err"); }
    };
  });
  $("#pu-list").addEventListener("click", async (e) => {
    const btn = e.target.closest("button[data-act]");
    if (!btn) return;
    const u = btn.dataset.u, act = btn.dataset.act;
    if (act === "reset") {
      const mask = modal(`
        <h3>重置口令：${esc(u)}</h3>
        <div class="field"><label>新口令（至少 6 位）</label><input id="rp-n" type="text" /></div>
        <div class="foot"><button class="btn" id="rp-cancel">取消</button><button class="btn primary" id="rp-save">重置</button></div>`);
      $("#rp-cancel", mask).onclick = closeModal;
      $("#rp-save", mask).onclick = async () => {
        const np = $("#rp-n", mask).value;
        if (np.length < 6) { toast("口令至少 6 位", "err"); return; }
        try {
          await api(`/platform/users/${encodeURIComponent(u)}/reset-password`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ new: np }) });
          toast("口令已重置", "ok"); closeModal();
        } catch (err) { toast(err.message, "err"); }
      };
    } else if (act === "edit") {
      let info = null;
      try { info = (await api("/platform/users")).find((x) => x.username === u); } catch (err) {}
      const mask = modal(`
        <h3>编辑账号：${esc(u)}</h3>
        <div class="field"><label>展示名</label><input id="eu-d" value="${esc(info ? info.display_name : "")}" /></div>
        <div class="field"><label><input type="checkbox" id="eu-a" ${info && info.is_admin ? "checked" : ""} /> 管理员</label></div>
        <div class="field"><label><input type="checkbox" id="eu-x" ${info && info.disabled ? "checked" : ""} /> 停用该账号</label></div>
        <div class="foot"><button class="btn" id="eu-cancel">取消</button><button class="btn primary" id="eu-save">保存</button></div>`);
      $("#eu-cancel", mask).onclick = closeModal;
      $("#eu-save", mask).onclick = async () => {
        try {
          await api(`/platform/users/${encodeURIComponent(u)}`, { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ display_name: $("#eu-d", mask).value.trim(), is_admin: $("#eu-a", mask).checked, disabled: $("#eu-x", mask).checked }) });
          toast("已保存", "ok"); closeModal(); load();
        } catch (err) { toast(err.message, "err"); }
      };
    } else if (act === "dev") {
      if (!(await confirmDialog(`重置「${u}」的设备绑定？将解绑「全部」已绑定设备（上限 2 台）并强制下线，下次登录重新绑定。`, true))) return;
      try { await api(`/platform/users/${encodeURIComponent(u)}/reset-device`, { method: "POST" }); toast("设备绑定已重置", "ok"); load(); } catch (err) { toast(err.message, "err"); }
    } else if (act === "del") {
      if (!(await confirmDialog(`确定删除账号「${u}」？该操作不可恢复（其创建的账套需先删除）。`, true))) return;
      try { await api(`/platform/users/${encodeURIComponent(u)}`, { method: "DELETE" }); toast("已删除", "ok"); load(); } catch (err) { toast(err.message, "err"); }
    }
  });
  await load();
}

// ===========================================================================
// 合并报表（跨账套汇总，仅平台管理员；v1 汇总，不含内部往来抵销）
// ===========================================================================
async function viewConsolidate(main) {
  main.innerHTML = `<h2>合并报表 <span class="muted" style="font-size:12px">跨账套汇总（管理员）</span></h2>
    <div class="toolbar">
      <label>期间 <input id="cs-period" value="${(state.current || "").replace("-", "")}" style="width:90px" placeholder="YYYYMM" /></label>
      <button class="btn primary sm" id="cs-run">合并汇总</button>
      <button class="btn ghost sm" id="cs-print">打印预览</button>
      <span class="muted" style="font-size:12px">口径：各账套仅已记账余额；v1 为汇总，内部往来抵销留待 v2</span>
    </div>
    <div class="panel"><div id="cs-books" class="muted">加载账套…</div></div>
    <div class="panel" style="margin-top:8px"><div id="cs-table" class="muted">选择账套后点「合并汇总」</div></div>`;
  let books = [];
  try {
    const r = await api("/books");
    books = r.books || [];
  } catch (e) { $("#cs-books").textContent = e.message; }
  $("#cs-books").innerHTML = books.length
    ? `<b style="font-size:12.5px">选择账套</b><div style="display:flex;gap:12px;flex-wrap:wrap;margin-top:6px">${books.map((b) => `<label style="display:inline-flex;gap:4px;align-items:center"><input type="checkbox" class="cs-bk" value="${esc(b.key)}" checked /> ${esc(b.company || b.key)} <span class="muted">(${esc(b.key)})</span></label>`).join("")}</div>`
    : `<div class="muted">无可合并账套</div>`;
  $("#cs-run").onclick = async () => {
    const keys = $all(".cs-bk").filter((x) => x.checked).map((x) => x.value);
    if (!keys.length) { toast("请至少选择一个账套", "err"); return; }
    try {
      const r = await api(`/consolidate/preview?period=${encodeURIComponent($("#cs-period").value.trim())}&books=${encodeURIComponent(keys.join(","))}`);
      const bks = r.books || [], rows = r.rows || [];
      $("#cs-table").innerHTML = `<b>合并汇总（${esc(r.period)}）</b><table class="grid" id="cs-grid" style="margin-top:6px"><thead><tr><th>科目</th><th>名称</th>${bks.map((b) => `<th class="num">${esc(b.company || b.key)}</th>`).join("")}<th class="num">合计</th></tr></thead><tbody>${rows.length
        ? rows.map((x) => `<tr><td>${esc(x.account_code)}</td><td>${esc(x.account_name)}</td>${bks.map((b) => `<td class="num">${fmt(x.values[b.key] || "0")}</td>`).join("")}<td class="num"><b>${fmt(x.total)}</b></td></tr>`).join("")
        : `<tr><td colspan="${bks.length + 3}" class="muted">所选账套该期间无余额</td></tr>`}</tbody></table><p class="muted" style="font-size:12px">${esc(r.note || "")}</p>`;
    } catch (e) { $("#cs-table").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  };
  $("#cs-print").onclick = () => {
    const t = $("#cs-grid");
    if (!t) { toast("先执行合并汇总", "err"); return; }
    printPreview("合并汇总", t);
  };
}

async function viewPlatformBooks(main) {
  main.innerHTML = `<h2>平台经营总览 · 全部账套</h2>
    <div class="muted" style="margin-bottom:10px">跨账套汇总各业务域（凭证/资金/销售/采购/仓管/生产/成本/报销/审批）。管理员可进入任意账套查看：以临时管理员身份进入，不在该账套留下账号记录，操作会记入账套审计日志。</div>
    <div id="po-meta" class="cards"></div>
    <div id="po-domains"></div>
    <div id="po-books"></div>
    <h3 style="margin:18px 0 8px">开放 API 密钥</h3>
    <div class="muted" style="margin-bottom:8px;line-height:1.6">
      给 WMS / MES / 电商 / 银行 / 税务等外部系统签发调用凭据。
      <b>明文只在创建时显示一次</b>，库里只存哈希，找不回来——丢了就重新签一把。
      v1 为<b>只读接口</b>：空权限 = 只读基础档案，填 <code>fin_report</code> 才能读试算平衡与明细账。
      密钥绑定的账套就是它能读到的账套；平台级密钥（不绑账套）能读所有账套。
    </div>
    <div style="display:flex;gap:8px;align-items:center;flex-wrap:wrap;margin-bottom:8px">
      <button class="btn primary sm" id="ak-new">签发密钥</button>
      <button class="btn ghost sm" id="ak-reload">刷新</button>
      <a class="btn ghost sm" href="/api/v1/openapi.json" target="_blank" rel="noopener">接口文档（OpenAPI）</a>
    </div>
    <div id="ak-new-wrap" style="display:none;margin-bottom:10px">
      <div class="panel" style="box-shadow:none;border:1px solid var(--warn,#c47f00);padding:10px">
        <div style="font-weight:600;margin-bottom:6px">签发新密钥</div>
        <div style="display:flex;gap:8px;align-items:center;flex-wrap:wrap;margin-bottom:8px">
          <label>名称</label><input id="ak-name" placeholder="如：仓库 WMS 对接" style="width:180px" />
          <label>绑定账套</label>
          <select id="ak-book" style="width:220px"><option value="">（平台级 · 可读所有账套）</option></select>
          <label>权限</label>
          <select id="ak-scopes" style="width:200px">
            <option value="">只读（基础档案）</option>
            <option value="fin_report">只读 + 财务报表</option>
          </select>
          <label>到期</label><input id="ak-expires" placeholder="留空 = 永不过期" style="width:180px" />
          <button class="btn primary sm" id="ak-do">确认签发</button>
        </div>
        <div id="ak-plain" style="display:none">
          <div style="font-weight:600;color:var(--err);margin-bottom:4px">⚠ 这是密钥明文，只显示这一次。关掉就再也看不到了。</div>
          <code id="ak-plain-val" style="display:block;padding:8px;background:var(--bg);border-radius:4px;word-break:break-all;user-select:all"></code>
        </div>
      </div>
    </div>
    <div id="ak-list" class="muted">加载中…</div>
    <h3 style="margin:18px 0 8px">账套管理</h3>
    <div id="pb-list" class="muted">加载中…</div>`;

  // ---------------------------------------------------------------- 平台总览
  // 按单位分流：金额（元）走对比图与汇总卡，计数类（张/单/件）只进汇总卡与表格。
  // 把「元」和「张」画在同一根坐标轴上是错的——量纲不同，谁也压不过谁。
  const MONEY = "元";
  const poFmt = (n, unit) => unit === MONEY ? fmt(Number(n).toFixed(2)) : fmt(n);

  async function loadOverview() {
    $("#po-meta").innerHTML = `<div class="card"><div class="k">账套取数中</div><div class="v">…</div></div>`;
    let ov;
    try { ov = await api("/platform/overview"); }
    catch (e) {
      $("#po-meta").innerHTML = "";
      $("#po-domains").innerHTML = `<div class="panel"><div style="color:var(--err)">总览加载失败：${esc(e.message)}</div></div>`;
      $("#po-books").innerHTML = "";
      return;
    }
    const books = ov.books || [];
    const okBooks = books.filter((b) => b.ok);
    const todoSum = (ov.todos || []).reduce((a, t) => a + (Number(t.count) || 0), 0);

    // 顶部概览条
    $("#po-meta").innerHTML = `
      <div class="card"><div class="k">账套总数</div><div class="v">${ov.book_count}<span style="font-size:12px;font-weight:400;opacity:.65"> 个</span></div></div>
      <div class="card"><div class="k">取数成功</div><div class="v">${okBooks.length}<span style="font-size:12px;font-weight:400;opacity:.65"> / ${ov.book_count}</span></div></div>
      <div class="card"><div class="k">覆盖业务域</div><div class="v">${(ov.domains || []).length}<span style="font-size:12px;font-weight:400;opacity:.65"> 个</span></div></div>
      <div class="card"><div class="k">待办合计</div><div class="v">${todoSum}<span style="font-size:12px;font-weight:400;opacity:.65"> 条</span></div></div>
      ${Number(ov.failed) > 0 ? `<div class="card" style="border-color:var(--err)"><div class="k">取数失败</div><div class="v" style="color:var(--err)">${ov.failed}<span style="font-size:12px;font-weight:400;opacity:.65"> 个</span></div></div>` : ""}
      ${ov.truncated ? `<div class="card"><div class="k">已截断</div><div class="v">${ov.scanned}<span style="font-size:12px;font-weight:400;opacity:.65"> / ${ov.book_count}（上限 ${ov.max_books}）</span></div></div>` : ""}`;

    if (!books.length) {
      $("#po-domains").innerHTML = `<div class="panel"><div class="muted">系统中还没有账套，无法汇总。</div></div>`;
      $("#po-books").innerHTML = "";
      return;
    }

    // 每个指标 → 各账套取值（用于占比条）。只统计打得开的账套。
    const split = (domain, key) => okBooks.map((b) => ({
      name: b.company || b.key,
      key: b.key,
      value: (b.cards.find((c) => c.domain === domain && c.key === key) || {}).num || 0,
    }));
    // 金额指标：各账套都非零才值得横向比（全 0 或只有一个账套有数时无意义）
    const moneyMetrics = (ov.totals || []).filter((t) => t.unit === MONEY);
    const cntMetrics = (ov.totals || []).filter((t) => t.unit !== MONEY);

    const metricCard = (t) => {
      const parts = split(t.domain, t.key);
      const total = parts.reduce((a, p) => a + p.value, 0);
      const pos = parts.filter((p) => p.value > 0);
      const bar = pos.length > 1 && total > 0
        ? `<div style="display:flex;height:6px;border-radius:3px;overflow:hidden;margin-top:6px;background:var(--z-200)">${pos.map((p, i) => {
            const pct = (p.value / total) * 100;
            const hue = 210 - (i * 137) % 150;
            return `<span title="${esc(p.name)}：${poFmt(p.value, t.unit)}" style="width:${pct.toFixed(2)}%;background:hsl(${hue} 62% 52%)"></span>`;
          }).join("")}</div>
           <div class="muted" style="font-size:11px;margin-top:3px">${pos.map((p) => `${esc(p.name)} ${((p.value / total) * 100).toFixed(0)}%`).join(" · ")}</div>`
        : "";
      return `<div class="card"><div class="k">${esc(t.label || t.key)}</div>
        <div class="v">${poFmt(t.value, t.unit)}${t.unit ? `<span style="font-size:12px;font-weight:400;opacity:.65"> ${esc(t.unit)}</span>` : ""}</div>${bar}</div>`;
    };

    // 金额域：跨账套对比柱状图（X=账套，组=指标）
    const moneyByDom = {};
    moneyMetrics.forEach((t) => { (moneyByDom[t.domain] = moneyByDom[t.domain] || []).push(t); });
    const charts = Object.keys(moneyByDom).map((dom) => {
      const ms = moneyByDom[dom];
      const groups = ms.slice(0, 6).map((t, i) => ({
        name: t.label || t.key,
        color: `hsl(${(i * 67) % 360} 62% 52%)`,
        values: okBooks.map((b) => (b.cards.find((c) => c.domain === dom && c.key === t.key) || {}).num || 0),
      }));
      if (!groups.length || !groups.some((g) => g.values.some((v) => v !== 0))) return "";
      return `<div class="wb-sec"><h4 style="margin:14px 0 6px">${esc(dom)} · 跨账套对比<span class="muted" style="font-weight:400;font-size:12px"> （单位：元）</span></h4>
        <div class="panel" style="margin:0;padding:10px 12px">${chartBox("bar", dom + " · 跨账套对比", okBooks.map((b) => b.company || b.key), groups, 240, "元")}</div></div>`;
    }).join("");

    const domSections = Object.keys(Object.assign({}, ...Object.values(moneyByDom), ...Object.values(
      cntMetrics.reduce((a, t) => { a[t.domain] = 1; return a; }, {})))).map((dom) => {
      const mm = moneyByDom[dom] || [];
      const cm = cntMetrics.filter((t) => t.domain === dom);
      if (!mm.length && !cm.length) return "";
      return `<div class="wb-sec"><h4 style="margin:14px 0 6px">${esc(dom)}</h4>
        <div class="cards">${mm.concat(cm).map(metricCard).join("")}</div></div>`;
    }).join("");

    $("#po-domains").innerHTML = (domSections || `<div class="panel"><div class="muted">各账套本期暂无业务数据。</div></div>`) + charts;

    // 各账套明细：域卡片 + 近 6 期趋势
    const bookPanels = books.map((b) => {
      if (!b.ok) {
        return `<div class="panel" style="margin-top:12px">
          <div style="display:flex;align-items:center;gap:8px;flex-wrap:wrap">
            <b>${esc(b.company || b.key)}</b><span class="muted">${esc(b.key)}</span>
            <span class="tag" style="color:var(--err)">取数失败</span><span class="grow"></span>
            <button class="btn ghost sm" data-po-enter="${esc(b.key)}" data-po-company="${esc(b.company || b.key)}">进入</button>
          </div>
          <div style="color:var(--err);font-size:12px;margin-top:6px">${esc(b.error || "未知原因")}</div>
        </div>`;
      }
      const cg = {};
      (b.cards || []).forEach((c) => { (cg[c.domain] = cg[c.domain] || []).push(c); });
      const cards = Object.keys(cg).map((d) => `<div class="wb-sec"><h4 style="margin:8px 0 4px">${esc(d)}</h4>
        <div class="cards">${cg[d].map((c) => `<div class="card"><div class="k">${esc(c.label)}</div>
          <div class="v">${esc(c.value)}${c.unit === MONEY || c.unit === "件" ? `<span style="font-size:12px;font-weight:400;opacity:.65"> ${esc(c.unit)}</span>` : ""}</div></div>`).join("")}</div></div>`).join("");
      const trends = (b.trends || []).slice(0, 3).map((t) => `<div class="panel" style="margin:0;padding:10px 12px">
        <div style="display:flex;align-items:center;gap:8px"><b style="font-size:13px">${esc(t.title)}</b><span class="muted" style="font-size:12px">单位：${esc(t.unit)}</span></div>
        ${chartBox("line", t.title, t.periods, t.series.map((s) => ({ name: s.name, color: s.color, values: s.points })), 170, t.unit)}
      </div>`).join("");
      const todo = (b.todos || []).filter((t) => Number(t.count) > 0);
      return `<div class="panel" style="margin-top:12px">
        <div style="display:flex;align-items:center;gap:8px;flex-wrap:wrap">
          <b>${esc(b.company || b.key)}</b><span class="muted">${esc(b.key)} · ${esc(b.period)}</span>
          <span class="grow"></span>
          ${todo.length ? `<span class="tag warn">待办 ${todo.reduce((a, t) => a + (Number(t.count) || 0), 0)}</span>` : ""}
          <button class="btn ghost sm" data-po-enter="${esc(b.key)}" data-po-company="${esc(b.company || b.key)}">进入</button>
        </div>
        ${cards}
        ${trends ? `<h4 style="margin:10px 0 4px">近 6 期走势</h4><div style="display:grid;grid-template-columns:repeat(auto-fill,minmax(400px,1fr));gap:12px">${trends}</div>` : ""}
      </div>`;
    }).join("");
    $("#po-books").innerHTML = `<h3 style="margin:18px 0 8px">各账套业务明细</h3>` + bookPanels;
    $all("[data-po-enter]", main).forEach((b) => b.onclick = () => enterBook(b.dataset.poEnter, b.dataset.poCompany));
  }

  async function load() {
    await loadOverview();
    try {
      const data = await loadMyBooks();
      const books = data.books || [];
      $("#pb-list").innerHTML = books.length ? `<table class="grid"><thead><tr>
        <th>账套标识</th><th>公司名称</th><th>归属用户</th><th>操作</th></tr></thead>
        <tbody>${books.map((b) => `<tr>
          <td>${esc(b.key)}</td><td>${esc(b.company || "（未命名）")}</td><td>${esc(b.owner)}</td>
          <td><button class="btn sm" data-key="${esc(b.key)}" data-company="${esc(b.company || b.key)}">进入</button>
          <button class="btn sm ghost" data-del="${esc(b.key)}" data-name="${esc(b.company || b.key)}">删除</button></td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">系统中还没有账套。普通用户登录后可自行创建。</div>`;
    } catch (e) { $("#pb-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  }
  $("#pb-list").addEventListener("click", (e) => {
    const btn = e.target.closest("button[data-key]");
    if (btn) { enterBook(btn.dataset.key, btn.dataset.company); return; }
    const del = e.target.closest("button[data-del]");
    if (del) deleteBook(del.dataset.del, del.dataset.name);
  });

  // ------------------------------------------------------------ 开放 API 密钥
  const akFmtTime = (s) => (s && s !== "0000-00-00 00:00:00" ? String(s).slice(0, 16) : "—");

  async function loadKeys() {
    $("#ak-list").innerHTML = `<div class="muted">加载中…</div>`;
    try {
      const keys = await api("/api-keys");
      const list = keys.keys || [];
      $("#ak-list").innerHTML = list.length ? `<table class="grid"><thead><tr>
        <th>名称</th><th>密钥前缀</th><th>绑定账套</th><th>权限</th><th>最近调用</th><th>到期</th><th>状态</th><th>操作</th>
        </tr></thead>
        <tbody>${list.map((k) => `<tr>
          <td>${esc(k.name)}</td>
          <td><code>${esc(k.prefix)}…</code></td>
          <td>${k.book_key ? esc(k.book_key) : `<span class="muted">平台级（全部账套）</span>`}</td>
          <td>${k.scopes ? esc(k.scopes) : `<span class="muted">只读</span>`}</td>
          <td>${esc(akFmtTime(k.last_used_at))}</td>
          <td>${esc(akFmtTime(k.expires_at))}</td>
          <td>${k.disabled ? `<span style="color:var(--err)">已停用</span>` : `<span style="color:var(--ok)">启用中</span>`}</td>
          <td>
            <button class="btn sm" data-ak-toggle="${k.id}" data-dis="${k.disabled ? "0" : "1"}">${k.disabled ? "启用" : "停用"}</button>
            <button class="btn sm ghost" data-ak-del="${k.id}" data-name="${esc(k.name)}">删除</button>
          </td></tr>`).join("")}</tbody></table>`
        : `<div class="muted">还没有签发任何密钥。外部系统要对接时点上方「签发密钥」。</div>`;
    } catch (e) { $("#ak-list").innerHTML = `<div style="color:var(--err)">${esc(e.message)}</div>`; }
  }

  async function fillBookSelect() {
    try {
      const data = await loadMyBooks();
      const opts = (data.books || []).map((b) => `<option value="${esc(b.key)}">${esc(b.company || b.key)}（${esc(b.key)}）</option>`).join("");
      $("#ak-book").innerHTML = `<option value="">（平台级 · 可读所有账套）</option>${opts}`;
    } catch (e) { /* 账套列表拿不到时只留平台级选项，不阻断密钥管理 */ }
  }

  $("#ak-new").addEventListener("click", async () => {
    const w = $("#ak-new-wrap");
    w.style.display = w.style.display === "none" ? "" : "none";
    $("#ak-plain").style.display = "none";
    if (w.style.display === "") await fillBookSelect();
  });
  $("#ak-reload").addEventListener("click", loadKeys);
  $("#ak-do").addEventListener("click", async () => {
    const name = $("#ak-name").value.trim();
    if (!name) { toast("请填密钥名称（便于日后辨认是哪套系统在用）", "err"); return; }
    try {
      const r = await api("/api-keys", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name,
          book_key: $("#ak-book").value,
          scopes: $("#ak-scopes").value,
          expires_at: $("#ak-expires").value.trim(),
        }),
      });
      $("#ak-plain-val").textContent = r.secret;
      $("#ak-plain").style.display = "";
      $("#ak-name").value = "";
      await loadKeys();
    } catch (e) { toast(e.message, "err"); }
  });
  $("#ak-list").addEventListener("click", async (e) => {
    const tog = e.target.closest("button[data-ak-toggle]");
    if (tog) {
      try {
        await api(`/api-keys/${tog.dataset.akToggle}`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ disabled: tog.dataset.dis === "1" }),
        });
        await loadKeys();
      } catch (err) { toast(err.message, "err"); }
      return;
    }
    const del = e.target.closest("button[data-ak-del]");
    if (del && confirm(`确定删除密钥「${del.dataset.name}」？\n删掉后正在使用它的外部系统会立刻全部 401。`)) {
      try {
        await api(`/api-keys/${del.dataset.akDel}`, { method: "DELETE" });
        await loadKeys();
      } catch (err) { toast(err.message, "err"); }
    }
  });

  await load();
  await loadKeys();
}

function openChangePwd(forced, onDone) {
  const mask = modal(`
    <h3>${forced ? "首次登录 / 口令已重置：请设置新口令" : "修改口令"}</h3>
    ${forced ? `<div class="banner unset" style="margin-bottom:12px">为安全起见，请先设置一个强度较高的新口令。</div>` : ""}
    <div class="field"><label>原口令</label><input id="cp-o" type="password" ${forced ? "placeholder='首次登录可留空'" : ""} /></div>
    <div class="field"><label>新口令（至少 6 位）</label><input id="cp-n" type="password" /></div>
    <div class="field"><label>确认新口令</label><input id="cp-c" type="password" /></div>
    <div class="foot"><button class="btn" id="cp-save">保存</button>${forced ? "" : `<button class="btn ghost" id="cp-cancel">取消</button>`}</div>`);
  if (!forced) $("#cp-cancel", mask).onclick = closeModal;
  $("#cp-save", mask).onclick = async () => {
    const oldp = $("#cp-o", mask).value, np = $("#cp-n", mask).value, cp = $("#cp-c", mask).value;
    if (np.length < 6) { toast("新口令至少 6 位", "err"); return; }
    if (np !== cp) { toast("两次输入不一致", "err"); return; }
    try {
      await api("/change-password", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ old: oldp, new: np }) });
      toast("口令已更新", "ok");
      closeModal();
      if (typeof onDone === "function") onDone();
    } catch (e) { toast(e.message, "err"); }
  };
}

// ===========================================================================
// 操作日志：GET /api/logs（后端已做权限 Perm::AuditLog + 搜索 + limit）
// ===========================================================================
async function viewAuditLog(main) {
  const qp = new URLSearchParams(location.hash.slice(1).split("?")[1] || "");
  const kw = qp.get("q") || "";
  const limit = qp.get("limit") || "200";
  main.innerHTML = `
    <h2>操作日志</h2>
    <div class="toolbar">
      <label>搜索 <input id="al-q" value="${esc(kw)}" placeholder="用户/模块/动作/详情" style="width:260px" /></label>
      <button class="btn primary" id="al-search">查询</button>
      <a class="btn ghost" href="#/audit-log">重置</a>
    </div>
    <div id="al-list"><div class="muted">加载中…</div></div>
  `;
  const listEl = document.getElementById("al-list");
  const searchInput = document.getElementById("al-q");
  const btn = document.getElementById("al-search");
  const load = async () => {
    const k = searchInput.value.trim();
    // api() 已统一补 /api 前缀；这里再写 /api 会请求成 /api/api/logs。
    const url = "/logs" + (k ? `?q=${encodeURIComponent(k)}&limit=${limit}` : `?limit=${limit}`);
    let rows;
    try { rows = await api(url); } catch (e) { listEl.innerHTML = `<div class="muted" style="color:var(--err)">加载失败：${esc(e.message)}</div>`; return; }
    if (!rows || rows.length === 0) { listEl.innerHTML = `<div class="muted">暂无操作日志</div>`; return; }
    const th = `<tr><th>时间</th><th>用户</th><th>模块</th><th>动作</th><th>详情</th></tr>`;
    const trs = rows.map(l => `
      <tr>
        <td>${esc(l.ts || "")}</td>
        <td>${esc(l.user || "")}</td>
        <td>${esc(l.module || "")}</td>
        <td>${esc(l.action || "")}</td>
        <td class="nowrap">${esc(l.detail || "")}</td>
      </tr>`).join("");
    listEl.innerHTML = `<table class="data"><thead>${th}</thead><tbody>${trs}</tbody></table>`;
  };
  await load();
  btn.onclick = () => { location.hash = `#/audit-log?q=${encodeURIComponent(searchInput.value.trim())}`; };
  searchInput.onkeydown = (e) => { if (e.key === "Enter") btn.onclick(); };
}
