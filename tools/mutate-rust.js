// 变异测试（Rust 侧）：把修复逐条改回去，验证新加的测试**真的会失败**。
// 只会通过的测试等于没有测试。
//
// 做法：改源码 → 跑对应测试 → 期望 FAILED → 还原。
//
// 这个脚本自己踩过两个坑，都写在这里防止重犯：
//   1. **变异串没匹配上源码**（CRLF vs LF）→ 什么都没改 → 测试照过 → 被读成
//      「测试放过了」。第一版 7 条里有 3 条是这个情况，结论完全反了。
//      所以「没生效」必须单独报错，不能混进「放过」。
//   2. **测试根本没跑**（命令指向了别的 crate / 名字打错）→ 输出是
//      「0 passed; 0 filtered out; test result: ok」→ 同样被读成 PASS。
//      所以除了判 FAILED，还要判「至少跑了 1 条」。
const fs = require("fs");
const { execFileSync } = require("child_process");

const API = (name) => ["test", "-p", "finweb", "--test", "api", "--", name];
const CORE = (name) => ["test", "-p", "fincore", "--lib", "user::tests::"];

const MUT = [
  {
    name: "③ 去掉期初的科目存在性校验",
    file: "crates/finweb/src/handlers.rs",
    from: "    if !codes.is_empty() {",
    to: "    if false {",
    cmd: API("begin_rejects_nonexistent_account"),
  },
  {
    name: "④ 去掉 delete_ids 的删除语义",
    file: "crates/finweb/src/handlers.rs",
    from: "        if *id > 0 {\n            balances::delete_begin_on",
    to: "        if false {\n            balances::delete_begin_on",
    cmd: API("begin_delete_ids_actually_remove_rows"),
  },
  {
    name: "⑤ 会计去掉结账权",
    file: "crates/fincore/src/user.rs",
    from: "                // 实测确认过这条死路。\n                PeriodClose,\n                Opening,\n                CarryForward,",
    to: "                // 实测确认过这条死路。\n                Opening,\n                CarryForward,",
    cmd: CORE("two_person_company_can_close_the_month"),
  },
  {
    name: "⑤ 会计读 /api/options 重新收紧成 403",
    file: "crates/finweb/src/handlers.rs",
    from:
      "    if !user.can(Perm::SysOption) && !(user.can(Perm::VoucherPost) || user.can(Perm::VoucherAudit))\n    {",
    to: "    if !user.can(Perm::SysOption) {",
    cmd: API("accountant_can_read_book_options_to_see_audit_flag"),
  },
  {
    // 这一条打的是 put_options，不是 get_options。
    // 我第一版把反向变异打在 get_options 的守卫上，测试照样通过 —— 因为写路径
    // 自己也有 `user.require(Perm::SysOption)`。**变异打错地方比不打更糟**：它会
    // 让人以为"这条测过了"。
    name: "⑤ 读放宽时把写权限检查也一起删掉（读放宽≠写放宽）",
    file: "crates/finweb/src/handlers.rs",
    from:
      ') -> Result<Json<serde_json::Value>, AppError> {\n    user.require(Perm::SysOption)?;\n    let db = state.db_for(&user.book_key)?;\n    // 落库前校验',
    to:
      ') -> Result<Json<serde_json::Value>, AppError> {\n    let db = state.db_for(&user.book_key)?;\n    // 落库前校验',
    cmd: API("accountant_can_read_book_options_to_see_audit_flag"),
  },
  {
    name: "⑦ 账套层免改密不再同步到平台层",
    file: "crates/finweb/src/handlers.rs",
    from: "    if !req.must_change_pwd && ru.must_change_pwd {",
    to: "    if false {",
    cmd: API("book_level_no_password_change_flag_reaches_the_login_gate"),
  },
  {
    name: "⑧ 去掉「已结转过」判定",
    file: "crates/finweb/src/handlers.rs",
    from: "    if !carried.is_empty() && rows",
    to: "    if false && !carried.is_empty() && rows",
    cmd: API("carry_forward_twice_explains_it_was_already_carried"),
  },
  {
    name: "⑧ 恢复「导入数据有误」万能前缀",
    file: "crates/finweb/src/state.rs",
    from: "        AppError::BadRequest(e.to_string())",
    to: '        AppError::BadRequest(format!("导入数据有误：{e}"))',
    cmd: API("carry_forward_twice_explains_it_was_already_carried"),
  },
  {
    name: "⑧ 提示改成原来那句「损益发生额均为零」",
    file: "crates/fincore/src/engine/period_end.rs",
    from: "        return Err(FinError::msg(\n            \"本期各损益类科目的净发生额均为 0，没有可结转的分录\",\n        ));",
    to: '        return Err(FinError::msg("本期没有需要结转的损益类科目（损益发生额均为零）"));',
    cmd: API("carry_forward_twice_explains_it_was_already_carried"),
  },
  {
    // ⑨ 收付款单的审核判据：VoucherAudit || VoucherPost
    //    去掉 VoucherPost 会计就审不了 → 两人公司死路回来
    name: "⑨ 收付款单审核不再认 VoucherPost（两人公司死路）",
    file: "crates/finweb/src/handlers.rs",
    from: "    if user.can(Perm::VoucherAudit) || user.can(Perm::VoucherPost) {",
    to: "    if user.can(Perm::VoucherAudit) {",
    cmd: API("receipt_audit_gates_on_audit_or_post_permission"),
  },
  {
    // 只放开"审"不放开"撤审" → 会计能审却撤不回来
    name: "⑨ 撤审仍用旧判据（能审不能撤）",
    file: "crates/finweb/src/handlers.rs",
    from: "    require_receipt_gate(&user)?;\n    let db = state.db_for(&user.book_key)?;\n    findb::receipt::receipt_unaudit(&db, id)?;",
    to: "    user.require(Perm::VoucherAudit)?;\n    let db = state.db_for(&user.book_key)?;\n    findb::receipt::receipt_unaudit(&db, id)?;",
    cmd: API("receipt_audit_gates_on_audit_or_post_permission"),
  },
  {
    // ⑩ 财务主管兼回出纳签字 → 这个岗位就建不出来了
    name: "⑩ 财务主管重新兼上出纳签字（岗位建不出来）",
    file: "crates/fincore/src/user.rs",
    from:
      "                VoucherPost,\n                VoucherUnpost,\n                AccountEdit,\n                AuxEdit,\n                Opening,",
    to:
      "                VoucherPost,\n                VoucherUnpost,\n                CashierSign,\n                AccountEdit,\n                AuxEdit,\n                Opening,",
    cmd: CORE("every_role_passes_duty_separation"),
  },
  {
    // 设备绑定 2 台：登录时若又清该账号全部会话，第 2 台会把第 1 台踢下线，
    // 「允许绑两台」就只剩一个数字
    name: "设备绑定 2 台：登录时又改成清该账号全部会话（两台无法并存）",
    file: "crates/finweb/src/handlers.rs",
    from: "            .remove_by_username_and_device(&username, &device_id);",
    to: "            .remove_by_username(&username);",
    cmd: API("platform_device_binding_and_reset"),
  },
  {
    // 上限退回 1 台：第三台就不该被拒了
    name: "设备绑定上限退回 1 台（第三台又能登了）",
    file: "crates/finweb/src/realm.rs",
    from: "    pub const MAX_DEVICES_PER_USER: usize = 2;",
    to: "    pub const MAX_DEVICES_PER_USER: usize = 1;",
    cmd: API("platform_device_binding_and_reset"),
  },
  {
    // 逐请求复核改回读单列 device_id → 退回一台机，且两处判定会分叉
    name: "逐请求设备复核改回读单列 device_id（退回一台机）",
    file: "crates/finweb/src/state.rs",
    from:
        "        if !ru.is_admin\n            && !state\n                .realm\n                .device_allowed(&info.username, &info.device_id)?\n        {",
    to:
        "        if !ru.is_admin && !ru.device_id.is_empty() && ru.device_id != info.device_id {",
    cmd: API("platform_device_binding_and_reset"),
  },
  {
    // 旧库的绑定不迁移过去 → 升级后所有账号静默解绑
    name: "设备绑定不迁移旧库（旧账号静默解绑）",
    file: "crates/finweb/src/realm.rs",
    from:
      "        let moved = conn.execute(\n            \"INSERT OR IGNORE INTO realm_user_device(username, device_id, bound_at, last_seen)",
    to:
      "        let moved: usize = 0; let _ = conn.execute(\n            \"INSERT OR IGNORE INTO realm_user_device(username, device_id, bound_at, last_seen)",
    cmd: API("platform_device_binding_and_reset"),
  },
  {
    // 出纳签字默认关 → 回到「新建账套一出生就在默认绕过出纳」
    name: "出纳签字默认值退回关",
    file: "crates/fincore/src/account.rs",
    from: "            require_cashier: true,",
    to: "            require_cashier: false,",
    cmd: API("audit_default_on_new_books"),
  },];

