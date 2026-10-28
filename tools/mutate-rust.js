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
// 注意：这里曾经写成固定的 "user::tests::"（name 参数被丢掉），效果是「跑所有 user 测试」。
// 那样一条变异被别的用例顺手拦住，也会显示 PASS，看不出目标用例到底守没守住。
const CORE = (name) => ["test", "-p", "fincore", "--lib", `user::tests::${name}`];
// v1.8.21「导入导出闭环」的三个新 target。这类功能最典型的失效是
// 「界面点得动、文件也下来了，但数据其实没进去/没出来」，所以每条都要有变异守着。
const XLSX = (name) => ["test", "-p", "finweb", "--test", "export_xlsx", "--", name];
const STATIC = (name) => ["test", "-p", "finweb", "--test", "static_cache", "--", name];
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
const OBOOK = (name) => ["test", "-p", "findb", "--test", "opening_book", "--", name];
// v1.8.28 套打：凭证/账簿/单据也走账套里的纸张设置。失效形态是"设置对了但纸上没变"。
const FPFORM = (name) => ["test", "-p", "findb", "--lib", `printform::tests::${name}`];
// 建账/导入账套的参数校验（v1.8.38）
const FLIB = (name) => ["test", "-p", "findb", "--lib", `tests::${name}`];
// 未核销清单分页（v1.8.38）
const SOPEN = (name) => ["test", "-p", "finweb", "--test", "settle_open_page", "--", name];
// 账套参数结构校验（fincore::BookOptions::validate）
const FOPT = (name) => ["test", "-p", "fincore", "--lib", `account::tests::${name}`];
const FSETTLE = (name) => ["test", "-p", "findb", "--lib", `settle::tests::${name}`];
const FUNS = (name) => ["test", "-p", "finweb", "--test", "settle_unsettle_api", "--", name];
// v1.8.36 应收应付：催款单口径 + 账龄基准日 + 催款单导出明细。
// 这三处的共同失效形态是「文件/页面都正常，只有数字是错的」——客户收到错的催款单。
const FCUST = (name) => ["test", "-p", "finweb", "--test", "customers", "--", name];

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
      ') -> Result<Json<serde_json::Value>, AppError> {\n    user.require(Perm::SysOption)?;\n    let db = &user.db;\n    // 落库前校验',
    to:
      ') -> Result<Json<serde_json::Value>, AppError> {\n    let db = &user.db;\n    // 落库前校验',
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
    file: "crates/finweb/src/handlers.rs",
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
    name: "⑨ 撤审仍用旧判据（能审不能撤）",
    file: "crates/finweb/src/handlers.rs",
    from: "    require_fund_doc_gate(&user)?;\n    let db = &user.db;\n    findb::receipt::receipt_unaudit(&db, id)?;",
    to: "    user.require(Perm::VoucherAudit)?;\n    let db = &user.db;\n    findb::receipt::receipt_unaudit(&db, id)?;",
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
    from: '        "现金流量" | "现金流量项目" | "现金流" | "cashflow" | "cf" => fincore::AuxKind::CashFlow,',
    to: '        "__not_a_kind__" => fincore::AuxKind::CashFlow,',
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
    from: "                |e: &AuxEntity| e.prop(fincore::auxiliary::prop::TAX_RATE).cloned().unwrap_or_default(),",
    to: "                |_e: &AuxEntity| String::new(),",
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
    from: "                e.prop(fincore::auxiliary::prop::UNIT).cloned().unwrap_or_default(),",
    to: "                String::new(),",
    cmd: ROUND("存货单位与停用状态导出后能导回"),
  },
  {
    // 导入账套后不与平台身份对齐改密标记 → 桌面版搬来的账套（admin 那行是
    // 「首次必须改密」）在 Web 端每个接口都 401，前端退回选择页
    name: "⑯ 导入账套后不对齐改密标记（导入成功但进不去）",
    file: "crates/finweb/src/handlers.rs",
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
    from: "        unposted: draft + audited,",
    to: "        unposted: 0,",
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
    name: "⑱ 进账套的当前期间退回建账首月",
    file: "crates/finweb/src/handlers.rs",
    from: "        let ymm = periods::current_period(&db)\n            .unwrap_or_else(|_| db.options().start_period)\n            .ymm();",
    to: "        let ymm = db.options().start_period.ymm();",
    cmd: HOME("selecting_a_book_lands_on_current_period_not_start_period"),
  },
  {
    // 快照缓存的键里去掉写入计数 → 录完凭证、看报表还是旧数字，
    // 而且刷新也没用（同一个连接）。这是"优化出来的错"里最难查的一种。
    name: "⑱ 余额快照缓存的键忽略写入计数",
    file: "crates/findb/src/balances.rs",
    from: "                q.prepared_by.as_deref().unwrap_or(\"\"),\n                db.conn().total_changes()",
    to: "                q.prepared_by.as_deref().unwrap_or(\"\"),\n                0i64",
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
    from: '        if !matches!(\n            self.budget_control.trim(),\n            "" | "off" | "warn" | "strong"\n        ) {',
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
    file: "crates/finweb/src/handlers.rs",
    from: '        "total": total,\n        "returned": page.len(),',
    to: '        "total": page.len(),\n        "returned": page.len(),',
    cmd: SOPEN("settle_open_pages_without_losing_the_total"),
  },
  {
    // 截断了却不说 → 表现是「这单明明还欠着，核销页里找不到」
    name: "㉓ 截断了却不说",
    file: "crates/finweb/src/handlers.rs",
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
    from: '    if res.ar.total.is_positive() {\n        begin_rows.push(("112201".to_string(), res.ar.total));\n    }',
    to: "",
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
    file: "crates/finweb/src/handlers.rs",
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
    file: "crates/findb/src/settle.rs",
    from: "        let owed = !signed.is_negative();",
    to: "        let owed = true;",
    cmd: FSETTLE("dunning_counts_only_what_is_actually_owed"),
  },
  {
    // 账龄基准日回退成「今天」→ 查 1 月的账龄全落最老一档，与桌面端两个答案
    name: "㉔ 账龄基准日回退成今天",
    file: "crates/finweb/src/handlers.rs",
    from: "        .unwrap_or_else(|| upto.last_day());",
    to: "        .unwrap_or_else(|| chrono::Local::now().date_naive());",
    cmd: FCUST("settle_aging_defaults_to_period_end"),
  },
  {
    // 催款单导出只剩汇总 Sheet → 客户问「哪几张单凑的」答不上来
    name: "㉔ 催款单导出丢掉明细表",
    file: "crates/finweb/src/handlers.rs",
    from: 'build_xlsx(&[sheet, dsheet], &fname, "催款单")',
    to: 'build_xlsx(&[sheet], &fname, "催款单")',
    cmd: FCUST("dunning_export_xlsx_has_detail"),
  },
  {
    name: "㉒ 撤销不看结账线",
    file: "crates/findb/src/settle.rs",
    from: "        if rec.period <= upto {",
    to: "        if false {",
    cmd: FSETTLE("unsettle_refuses_records_in_a_closed_period"),
  },
  {
    name: "㉒ 部分撤销不夹住记录金额",
    file: "crates/findb/src/settle.rs",
    from: "    if amount > rec.amount {",
    to: "    if false {",
    cmd: FSETTLE("unsettle_part_rejects_more_than_the_record"),
  },
  {
    name: "㉒ 核销记录仍按单据期间",
    file: "crates/findb/src/settle.rs",
    from: "    let rec_period = crate::periods::current_period_of(tx)?;",
    to: "    let rec_period = f.period;",
    cmd: FSETTLE("settle_record_is_dated_by_the_settlement_period_not_the_invoice"),
  },
  {
    name: "㉒ 撤到零不删记录",
    file: "crates/findb/src/settle.rs",
    from: "    if left.is_zero() {",
    to: "    if false {",
    cmd: FSETTLE("unsettle_part_reduces_the_record_and_reopens_the_amount"),
  },
  {
    name: "㉒ 空串被当成非法金额",
    file: "crates/finweb/src/handlers.rs",
    from: "        Some(s) if !s.is_empty() => Some(fincore::Money::parse(s).map_err(|e| {",
    to: "        Some(s) => Some(fincore::Money::parse(s).map_err(|e| {",
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
    file: "crates/finweb/src/sheets.rs",
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
const backups = new Map();
for (const m of RUN) if (!backups.has(m.file)) backups.set(m.file, fs.readFileSync(m.file, "utf8"));

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
  bad === 0 && notRun === 0
    ? `\n全部 ${RUN.length} 个变异都被对应测试拦下，源码已还原`
    : `\n${bad} 个变异没被拦下 / 没生效（共 ${RUN.length} 个${notRun ? `，其中 ${notRun} 个是测试根本没跑` : ""}）`
);
// “测试没跑”不能算成功：源码改坏了但测试命令没匹配到任何用例时，
// 旧逻辑只累计 notRun，最终仍 exit 0，等于把变异测试变成摆设。
process.exit(bad === 0 && notRun === 0 ? 0 : 1);
