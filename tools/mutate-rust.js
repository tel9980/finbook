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

// v1.8.52 桌面端补功能：发票税额拆分、现金流上期对比、期初收集表契约。
// 共同失效形态是「看得见的地方都对，算出来的数不对」——发票拆错一分、
// 对比表吞掉上期独有项目、期初表多列一个 1122 造成往来双计，都不报错。
const SCOPE = (name) => ["test", "-p", "finweb", "--test", "scope_dashboard", "--", name];
const SCLAIM = (name) => ["test", "-p", "finweb", "--test", "scope_claims", "--", name];
const FUI = (name) => ["test", "-p", "finui", "--lib", name];
// v1.8.53 设备绑定（M-4）：政策与指纹。三条变异对应三个「不报错就会静悄悄变松」的点。
const REALM = (name) => ["test", "-p", "finweb", "--lib", `realm::tests::${name}`];
const API = (name) => ["test", "-p", "finweb", "--test", "api", "--", name];
// 注意：这里曾经写成固定的 "user::tests::"（name 参数被丢掉），效果是「跑所有 user 测试」。
// 那样一条变异被别的用例顺手拦住，也会显示 PASS，看不出目标用例到底守没守住。
const CORE = (name) => ["test", "-p", "fincore", "--lib", `user::tests::${name}`];
// v1.8.21「导入导出闭环」的三个新 target。这类功能最典型的失效是
// 「界面点得动、文件也下来了，但数据其实没进去/没出来」，所以每条都要有变异守着。
const XLSX = (name) => ["test", "-p", "finweb", "--test", "export_xlsx", "--", name];
const CFRINT = (name) =>
  ["test", "-p", "finweb", "--test", "cashflow_print_compare", "--", name];
const FCFW = (name) => ["test", "-p", "findb", "--lib", `reports::compare_window_tests::${name}`];
const STATIC = (name) => ["test", "-p", "finweb", "--test", "static_cache", "--", name];
const MC = (name) => ["test", "-p", "finweb", "--test", "month_close_wizard", "--", name];
const ROUND = (name) => ["test", "-p", "finweb", "--test", "roundtrip", "--", name];
// imports.rs 的测试都在 `mod tests` 里，全路径是 `imports::tests::<name>`
const FIMPORT = (name) => ["test", "-p", "findb", "--lib", `imports::tests::${name}`];
// 期初收集表闭环（下载模板 → 填 → 预检 → 导入）的集成测试
const LOOP = (name) => ["test", "-p", "finweb", "--test", "opening_sheet_loop", "--", name];
// 批量粘贴解析（fincore）：表头按列名取值、停用/上级/备注还原
const FBATCH = (name) => ["test", "-p", "fincore", "--lib", `batch::auxiliary::tests::${name}`];
// v1.8.24 网页版首页首屏：数字全由凭证推出，错一位老板就按错的数做决定
const HOME = (name) => ["test", "-p", "finweb", "--test", "home_brief", "--", name];
// v1.8.25 性能优化。优化最阴的失效不是「变慢」，是「变快但算错」——
// 缓存把旧结果当新结果发出去、口径为了提速悄悄缩窄，都是静默错数。
const FADV = (name) => ["test", "-p", "findb", "--lib", `advanced::tests::${name}`];
// v1.8.27 打印版式（fincore::print）。失效形态全是"看起来没动静"：
// 勾了页码纸上没有、选了横向还是竖的、账套参数被拒了却照样存进去。
const FPRINT = (name) => ["test", "-p", "fincore", "--lib", `print::tests::${name}`];
const OPT = (name) => ["test", "-p", "finweb", "--test", "options_print", "--", name];
/** findb 库内的期初导入单测（`mod tests` 里，全路径要是 `opening_book::tests::xxx`） */
const FOB = (name) => ["test", "-p", "findb", "--lib", `opening_book::tests::${name}`];
/** 打印预览「点一下、拖一下就能存」的接口（v1.8.44） */
const ICOST = (name) => ["test", "-p", "findb", "--test", "income_vs_dashboard", "--", name];
const FPS = (name) => ["test", "-p", "finweb", "--test", "print_settings", "--", name];
const OBOOK = (name) => ["test", "-p", "findb", "--test", "opening_book", "--", name];
// v1.8.28 套打：凭证/账簿/单据也走账套里的纸张设置。失效形态是"设置对了但纸上没变"。
const FPFORM = (name) => ["test", "-p", "findb", "--lib", `printform::tests::${name}`];
// 建账/导入账套的参数校验（v1.8.38）
const FLIB = (name) => ["test", "-p", "findb", "--lib", `tests::${name}`];
// 未核销清单分页（v1.8.38）
const SOPEN = (name) => ["test", "-p", "finweb", "--test", "settle_open_page", "--", name];
// 账套参数结构校验（fincore::BookOptions::validate）
const FOPT = (name) => ["test", "-p", "fincore", "--lib", `account::tests::${name}`];
const FSH = (name) => ["test", "-p", "finweb", "--lib", `sheets::tests::${name}`];
const FSETTLE = (name) => ["test", "-p", "findb", "--lib", `settle::tests::${name}`];
const OCFG = (name) => ["test", "-p", "finweb", "--test", "options_config", "--", name];
const FUNS = (name) => ["test", "-p", "finweb", "--test", "settle_unsettle_api", "--", name];
// v1.8.46 现金流量表按对方科目自动归集。失效形态都是「表还是那张表，只是空着」：
// 规则表静默不生效、税额伴生行把整笔拖成"分不清"、内部划转被当成未归集。
const CF = (name) => ["test", "-p", "findb", "--test", "cashflow_auto", "--", name];
const FRULE = (name) => ["test", "-p", "fincore", "--lib", `report::cashflow_rules::tests::${name}`];
const FREP = (name) => ["test", "-p", "findb", "--lib", `reports::tests::${name}`];
// v1.8.36 应收应付：催款单口径 + 账龄基准日 + 催款单导出明细。
// 这三处的共同失效形态是「文件/页面都正常，只有数字是错的」——客户收到错的催款单。
const FCUST = (name) => ["test", "-p", "finweb", "--test", "customers", "--", name];