/** 对换行不敏感地套用变异（源码是 CRLF，变异串按 LF 写） */
function applyMut(src, from, to) {
  for (const nl of ["\n", "\r\n"]) {
    const f = from.replace(/\n/g, nl);
    if (src.includes(f)) return { out: src.replace(f, to.replace(/\n/g, nl)), ok: true };
  }
  return { out: src, ok: false };
}

/** 解析 cargo test 输出里的 "N passed; M failed" */
function counts(out) {
  let passed = 0;
  let failed = 0;
  for (const m of out.matchAll(/(\d+) passed; (\d+) failed/g)) {
    passed += +m[1];
    failed += +m[2];
  }
  return { passed, failed };
}
const backups = new Map();
for (const m of MUT) if (!backups.has(m.file)) backups.set(m.file, fs.readFileSync(m.file, "utf8"));

// --list：只把变异清单打成 JSON，不执行。让 tools/check-ci.js 检查「清单是否已过期」
// （源码改了但 from 串对不上 = 这条变异形同虚设，而且会被读成「检查放过了」）。
//
// 为什么不让 check-ci.js 自己解析本文件：它得把 JS 字符串字面量再解一遍转义，
// 两层转义必错。让脚本自己导出真值，别在外部猜。
if (process.argv.includes("--list")) {
  process.stdout.write(JSON.stringify({ cases: MUT }, null, 1));
  process.exit(0);
}

