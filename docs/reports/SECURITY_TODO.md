# FinBook 安全加固待办事项

> 本文件反映**当前代码真实状态**（2026-09 核实，逐项带代码证据）。
> 结论：20 项历史修复**全部核实完成**；三项遗留（L-1 除零 / L-2 期间构造 / M-9 路径泄露）已于 2026-09 本轮清零。H-3/M-15 产品决策已定案并落地。

## 一、已完成并核实（18 项）

### P0 级 ✅

| ID | 问题 | 证据 |
|----|------|------|
| CRIT-1 | 默认监听 127.0.0.1 | `crates/finweb/src/main.rs:35`（`FINBOOK_LISTEN` 默认 `127.0.0.1:8080`） |
| HIGH-1 | Web 层落地 DataScope | `crates/finweb/src/handlers.rs:1579` `with_data_scope`、`:1604` `retain(can_see_voucher)`、`:1621` get 分支 403 |
| HIGH-2 | voucher_unaudit 权限校验 | `crates/finweb/src/handlers.rs:1853` `require(Perm::VoucherUnaudit)` |
| HIGH-3 | 凭证删除守卫 | `crates/finweb/src/handlers.rs:1999-2002` 已记账 400；`:1995` 已审核拦截；`:2005` 结账期间拦截 |
| HIGH-4 | prepared_by 统一 username | `crates/finweb/src/handlers.rs:1730`；桌面 `crates/finui/src/views/voucher_edit.rs:484` |
| HIGH-5 | DataScope 默认收紧 | ⚠️ 策略调整（多岗位协作，2026-09）：默认已改回**放开**——只看本人会让各会计互相看不见分录、余额/试算口径碎裂；过滤机制保留，管理员在「用户编辑 → 数据范围」按账号勾选「仅看本人填制的凭证」收紧（原收紧方案见 SECURITY_FIX_SUMMARY） |
| HIGH-6 | must_change_pwd 服务端拦截 | `crates/finweb/src/state.rs:443-450`、`state.rs:575-582` 白名单外一律 401 |
| HIGH-7 | Cookie Secure 标志 | `crates/finweb/src/state.rs:717-728`（HttpOnly; SameSite=Lax，`FINWEB_SECURE_COOKIE=true` 时追加 Secure） |

### P1 级 ✅

| ID | 问题 | 证据 |
|----|------|------|
| H-C-1 | 资产负债表 `.neg()` 笔误 | `crates/fincore/src/report/balance_sheet.rs:45`、`:85`（已无 `.neg()`） |
| H-2 | 核销金额精度 | `crates/findb/src/settle.rs:187-202`（Rust 侧累加 + `money_param`，不 SQL SUM） |
| M-1 | 登录枚举消除 | `crates/finweb/src/realm.rs:195-222` 三种失败统一 `Ok(None)` + `burn_argon2`；`handlers.rs:496` 统一话术 |
| M-3 | 锁定窗口单位修复 | `crates/findb/src/security.rs:13-15` 分钟窗口；`state.rs:159/209` 秒 + `handlers.rs:474` `div_ceil(60)` |
| M-5 | 附件大小上限 + 白名单 | `crates/findb/src/attach.rs:20`（10MB）、`:23-28` 扩展名白名单、`:175-191` 强制校验；`handlers.rs:2089-2091` 二次校验 |
| M-6 | 附件路径穿越防护 | `crates/findb/src/attach.rs:209`（sha256 落盘名）、`:239-241` 拒 `..`、`:388-397` 导出名 sanitize；测试 `finweb/tests/api.rs:1174` |
| M-10 | parse_money Unicode | `crates/findb/src/imports.rs:322-334`（全角/负号归一，测试 `:957-964`）；`crates/fincore/src/money.rs:58-68` |
| M-14 | argon2 降级防护 | `crates/fincore/src/user.rs:459-466`（哈希失败即 panic，不降级） |
| M-2 | 登录失败 IP 限流 | `crates/finweb/src/state.rs:34,68`（50 次/IP 窗口）；`handlers.rs:481/494/503/442` 检查/记录/清除/取 IP |
| M-7 | 导入事务化 | `crates/findb/src/imports.rs:543→594`（begin）、`:653→729`（vouchers），整批同事务 |

## 二、产品决策已定案并落地 ✅

### H-3：余额口径 —— 定案 A（只算已记账）