const MUT = [
  // v1.8.56：锁中毒不可连带打死登录。
  {
    name: "㊲ 锁中毒后不取回内值（登录会跟着 panic）",
    file: "crates/finweb/src/realm.rs",
    from: "        self.inner.lock().unwrap_or_else(|e| e.into_inner())",
    to: "        self.inner.lock().unwrap()",
    cmd: ["test", "-p", "finweb", "--lib", "realm::tests::a_poisoned_lock_does_not_kill_login"],
  },
  // v1.8.55：部门范围与写侧越权。
  // 这两条对应的 bug 都是「读和写各抄一遍口径，抄漏了部门」——
  // 抄漏之后列表会多出别的部门的单子，而且**不报错**。
  {
    name: "㊱ 报销列表不按部门过滤",
    file: "crates/finweb/src/handlers.rs",
    from: "    rows.iter().filter(|c| claim_visible(user, c)).collect()",
    to: "    rows.iter().filter(|c| doc_in_scope(user, &c.applicant)).collect()",
    cmd: SCLAIM("claim_list_and_export_respect_dept_scope"),
  },
  {
    name: "㊱ 报销写侧不按部门拦（别的部门也能审批）",
    file: "crates/finweb/src/handlers.rs",
    from: "    user.user.data_scope.allows_dept(&c.dept) && doc_in_scope(user, &c.applicant)",
    to: "    doc_in_scope(user, &c.applicant)",
    cmd: SCLAIM("claim_write_endpoints_refuse_other_dept"),
  },
  // v1.8.54：数据范围（DataScope）在「有金额的出口」上是否真的生效。
  // 这两条都不是「报表算错了」，而是「报表裁了、首页/往来没裁」——
  // 两端数字不一样，各自看着都对，是最难查的那类。
  {
    name: "㉟ 首页取数不带数据范围（首页看到全公司）",
    file: "crates/finweb/src/handlers.rs",
    from: "    let scope = Some(user);",
    to: "    let scope: Option<&fincore::user::User> = None;",
    cmd: SCOPE("dashboard_money_position_respects_data_scope"),
  },
  {
    name: "㉟ 账龄不查科目范围（范围外也给出数字）",
    file: "crates/finweb/src/handlers/settle.rs",
    from: "    if !user.user.can_see_account(account.trim()) {",
    to: "    if false {",
    cmd: SCOPE("settle_endpoints_refuse_out_of_scope_accounts"),
  },
  {
    name: "③ 去掉期初的科目存在性校验",
    file: "crates/finweb/src/handlers/settle.rs",
    from: "    if !codes.is_empty() {",
    to: "    if false {",
    cmd: API("begin_rejects_nonexistent_account"),
  },
  {
    name: "④ 去掉 delete_ids 的删除语义",
    file: "crates/finweb/src/handlers/settle.rs",
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
    file: "crates/finweb/src/handlers/books.rs",
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
    file: "crates/finweb/src/handlers/books.rs",
    from:
      ') -> Result<Json<serde_json::Value>, AppError> {\n    user.require(Perm::SysOption)?;\n    let db = &user.db;\n    // 落库前校验',
    to:
      ') -> Result<Json<serde_json::Value>, AppError> {\n    let db = &user.db;\n    // 落库前校验',
    cmd: API("accountant_can_read_book_options_to_see_audit_flag"),
  },
  {
    name: "⑦ 账套层免改密不再同步到平台层",
    file: "crates/finweb/src/handlers/auth.rs",
    from: "    if !req.must_change_pwd && ru.must_change_pwd {",
    to: "    if false {",
    cmd: API("book_level_no_password_change_flag_reaches_the_login_gate"),
  },
  {
    name: "⑧ 去掉「已结转过」判定",
    file: "crates/finweb/src/handlers/reports.rs",
    from: "    if !carried.is_empty() && rows",
    to: "    if false && !carried.is_empty() && rows",
    cmd: API("carry_forward_twice_explains_it_was_already_carried"),
  },
  {
    name: "⑧ 恢复「导入数据有误」万能前缀",
    file: "crates/finweb/src/state.rs",
    from: "        AppError::BadRequest(e.to_string())",
    to: '        AppError::BadRequest(format!("导入数据有误：{e}"))',
    // 结转重复那条走的是 handlers 自己拼的 AppError::bad_request，
    // POST 一张不平的凭证也在 handlers 里就被拦下 —— 两条都不经过 From<FinError>，
    // 所以改这里它们都看不见 → 一直「放过了」。这条只能直接测转换本身。
    cmd: ["test", "-p", "finweb", "--lib",
          "state::tests::engine_error_keeps_its_own_wording"],
  },
  {
    name: "⑧ 重复结转不再说明「已结转过」",
    // 原来改的是引擎里「净发生额均为 0」那句话术 —— 但请求根本走不到那里：
    // handlers 在调用引擎之前已经查过本期结转凭证并直接返回「已结转过」。
    // 变异指向一条**不可达的路径**，测试只能靠运气（CI 上偶然红过）。
    // 改成把守卫的条件关掉，才真正考验测试守不守这件事。
    file: "crates/finweb/src/handlers/reports.rs",
    from: "    if !carried.is_empty() && rows.iter().all(|r| r.debit.round2().eq(&r.credit.round2())) {",
    to: "    if false {",
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
    name: "⑨ 撤审仍用旧判据（能审却撤不回）",
    file: "crates/finweb/src/handlers/funds.rs",
    from: "    require_fund_doc_gate(&user)?;\n    let db = &user.db;\n    findb::receipt::receipt_unaudit(db, id)?;",
    to: "    user.require(Perm::VoucherAudit)?;\n    let db = &user.db;\n    findb::receipt::receipt_unaudit(db, id)?;",
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
    // 应收/应付会计没有导出权限 → 账龄表只能截图发给客户
    name: "㉓ 应收会计失去导出权（账龄表只能截图发客户）",
    file: "crates/fincore/src/user.rs",
    from: "            Role::Receivables => &[VoucherNew, AuxEdit, Report, FinReport, Export],",
    to: "            Role::Receivables => &[VoucherNew, AuxEdit, Report, FinReport],",
    cmd: CORE("ar_ap_clerks_can_export_but_others_cannot"),
  },
  {
    // 反向：为了省事把导出开给所有会计 = 数据外带通道全打开
    name: "㉓ 导出权限顺手开给会计（数据外带通道全开）",
    file: "crates/fincore/src/user.rs",
    // 必须改**真的那个** Accountant 分支：先前插了一条重复 arm，匹配取第一条，
    // 变异等于没生效（测试照过，被误读成「放过了」）。
    from: "            Role::Accountant => &[\n                VoucherNew,\n                VoucherEdit,",
    to: "            Role::Accountant => &[\n                Export,\n                VoucherNew,\n                VoucherEdit,",
    cmd: CORE("ar_ap_clerks_can_export_but_others_cannot"),
  },
  // v1.8.53 删掉的两条变异（都因为靶子随政策变化而**不可观测**）：
  // ① `MAX_DEVICES_PER_USER` 退回 1 —— 这个常量已经不存在；
  // ② 「登录时清该账号全部会话」—— 普通账号现在只会绑**一台**设备，
  //    `remove_by_username_and_device` 与 `remove_by_username` 移除的集合完全相同，
  //    任何测试都分不出来。留着它就是一条永远绿的假守卫（CI 就是被它红掉的）。
  // v1.8.53：`MAX_DEVICES_PER_USER` 概念已不存在（首次绑定后禁改），
  // 「政策回潮」改由 ㉞ 的新变异守。
  // ③ 「逐请求设备复核改回读单列 device_id」也删了：普通账号现在只会绑**一台**，
  //    单列里的值恒等于榜上那一台，读列与读表的结果完全一样 —— 又一次「政策变了、
  //    靶子不再可观测」。它的原始动机（两处判定会分叉）在新政策下不再成立。
  {
    // 旧库的绑定不迁移过去 → 升级后所有账号静默解绑
    // 原来的变异是 `let moved: usize = 0; let _ = conn.execute(...)` —— 只丢掉了
    // 行数，SQL 照样执行，什么都拦不住（这也是它「一直放过了」的原因之一）。
    // 真要不搬，得让 SELECT 一行都选不出来。
    name: "设备绑定不迁移旧库（旧账号静默解绑）",
    file: "crates/finweb/src/realm.rs",
    from: "             FROM realm_user WHERE device_id <> ''\",",
    to: "             FROM realm_user WHERE 1 = 0\",",
    // 集成测试用的是全新 realm，旧库形态根本不存在 → 搬不搬都看不出来。
    // 这条要在 realm 自己修：构造「单列有值、集合表空」，重开库必须搬过去。
    cmd: ["test", "-p", "finweb", "--lib",
          "realm::tests::legacy_single_column_device_binding_is_migrated"],
  },
  {
    // 出纳签字默认关 → 回到「新建账套一出生就在默认绕过出纳」
    name: "出纳签字默认值退回关",
    file: "crates/fincore/src/account.rs",
    from: "            require_cashier: true,",
    to: "            require_cashier: false,",
    cmd: API("audit_default_on_for_new_books"),
  },
  {
    // 期初表有表头时也退回按位置猜布局 → 用户把「期初余额」和「方向」对调，
    // 金额会被当成方向、方向被当成金额，整批进 warnings 丢掉。
    // （原本守的是「只按 3 列解析」那条，有了「有表头就按列名取」之后那条分支
    //   对带表头的文件不再走到，变异会假失败 —— 改成守新分支。）
    name: "⑬ 期初表有表头也按位置猜布局",
    file: "crates/findb/src/imports.rs",
    from: "        ImportTemplate::Generic if map.has_header => {",
    to: "        ImportTemplate::Generic if false => {",
    cmd: FIMPORT("期初表列顺序换了也认得"),
  },
  {
    // 现金流量项目档案无法经数据导入迁移 → 「导出全部 → 导进另一个账套」不完整
    name: "⑬ 辅助档案不认现金流量项目",
    file: "crates/findb/src/imports.rs",
    from: '        "现金流量" | "现金流量项目" | "现金流" | "cashflow" | "cf" => {\n            fincore::AuxKind::CashFlow\n        }',
    to: '        "__not_a_kind__" => {\n            fincore::AuxKind::CashFlow\n        }',
    cmd: FIMPORT("import_aux_accepts_cashflow_kind"),
  },
  {
    // 上传账套不再核对「是不是 FinBook」→ 随便一个 sqlite 都会被迁成空账套
    name: "⑬ 上传账套不再核对科目表",
    file: "crates/finweb/src/handlers.rs",
    from: "    if has_account == 0 {",
    to: "    if false {",
    cmd: XLSX("upload_rejects_non_finbook_file"),
  },
  {
    // 科目导出漏掉「方向」列 → 负债/权益科目方向被类别默认值盖掉
    // （219901 待确认往来：类别=负债 默认贷，档案里是借 —— 导出-导回就反了）
    name: "⑬ 科目导出不带方向列",
    file: "crates/finweb/src/sheets.rs",
    from: "                a.dir.label().to_string(),",
    to: "                String::new(),",
    cmd: ROUND("科目导出后类别与方向能导回"),
  },
  {
    // 表头按列名取值退回「整行都是表头词才算表头」→ 导出的五列文件粘回来
    // 会把表头行当成一条档案，凭空生成「编码 名称」
    name: "⑭ 导出文件表头又被当成数据",
    file: "crates/fincore/src/batch/auxiliary.rs",
    from: "            if let Some(m) = header_map(&cols) {",
    to: "            if let Some(m) = header_map(&cols).filter(|_| false) {",
    cmd: FBATCH("导出的五列表头不会被当成数据"),
  },
  {
    // 只认全等表头（不认「客户编码」这种带前缀的）→ 粘贴框里自家示例
    // 复制进来就多一条垃圾档案
    name: "⑭ 表头不认类别前缀",
    file: "crates/fincore/src/batch/auxiliary.rs",
    from: '        } else if c.ends_with("编码") || c.ends_with("代码") || c.ends_with("编号") {',
    to: "        } else if false {",
    cmd: FBATCH("带类别前缀的表头也认得"),
  },
  {
    // 停用状态不还原 → 搬完账套停掉的客户又冒出来
    name: "⑭ 导入档案不还原停用状态",
    file: "crates/findb/src/imports.rs",
    from: '        e.disabled = map.cell(f, &["状态"], 5) == "停用";',
    to: "        e.disabled = false;",
    cmd: ROUND("停用与上级编码导出后能导回"),
  },
  {
    // 档案导出不带税点列 → 59 个客户的税点只能一个个点开改，导出表改完导回去全没了
    name: "⑮ 档案导出不带税点列",
    file: "crates/finweb/src/sheets.rs",
    from: "                e.prop(fincore::auxiliary::prop::TAX_RATE)\n                    .cloned()\n                    .unwrap_or_default()",
    to: "                String::new()",
    cmd: ROUND("客户税点与开票情况导出后能导回"),
  },
  {
    // 有表头时缺列又退回按位置猜 → 删掉「上级编码」这一列，会在别处取到编码，
    // 变成「上级编码=自己」，导进去报「会形成环」
    name: "⑮ 有表头时缺列仍按位置兜底",
    file: "crates/findb/src/imports.rs",
    from: "        if self.has_header {\n            return String::new();\n        }",
    to: "        if false {\n            return String::new();\n        }",
    cmd: FIMPORT("表头认列名所以列顺序可以换"),
  },
  {
    // 数量期初不导出 → 原材料/库存商品搬过去数量归零，金额却对得上
    name: "⑮ 期初导出不带数量列",
    file: "crates/finweb/src/sheets.rs",
    from: "                r.qty_begin.map(|q| q.fmt_plain()).unwrap_or_default(),",
    to: "                String::new(),",
    cmd: ROUND("期初数量与分户导出后能导回"),
  },
  {
    // 科目导出不带辅助核算维度 → 余额表按客户拆不开
    name: "⑮ 科目导出不带辅助核算维度",
    file: "crates/finweb/src/sheets.rs",
    from: "                .filter(|k| a.aux.contains(**k))",
    to: "                .filter(|_k| false)",
    cmd: ROUND("科目辅助核算与数量单位导出后能导回"),
  },
  {
    // 存货导出不带计量单位 → 改完导回去单位没了
    name: "⑮ 存货导出不带计量单位",
    file: "crates/finweb/src/sheets.rs",
    from: "                e.prop(fincore::auxiliary::prop::UNIT)\n                    .cloned()\n                    .unwrap_or_default(),",
    to: "                String::new(),",
    cmd: ROUND("存货单位与停用状态导出后能导回"),
  },
  {
    // 导入账套后不与平台身份对齐改密标记 → 桌面版搬来的账套（admin 那行是
    // 「首次必须改密」）在 Web 端每个接口都 401，前端退回选择页
    name: "⑯ 导入账套后不对齐改密标记（导入成功但进不去）",
    file: "crates/finweb/src/handlers/books.rs",
    from: "                    if u.must_change_pwd != ru.must_change_pwd {\n                        u.must_change_pwd = ru.must_change_pwd;",
    to: "                    if false {\n                        u.must_change_pwd = ru.must_change_pwd;",
    cmd: XLSX("upload_fbk_registers_a_usable_book"),
  },
  {
    // 账套层的改密标记重新参与鉴权 → 平台已认证的用户被影子挡在门外
    name: "⑯ 账套层的改密标记又参与鉴权",
    file: "crates/finweb/src/state.rs",
    from: "        let company = db.options().company;\n        state.sessions.touch(&token);\n        Ok(CurrentUser {",
    to: "        if user.must_change_pwd {\n            return Err(AppError::unauthorized(\"你的口令已过期或需首次设置，请先修改口令\"));\n        }\n        let company = db.options().company;\n        state.sessions.touch(&token);\n        Ok(CurrentUser {",
    cmd: XLSX("upload_fbk_registers_a_usable_book"),
  },
  {
    // 期初导入不还原分户 → 期初总额对得上，按客户一查是空的
    name: "⑮ 期初导入不还原分户",
    file: "crates/findb/src/imports.rs",
    from: "                        aux: line.aux.clone(),",
    to: "                        aux: AuxRef::default(),",
    cmd: ROUND("期初数量与分户导出后能导回"),
  },
  {
    // 成本口径退回只算 6401/6402 → 房租水电（510105 制造费用）整块消失在利润表外，
    // 净利润虚高。这是 v1.8.12 修过的老 bug，首页换了界面也不能把它放回来。
    name: "⑰ 首页成本口径退回只算 6401/6402",
    file: "crates/findb/src/advanced.rs",
    from: '    let cost = exp("6401") + exp("6402") + unrecovered_cost;',
    to: '    let cost = exp("6401") + exp("6402");',
    cmd: HOME("home_brief_is_derived_from_vouchers"),
  },
  {
    // 利润率写死 0% → 老板看到"这月白干"（实际是 77%）
    name: "⑰ 首页利润率永远 0%",
    file: "crates/finweb/src/handlers.rs",
    from: "        Some((t.net_profit.to_f64() / t.revenue.to_f64() * 100.0 * 10.0).round() / 10.0)",
    to: "        Some(0.0)",
    cmd: HOME("home_brief_is_derived_from_vouchers"),
  },
  {
    // 没记账的凭证不再提示 → 老板看到"这个月只赚这么点"却不知道是自己没记账
    name: "⑰ 首页不提示未记账凭证",
    file: "crates/finweb/src/handlers.rs",
    from: "    let unposted = draft + audited;",
    to: "    let unposted = 0;",
    cmd: HOME("draft_vouchers_warn_but_do_not_count"),
  },
  {
    // 同名客户/供应商不标「同一家两种身份」→ 宏源那种一厂两面被当成两家，
    // 一个欠我 15,000、一个我欠他 28,000，老板要自己心算
    name: "⑰ 首页不标同一家的两种身份",
    file: "crates/finweb/src/handlers.rs",
    from: "        both: !p.also.is_zero(),",
    to: "        both: false,",
    cmd: HOME("party_with_two_identities_shows_net"),
  },
  {
    // 进账套仍停在「建账首月」→ 结了 9 个月的账，首页整块显示的是 1 月的数，
    // 而且不会自己动。老板只会得出一个结论：系统算错了。
    name: "⑱ 进账套仍停在建账首月",
    file: "crates/finweb/src/handlers/books.rs",
    from: "        let ymm = periods::current_period(&db)\n            .unwrap_or_else(|_| db.options().start_period)\n            .ymm();",
    to: "        let ymm = db.options().start_period.ymm();",
    cmd: HOME("selecting_a_book_lands_on_current_period_not_start_period"),
  },
  {
    // 快照缓存的键里去掉写入计数 → 录完凭证、看报表还是旧数字，
    // 而且刷新也没用（同一个连接）。这是"优化出来的错"里最难查的一种。
    name: "⑱ 余额快照缓存的键忽略写入计数",
    file: "crates/findb/src/balances.rs",
    from: "            q.prepared_by.as_deref().unwrap_or(\"\"),\n            db.conn().total_changes()",
    to: "            q.prepared_by.as_deref().unwrap_or(\"\"),\n            0i64",
    cmd: FADV("snapshot_cache_sees_new_voucher"),
  },
  {
    // 科目表缓存不校验写入计数 → 新建的科目永远取不到，界面"新建成功了"
    // 但下拉框里没有，用户会以为保存失败反复点。
    name: "⑱ 科目表缓存不校验写入计数",
    file: "crates/findb/src/lib.rs",
    from: "            Some((g, ch)) if *g == gen => Some(ch.clone()),",
    to: "            Some((_g, ch)) => Some(ch.clone()),",
    cmd: FADV("chart_cache_sees_new_account"),
  },
  {
    // 首页成本构成退回「只算 6401/6402」→ 首页净利润减掉了房租水电，
    // 点开明细却是空的，两处口径打架（v1.8.13 修过的老毛病）
    name: "⑱ 成本构成漏掉制造费用",
    file: "crates/findb/src/advanced.rs",
    from: "        if !matches!(a.category, fincore::account::AcctCategory::Cost) || !is_leaf(&a.code) {",
    to: "        if true {",
    cmd: FADV("cost_drivers_of_matches_financial_analysis"),
  },
  {
    // 大响应不再 gzip → 手机打开首页要下 780KB 的 app.js。
    // 门槛抬到 u16::MAX（这是该参数的合法上限）等于「全都不压」，
    // 但比删掉整个 layer 好：仍然编译得过，变异测的是行为而不是语法。
    name: "⑱ 大响应不再 gzip",
    file: "crates/finweb/src/handlers.rs",
    from: "            SizeAbove::new(1024)",
    to: "            SizeAbove::new(u16::MAX)",
    cmd: XLSX("big_responses_are_gzipped_but_xlsx_is_not"),
  },
  {
    // 本年累计偷偷退回「读区间列」→ 首页「今年累计赚多少」变成「这个月赚多少」，
    // 两个数各自都像对的，只有年终才发现对不上。
    name: "⑲ 本年累计读成区间发生额",
    file: "crates/findb/src/advanced.rs",
    from: "            Flow::Ytd => r.ytd_credit - r.ytd_debit,",
    to: "            Flow::Ytd => r.credit - r.debit,",
    cmd: FADV("financial_ytd_matches_scanning_from_january"),
  },
  {
    // 带版本号的静态资源又变回每次回源验证 → 手机每次打开都重下 211KB。
    // 这是性能退化，不会报错，只能靠这类测试守。
    name: "⑲ 版本化静态资源不再长缓存",
    file: "crates/finweb/src/handlers.rs",
    from: "                    if versioned {",
    to: "                    if false {",
    cmd: STATIC("versioned_asset_is_cacheable_for_a_year"),
  },
  {
    // 首页也被缓存 → 升级后用户浏览器里钉着旧的 ?v=，拿旧脚本打新接口。
    name: "⑲ 首页被缓存住",
    file: "crates/finweb/src/handlers.rs",
    from: "    let mut r = Html(html.replace(\"{{V}}\", &state.assets_ver)).into_response();\n    r.headers_mut().insert(\n        header::CACHE_CONTROL,\n        axum::http::HeaderValue::from_static(\"no-cache\"),\n    );",
    to: "    let r = Html(html.replace(\"{{V}}\", &state.assets_ver)).into_response();",
    cmd: STATIC("index_html_is_never_cached"),
  },
  {
    // 页码开关失效 → 设置里勾了页码，纸上不出现（旧内核忽略页边距盒子也是这个症状，
    // 所以只有测试能分辨"我们的代码没生成"和"浏览器不支持"）
    name: "⑳ 勾了页码却不生成页边距盒子",
    file: "crates/fincore/src/print.rs",
    from: "        if self.show_page_no {",
    to: "        if false {",
    cmd: FPRINT("page_number_can_be_turned_off"),
  },
  {
    // 选了横向仍按纵向出纸：A4 纵向印 12 列的明细账，右边两列直接被裁掉
    name: "⑳ 选横向也不横向",
    file: "crates/fincore/src/print.rs",
    from: '            "landscape" => true,',
    to: '            "landscape" => false,',
    cmd: FPRINT("explicit_orientation_wins_over_column_count"),
  },
  {
    // 打印设置的结构校验不并进账套参数校验 → 越界字号存得进去，打印时才崩
    name: "⑳ 打印设置不进账套参数校验",
    file: "crates/fincore/src/account.rs",
    from: "        out.extend(self.print.problems());",
    to: "        // out.extend(self.print.problems());",
    cmd: OPT("out_of_range_print_settings_are_rejected_with_the_value"),
  },
  {
    // 偏移不进页边距 → 用户把"套打偏移"调到 +5mm，纸上一点没动
    name: "㉑ 套打偏移不进页边距",
    file: "crates/fincore/src/print.rs",
    from: "        let l = (base + self.offset_x_mm as i32).max(0);",
    to: "        let l = base;",
    cmd: FPRINT("paper_offset_shifts_the_printable_area"),
  },
  {
    // 负偏移不夹零 → 生成 margin:-8mm，浏览器整条丢弃，纸张回到默认边距（比不设还乱）
    name: "㉑ 负偏移不夹到零",
    file: "crates/fincore/src/print.rs",
    from: "        let l = (base + self.offset_x_mm as i32).max(0);\n        let r = (base - self.offset_x_mm as i32).max(0);",
    to: "        let l = base + self.offset_x_mm as i32;\n        let r = base - self.offset_x_mm as i32;",
    cmd: FPRINT("paper_offset_never_produces_a_negative_margin"),
  },
  {
    // 凭证套打自己写死纸张 → 账套里设的页码/偏移对凭证不生效（历史分叉复现）
    name: "㉑ 凭证套打不认账套版式",
    file: "crates/findb/src/printform.rs",
    // v1.8.38 起这行改成按纸型取值，靶子跟着挪（老靶子已不存在，
    // 不挪的话这条变异会静默失效 —— 「看着在守，其实没守」）
    from: 'page = po.page_box_css(&po.paper_css(fincore::print::PaperUse::Voucher), 10)',
    to: 'page = String::new()',
    cmd: FPFORM("voucher_form_honours_paper_offset_and_page_numbers"),
  },
  {
    // 预算控制值拼错（warning）不报 → 静默按"关闭"跑，超预算一路放行
    name: "㉑ 预算控制拼错也放行",
    file: "crates/fincore/src/account.rs",
    from: '        if !matches!(self.budget_control.trim(), "" | "off" | "warn" | "strong") {',
    to: '        if false {',
    cmd: FOPT("options_validation_catches_silent_misconfigurations"),
  },
  {
    // 建账/导入不过校验 → 建出一个「三天后才发现设置填错」的账套
    name: "㉓ 建账不校验账套参数",
    file: "crates/findb/src/lib.rs",
    from: '        let problems = Self::validate_options_for_new_book(opts);\n        if !problems.is_empty() {',
    to: '        let problems = Self::validate_options_for_new_book(opts);\n        if false {',
    cmd: FLIB("create_rejects_invalid_options"),
  },
  {
    // 总数按本页算 → 翻页时「共 N 笔」变小，会计会以为钱少了
    name: "㉓ 未核销总数跟着页走",
    file: "crates/finweb/src/handlers/settle.rs",
    from: '        "total": total,\n        "returned": page.len(),',
    to: '        "total": page.len(),\n        "returned": page.len(),',
    cmd: SOPEN("settle_open_pages_without_losing_the_total"),
  },
  {
    // 截断了却不说 → 表现是「这单明明还欠着，核销页里找不到」
    name: "㉓ 截断了却不说",
    file: "crates/finweb/src/handlers/settle.rs",
    from: '        "truncated": limit != 0 && offset + page.len() < total,',
    to: '        "truncated": false,',
    cmd: SOPEN("settle_open_pages_without_losing_the_total"),
  },
  {
    // 账龄天数按「今天」而不是期间末 → 与账龄表的 max_days 不同基准，两处对不上
    name: "㉓ 下钻天数不按期间末算",
    file: "crates/finweb/src/handlers.rs",
    from: '    v["days"] = serde_json::json!((as_of - e.date).num_days().max(0));',
    to: '    v["days"] = serde_json::json!(0);',
    cmd: SOPEN("settle_open_drills_into_one_party_with_age_in_days"),
  },
  {
    // 凭证纸型不生效 → 选了 A5 还在打 A4，套打必然歪
    name: "㉓ 凭证不认选定的纸型",
    file: "crates/findb/src/printform.rs",
    from: '        page = po.page_box_css(&po.paper_css(fincore::print::PaperUse::Voucher), 10)',
    to: '        page = po.page_box_css("A4 landscape", 10)',
    cmd: FPFORM("voucher_and_ledger_honour_the_chosen_paper"),
  },
  {
    // 账簿整块不认 PrintOptions → 偏移/页码/纸型对它全部无效（历史缺陷复现）
    name: "㉓ 账簿套打不认账套版式",
    file: "crates/findb/src/printform.rs",
    from: '        page_css = po.page_box_css(&po.paper_css(fincore::print::PaperUse::Ledger), 10),',
    to: '        page_css = String::new(),',
    cmd: FPFORM("voucher_and_ledger_honour_the_chosen_paper"),
  },
  {
    // 往来台账不写科目期初 → 账龄表上有欠款、资产负债表一分没有（本轮最想守住的一条）
    name: "㉒ 期初收集表只导台账不写科目期初",
    file: "crates/findb/src/opening_book.rs",
    from: '    if res.ar.total.is_positive() {\n        begin_rows.push(BeginLine {\n            code: "112201".to_string(),',
    to: '    if false {\n        begin_rows.push(BeginLine {\n            code: "112201".to_string(),',
    cmd: OBOOK("collection_sheet_lands_in_ledger_and_opening_balances"),
  },
  {
    // 不平也放行 → 试算平衡开账就不平，后面每个月都歪
    name: "㉒ 期初不平也照单全收",
    file: "crates/findb/src/opening_book.rs",
    from: "    if debit != credit {",
    to: "    if false {",
    cmd: OBOOK("unbalanced_sheet_is_rejected_and_nothing_is_written"),
  },
  {
    // 不是收集表也当收集表导 → 导进去一片空，用户以为成功了
    name: "㉒ 随便什么表都当收集表",
    file: "crates/findb/src/opening_book.rs",
    from: "    if ar_sheet.is_none() || ap_sheet.is_none() || bal_sheet.is_none() {",
    to: "    if false {",
    cmd: OBOOK("non_collection_workbook_is_rejected_with_a_readable_reason"),
  },
  {
    // 账龄导出不认往来科目 → 导出来的表永远是 1122 的，导出 2202 时用户不知道
    name: "㉒ 账龄导出写死应收科目",
    file: "crates/finweb/src/handlers/settle.rs",
    from: '    let account = q.get("account").cloned().unwrap_or_else(|| "1122".into());\n    let account = account.trim().to_string();',
    to: '    let account = "1122".to_string();',
    cmd: XLSX("settle_aging_export_as_xlsx"),
  },
  {
    // 业务默认科目不查末级：填了一级科目照样存得进，等到点"发货"才炸
    name: "⑳ 业务默认科目不查末级",
    file: "crates/findb/src/accounts.rs",
    from: "                if !chart.is_leaf(code) {",
    to: "                if false {",
    cmd: OPT("biz_account_must_be_a_leaf_account"),
  },
  {
    // 预收/预付被当成欠款累加 → 客户预存 300 变成「应向我方支付 1300」
    name: "㉔ 催款单把预收也当欠款",
    file: "crates/findb/src/settle/dunning.rs",
    from: "        let owed = !signed.is_negative();",
    to: "        let owed = true;",
    cmd: FSETTLE("dunning_counts_only_what_is_actually_owed"),
  },
  {
    // 账龄基准日回退成「今天」→ 查 1 月的账龄全落最老一档，与桌面端两个答案
    name: "㉔ 账龄基准日回退成今天",
    file: "crates/finweb/src/handlers/settle.rs",
    from: "        .unwrap_or_else(|| upto.last_day());",
    to: "        .unwrap_or_else(|| chrono::Local::now().date_naive());",
    cmd: FCUST("settle_aging_defaults_to_period_end"),
  },
  {
    // 催款单导出只剩汇总 Sheet → 客户问「哪几张单凑的」答不上来
    name: "㉔ 催款单导出丢掉明细表",
    file: "crates/finweb/src/handlers/export.rs",
    from: 'build_xlsx(&[sheet, dsheet], &fname, "催款单")',
    to: 'build_xlsx(&[sheet], &fname, "催款单")',
    cmd: FCUST("dunning_export_xlsx_has_detail"),
  },
  {
    name: "㉒ 撤销不看结账线",
    file: "crates/findb/src/settle/core.rs",
    from: "        if rec.period <= upto {",
    to: "        if false {",
    cmd: FSETTLE("unsettle_refuses_records_in_a_closed_period"),
  },
  {
    name: "㉒ 部分撤销不夹住记录金额",
    file: "crates/findb/src/settle/core.rs",
    from: "    if amount > rec.amount {",
    to: "    if false {",
    cmd: FSETTLE("unsettle_part_rejects_more_than_the_record"),
  },
  {
    name: "㉒ 核销记录仍按单据期间",
    file: "crates/findb/src/settle/core.rs",
    from: "    let rec_period = crate::periods::current_period_of(tx)?;",
    to: "    let rec_period = f.period;",
    cmd: FSETTLE("settle_record_is_dated_by_the_settlement_period_not_the_invoice"),
  },
  {
    name: "㉒ 撤到零不删记录",
    file: "crates/findb/src/settle/core.rs",
    from: "    if left.is_zero() {",
    to: "    if false {",
    cmd: FSETTLE("unsettle_part_reduces_the_record_and_reopens_the_amount"),
  },
  {
    name: "㉒ 空串被当成非法金额",
    file: "crates/finweb/src/handlers/settle.rs",
    from: "        Some(s) if !s.is_empty() => Some(",
    to: "        Some(s) => Some(",
    cmd: FUNS("blank_amount_means_full_unsettle"),
  },
  {
    name: "㉓ 预检其实也落库",
    file: "crates/findb/src/opening_book.rs",
    from: "    if dry_run {\n        return Ok(res);\n    }",
    to: "    if false {\n        return Ok(res);\n    }",
    cmd: LOOP("preview_does_not_write_then_import_does"),
  },
  {
    name: "㉓ 模板不预填科目清单",
    // v1.8.52：模板构造逻辑搬到了 findb，靶子跟着搬
    file: "crates/findb/src/opening_book.rs",
    from: "        .filter(|a| a.code.chars().next().is_some_and(|c| c == '1' || c == '2'))",
    to: "        .filter(|_a| false)",
    cmd: LOOP("template_carries_the_current_books_account_codes"),
  },
  {
    name: "㉓ 不平也预检通过",
    file: "crates/findb/src/opening_book.rs",
    from: "    if debit != credit {\n        let diff = debit - credit;",
    to: "    if false {\n        let diff = debit - credit;",
    cmd: LOOP("preview_rejects_an_unbalanced_sheet"),
  },
  {
    name: "㉔ 对账不比对台账与科目",
    file: "crates/findb/src/opening_check.rs",
    from: "        ok: ar_ledger_total == ar_booked,",
    to: "        ok: true,",
    cmd: LOOP("reconcile_catches_ledger_without_booked_accounts"),
  },
  {
    name: "㉔ 对账不查往来编码是否在档案里",
    file: "crates/findb/src/opening_check.rs",
    from: "            if crate::auxs::get(db, kind, &r.party_code)?.is_none() {\n                missing.push(format!(\"{}({})\", r.party_code, r.party_name));\n            }",
    to: "            let _ = (&db, kind, r);",
    cmd: LOOP("reconcile_flags_a_party_code_missing_from_the_archive"),
  },
  {
    name: "㉔ 负债权益不按余额方向取反（亏损变收益）",
    file: "crates/findb/src/opening_check.rs",
    from: "            _ => -amt,",
    to: "            _ => amt,",
    cmd: LOOP("reconcile_treats_debit_profit_as_a_loss"),
  },
  {
    name: "㉕ ③ 表不读银行账户列",
    file: "crates/findb/src/opening_book.rs",
    from: '    let c_bank = col(&head, &["银行账户", "账户编码"], usize::MAX);',
    to: "    let c_bank = usize::MAX;",
    cmd: LOOP("balance_sheet_carries_quantity_stock_and_bank"),
  },
  {
    name: "㉕ ③ 表不读数量列",
    file: "crates/findb/src/opening_book.rs",
    from: '    let c_qty = col(&head, &["数量", "期初数量", "数量期初"], usize::MAX);',
    to: "    let c_qty = usize::MAX;",
    cmd: LOOP("balance_sheet_carries_quantity_stock_and_bank"),
  },
  {
    name: "㉕ 客商列按资产/负债方向判（预收挂成供应商）",
    file: "crates/findb/src/opening_book.rs",
    from: '            let to_customer = code.starts_with("1122") || code.starts_with("2203");',
    to: "            let to_customer = code.starts_with('1');",
    cmd: FOB("balances_sheet_keeps_quantity_item_bank_and_party"),
  },
  {
    name: "㉖ 预览保存忽略字号（拖了不生效）",
    file: "crates/fincore/src/print.rs",
    from: "        if let Some(v) = p.font_pt {\n            self.font_pt = v;\n        }",
    to: "        if p.font_pt.is_some() {}",
    cmd: FPS("a_single_drag_is_saved_without_touching_the_rest"),
  },
  {
    name: "㉖ 预览保存不校验就落库（20pt 也能存）",
    file: "crates/findb/src/lib.rs",
    from: "        let mut problems = o.validate();",
    to: "        let mut problems: Vec<String> = Vec::new();",
    cmd: FPS("out_of_range_value_is_rejected_with_the_number"),
  },
  {
    name: "㉖ 局部改动退化成整份覆盖（纸型被冲掉）",
    file: "crates/fincore/src/print.rs",
    from: "        if let Some(v) = &p.orientation {\n            self.orientation = v.trim().to_string();\n        }",
    to: "        *self = PrintOptions::default();\n        if let Some(v) = &p.orientation {\n            self.orientation = v.trim().to_string();\n        }",
    cmd: FPS("a_single_drag_is_saved_without_touching_the_rest"),
  },
  {
    name: "㉗ 营业成本退回只算 6401/6402（5101 不进利润表）",
    file: "crates/fincore/src/report/income.rs",
    from: '        expense(&["6401", "6402", "5001", "5101", "5201"]),',
    to: '        expense(&["6401", "6402"]),',
    cmd: ICOST("unclosed_manufacturing_cost_reaches_the_income_statement"),
  },
  {
    name: "㉘ 归集规则表静默不生效（表永远是空的）",
    file: "crates/fincore/src/report/cashflow_rules.rs",
    from: "    for r in candidates {",
    to: "    for r in candidates.into_iter().take(0) {",
    cmd: FRULE("sales_of_services_is_operating_inflow"),
  },
  {
    name: "㉘ 未归集金额不计入净增加额（勾稽永远不平）",
    file: "crates/fincore/src/report/cashflow.rs",
    from: "        net_increase: operating_net + investing_net + financing_net + unassigned,",
    to: "        net_increase: operating_net + investing_net + financing_net,",
    cmd: FREP("unassigned_reported_only_when_ambiguous"),
  },
  {
    name: "㉘ 税额伴生行也投票（收入+销项被判成分不清）",
    file: "crates/findb/src/reports.rs",
    from: '                        .filter(|c| !(counters.len() > 1 && c.starts_with("2221")))',
    to: "                        .filter(|_c| true)",
    cmd: CF("sales_with_output_tax_goes_to_operating_inflow"),
  },
  {
    name: "㉘ 提现/存现被当成未归集的现金流量",
    file: "crates/findb/src/reports.rs",
    from: "                    if counters.is_empty() {\n                        out.transfers += 1;\n                        continue;\n                    }",
    to: "                    if false {\n                        out.transfers += 1;\n                        continue;\n                    }",
    cmd: CF("cash_transfer_is_not_a_cash_flow"),
  },
  {
    name: "㉘ 人工标注被规则覆盖",
    file: "crates/findb/src/reports.rs",
    from: '                Some(t) if !t.trim().is_empty() => Some(t.trim().to_string()),',
    to: "                Some(_t) if false => None,",
    cmd: CF("manual_label_wins_over_rules"),
  },
  // --- v1.8.47 月结向导 -------------------------------------------------
  {
    name: "㉙ 向导把未记账当完成",
    file: "crates/findb/src/month_close.rs",
    from: "        ok: unposted == 0,",
    to: "        ok: true,",
    cmd: MC("draft_voucher_blocks_closing"),
  },
  {
    name: "㉙ 未记账不再阻止结账",
    file: "crates/findb/src/month_close.rs",
    from: `        code: "unposted",
        label: "凭证全部记账",
        ok: unposted == 0,
        blocking: true,`,
    to: `        code: "unposted",
        label: "凭证全部记账",
        ok: unposted == 0,
        blocking: false,`,
    cmd: MC("draft_voucher_blocks_closing"),
  },
  {
    name: "㉙ 已结账期间仍说可以结",
    file: "crates/findb/src/month_close.rs",
    from: "        !self.closed && self.steps.iter().all(|s| !s.blocking || s.ok)",
    to: "        self.steps.iter().all(|s| !s.blocking || s.ok)",
    cmd: MC("closed_period_reports_state_and_refuses"),
  },

  // --- v1.8.48 账期落地 ---------------------------------------------------
  {
    name: "㉚ 账期不参与逾期计算",
    file: "crates/findb/src/settle/aging.rs",
    from: "                let due_days = *d - l.max_days;",
    to: "                let due_days = 0 - l.max_days;",
    cmd: FSETTLE("aging_uses_pay_days_to_compute_overdue"),
  },
  {
    name: "㉚ 账龄读不到客商档案的账期",
    file: "crates/findb/src/settle/aging.rs",
    from: "        if let Some(v) = a.props.get(fincore::auxiliary::prop::PAY_DAYS) {",
    to: `        if let Some(v) = a.props.get("__no_such_prop__") {`,
    cmd: FSETTLE("aging_uses_pay_days_to_compute_overdue"),
  },

  // --- v1.8.49 参数导出/导入/恢复默认 -------------------------------------
  {
    name: "㉛ 恢复默认顺手清掉企业名称",
    file: "crates/fincore/src/account.rs",
    from: "        self.budget_control = String::new();\n        self.doc_prefixes.clear();",
    to: "        self.budget_control = String::new();\n        self.company.clear();\n        self.doc_prefixes.clear();",
    cmd: FOPT("reset_preferences_keeps_what_is_bound_to_existing_data"),
  },
  {
    name: "㉛ 导入配置结构不同也不拒绝",
    file: "crates/fincore/src/account.rs",
    from: "        if other.code_scheme != self.code_scheme {",
    to: "        if false {",
    cmd: OCFG("import_moves_preferences_but_refuses_structural_mismatch"),
  },
  {
    name: "㉛ 恢复默认不清预算控制",
    file: "crates/fincore/src/account.rs",
    from: "        self.budget_control = String::new();\n        self.doc_prefixes.clear();",
    to: "        self.doc_prefixes.clear();",
    cmd: OCFG("reset_only_touches_preferences"),
  },

  // --- v1.8.50 催款函逾期 + 现金流对比 -------------------------------------
  {
    name: "㉜ 催款明细不算逾期",
    file: "crates/findb/src/settle/dunning.rs",
    from: "    (letter_date - doc_date)\n        .num_days()\n        .saturating_sub(days)\n        .max(0)",
    to: "    0",
    cmd: FSETTLE("dunning_items_carry_overdue_days_from_pay_days"),
  },
  {
    name: "㉜ 没设账期也硬算逾期",
    file: "crates/findb/src/settle/dunning.rs",
    from: "    let Some(days) = pay_days else {\n        return 0;\n    };",
    to: "    let days = pay_days.unwrap_or(0);",
    cmd: FSETTLE("dunning_overdue_is_zero_without_pay_days"),
  },
  {
    name: "㉜ 现金流对比吞掉上期独有项目",
    file: "crates/finweb/src/sheets.rs",
    from: "            if seen.contains(&l.code.as_str()) {\n                continue;\n            }",
    to: "            continue;",
    cmd: FSH("cash_flow_compare_keeps_last_period_only_items"),
  },

  // ⑭ 现金流量表打印页也要有上期对比（v1.8.53）
  {
    name: "⑭ 现金流量表打印页丢掉上期对比列",
    file: "crates/finweb/src/handlers/reports.rs",
    from: "        Some(&prev),\n    );\n    Ok(([(header::CONTENT_TYPE, \"text/html; charset=utf-8\")], html).into_response())",
    to: "        None,\n    );\n    Ok(([(header::CONTENT_TYPE, \"text/html; charset=utf-8\")], html).into_response())",
    cmd: CFRINT("cash_flow_print_page_carries_last_period_columns"),
  },
  {
    name: "⑭ 上期区间多往前推几个月（3 个月报成 2 个月前那段）",
    file: "crates/findb/src/reports.rs",
    from: "    let prev_to = from.prev();\n    let mut prev_from = prev_to;",
    to: "    let mut prev_to = from.prev();\n    for _ in 1..months {\n        prev_to = prev_to.prev();\n    }\n    let mut prev_from = prev_to;",
    cmd: FCFW("quarter_takes_the_previous_quarter_not_one_month"),
  },

  // ㉞ 设备绑定 M-4（v1.8.53）
  {
    name: "㉞ 新设备又能自动绑进来（M-4 回潮）",
    file: "crates/finweb/src/realm.rs",
    from: "        if count > 0 {",
    to: "        if false {",
    cmd: REALM("first_bind_is_automatic_but_a_second_device_is_refused"),
  },
  {
    name: "㉞ 同设备重登不再刷新行（每次都新插一行）",
    file: "crates/finweb/src/realm.rs",
    from: "        if updated > 0 {",
    to: "        if false {",
    cmd: REALM("rebinding_the_same_device_refreshes_instead_of_growing"),
  },
  {
    name: "㉞ 存量绑定行不回填指纹",
    file: "crates/finweb/src/realm.rs",
    from: "            .prepare(\"SELECT username, device_id FROM realm_user_device WHERE device_hash = ''\")?",
    to: "            .prepare(\"SELECT username, device_id FROM realm_user_device WHERE 1 = 0\")?",
    cmd: REALM("legacy_single_column_device_binding_is_migrated"),
  },

  // ㉝ 桌面端补齐（v1.8.52）
  {
    name: "㉝ 发票税额各自算一遍（不含税+税额对不上合计）",
    file: "crates/finui/src/views/invoice_view.rs",
    from: "    let amount = (total.inner() / divisor).round_dp(2);",
    to: "    let amount = (total.inner() / divisor).round_dp(1);",
    cmd: FUI("views::invoice_view::tests::thirteen_percent_splits_cleanly"),
  },
  {
    name: "㉝ 现金流对比表吞掉上期独有项目",
    file: "crates/finui/src/views/reports.rs",
    from: "            if !cur.iter().any(|c| c.name == l.name) {",
    to: "            if false {",
    cmd: FUI("views::reports::compare_sheet_tests::last_period_only_lines_are_kept_and_marked"),
  },
  {
    name: "㉝ 期初③不再排除 1122/2202（往来双计）",
    file: "crates/findb/src/opening_book.rs",
    from: ".filter(|a| !a.code.starts_with(\"1122\") && !a.code.starts_with(\"2202\"))",
    to: ".filter(|_a| true)",
    cmd: ["test", "-p", "findb", "--lib", "opening_book::template_tests::balances_sheet_excludes_generated_ar_and_ap_but_keeps_the_rest"],
  },
  {
    name: "㉝ 期初收集表只列资产类（负债全丢）",
    file: "crates/findb/src/opening_book.rs",
    from: ".filter(|a| a.code.chars().next().is_some_and(|c| c == '1' || c == '2'))",
    to: ".filter(|a| a.code.chars().next().is_some_and(|c| c == '1'))",
    cmd: ["test", "-p", "findb", "--lib", "opening_book::template_tests::balances_sheet_excludes_generated_ar_and_ap_but_keeps_the_rest"],
  },

  // ㉞ 部门核算（v1.8.61）：三条都是「不报错但静悄悄不对」的形态。
  {
    // 结转取数退回「按科目聚合」：结果就是挂了部门的损益/成本科目生成一张
    // 不带部门、存不进去的结转凭证，而报表上看不出任何异常。
    name: "㉞ 结转损益丢失部门维度",
    file: "crates/findb/src/balances.rs",
    from: "                self.aux_breakdown(&a.code)\n            };",
    to: "                vec![self.for_account(&a.code, None)]\n            };",
    cmd: ["test", "-p", "findb", "--lib", "balances::drill_tests::carry_forward_keeps_dept_dimension"],
  },
  {
    // 部门退回「必填」：系统自动生成的凭证（成本分摊、费用计提）会全部存不进去。
    name: "㉞ 部门被当成必填维度",
    file: "crates/fincore/src/engine/validate.rs",
    from: "        if k == AuxKind::Dept {\n            continue;\n        }\n",
    to: "",
    cmd: ["test", "-p", "fincore", "--lib", "engine::validate::tests::dept_aux_is_optional"],
  },
  {
    // 明细账不收窄部门：部门会计在凭证列表里看不到别部门，明细账里却看得见。
    name: "㉟ 明细账不吃部门范围",
    file: "crates/findb/src/balances.rs",
    from: '    if let Some(c) = dept_scope_cond("e", &q.dept_scope, &mut params) {\n        sql.push_str(&format!(" AND {c}"));\n    }\n    sql.push_str(" ORDER BY v.date, v.word, v.no, e.line");',
    to: '    sql.push_str(" ORDER BY v.date, v.word, v.no, e.line");',
    cmd: ["test", "-p", "findb", "--lib", "balances::drill_tests::dept_scope_narrows_ledgers_but_not_reports"],
  },
  {
    // 余额表也按部门收窄（我差点这么改）：借方本部门、贷方银行全公司，
    // 一张本来平衡的表被算成不平，而且没人会觉得是权限问题。
    name: "㉟ 余额表也按部门收窄（借贷不平）",
    file: "crates/finweb/src/handlers.rs",
    from: "    let bq = BalanceQuery::range(from, to).with_user_scope(&user.user);",
    to: "    let bq = BalanceQuery::range(from, to)\n        .with_user_scope(&user.user)\n        .with_dept_scope(&user.user);",
    cmd: ["test", "-p", "finweb", "--test", "api", "--", "voucher_page_total_respects_dept_scope"],
  },
  {
    // 折旧凭证要求必填部门：默认科目表给费用科目挂了部门之后，没填「使用部门」的卡片
    // 就再也计提不了（E2E `#as-accrue` 直接 400，界面显示「本期已计提记录 0 条」）。
    name: "㊱ 折旧凭证要求必填部门（没填部门就报错）",
    file: "crates/findb/src/assets.rs",
    from: '            AuxKind::Dept => {\n                if !dept.trim().is_empty() {\n                    e.aux.dept = Some(dept.to_string());\n                }\n            }',
    to: '            AuxKind::Dept => {\n                if dept.trim().is_empty() {\n                    return Err(                    fincore::FinError::msg(format!("科目 {code} {} 要求核算部门", acct.name)).into());\n                }\n                e.aux.dept = Some(dept.to_string());\n            }',
    cmd: ["test", "-p", "findb", "--lib", "assets::tests::dep_accrual_does_not_require_dept"],
  },
  {
    // 部门只挂上级科目：default_accounts 会把非末级的 aux 清零，等于一条都没开。
    name: "㉞ 部门只挂在上级科目上",
    file: "crates/fincore/src/chart.rs",
    from: '        "5101" | "510101" | "510102" | "510103" | "510104" | "510105" => {',
    to: '        "5101" => {',
    cmd: ["test", "-p", "fincore", "--lib", "chart::tests::dept_aux_lands_on_leaf_accounts"],
  },
  // v1.8.62：多维损益搬上 Web。三条都是「数字看着正常、口径已经错了」的形态：
  // 未分配行消失、收入翻倍、部门会计看到别部门。
  {
    // 不剔除结转损益凭证：结转过的一期，未分配行会算成 0 而消失。
    name: "㊲ 多维损益不剔除结转损益凭证",
    file: "crates/findb/src/mgmt.rs",
    from: "        .with_exclude_carry(true)",
    to: "        .with_exclude_carry(false)",
    cmd: [
      "test",
      "-p",
      "findb",
      "--lib",
      "mgmt::tests::dim_profit_report_keeps_unallocated_after_carry",
    ],
  },
  {
    // 父科目也参与聚合：快照已把子科目上卷到父科目，再来一遍就是双份。
    name: "㊲ 多维损益把父科目也算一遍（收入翻倍）",
    file: "crates/findb/src/mgmt.rs",
    from: "        .filter(|a| a.category.is_profit_loss() && chart.is_leaf(&a.code))",
    to: "        .filter(|a| a.category.is_profit_loss())",
    cmd: [
      "test",
      "-p",
      "findb",
      "--lib",
      "mgmt::tests::dim_profit_report_keeps_unallocated_after_carry",
    ],
  },
  {
    // 不按部门数据范围收窄：部门会计能看到别的部门那一行。
    name: "㊲ 多维损益不吃部门数据范围",
    file: "crates/finweb/src/handlers/reports.rs",
    from: "        report.narrow_to_depts(&user.user.data_scope.depts);",
    to: "        let _ = &user.user.data_scope.depts;",
    cmd: ["test", "-p", "finweb", "--test", "api", "--", "dim_profit_web_by_dept"],
  },
  // v1.8.63：受托加工的现场三件事（客供料台账 / 加工费规则 / 计件工资）。
  // 这一批的失效形态是「单子照样存进去、金额照样算出来，只是不该放行的放行了」：
  // 超交不拦、编辑时把自己算两遍、退料当来料、最低收费不兜底、加急漏固定费、
  // 规则匹配取到最差的那条、计件不按期间、回填重复加钱。
  {
    // 超交不拦：来料 100、交货 60 之后还能再交 50，结存变负也没人管。
    name: "㊳ 客供料交货不校验结存（可以超交）",
    file: "crates/findb/src/consign.rs",
    from: '            if l.qty > have {',
    to: '            if false && l.qty > have {',
    cmd: ["test", "-p", "findb", "--lib", "consign::tests::交货超过现存数量要报错"],
  },
  {
    // 编辑不排除自身：把本单已占用的数量又算一遍，改大数量时会被自己拦住。
    name: "㊳ 客供料编辑单据不排除自身占用",
    file: "crates/findb/src/consign.rs",
    from: '            let have = available(db, &customer, l.item.trim(), batch, d.id)?;',
    to: '            let have = available(db, &customer, l.item.trim(), batch, 0)?;',
    cmd: ["test", "-p", "findb", "--lib", "consign::tests::编辑单据要排除自身占用"],
  },
  {
    // 退料当来料：只有 out 算减，return/loss 会被记成加，结存越退越多。
    name: "㊳ 客供料退料/损耗算成加数（结存越退越多）",
    file: "crates/findb/src/consign.rs",
    from: '    if kind == "in" {',
    to: '    if kind != "out" {',
    cmd: ["test", "-p", "findb", "--lib", "consign::tests::来料交货退料损耗的结存与对账单"],
  },
  {
    // 最低收费不兜底：0.5 ㎡ × 18 = 9 元直接报 9，加工费还不够开一次槽。
    name: "㊳ 加工费最低收费不兜底",
    file: "crates/findb/src/processfee.rs",
    from: '    let floor_extra = if rule.min_charge > base {\n        rule.min_charge - base\n    } else {\n        Money::ZERO\n    };',
    to: '    let floor_extra = Money::ZERO;',
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::面积计价_最低收费_加急费"],
  },
  {
    // 加急漏固定费：只剩按比例的加价，20 元的开机等待费凭空消失。
    name: "㊳ 加工费加急只按比例、漏固定费",
    file: "crates/findb/src/processfee.rs",
    from: '        (rule.rush_fee + pct).round2()',
    to: '        pct.round2()',
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::面积计价_最低收费_加急费"],
  },
  {
    // 规则匹配取最差：物料+客户的专属规则被通配规则顶掉，报价按错单价算。
    name: "㊳ 加工费规则匹配取到最差的那条",
    file: "crates/findb/src/processfee.rs",
    // v49 起匹配改成「按精度 0→3 依次挑，挑到就返回」（同一个循环里还要比生效日），
    // 所以「取最差」等于把这个循环倒过来走。
    from: "    for rank in 0..=3u8 {",
    to: "    for rank in (0..=3u8).rev() {",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::规则匹配优先级与停用"],
  },
  {
    // 计件不按期间：上个月的报工也算进本月，工资会重复发。
    name: "㊳ 计件工资不按期间过滤报工流水",
    file: "crates/findb/src/piecepay.rs",
    from: '         FROM prod_op_log WHERE period=?1 ORDER BY worker,id",',
    to: '         FROM prod_op_log WHERE ?1=?1 ORDER BY worker,id",',
    cmd: ["test", "-p", "findb", "--lib", "piecepay::tests::计件工资按期间汇总到人"],
  },
  {
    // 工资凭证是按旧金额入账的，改完工资表金额就对不上；这道闸放空了，
    // 界面上还挂着 voucher_id，看着像一致。
    name: "㊴ 工资表不检查是否已生成工资凭证",
    file: "crates/findb/src/business.rs",
    from: "    if linked > 0 {",
    to: "    if false {",
    cmd: [
      "test",
      "-p",
      "findb",
      "--lib",
      "工资表在生成凭证后与结账后都不许改",
    ],
  },
  {
    // 结账闸：已结账月份连凭证都不许录，工资表自然更不许改。
    name: "㊴ 工资表不检查期间是否已结账",
    file: "crates/findb/src/business.rs",
    from: "        if period <= upto {\n            return Err(fincore::FinError::state(format!(\n                \"{} 及以前期间已结账，工资表不能再改\",",
    to: "        if false {\n            return Err(fincore::FinError::state(format!(\n                \"{} 及以前期间已结账，工资表不能再改\",",
    cmd: [
      "test",
      "-p",
      "findb",
      "--lib",
      "工资表在生成凭证后与结账后都不许改",
    ],
  },
  {
    // 回填是第三条改工资的路，闸漏在这里，前面两条拦得再严也白搭。
    name: "㊴ 计件回填绕过工资闸",
    file: "crates/findb/src/piecepay.rs",
    from: "    crate::business::payroll_guard_editable(db, period)?;",
    to: "    let _ = period;",
    cmd: [
      "test",
      "-p",
      "findb",
      "--lib",
      "piecepay::tests::计件回填不改已生成工资凭证的月份",
    ],
  },
  {
    // 回填重复加钱：应发已经不低于计件额也照样再算一遍，跑两次发两份；
    // 底薪场景更糟——会把 500 的应发压到计件的 100。
    // v1.8.64 起这段判据搬进了 findb::piecepay::fill_one（网页与桌面共用），
    // 变异随之改锚到这里，否则改了半天界面层，数据层还照旧能重复加钱。
    name: "㊳ 计件回填不判已有应发（重复加钱）",
    file: "crates/findb/src/piecepay.rs",
    from: '    if gross >= p.amount {',
    to: '    if false {',
    cmd: [
      "test",
      "-p",
      "findb",
      "--lib",
      "piecepay::tests::计件回填保留比计件额高的应发",
    ],
  },
  {
    // 挂具台账只管数量，领用超过在库就会变成负库存，账上就开始骗人。
    name: "㊵ 挂具领用不校验在库",
    file: "crates/findb/src/fixture.rs",
    from: '        "issue" if m.qty.cents() > bal.in_stock.cents() => {',
    to: '        "issue" if false => {',
    cmd: ["test", "-p", "findb", "--lib", "fixture::tests::领用超过在库要报错"],
  },
  {
    name: "㊵ 挂具归还不校验在用",
    file: "crates/findb/src/fixture.rs",
    from: '        "back" if m.qty.cents() > bal.in_use.cents() => {',
    to: '        "back" if false => {',
    cmd: ["test", "-p", "findb", "--lib", "fixture::tests::归还未领用的挂具要报错"],
  },
  {
    name: "㊵ 报废不校验在库（允许从在用扣）",
    file: "crates/findb/src/fixture.rs",
    from: '        "scrap" if m.qty.cents() > bal.in_stock.cents() => {',
    to: '        "scrap" if false => {',
    cmd: [
      "test",
      "-p",
      "findb",
      "--lib",
      "fixture::tests::报废数量超过在库要提示先归还",
    ],
  },
  {
    // life_times=0 表示不限次数；把这下限判断去掉，所有挂具都会被判「该换新」。
    name: "㊵ 挂具不限次数也判该换新",
    file: "crates/findb/src/fixture.rs",
    from: "        self.life_times > 0 && self.used_times >= self.life_times",
    to: "        self.used_times >= self.life_times",
    cmd: ["test", "-p", "findb", "--lib", "fixture::tests::使用次数用完要提示换新"],
  },
  {
    // 删掉档案就无从重算结存：有流水必须拦住。
    name: "㊵ 挂具有流水也能删档案",
    file: "crates/findb/src/fixture.rs",
    from: "    if n > 0 {",
    to: "    if false {",
    cmd: ["test", "-p", "findb", "--lib", "fixture::tests::档案编号必填且有条流水不能删"],
  },

  {
    // 单价不看工艺版本 = 工艺改版后老订单按新价算（或多算或少算）。
    name: "㊶ 计件单价不看工艺版本",
    file: "crates/findb/src/piecepay.rs",
    from: "        let ver = r.version.trim();\n        let ver_rank = if ver.is_empty() {\n            1u8\n        } else if ver == version {\n            0\n        } else {\n            continue;\n        };",
    to: "        let ver_rank = 0u8;\n        let _ = version;",
    cmd: ["test", "-p", "findb", "--lib", "piecepay::tests::计件单价按工艺版本匹配"],
  },
  {
    // 不分先后 = 「不限版本」的兜底价压过钉住版本的专用价。
    name: "㊶ 计件单价版本不分先后",
    file: "crates/findb/src/piecepay.rs",
    from: "        let ver_rank = if ver.is_empty() {\n            1u8\n        } else if ver == version {\n            0\n        } else {\n            continue;\n        };",
    to: "        let ver_rank = if ver.is_empty() {\n            0u8\n        } else if ver == version {\n            1\n        } else {\n            continue;\n        };",
    cmd: ["test", "-p", "findb", "--lib", "piecepay::tests::计件单价按工艺版本匹配"],
  },
  {
    // 报工流水不记版本，钉住版本的单价就永远匹配不上。
    name: "㊶ 报工流水不记工艺版本",
    file: "crates/findb/src/advanced.rs",
    from: "                o.op_name, o.worker, COALESCE(rt.version,'')",
    to: "                o.op_name, o.worker, ''",
    cmd: [
      "test",
      "-p",
      "finweb",
      "--test",
      "api",
      "--",
      "piece_rate_matches_routing_version",
    ],
  },
  {
    // 只有非默认版本工艺的小厂，会变成「没有工艺路线」调不出工序。
    name: "㊶ 没有默认版本就不生成工序",
    file: "crates/findb/src/advanced.rs",
    from: "            match routing_versions(db, item_code)?.first().cloned() {",
    to: "            match None::<String> {",
    cmd: [
      "test",
      "-p",
      "finweb",
      "--test",
      "api",
      "--",
      "piece_rate_matches_routing_version",
    ],
  },

  {
    // 下单不记版本：订单上永远空，事后重生成工序只能退回默认版本。
    name: "㊷ 生产订单不记工艺版本",
    file: "crates/finweb/src/handlers/prod.rs",
    from: "        routing_version: req.version.trim().to_string(),",
    to: "        routing_version: String::new(),",
    cmd: [
      "test",
      "-p",
      "finweb",
      "--test",
      "api",
      "--",
      "prod_order_records_routing_version",
    ],
  },
  {
    // 生成工序不吃订单版本：选了 V1 也按默认版本建，报工版本随之错位。
    name: "㊷ 生成工序不吃订单版本",
    file: "crates/finweb/src/handlers/prod.rs",
    from: "    let ops = advanced::prod_op_init_from_routing_version(db, id, code, &order.routing_version)\n        .unwrap_or(0);",
    to: "    let ops = advanced::prod_op_init_from_routing(db, id, code).unwrap_or(0);",
    cmd: [
      "test",
      "-p",
      "finweb",
      "--test",
      "api",
      "--",
      "prod_order_records_routing_version",
    ],
  },
  {
    // 存库漏写这一列：读回来永远是空。
    name: "㊷ 生产订单存库漏写版本",
    file: "crates/findb/src/scm.rs",
    from: "                order.plan_start, order.plan_end, order.routing_version,",
    to: "                order.plan_start, order.plan_end, String::new(),",
    cmd: ["test", "-p", "findb", "--lib", "scm::tests::prod_order_keeps_routing_version"],
  },

  {
    // 分子退化成「材料+人工」占比：界面还写着按工时，分出来的数其实是成本比例。
    name: "㊸ 工时分摊按成本算分子",
    file: "crates/findb/src/manufacturing.rs",
    from: "            OverheadBase::Hours => w.hours,",
    to: "            OverheadBase::Hours => w.material + w.labor,",
    cmd: ["test", "-p", "findb", "--lib", "manufacturing::tests::overhead_by_hours"],
  },
  {
    // 分母也退化成成本合计：分子分母一起错，比例看着"正常"，合计却对不上费用池。
    name: "㊸ 工时分摊按成本算分母",
    file: "crates/findb/src/manufacturing.rs",
    from: "        OverheadBase::Hours => wip.iter().map(|w| w.hours).sum(),",
    to: "        OverheadBase::Hours => wip.iter().map(|w| w.material + w.labor).sum(),",
    cmd: ["test", "-p", "findb", "--lib", "manufacturing::tests::overhead_by_hours"],
  },
  {
    // 没报工也照分：分母是 0，逐单份额全变 0，分摊"成功"但一分钱没分出去。
    name: "㊸ 没有报工工时也照分",
    file: "crates/findb/src/manufacturing.rs",
    from: "    if base == OverheadBase::Hours && base_sum.is_zero() {",
    to: "    if false {",
    cmd: ["test", "-p", "findb", "--lib", "manufacturing::tests::overhead_by_hours"],
  },
  {
    // 费用池取贷方：制造费用科目贷方是期末结转出去的那笔，取它等于把"转走的钱"再分一次。
    name: "㊸ 费用池取贷方发生额",
    file: "crates/findb/src/manufacturing.rs",
    from: "    Ok(snap.for_account(account_code, None).debit)",
    to: "    Ok(snap.for_account(account_code, None).credit)",
    cmd: ["test", "-p", "findb", "--lib", "manufacturing::tests::overhead_pool"],
  },
  {
    // 挂订单不校验客户：交货量会记到别人头上。
    name: "㊻ 交货单挂订单不校验客户",
    file: "crates/findb/src/consign.rs",
    from: "        if so.customer_code.trim() != customer {",
    to: "        if false {",
    cmd: ["test", "-p", "findb", "--lib", "consign::tests::挂别人的销售订单要报错"],
  },
  {
    // 进度不扣已交：每单永远显示未交 = 订购量。
    name: "㊻ 交货进度不扣已交",
    file: "crates/findb/src/consign.rs",
    from: "        row.delivered = got;",
    to: "        row.delivered = Money::ZERO;",
    cmd: ["test", "-p", "findb", "--lib", "consign::tests::交货单挂销售订单并算出交货进度"],
  },
  {
    // 退料/来料也算交付：交付进度虚高，看着像快交完了。
    name: "㊻ 退料来料都算成品交付",
    file: "crates/findb/src/consign.rs",
    from: "         WHERE d.customer = ?1 AND d.kind = 'out' AND d.so_id > 0",
    to: "         WHERE d.customer = ?1 AND d.so_id > 0",
    cmd: ["test", "-p", "findb", "--lib", "consign::tests::交货单挂销售订单并算出交货进度"],
  },
  {
    // 自动挂科目不管科目是否已被别的行占用：同一科目挂两行，合计翻倍。
    name: "㊾ 自动挂科目不管科目已被占用",
    file: "crates/findb/src/profit_sheet.rs",
    from: "        if used.contains(&a.code) {",
    to: "            if false {",
    cmd: ["test", "-p", "findb", "--lib", "profit_sheet::tests::自动挂科目按现有科目名找末级科目且不重复"],
  },
  {
    // 支出行也认资产/负债类科目：「应付职工薪酬」会被「薪酬」两个字捞进来。
    name: "㊾ 自动挂科目把负债类科目挂到支出行",
    file: "crates/findb/src/profit_sheet.rs",
    from: "            matches!(a.category, AcctCategory::Cost | AcctCategory::Expense)",
    to: "                true",
    cmd: ["test", "-p", "findb", "--lib", "profit_sheet::tests::自动挂科目按现有科目名找末级科目且不重复"],
  },
  {
    // 预览也写库：人还没看，行就被改掉了。
    name: "㊾ 自动挂科目预览也写库",
    file: "crates/findb/src/profit_sheet.rs",
    from: "        if apply {\n            save(db, &mut row)?;",
    to: "        if true {\n            save(db, &mut row)?;",
    cmd: ["test", "-p", "findb", "--lib", "profit_sheet::tests::自动挂科目按现有科目名找末级科目且不重复"],
  },
  {
    // 收入行也按借方取数：收入会显示成负数，净利润整张表跟着错。
    name: "㊽ 利润分析表收入行按借方取数",
    file: "crates/findb/src/profit_sheet.rs",
    from: "                r.credit - r.debit",
    to: "                r.debit - r.credit",
    cmd: ["test", "-p", "findb", "--lib", "profit_sheet::tests::利润分析表按行取数并算出净利润"],
  },
  {
    // 同一科目挂两行：合计翻倍，而报表看起来完全正常。
    name: "㊽ 同一科目可以挂两行（合计翻倍）",
    file: "crates/findb/src/profit_sheet.rs",
    from: "            if mine.contains(&code) {",
    to: "            if false && mine.contains(&code) {",
    cmd: ["test", "-p", "findb", "--lib", "profit_sheet::tests::同一科目挂两行要报错"],
  },
  {
    // 没挂科目的行不标出来：用户看着一片 0 以为这月没花钱。
    name: "㊽ 没挂科目的行不标记",
    file: "crates/findb/src/profit_sheet.rs",
    from: "        let is_unmapped = codes.is_empty();",
    to: "        let is_unmapped = false && codes.is_empty();",
    cmd: ["test", "-p", "findb", "--lib", "profit_sheet::tests::未挂科目的行会被标出来"],
  },
  {
    // 结转不补回来：月末一结转，支出行全变 0，净利等于全部收入。
    name: "㊽ 结转过账后支出行照旧被冲成 0",
    file: "crates/findb/src/profit_sheet.rs",
    from: "                r.debit - r.credit + carry_out_of(&carried, code)",
    to: "                r.debit - r.credit",
    cmd: ["test", "-p", "findb", "--lib", "profit_sheet::tests::结转过账的月份支出行仍是本月实际花的钱"],
  },
  {
    // 体检把上级科目也算一遍：同一笔钱报两次，人以为漏了双倍的。
    name: "㊽ 表外体检把上级科目也算一遍",
    file: "crates/findb/src/profit_sheet.rs",
    from: "        if a.disabled || !chart.is_leaf(&a.code) {",
    to: "        if a.disabled {",
    cmd: ["test", "-p", "findb", "--lib", "profit_sheet::tests::没落到行上的费用会被体检列出来"],
  },
  {
    // 没录工单也摆一个 0.00：0 和「没录」分不开，用户以为软件算错了。
    name: "㊽ 没录生产工单也摆一个 0.00",
    file: "crates/findb/src/profit_sheet.rs",
    from: "            qty: if row.is_revenue() && has_orders {",
    to: "            qty: if row.is_revenue() {",
    cmd: ["test", "-p", "findb", "--lib", "profit_sheet::tests::利润分析表按行取数并算出净利润"],
  },
  {
    // 全对上时也造凭证：一分钱差额都没有却凭空多一张调整凭证，账面就被改坏了。
    name: "㊾ 全部对上时也造一张阴阳凭证",
    file: "crates/findb/src/arap_recon.rs",
    from: "    if need.is_empty() {",
    to: "    if false && need.is_empty() {",
    cmd: ["test", "-p", "findb", "--lib", "arap_recon::tests::全部对上时不生成凭证也照样确认"],
  },
  {
    // 应付差额也往借方调：付钱的一方被记成收钱，应付账款越调越离谱。
    name: "㊾ 应付差额往借方调",
    file: "crates/findb/src/arap_recon.rs",
    from: "        let on_debit = if ar { d.is_positive() } else { d.is_negative() };",
    to: "        let on_debit = d.is_positive();",
    cmd: ["test", "-p", "findb", "--lib", "arap_recon::tests::应付差额往贷方调"],
  },
  {
    // 按账面列名单把已录的户也覆盖掉：用户辛苦敲的对方金额被账面数冲平，差额全变 0。
    name: "㊾ 按账面列名单覆盖用户已录的数",
    file: "crates/findb/src/arap_recon.rs",
    from: "        if have.contains(&code) || bal.is_zero() {",
    to: "        if bal.is_zero() {",
    cmd: ["test", "-p", "findb", "--lib", "arap_recon::tests::按账面列名单只补缺的户"],
  },
  {
    // 往来期初的旧账不计入账面：系统上线前的欠款一律对不上，逼着人整个月手工找。
    name: "㊾ 对账不算往来期初的老账",
    file: "crates/findb/src/arap_recon.rs",
    from: "        *out.entry(o.party_code.clone()).or_insert(Money::ZERO) += o.amount;",
    to: "        let _ = o;",
    cmd: ["test", "-p", "findb", "--lib", "arap_recon::tests::往来期初的旧账也计入账面余额"],
  },
  {
    // 已确认的批次还能改：账面数和已生成的调整凭证就对不上了。
    name: "㊾ 已确认的批次还能接着改",
    file: "crates/findb/src/arap_recon.rs",
    from: "    if b.status != ST_OPEN {",
    to: "    if false && b.status != ST_OPEN {",
    cmd: ["test", "-p", "findb", "--lib", "arap_recon::tests::已确认的批次不许改数"],
  },
  {
    // 上月没账也硬做同比：新账套第一个月会显示一堆 0%，被读成「和上月持平」。
    name: "㊼ 简报上月没账也硬做同比",
    file: "crates/findb/src/brief.rs",
    from: "    let has_prev = prev_has;",
    to: "    let has_prev = true;",
    cmd: ["test", "-p", "findb", "--lib", "brief::tests::上个月没有账时同比显示空"],
  },
  {
    // 应收取成贷方：欠我们的钱显示成负的，看着像我们欠别人。
    name: "㊼ 简报的应收账款正负颠倒",
    file: "crates/findb/src/brief.rs",
    from: "    let receivable = b(\"1122\");",
    to: "    let receivable = b(\"1122\").negated();",
    cmd: ["test", "-p", "findb", "--lib", "brief::tests::简报把一个月拆成"],
  },
  {
    // 百分比少乘 100：翻倍显示成 +1.00%。
    name: "㊼ 简报同比百分比少乘一百",
    file: "crates/findb/src/brief.rs",
    from: "                    .checked_mul(100i64)",
    to: "                    .checked_mul(1i64)",
    cmd: ["test", "-p", "findb", "--lib", "brief::tests::简报把一个月拆成"],
  },
  {
    // 分析不按原因分组：所有单据并进一桶，等于没按原因汇总。
    name: "㊺ 返工分析不按原因分组（全并一桶）",
    file: "crates/findb/src/rework.rs",
    from: "        let rk = s.reason_code.trim().to_string();",
    to: "        let rk = String::new();",
    cmd: ["test", "-p", "findb", "--lib", "rework::tests::返工成本按原因与部门分组"],
  },
  {
    // 占比分母取行内金额：每行都是 100%，看不出谁占大头。
    name: "㊺ 返工占比分母取行内金额（永远 100%）",
    file: "crates/findb/src/rework.rs",
    from: "            .checked_div(grand)",
    to: "            .checked_div(acc.total)",
    cmd: ["test", "-p", "findb", "--lib", "rework::tests::返工成本按原因与部门分组"],
  },
  {
    // 未分类/未指定被丢掉：各分组合计不等于总合计，看上去像算错了。
    name: "㊺ 未分类的返工单在分析里被丢掉",
    file: "crates/findb/src/rework.rs",
    from: "        let rk = s.reason_code.trim().to_string();",
    to: "        if s.reason_code.trim().is_empty() {\n            continue;\n        }\n        let rk = s.reason_code.trim().to_string();",
    cmd: ["test", "-p", "findb", "--lib", "rework::tests::返工成本按原因与部门分组"],
  },
  {
    // 返工数量不校验原单计划量：凭空冒出一批返工件，成本无处对账。
    name: "㊹ 返工数量不校验原单计划量",
    file: "crates/findb/src/rework.rs",
    from: "    if o.qty > po.planned_qty {",
    to: "    if false {",
    cmd: ["test", "-p", "findb", "--lib", "rework::tests::返工单必须挂来源订单"],
  },
  {
    // 结案后还能追加成本：上个月看过的报表会变样。
    name: "㊹ 已结案仍可追加返工成本",
    file: "crates/findb/src/rework.rs",
    from: "    if order.is_closed() {\n        return Err(blocked(\n            \"返工单已结案，不能再追加成本（要补先把状态改回「进行中」）\",\n        ));\n    }",
    to: "    if false {\n        return Err(blocked(\"返工单已结案\"));\n    }",
    cmd: ["test", "-p", "findb", "--lib", "rework::tests::已结案的返工单不许改也不许删"],
  },
  {
    // 结账后还能改返工单：与工资表那道闸同一个理由，账已经报出去了。
    name: "㊹ 结账后仍可改返工单",
    file: "crates/findb/src/rework.rs",
    // 只改判断本身（整块替换会让 upto 变成死变量、编译不过，变异就成了"没跑"）
    from: "        if period <= upto {",
    to: "        if period <= upto && false {",
    cmd: ["test", "-p", "findb", "--lib", "rework::tests::结账后的返工单不许改"],
  },
  {
    // 占比的分子用返工自身成本当分母：占比永远 100%，等于没算。
    name: "㊹ 返工占比分母取返工自身",
    file: "crates/findb/src/rework.rs",
    from: "        let po_cost = crate::manufacturing::get_prod_total_cost(db, o.po_id)?;",
    to: "        let po_cost = total;",
    cmd: ["test", "-p", "findb", "--lib", "rework::tests::返工成本三段汇总"],
  },
  {
    // 已结案的返工单也允许删：成本明细跟着没了，历史对不上。
    name: "㊹ 已结案也能删返工单",
    file: "crates/findb/src/rework.rs",
    from: "    if old.is_closed() {\n        return Err(blocked(\"返工单已结案，要删先改回「进行中」\"));\n    }",
    to: "    if false {\n        return Err(blocked(\"返工单已结案\"));\n    }",
    cmd: ["test", "-p", "findb", "--lib", "rework::tests::已结案的返工单不许改也不许删"],
  },
  {
    // 报工流水记成"今天所在期间"：账套期间与今天不一致时，工时分摊永远分不到。
    name: "㊸ 报工流水不跟订单期间",
    file: "crates/findb/src/advanced.rs",
    from: "            if info.7 > 0 {\n                info.7\n            } else {\n                Period::from_date(today).ymm()\n            },",
    to: "            Period::from_date(today).ymm(),",
    cmd: ["test", "-p", "finweb", "--test", "api", "--", "overhead_by_hours_takes_pool_from_account"],
  },
  {
    // 漏做月末「转出未交增值税」时不提醒：222101xx 三个明细挂着余额、
    // 222102 是 0，下月缴税无处可冲，而申报表看着完全正常。
    name: "㊺ 申报体检不提醒漏做转出未交增值税",
    file: "crates/findb/src/taxprecheck.rs",
    from: "    if form.payable > Money::ZERO && carry_out.is_zero() {",
    to: "    if false {",
    cmd: ["test", "-p", "findb", "--lib", "taxprecheck::tests::matched_voucher_passes_but_missing_transfer_is_warned"],
  },
  {
    // 未认证的进项票被当成不存在：会计以为「票录了就能抵」，
    // 申报表里却没有这张票的税额，月底对不上时已经过了申报期。
    name: "㊻ 申报体检不点名未认证的进项票",
    file: "crates/findb/src/taxprecheck.rs",
    from: "            i.kind == \"in\"\n                && i.status != \"verified\"\n                && i.status != \"rejected\"\n                && in_month(i.date.as_str(), first, last)",
    to: "            i.kind == \"in\" && false",
    cmd: ["test", "-p", "findb", "--lib", "taxprecheck::tests::unverified_input_invoice_is_warned_not_errored"],
  },
  {
    // 销项账票不符从「错误」降级成「提醒」：差了 130 元销项也照样放行申报，
    // 而这正是「票录了、凭证没记」的典型信号。
    name: "㊼ 销项账票不符只当提醒不报错",
    file: "crates/findb/src/taxprecheck.rs",
    from: "                    Level::Error,\n                    \"output-tax-vs-ledger\",",
    to: "                    Level::Warn,\n                    \"output-tax-vs-ledger\",",
    cmd: ["test", "-p", "findb", "--lib", "taxprecheck::tests::invoice_without_voucher_is_error"],
  },
  {
    // 结转时跳过进项：可抵的税被计进成本，成本虚高、税白交
    name: "㊽ 抄表结转漏掉进项税",
    file: "crates/findb/src/meter.rs",
    from: "    if tax.is_positive() {\n        v.push_entry(Entry {\n            debit: tax,",
    to: "    if false {\n        v.push_entry(Entry {\n            debit: tax,",
    cmd: ["test", "-p", "findb", "--lib", "meter::tests::close_books_recharge_to_cost"],
  },
  {
    // 按权限而不是「用过没用过」判定业务域：小厂老板通常只有一个全权限账号，
    // 按 Perm 过滤等于什么都不过滤，首屏又回到「生产在产 0 份」那一堆空卡。
    name: "㊿ 工作台按权限判定业务域（等于没过滤）",
    file: "crates/findb/src/workbench.rs",
    from: "    let mut out = Vec::new();\n    for (domain, tables) in DOMAIN_TABLES {",
    to: "    let mut out: Vec<String> = OPTIONAL_DOMAINS.iter().map(|d| d.to_string()).collect();\n    for (domain, tables) in DOMAIN_TABLES {",
    cmd: ["test", "-p", "findb", "--lib", "workbench::tests::active_domains_only_lists_used_modules"],
  },
  {
    // 只看本期有没有单据：这个月没开销售单，整块就消失；下个月开了一张又冒出来。
    // 同一块区域时有时无，比一直显示更让人糊涂。
    name: "㊿ 业务域只看本期有没有单据",
    file: "crates/findb/src/workbench.rs",
    from: "            let sql = format!(\"SELECT COUNT(*) FROM \\\"{t}\\\" LIMIT 1\");",
    to: "            let sql = format!(\"SELECT COUNT(*) FROM \\\"{t}\\\" WHERE period=?1 LIMIT 1\");\n            let _ = crate::periods::current_period(db);",
    cmd: ["test", "-p", "findb", "--lib", "workbench::tests::active_domains_only_lists_used_modules"],
  },
  {
    // 已结转的台账行允许删除：凭证还在，删了就是账证不符
    name: "㊾ 已结转的台账行允许删除",
    file: "crates/findb/src/meter.rs",
    from: "    if r.is_closed() {\n        return Err(FinError::msg(format!(",
    to: "    if false {\n        return Err(FinError::msg(format!(",
    cmd: ["test", "-p", "findb", "--lib", "meter::tests::close_books_recharge_to_cost"],
  },
  {
    // 认领不设上限：一笔 600 的收款能认掉 800 的挂账，钱凭空多出来
    name: "㊿ 收款认领不检查未认额上限",
    file: "crates/findb/src/claim.rs",
    from: "    if total > open {",
    to: "    if false {",
    cmd: ["test", "-p", "findb", "--lib", "claim::tests::cannot_over_claim"],
  },
  {
    // 把借方挂账当成收款：认领工作台会把「还欠着的单」列成「收到的钱」
    name: "51 认领工作台把借方挂账当收款",
    file: "crates/findb/src/claim.rs",
    from: ".filter(|e| e.credit > Money::ZERO && e.open() > Money::ZERO)",
    to: ".filter(|e| e.debit > Money::ZERO && e.open() > Money::ZERO)",
    cmd: ["test", "-p", "findb", "--lib", "claim::tests::claim_splits_one_receipt_across_customers"],
  },
  {
    // 建议不再优先同一客户：把「这笔钱属于他」的常识丢掉，先按日期乱点鸳鸯
    name: "52 认领建议不优先同一客户",
    file: "crates/findb/src/claim.rs",
    from: "    let mut same: Vec<&Candidate> = cands.iter().filter(|c| c.party == inflow.party).collect();",
    to: "    let mut same: Vec<&Candidate> = cands.iter().filter(|c| c.party != inflow.party).collect();",
    cmd: ["test", "-p", "findb", "--lib", "claim::tests::claim_splits_one_receipt_across_customers"],
  },
  // v1.8.83 往来期初明细「修改」：改一行老账最容易悄悄放宽口径，
  // 这四条盯着「改完不校验」「改完把原始录入人冲掉」「改完撞了单据号」「谁都能改」。
  {
    // 允许零/负数金额：和 CSV 导入口径分家，影子挂账会出现负数欠款
    name: "53 修改往来期初不校验金额正数",
    file: "crates/findb/src/settle/opening.rs",
    from: "    if !o.amount.is_positive() {",
    to: "    if false {",
    cmd: FSETTLE("arap_opening_update_validates_and_keeps_creator"),
  },
  {
    // 把原始录入人一并覆盖：审计痕迹被 UPDATE 冲掉，看不出这行原本是谁导进来的。
    // 落点必须在 findb 的 UPDATE SQL —— handler 里传 created_by 是没用的（SQL 不写这列，
    // 改 handler 根本破坏不了数据，测试会假阳性放过）。findb 单测里 o.created_by 是空串，
    // 一旦把它加进 SET，断言「仍是 importer」立刻红。
    name: "54 修改往来期初冲掉原始录入人",
    file: "crates/findb/src/settle/opening.rs",
    from: "    let n = db.conn().execute(\n        \"UPDATE arap_opening SET kind=?2,party_code=?3,party_name=?4,doc_no=?5,doc_date=?6,amount=?7,memo=?8 WHERE id=?1\",\n        rusqlite::params![\n            id,\n            o.kind,\n            o.party_code,\n            o.party_name,\n            o.doc_no,\n            o.doc_date,\n            crate::exact_param(o.amount),\n            o.memo\n        ],\n    )?;",
    to: "    let n = db.conn().execute(\n        \"UPDATE arap_opening SET kind=?2,party_code=?3,party_name=?4,doc_no=?5,doc_date=?6,amount=?7,memo=?8,created_by=?9 WHERE id=?1\",\n        rusqlite::params![\n            id,\n            o.kind,\n            o.party_code,\n            o.party_name,\n            o.doc_no,\n            o.doc_date,\n            crate::exact_param(o.amount),\n            o.memo,\n            o.created_by\n        ],\n    )?;",
    cmd: FSETTLE("arap_opening_update_validates_and_keeps_creator"),
  },
  {
    // 允许单据号撞上另一行：下次 CSV 重导按三元组幂等跳过，会静悄悄漏一笔老账
    name: "55 修改往来期初允许单据号撞车",
    file: "crates/findb/src/settle/opening.rs",
    from: "    if dup > 0 {",
    to: "    if false {",
    cmd: FSETTLE("arap_opening_update_validates_and_keeps_creator"),
  },
  {
    // 只查不改权：viewer 也能改老账，等于绕开删除按钮的会计权限
    name: "56 修改往来期初不查会计权限",
    file: "crates/finweb/src/handlers/settle.rs",
    from: "    user.require(Perm::VoucherNew)?;\n    let db = &user.db;\n    // 先确认目标还在",
    to: "    let db = &user.db;\n    // 先确认目标还在",
    cmd: API("arap_opening_update_requires_voucher_new"),
  },
  {
    // 负数金额的提示讲错方向：应收行被教去填 1123 预付账款。
    // 账仍然是平的（这一行本来就被跳过），所以只有提示文案错 —— 用户照着做就把
    // 客户预交的钱记成「我先付给供应商」，科目和方向双错，事后极难发现。
    name: "57 往来期初导入负数提示讲错方向",
    file: "crates/findb/src/imports.rs",
    from: "            let tip = if kind == \"ar\" {",
    to: "            let tip = if kind == \"customer\" {",
    cmd: FIMPORT("import_arap_opening_negative_amount_tip_matches_direction"),
  },
  // v1.8.86 首屏「一句话结论」+「账有没有记错」：判定搬到 findb 给两端共用，
  // 失效形态是「话还在、但挑错了那一件」——用户照着做，先办的事是错的。
  {
    // 把「有凭证没记账」这一条漏掉：这句 PR 优先级最高的提醒一消失，老板就会
    // 拿着偏小的利润去做报价、发工资，而且看不出哪里不对。
    name: "58 首屏漏掉「没记账」这条最急的提醒",
    file: "crates/findb/src/verdict.rs",
    from: "    if f.unposted > 0 {",
    to: "    if false {",
    cmd: ["test", "-p", "findb", "--lib", "verdict::tests::有凭证没记账排在最前面"],
  },
  {
    // 净欠外面时不看应付：客户预交的钱（预收）也会被说成「欠供应商」，
    // 让老板去安排一笔根本不该付的款。
    name: "59 净欠外面不看应付款（预收被当成欠供应商）",
    file: "crates/findb/src/verdict.rs",
    from: "    if !f.net.is_zero() && f.net.is_negative() && !f.payable.is_zero() {",
    to: "    if !f.net.is_zero() && f.net.is_negative() {",
    cmd: ["test", "-p", "findb", "--lib", "verdict::tests::没有应付款就不算净欠外面"],
  },
  {
    // 账平不平只看期末：本期记错 + 期初恰好反向，期末看着就是平的，
    // 于是首页说「✔ 账是平的」，用户放心出报表。
    name: "60 账平不平只看期末（本期错账看不见）",
    file: "crates/findb/src/verdict.rs",
    from: "        balanced: tb.is_balanced(),",
    to: "        balanced: tb.end_balanced(),",
    cmd: ["test", "-p", "findb", "--lib", "verdict::tests::期末看着平但本期记错也不能算平"],
  },
  {
    // 差额报错地方：本期不平却拿期初的差额去报，用户按这个数一查就对不上。
    name: "61 账不平的差额报错那一处",
    file: "crates/findb/src/verdict.rs",
    from: "    } else if !period_diff.is_zero() {",
    to: "    } else if false {",
    cmd: ["test", "-p", "findb", "--lib", "verdict::tests::期末看着平但本期记错也不能算平"],
  },
  // v1.8.87 加工费多口径计价：一个工件同时按 只 / 米 / kg / ㎡ 报价。
  // 失效形态基本都是「数字算错了但看着挺像」—— 单子照样出，钱就是错的。
  {
    // 折算不按口径位数抹平：300 只 × 0.0234 = 7.019999…，单上写 7.02，
    // 客户拿计算器按一遍是 68.75 而不是 68.80，一句「你怎么算的」说不清。
    name: "62 折算不按口径位数抹平（乘出来带一长串小数）",
    file: "crates/findb/src/processfee.rs",
    // 带上一行函数签名：只写这一行的话，quote_with_units 里缩进更深的同样一行
    // 也包含它，replace 会先命中那里，变异成了空操作（shard 5 曾因此判「放过」）。
    from: "pub fn convert(qty: Money, c: &CalcUnit) -> Money {\n    (qty * c.factor).round_dp(c.scale.clamp(0, SCALE_MAX) as u32)",
    to: "pub fn convert(qty: Money, c: &CalcUnit) -> Money {\n    qty * c.factor",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::折算先按位数抹平再乘单价"],
  },
  {
    // 报价不认口径：12 元/㎡ 的价拿去乘「300 只」，等于把面积价按只算，
    // 一张 300 只的加工单能多算几百倍，而且单子上看不出错。
    name: "63 报价不认口径（面积价乘只数）",
    file: "crates/findb/src/processfee.rs",
    from: "        if !want_unit.is_empty() && r.unit.trim() != want_unit {",
    to: "        if false {",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::单价必须同口径才认"],
  },
  {
    // 只有未来价时干脆说「没有规则」：老板明明录过价，系统说没录，
    // 于是他去重录一遍，两条价并存，下次调价更乱。
    name: "64 只录了未来价就说「没有规则」",
    file: "crates/findb/src/processfee.rs",
    from: "            return Ok(Some((r.clone(), true)));",
    to: "            return Ok(None);",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::只有未来价时也报价但标明未生效"],
  },
  {
    // 同精度不比生效日：提前录的下一版价永远排在后面，到了生效日也不会自动换过来，
    // 「提前录价」这个功能等于没做。
    name: "65 同精度不比生效日（新价永远轮不到）",
    file: "crates/findb/src/processfee.rs",
    // 把「比生效日」改成跟它自己比（永远相等），于是同精度退回按建的先后 ——
    // 先录的旧价赢，新价永远轮不到。
    from: "            b.effective_from",
    to: "            a.effective_from",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::同精度取生效日更晚的那条"],
  },
  {
    // 生效日格式不校验：2026-3-1 这种也能存进去，而判定是字符串比较，
    // 「2026-3-1」永远小于「2026-03-01」，那条价可能一辈子不生效。
    name: "66 生效日期格式不校验（2026-3-1 混进来）",
    file: "crates/findb/src/processfee.rs",
    from: '        .map(|d| d.format("%Y-%m-%d").to_string() == effective_from)',
    to: "        .map(|_| true)",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::同精度取生效日更晚的那条"],
  },
  {
    // 矩阵不看客户：给 A 客户谈的专属价会显示在 B 客户那一列，
    // 照着报价就是拿别人的底价去谈。
    name: "67 单价矩阵不分客户（别人的专属价漏出来）",
    file: "crates/findb/src/processfee.rs",
    from: '                if let Some(r) = best_for_cell(pick(want_cust, process), today) {',
    to: '                if let Some(r) = best_for_cell(pick("", process), today) {',
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::矩阵按物料行_口径列摆开"],
  },
  {
    // 系数不校验正数：填 0 或者负数，折算出来是 0 或负数，
    // 报价变成 0 元或倒找钱，单子照样能存。
    name: "68 折算系数允许 0 或负数",
    file: "crates/findb/src/processfee.rs",
    from: "    if !c.factor.is_positive() {",
    to: "    if false {",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::系数与口径的字段校验"],
  },

  // ── v1.8.88 批量导入 ──────────────────────────────────────────────────
  {
    // 表头当数据行：客户从金蝶/用友导出的第一行就是「物料编码,客户编码,…」，
    // 认不出来就整行报「单价不是数字」，导入结果里凭空多一条跳过，用户会以为文件坏了。
    name: "69 导入时不认表头行（把列名当数据）",
    file: "crates/findb/src/processfee.rs",
    from: "    hits >= 2\n}\n",
    to: "    hits >= 99\n}\n",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::导入单价_表头自动跳过"],
  },
  {
    // 同一格变成新增一条：客户发来一张新价表重导一遍，价目从 40 条涨到 80 条，
    // 矩阵里同一个物料同一口径出现两行，报价按「生效日更晚的赢」看起来还是对的，
    // 于是没人发现底下埋了一份重复价。
    name: "70 导入同一格不覆盖（价目表越导越臃肿）",
    file: "crates/findb/src/processfee.rs",
    from: "            id: exist.unwrap_or(0),\n            name: if name.is_empty() { auto_name } else { name },",
    to: "            id: 0,\n            name: if name.is_empty() { auto_name } else { name },",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::导入单价_覆盖同一格"],
  },
  {
    // 导入系数时不查已有的那一条：同一「物料 + 口径」重导会报「新增 1 条」而不是
    // 「覆盖 1 条」。写入本身是 upsert（`calc_unit_save` 自己会找），所以数据不错，
    // 错的是给用户看的那个数 —— 会计据此判断「这次导进去几条」，报错了就会以为漏了。
    name: "71 导入系数不查已有（覆盖被报成新增）",
    file: "crates/findb/src/processfee.rs",
    from: "                \"SELECT id FROM item_calc_unit WHERE item_code=?1 AND calc_unit=?2\",",
    to: "                \"SELECT id FROM item_calc_unit WHERE 0\",",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::导入系数_覆盖同物料同口径"],
  },
  {
    // 警告不带行号：一次导 300 行、跳过 12 行，用户拿着「单价不是数字」这句话
    // 去 Excel 里找，等于大海捞针。
    name: "72 导入警告不带行号（用户找不到是哪一行）",
    file: "crates/findb/src/processfee.rs",
    from: "                res.warnings.push(format!(\n                    \"第 {line_no} 行：单价「{price_s}」不是数字，这一行导不了\"\n                ));",
    to: "                res.warnings.push(format!(\n                    \"单价「{price_s}」不是数字，这一行导不了\"\n                ));",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::导入单价_坏行只跳过那一行"],
  },

  // ── v1.8.89 按规格算单价 ──────────────────────────────────────────────
  {
    // 只有一维也硬算：客户写「1200」本意可能是 1200*80 只写了长度，也可能是一根
    // 1200 长的管。猜成「1200 × 1」会给出一个看着正常、实际差几十倍的价。
    name: "73 规格看不懂也照算（缺一维就按 1 补）",
    file: "crates/findb/src/processfee.rs",
    from: "    if out.len() < 2 {\n        return None;\n    }\n    Some(out)",
    to: "    if out.len() < 2 {\n        out.push(Money::ONE);\n    }\n    Some(out)",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::规格写法宽容但看不懂就不猜"],
  },
  {
    // 手录系数被规格盖掉：老板特意为某个件录了「1 只 = 0.0234 ㎡」（实测值），
    // 系统却按规格重算，把人工核对过的数顶掉 —— 这种「帮忙」最招人烦。
    name: "74 手录系数被规格盖掉",
    file: "crates/findb/src/processfee.rs",
    from: "        Some(cu) if cu.factor.is_positive() => (cu.factor, cu.scale, \"manual\", String::new()),",
    to: "        Some(_cu) if false => (Money::ZERO, 2, \"manual\", String::new()),",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::手录系数优先于规格"],
  },
  {
    // 折合量只留 2 位：0.3125 ㎡ 的板显示成 0.31，而金额仍按 0.3125 算 ——
    // 单子上「0.31 × 80 = 25.00」自己就对不上算盘，客户当场就要怀疑。
    name: "75 折合量只显示 2 位（单子自己乘不上）",
    file: "crates/finweb/src/handlers/processfee.rs",
    from: "                \"factor\": sq.factor.fmt_dp(4),",
    to: "                \"factor\": sq.factor.fmt_money(),",
    cmd: API("processfee::spec_turns_a_rate_into_a_piece_price"),
  },
  {
    // 按重量算不看密度：密度为 0 时折合量是 0，报价变 0 元，单子看着正常。
    name: "76 按重量算不看密度（密度 0 也照算）",
    file: "crates/findb/src/processfee.rs",
    from: "            if n < SPEC_MAX_DIM || !density.is_positive() {",
    to: "            if n < SPEC_MAX_DIM {",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::规格算重量要密度"],
  },

  // ── v1.8.90 工艺档（表面处理分档） ────────────────────────────────────
  {
    // 档位不参与匹配：硬质的 18 元会被普通氧化的 12 元顶掉（或反过来），
    // 客户拿普通氧化的单子来对账，一句「系统报的」就说不清。
    name: "77 工艺档不参与匹配（各档互相串价）",
    file: "crates/findb/src/processfee.rs",
    from: "        } else if r.process.trim().is_empty() && !want_proc.is_empty() {\n            1u8\n        } else {\n            continue;\n        };",
    to: "        } else if r.process.trim().is_empty() && !want_proc.is_empty() {\n            1u8\n        } else {\n            0u8\n        };",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::工艺档不同价不同"],
  },
  {
    // 没选档位也抓档位价：开单时忘了填「硬质氧化」，系统随手抓一条档位价报出去。
    name: "78 没选档位也抓档位价",
    file: "crates/findb/src/processfee.rs",
    from: "        } else if r.process.trim().is_empty() && !want_proc.is_empty() {\n            1u8\n        } else {",
    to: "        } else if true {\n            1u8\n        } else {",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::工艺档不同价不同"],
  },
  {
    // 矩阵不回退通用价：这一档没单独定价就显示「未定价」，可开单时明明按通用价报得出来 ——
    // 界面说没有、开单能报，是最让人不敢用的一种不一致。
    name: "79 矩阵不回退通用价（界面说没价、开单却有价）",
    file: "crates/findb/src/processfee.rs",
    from: "                if process.is_empty() {\n                    return None;\n                }",
    to: "                if true {\n                    return None;\n                }",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::矩阵按档显示并标出借来的通用价"],
  },
  {
    // 导入身份漏工艺档：先导普通氧化、再导硬质氧化，第二条把第一条覆盖掉 ——
    // 一份新表能悄悄删价，比导不进来危险得多。
    name: "80 导入身份漏工艺档（导新档把旧档覆盖掉）",
    file: "crates/findb/src/processfee.rs",
    from: "                 AND unit=?3 AND effective_from=?4 AND process=?5",
    to: "                 AND unit=?3 AND effective_from=?4 AND (process=?5 OR 1=1)",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::导入能带工艺档且同档重导算覆盖"],
  },

  // ── v1.8.91 按规格分档定价 ────────────────────────────────────────────
  {
    // 上限判成「不含」：「200cm 以下」的 200 正好掉进两档中间的缝里，报价直接报「没价」
    name: "81 区间上限判成不含（200 掉进两档中间的缝）",
    file: "crates/findb/src/processfee.rs",
    from: "        if v > to {",
    to: "        if v >= to {",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::按规格分档_短件按件长料按米"],
  },
  {
    // 没给规格也拿分档价：300cm 那条 1.5 元/米会被乘到 80cm 的活儿上，差十几倍
    name: "82 没给规格也拿分档价",
    file: "crates/findb/src/processfee.rs",
    from: "            match dims {\n                Some(d) if band_ok(&r, d) => 0u8,\n                _ => continue,\n            }",
    to: "            match dims {\n                Some(d) if band_ok(&r, d) => 0u8,\n                _ => 0u8,\n            }",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::没给规格就不拿分档价"],
  },
  {
    // 分档价混进矩阵：一格是「物料×口径」，装不下「按件」和「按米」两条，看着像重复录入
    name: "83 分档价混进单价矩阵",
    file: "crates/findb/src/processfee.rs",
    from: "        .filter(|r| {\n            r.band_dim.trim().is_empty()\n                && r.qty_from.trim().is_empty()\n                && r.qty_to.trim().is_empty()\n        })",
    to: "        .filter(|_r| true)",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::分档价不进单价矩阵"],
  },
  {
    // 口径不由规格档决定：又变成「先自己选按件还是按米」，短件长料混着报时必选错
    name: "84 口径不由规格档决定（还得人先选）",
    file: "crates/findb/src/processfee.rs",
    from: "    if unit.is_empty() {\n        let dims = dims_opt.as_ref().ok_or_else(|| {",
    to: "    if false {\n        let dims = dims_opt.as_ref().ok_or_else(|| {",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::按规格分档_短件按件长料按米"],
  },

  // ── v1.8.92 按数量分档定价 ────────────────────────────────────────────
  {
    // 数量档不看数量：20 件的小单也拿「500 件以上」的批发价报出去，
    // 客户拿到单子先高兴，做完一算账才发现亏在人工上
    name: "85 数量档不看数量（批发价报给小单）",
    file: "crates/findb/src/processfee.rs",
    from: "        let qty_rank = match qty_rank_of(&r, qty) {\n            Some(k) => k,\n            None => continue,\n        };",
    to: "        let qty_rank = 1u8;",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::按数量分档_批发价不落到小单上"],
  },
  {
    // 数量下限也算命中：正好 500 件两条都中，落到哪条看谁先建
    name: "86 数量下限也算命中（500 件两条都中）",
    file: "crates/findb/src/processfee.rs",
    from: "        if q <= lo {\n            return None;\n        }",
    to: "        if q < lo {\n            return None;\n        }",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::按数量分档_批发价不落到小单上"],
  },
  {
    // 数量档没排除出矩阵：一格装两条不同批量的价，看着像重复录入
    name: "87 数量档混进单价矩阵",
    file: "crates/findb/src/processfee.rs",
    from: "                && r.qty_from.trim().is_empty()\n                && r.qty_to.trim().is_empty()",
    to: "                && true\n                && true",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::数量档不进矩阵但要列在分段表里"],
  },
  {
    // 拿库存数量而不是计价数量挑档：600 只（14 ㎡）被按「500 以上」算
    name: "88 数量档看库存数量而不是计价数量",
    file: "crates/findb/src/processfee.rs",
    from: "    let (rule, _future) = match_rule_full(db, item, customer, unit, process, None, Some(calc_qty))?",
    to: "    let (rule, _future) = match_rule_full(db, item, customer, unit, process, None, Some(stock_qty))?",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::数量档按折算后的计价数量判"],
  },
  {
    name: "89 调价不记改前值（只留一个改后的数）",
    file: "crates/findb/src/processfee.rs",
    from: "        .map(|(p, _, _)| Money::parse_or_zero(p).fmt_money())",
    to: "        .map(|_| String::new())",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::调价留下改前改后和是谁改的"],
  },
  {
    name: "90 改个备注也记一条改价（历史表变噪声）",
    file: "crates/findb/src/processfee.rs",
    from: "            let changed =\n                Money::parse_or_zero(p) != r.unit_price || Money::parse_or_zero(m) != r.min_charge;",
    to: "            let changed = true;",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::只改备注不动价就不留历史"],
  },
  {
    name: "91 删除不留痕（说是删了、查不出删的是哪个价）",
    file: "crates/findb/src/processfee.rs",
    from: "                \"删除\",\n                id,",
    to: "                \"停用\",\n                id,",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::停用启用和删除都留痕"],
  },
  {
    name: "92 到点提醒不看日期窗口（下个月的价也来占首页）",
    file: "crates/findb/src/processfee.rs",
    from: "d <= last.as_str()",
    to: "d <= \"9999-12-31\"",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::到点提醒只报真会生效的那几条"],
  },
  {
    name: "94 对价校验不乘折算系数（12 元/㎡ 和 0.15 元/件 直接比）",
    file: "crates/findb/src/processfee.rs",
    from: "let per = (rule.unit_price * cu.factor).round2();",
    to: "let per = rule.unit_price;",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::报价单的价和价目表对不上的时候要说清楚"],
  },
  {
    name: "95 口径对不上也硬减出一个差额",
    file: "crates/findb/src/processfee.rs",
    from: 'None => (rule.unit_price, None, "unit_unknown"),',
    to: 'None => (rule.unit_price, None, "mismatch"),',
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::报价单的价和价目表对不上的时候要说清楚"],
  },
  {
    name: "96 没有价目表却说「和价目表一致」",
    file: "crates/findb/src/processfee.rs",
    from: '        kind: "no_rule",',
    to: '        kind: "ok",',
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::报价单的价和价目表对不上的时候要说清楚"],
  },
  {
    name: "97 报价单单价不摊回库存单位（按 ㎡ 的价直接当每件价）",
    file: "crates/findb/src/processfee.rs",
    from: "    let unit_price = (q\n        .total\n        .checked_div(qty)\n        .ok_or_else(|| FinError::msg(\"数量不能为 0\"))?)\n    .round_dp(4);",
    to: "    let unit_price = q.total;",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::报价单一行按规格折出每件单价"],
  },
  {
    name: "98 报价单不看规格（规格分档白配）",
    file: "crates/findb/src/processfee.rs",
    from: "    let (q, calc_qty, calc_unit_out, factor, basis) = if !spec.is_empty() {",
    to: "    let (q, calc_qty, calc_unit_out, factor, basis) = if false {",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::报价单一行按规格挑档"],
  },
  {
    name: "99 没填单价也不按价目表算（存进去是 0）",
    file: "crates/finweb/src/handlers/sales.rs",
    from: "    if req.unit_price.is_zero() && (!req.spec.trim().is_empty() || !req.calc_unit.trim().is_empty())\n    {",
    to: "    if false {",
    cmd: ["test", "-p", "finweb", "--test", "api", "quotation_price_comes_from_price_list_and_stays"],
  },
  {
    name: "100 报价单不留折算快照（客户问『这价怎么来的』答不出）",
    file: "crates/finweb/src/handlers/sales.rs",
    from: "        req.calc_qty = p.calc_qty.fmt_qty();",
    to: "        req.calc_qty = String::new();",
    cmd: ["test", "-p", "finweb", "--test", "api", "quotation_price_comes_from_price_list_and_stays"],
  },
  // v1.8.97 批量调价：四个错法，用户都会当场踩到
  {
    name: "101 批量调价不按格分组（不同物料的价挤成一条）",
    file: "crates/findb/src/processfee.rs",
    from: "        groups.entry(slot_key(r)).or_default().push(r);",
    to: "        groups.entry(String::new()).or_default().push(r);",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::批量调价只动现在生效的那一版"],
  },
  {
    name: "102 批量调价涨跌算反（涨 3% 变成降 3%）",
    file: "crates/findb/src/processfee.rs",
    from: "            let k = (hundred + spec.value).checked_div(hundred)?;",
    to: "            let k = (hundred - spec.value).checked_div(hundred)?;",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::批量调价同一天重跑不叠加"],
  },
  {
    name: "103 同一天重跑又新建一条（价目表越调越臃肿）",
    file: "crates/findb/src/processfee.rs",
    from: "            rule_id: same_day.map(|r| r.id).unwrap_or(0),",
    to: "            rule_id: 0,",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::批量调价同一天重跑不叠加"],
  },
  {
    name: "104 调完不到 0 也照写（负数价进价目表）",
    file: "crates/findb/src/processfee.rs",
    from: "    if new.is_positive() {\n        Some(new)\n    } else {\n        None\n    }",
    to: "    Some(new)",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::批量调价跳过算成零或负数的"],
  },
  {
    name: "93 到点提醒把停用的价也报上来",
    file: "crates/findb/src/processfee.rs",
    from: "!d.is_empty() && !r.disabled && d >= today.as_str()",
    to: "!d.is_empty() && d >= today.as_str()",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::到点提醒只报真会生效的那几条"],
  },
  // v1.8.98 价目表导出：两个错法，客户拿去会看到「该停的价」或「该换的旧价」
  {
    name: "105 导出把停用的价也带出来",
    file: "crates/findb/src/processfee.rs",
    from: "        if r.disabled {\n            continue;\n        }\n        if !want_cust.is_empty() && r.customer_code.trim() != want_cust {\n            continue;\n        }\n        by_slot.entry(slot_key(r)).or_default().push(r);",
    to: "        if false {\n            continue;\n        }\n        if !want_cust.is_empty() && r.customer_code.trim() != want_cust {\n            continue;\n        }\n        by_slot.entry(slot_key(r)).or_default().push(r);",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::导出只给每格当前生效的那一版并排除停用"],
  },
  {
    name: "106 导出把历史价当现价（挑到最早那版）",
    file: "crates/findb/src/processfee.rs",
    from: "        .filter_map(|pool| best_for_cell(pool, &today).cloned())",
    to: "        .filter_map(|pool| pool.into_iter().min_by_key(|r| r.effective_from.clone()).cloned())",
    cmd: ["test", "-p", "findb", "--lib", "processfee::tests::导出只给每格当前生效的那一版并排除停用"],
  },
  {
    name: "107 报价单打印漏掉规格（客户对不出尺寸）",
    file: "crates/findb/src/sales.rs",
    from: "    if !q.spec.trim().is_empty() {",
    to: "    if false {",
    cmd: ["test", "-p", "findb", "--lib", "sales::tests::quo_print_memo_summarizes_the_locked_snapshot"],
  },
  {
    name: "108 报价单打印漏掉折合说明（客户算不出单价）",
    file: "crates/findb/src/sales.rs",
    from: "    if !q.calc_qty.trim().is_empty() && !q.calc_unit.trim().is_empty() {",
    to: "    if false {",
    cmd: ["test", "-p", "findb", "--lib", "sales::tests::quo_print_memo_summarizes_the_locked_snapshot"],
  },
  {
    name: "109 报价单打印漏掉工艺档（普通/硬质分不清）",
    file: "crates/findb/src/sales.rs",
    from: "    if !q.process.trim().is_empty() {",
    to: "    if false {",
    cmd: ["test", "-p", "findb", "--lib", "sales::tests::quo_print_memo_summarizes_the_locked_snapshot"],
  },

];

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
// 可选过滤：`node tools/mutate-rust.js ⑱` 只跑名字里含该串的变异。
// 没有过滤参数时跑全部（CI 走这条）。调参时别为了验一条去等四十分钟。
const FILTER = (process.argv[2] || "").startsWith("--") ? "" : process.argv[2] || "";
if (FILTER && !MUT.some((m) => m.name.includes(FILTER))) {
  console.error(`没有名字含「${FILTER}」的变异`);
  process.exit(2);
}
// 分片：`node tools/mutate-rust.js --shard 2/4`。
// 58 条变异串行要跑近 40 分钟，发一次版等一次；切成 4 片并行能压到 10 分钟出头。
// 分片按**下标取模**切，保证每条恰好被一个分片跑到，不会漏也不会重。
// 环境变量 MUT_SHARD 也认（CI 里写环境变量比拼命令行更不容易被引号坑）。
const shardSpec = (() => {
  const i = process.argv.indexOf("--shard");
  if (i > 0 && process.argv[i + 1]) return process.argv[i + 1];
  return process.env.MUT_SHARD || "";
})();
const SHARD = (() => {
  if (!shardSpec) return null;
  const m = /^(\d+)\/(\d+)$/.exec(shardSpec.trim());
  if (!m) {
    console.error(`--shard 的格式应为 i/N（如 2/4），收到「${shardSpec}」`);
    process.exit(2);
  }
  const idx = Number(m[1]);
  const n = Number(m[2]);
  if (idx < 1 || n < 1 || idx > n) {
    console.error(`--shard 越界：${shardSpec}（i 必须落在 1..N）`);
    process.exit(2);
  }
  return { idx, n };
})();