let bad = 0;
let notRun = 0;
try {
  for (const m of MUT) {
    const src = backups.get(m.file);
    const { out: mutated, ok } = applyMut(src, m.from, m.to);
    if (!ok) {
      console.log("!! 变异未生效（源码已变，测试失效）：" + m.name);
      bad++;
      continue;
    }
    fs.writeFileSync(m.file, mutated);
    let out = "";
    try {
      out = execFileSync("cargo", m.cmd, {
        encoding: "utf8",
        stdio: ["ignore", "pipe", "pipe"],
        maxBuffer: 64 * 1024 * 1024,
      });
    } catch (e) {
      out = (e.stdout || "") + (e.stderr || "");
    }
    const c = counts(out);
    if (c.passed + c.failed === 0) {
      // 编译失败也算"测试没跑"，但要区分开：编译不过本身就是一种拦截
      const compileErr = /error\[E/.test(out);
      console.log(
        (compileErr ? "WARN 变异后编译不过  " : "!! 测试根本没跑      ") + m.name
      );
      if (!compileErr) notRun++;
      else bad++;
      continue;
    }
    if (c.failed > 0) {
      console.log(`PASS 拦下了  ${m.name}   (${c.failed} failed / ${c.passed} passed)`);
    } else {
      bad++;
      console.log(`FAIL 放过了  ${m.name}   (${c.passed} passed —— 测试没在守它该守的)`);
    }
  }
} finally {
  for (const [f, s] of backups) fs.writeFileSync(f, s);
}
console.log(
  bad === 0
    ? `\n全部 ${MUT.length} 个变异都被对应测试拦下，源码已还原`
    : `\n${bad} 个变异没被拦下 / 没生效（共 ${MUT.length} 个${notRun ? `，其中 ${notRun} 个是测试没跑` : ""}）`
);
process.exit(bad === 0 ? 0 : 1);