- 科目余额表 / 三大报表 / 试算平衡 / 看板 = 期初余额 + **已记账分录**（`BalanceQuery` 默认 `posted_only=true`），草稿与已审核未记账不入余额。
- 账簿保留「只含已记账」开关（Web `#l-posted` 与桌面端默认勾选）；取消按「含未记账（排除作废）」查看，**行集与期初/滚动余额同口径**（修复原行集恒含非作废、快照按 posted 的打架 bug）。
- **例外**：期末结转类操作（结转损益 / 年末结转 / 预检 / 自动化清单展示）显式 `with_posted_only(false)`——结转凭证本身是草稿，靠含草稿口径判重防重复结转；结账前 checklist 要求全部记账，届时两种口径结果相同。
- 用例：`findb/balances.rs::balance_scope_h3_default_posted_only`、`ledger_rows_follow_posted_flag`；`finweb/tests/api.rs::trial_balance_default_posted_only_h3`。
- 文档：README §2.7/§2.8/§2.9/§2.10 + §5「余额口径（H-3 定案）」行。

### M-15：借贷平衡精度 —— 定案 A'（分位量化严格判平，不可配置）

- 分录金额**逐条量化到 2 位（分）**后借贷合计严格相等（`Voucher::balanced`），与落库 `money_param` 口径完全一致；差 0.005 的半分尾差不放过（否则入库后真差 0.01）。
- 试算平衡按 round2 判定，差额不足 1 分视为平衡。固定口径，不可配置。
- 用例：`fincore/voucher.rs::balanced_quantizes_per_entry`、`fincore/balance.rs::trial_balance_quantized_tolerance`；`finweb/tests/api.rs::unbalanced_voucher_rejected_m15`。
- 文档：README §5「借贷平衡（M-15 定案）」行。

## 三、原部分完成 / 待办项已全部收尾 ✅（2026-09 本轮）

| ID | 问题 | 现状 | 证据 |
|----|------|------|------|
| M-9 | 未认证接口路径泄露 | **已修复**：路由匹配前统一 401 未认证 `/api/*`（公开接口 health/login/logout/setup.status 放行）；未匹配路径由统一 fallback 回「已登录 404 / 未登录 401」，静态资源不再吞 `/api/*`；404/405/401 三路探测不可区分 | `finweb/src/handlers.rs::api_auth_gate`、`::spa_fallback`、`state.rs::session_of`（与提取器共用文案）；用例 `finweb/tests/api.rs::m9_unauthenticated_probe_uniform_401` |
| L-2 | Period::from_ymm 异常值 | **已修复**：Web 入口早已走 checked；桌面端删除折旧确认改 `from_ymm_checked`（非法期间报错中止、不落脏数据），"从未结账"哨兵改用命名常量 `Period::ZERO` | `finui/src/lib.rs::run_action`（DeleteDepreciation 分支）、`period_selector`（`Period::ZERO`）；Web 侧 `handlers.rs:1170/1179/2427` |
| L-1 | Money 除零返回 0 | **已修复**：三个 `impl Div`（Money/Decimal/i64 除数）删除，改为 `Money::checked_div -> Option`（除零返 `None`，不再静默 0）；全仓 31 处除法点显式化——有判零守卫处 `expect`（守卫与除数同源）、展示类/原"除零=0"语义处 `unwrap_or(Money::ZERO)` 带注释、**公式引擎除零改报错**（金额公式不再把 0 当合法结果）、无守卫处传播错误 | `fincore/src/money.rs::checked_div`/`Divisor`；调用点分布：fincore 12（costing/depreciation/formula）、findb 14、finui 4、finweb 1 |

## 四、待后续迭代

### 中优先级

| ID | 问题 | 现状 | 建议方案 | 预估 |
|----|------|------|----------|------|
| M-4 | 设备绑定可伪造 | 未完成：device_id 由客户端上报，可伪造换绑 | 首次绑定后禁改，服务端生成 device_hash | 中 |
| — | DataScope 未逐模块接入 | DataScope 默认收紧 + Web 凭证查询已接（HIGH-1/5），但其余模块查询未逐一走 scope | 逐模块排查接入 | 中 |

### 低优先级

| ID | 问题 | 说明 |
|----|------|------|
| L-3~L-14 | 各种边界/死代码/风格 | 见原审查报告 |

## 五、推荐修复顺序

```
1. M-4 设备绑定强化（服务端 device_hash）
2. DataScope 逐模块接入
3. L-3~L-14 渐进优化
```