const PICKED = FILTER ? MUT.filter((m) => m.name.includes(FILTER)) : MUT;
const RUN = SHARD
  ? PICKED.filter((_, i) => i % SHARD.n === SHARD.idx - 1)
  : PICKED;
if (RUN.length === 0) {
  // 空分片静默退出会被读成「全过了」，这比报错危险得多
  console.error(`本次没有变异可跑（过滤「${FILTER}」，分片 ${shardSpec || "无"}）—— 分片参数写错了？`);
  process.exit(2);
}
// --list 的输出必须是纯 JSON（check-ci 要 JSON.parse），别混进这行标题
if (SHARD && !process.argv.includes("--list")) {
  console.log(`分片 ${SHARD.idx}/${SHARD.n}：本次跑 ${RUN.length} 条（合计 ${PICKED.length} 条）\n`);
}
const LOCK = ".mutation-lock.json";
// 上次跑到一半被 Ctrl-C / CI 超时杀掉时，`finally` 不会执行，源码就**留着变异后的样子**。
// 2026-10 真的踩过一次：`save_begin` 的「科目存在性校验」被留成 `if false`，全量测试
// 里那条用例报了红才被发现。所以：跑之前先把原文写进锁文件，下次启动先恢复它。
function restoreFromLock() {
  if (!fs.existsSync(LOCK)) return;
  try {
    const saved = JSON.parse(fs.readFileSync(LOCK, "utf8"));
    for (const [f, b64] of Object.entries(saved)) {
      fs.writeFileSync(f, Buffer.from(b64, "base64"));
    }
    fs.unlinkSync(LOCK);
    console.log("!! 上次变异没跑完就退出了 —— 已按锁文件把源码还原\n");
  } catch (e) {
    console.error("!! 锁文件损坏，请先手工检查工作区：" + String(e.message || e));
    process.exit(2);
  }
}
if (!process.argv.includes("--list")) restoreFromLock();

const backups = new Map();
for (const m of RUN) if (!backups.has(m.file)) backups.set(m.file, fs.readFileSync(m.file, "utf8"));
function restoreAll() {
  for (const [f, s] of backups) fs.writeFileSync(f, s);
  if (fs.existsSync(LOCK)) fs.unlinkSync(LOCK);
}
if (!process.argv.includes("--list")) {
  const lock = {};
  for (const [f, s] of backups) lock[f] = Buffer.from(s, "utf8").toString("base64");
  fs.writeFileSync(LOCK, JSON.stringify(lock));
  // 被信号打断也要还原，否则工作区带着变异代码，而人以为只是"跑完了"
  for (const sig of ["SIGINT", "SIGTERM", "SIGHUP"]) {
    process.on(sig, () => {
      restoreAll();
      console.error(`\n收到 ${sig}，已还原源码`);
      process.exit(130);
    });
  }
}

// --list：只把变异清单打成 JSON，不执行。让 tools/check-ci.js 检查「清单是否已过期」
// （源码改了但 from 串对不上 = 这条变异形同虚设，而且会被读成「检查放过了」）。
//
// 为什么不让 check-ci.js 自己解析本文件：它得把 JS 字符串字面量再解一遍转义，
// 两层转义必错。让脚本自己导出真值，别在外部猜。
if (process.argv.includes("--list")) {
  // 带上分片时输出该分片的清单：这样「分片是否覆盖全、是否互斥」可以被机械验证，
  // 而不是靠肉眼看取模对不对。
  process.stdout.write(JSON.stringify({ cases: RUN }, null, 1));
  process.exit(0);
}

let bad = 0;
let notRun = 0;
try {
  for (const m of RUN) {
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
      // `cargo` 不在 PATH（常见于直接 `node tools/mutate-rust.js` 而没先 export
      // PATH）会让每条变异都报「测试根本没跑」，看着像 58 条全废——直接说清楚。
      if (e.code === "ENOENT" || /ENOENT/.test(String(e.message || ""))) {
        console.log(
          "!! 找不到 cargo —— 先 export PATH=\"$HOME/.cargo/bin:$PATH\"（或 CARGO_HOME 所在目录）再跑",
        );
        process.exit(2);
      }
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
  restoreAll();
}
console.log(
  bad === 0 && notRun === 0
    ? `\n全部 ${RUN.length} 个变异都被对应测试拦下，源码已还原`
    : `\n${bad} 个变异没被拦下 / 没生效（共 ${RUN.length} 个${notRun ? `，其中 ${notRun} 个是测试根本没跑` : ""}）`
);
// “测试没跑”不能算成功：源码改坏了但测试命令没匹配到任何用例时，
// 旧逻辑只累计 notRun，最终仍 exit 0，等于把变异测试变成摆设。
process.exit(bad === 0 && notRun === 0 ? 0 : 1);