> 原顺序中的 L-1 / M-9 / L-2 已于 2026-09 本轮完成（见第三节）。

## 六、出纳功能完整性（2026-09 落地）

出纳岗位评估的 10 项缺口全部实现（Web + 桌面双端，均带测试）：

| # | 缺口 | 落地 |
|---|------|------|
| 1 | 出纳日记账工作台 | 资金管理 → 日记账：已记账逐笔 + 滚动余额；**日清标记**（`day_clear` 表，按科目按日）；**收付登记**跳凭证录入预填科目（Web `state.pendingCash` / 桌面 `AppState.pending_cash`） |
| 2 | 出纳签字（原死权限） | `vouchers::sign/unsign` + Web `POST /api/vouchers/:id/sign|unsign` + 两端按钮 + `require_cashier` 记账前置（仅现金/银行科目，`post_tx` 把关）+ 日记账签字列 + 账套参数开关；用例 `cashier_sign_and_unsign`、`require_cashier_gates_post_and_scopes_to_funds` |
| 3 | 支票登记簿 | `check_register` 表 + 开出↔作废流转 + 两端页签；纯备查簿不入账；用例 `day_clear_toggle_and_check_book`、`cashier_day_clear_and_checks` |
| 4 | 员工借支闭环 | `advance` 表 approved→paid→settled；支付（借122105员工/贷资金）与核销（冲账费用+退回）同事务出凭证、幂等与超额守卫；用例 `advance_pay_and_settle_flow`、`advance_pay_settle_api_flow` |
| 5 | 现金盘点 | `cash_count` 表，账面按资金日报（按日、仅已记账）快照；差异→盘盈盘亏凭证（1901）；挂凭证不可删；用例 `cash_count_book_snapshot_and_voucher`、`cash_count_flow` |
| 6 | 台账-总账脱节 | 票据背书/贴现/兑付、融资结清**同事务自动生成台账凭证**（`bill.voucher_id` / `loan.voucher_id`+`settle_voucher_id`，迁移 v19），存量回填端点 + 「生成凭证」按钮；用例 `funds_ledger_voucher_linkage` |
| 7 | 资金日报名实不符 | 新增 `funds_daily_by_date`（上日结余/本日收支/日末结存，按日翻页，仅已记账），两端页签改为按日；期间口径 `funds_daily` 保留供预测用；用例 `funds_daily_by_date_single_day`、`funds_daily_by_date_report` |
| 8 | 报销支付不落账 + 工资无发放状态 | 报销**支付即自动出付款凭证**（默认贷100201），手动「生成凭证」幂等返回同一张；工资行新增 `paid_voucher_id`/`social_voucher_id` 回链（迁移 v20），两端展示计提/社保/发放三链状态；用例 `payroll_voucher_status_tracking` |
| 9 | 无资金预算视图 | `funds_budget`（过滤现金/银行科目预算行 vs 当期已记账净额），Web 与桌面「资金预算」页签 + 导出；用例 `funds_budget_filters_cash_accounts`、`funds_budget_view_api` |
| 10 | 文档与实现不一致 | README 新增 §4.9 出纳作业台；角色数 5→6、"审计"→"审核人"；`funds.rs`/`finui` 模块注释同步 |

附带修复存量 bug：**工资凭证幂等闸失效**——`ensure_unique_biz_voucher` 查 `source='Business'`，而落库序列化为小写 `business`，导致「同摘要只允许一张」从未生效（由 payroll 重复生成测试暴露）。

## 七、CI 保障

- `.github/workflows/ci.yml` 已含 **cargo-audit** 任务（读 Cargo.lock，发现未修复 RUSTSEC 漏洞公告即失败）。
- 仓库 `.cargo/audit.toml` 记录 3 条**有据可依的忽略项**（lopdf 仅写不读、quick-xml 0.30 被 accesskit/zbus 上游锁死），并注明解除条件；其余公告一律拦截。
- 2026-09 依赖加固：rust_decimal 1.42→1.43（rkyv 0.7 可选边移出锁，RUSTSEC-2026-0235 消除）；calamine 0.26→0.36（quick-xml 0.31 移除，xlsx 导入攻击面修复，RUSTSEC-2026-0194/0195 可达实例消除）。
- 本地验证：fincore 99 / findb 172+21+7 / finweb 54 全部通过（2026-09）。
