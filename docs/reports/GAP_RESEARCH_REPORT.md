# FinBook 功能对齐调研报告（2026-09 重写）

> 基准：金蝶云星空 / 用友 T+ Cloud。本版依据当前代码逐项核对，替换 2026 年初的旧版
> （旧版把大量当时未实现的功能标 ❌，此后供应链/制造/管理会计/资金/预算分析/成本均已补齐）。
> 完整的逐项对标表见 [`COMPREHENSIVE_COMPARISON.md`](COMPREHENSIVE_COMPARISON.md)。
>
> 范围约束（用户指定）：
> - ❌ **排除**：税务相关（税务申报、税企直连等；发票仅做台账不生成应付/应收）
> - ✅ **对齐**：其余所有功能点尽量对齐

---

## 一、总体结论（当前代码）

| 模块 | 功能点 | 已对齐 | 未实现/部分 | 对齐率 |
|------|:---:|:---:|:---:|:---:|
| 财务核算 | 100 | 96 | 4 | 96% |
| 供应链（采购/销售/库存/存货） | 80 | 78 | 2 | 97% |
| 生产制造（BOM/订单/工艺/成本） | 50 | 44 | 6 | 88% |
| 其他业务（资产/薪酬/报销/平台） | 30 | 29 | 1 | 97% |
| **合计** | **260** | **247** | **13** | **~95%** |

主要未对齐项（详见 `COMPREHENSIVE_COMPARISON.md` 第五节）：
报表合并、发票→应付/应收闭环、在制品/成本分摊/成本差异/成本预测、BOM 版本与变更历史 UI 接线、
资金调拨、电子考勤/计件工资。

---

## 二、财务模块（原 5 项缺口已全部关闭）

| 功能点 | 状态 | 说明 |
|--------|------|------|
| 多栏账 | ✅ | `findb::advanced::multi_column_table` + finui/SPA |
| 科目日报表 | ✅ | SPA「科目日报表」页 |
| 所有者权益变动表 | ✅ | SPA「权益变动表」页 |
| 报表对比分析 | ✅ | SPA「报表对比」页 |
| 期末对账 | ✅ | SPA「期末对账」页 + `findb::reports::period_reconcile` |
| 报表合并 | 🚫 排除（多法人并表） | - |

资金管理（旧版按用户指定排除，本轮已实现）：

| 功能点 | 状态 | 说明 |
|--------|------|------|
| 现金/银行日记账 | ✅ | `findb::balances::journal` + 账簿查询 |
| 资金日报 | ✅ 本轮新增 | `findb::funds::funds_daily` |
| 票据管理（应收/应付、背书/贴现/兑付） | ✅ 本轮新增 | `findb::funds::Bill` + `bill_transition` |
| 融资管理（借款/放款、结清） | ✅ 本轮新增 | `findb::funds::Loan` + `loan_settle` |
| 资金预测（头寸） | ✅ 本轮新增 | `findb::funds::funds_forecast` |
| 银行对账 | ✅ | `findb::bank`（导入/自动勾对/余额调节表） |
| 资金调拨 | ❌ | 未实现 |

---

## 三、供应链模块缺口（原 59 项，现仅剩 2 项）

采购（原 14 项待实现 → 全部实现）：

| 功能点 | 状态 | 说明 |
|--------|------|------|
| 采购请购单 | ✅ | `findb::procurement::pr_*` + SPA「采购单据」 |
| 采购到货 | ✅ | `findb::procurement`（po_receipt） |
| 采购付款 | ✅ | po_payment |
| 采购退货 | ✅ | po_return |
| 采购价格管理 / 历史价格 | ✅ | `get_price_history` |
| 采购暂估 | ✅ | SPA「采购暂估」 |
| 采购对账 | ✅ | `findb::scm2::po_reconcile` |
| 采购统计报表 / 执行跟踪 | ✅ | `findb::procurement` |
| 采购配额管理 | ✅ | SPA「供应商配额」 |
| 采购订单变更 | ✅ | `order_change_log` |
| 采购审批流程 | ✅ | 挂接通用审批中心 |

销售（原 13 项待实现 → 全部实现）：报价单 / 发货 / 收款 / 退货 / 价格 / 对账 / 统计 /
执行跟踪 / 配额 / 历史价格 / 订单变更 / 审批 / 信用管理 —— 均 ✅，对应 `findb::sales`、
`findb::scm2` 与 SPA「销售单据」。

库存（原 10 项待实现 → 全部实现）：盘点 / 组装拆卸 / 批次 / 序列号 / 多单位换算 /
账龄 / ABC / 调拨报表 / 分仓库 / 暂估 —— 均 ✅。

存货核算（原 3 项待实现 → 本轮全部关闭）：

| 功能点 | 状态 | 说明 |
|--------|------|------|
| 计价方式配置（按存货） | ✅ 本轮新增 | `findb::business::item_cost_method_set` |
| 全月一次加权平均 | ✅ 本轮新增 | `fincore::engine::costing::run_month_average` |
| 个别计价 / 标准成本 | ✅ | 计价引擎已支持 |
| 成本调整 | ✅ | `findb::business::stock_adjust` |
| 期末结价（统一重算+生成调整） | ✅ 本轮新增 | `findb::business::period_end_cost` |

---

## 四、生产制造模块缺口（26 → 6）

BOM（原 6 项 → 剩 2 项）：多层 BOM / 替代料 / 成本汇总 ✅；BOM 版本管理、BOM 变更历史
🟡（schema 已具备，UI 接线不全）；BOM 效率分析 ❌。

生产订单（原 7 项 → 剩 0 项）：报工 / 派工 / 进度跟踪 / 退料 / 变更 ✅（`findb::manufacturing`）。

工艺路线（原 6 项 → 剩 1 项）：工艺版本 / 替代 / 统计 ✅ 部分；工艺权限控制 🟡。

成本核算（原 7 项 → 剩 4 项）：工序成本 ✅；标准成本 ✅（本轮）；在制品成本 🟡、
成本分摊规则 🟡、成本差异分析 🟡、成本预测 ❌。

---

## 五、其他业务模块（原 18 项 → 剩 1 项）

资产管理：处置 / 盘点 / 类别 / 附属设备 / 减值 ✅（`findb::assets` + `asset_count`）。
电子考勤 / 计件工资 ❌（薪酬模块不做考勤）。

---

## 六、当前代码库状态快照（2026-09）

- 数据层：`fincore`（引擎/报表）+ `findb`（SQLite 仓储，schema v16）
- 本轮新增：
  - `findb::funds`（票据/融资/资金日报/资金预测）
  - `findb::mgmt::budget_analysis` / `budget_analysis_summary`（预算分析 + 部门维度）
  - `findb::business::item_cost_method_*` / `cost_configs` / `period_end_cost`（成本）
  - `fincore::engine::costing::CostMethod::MonthAverage` + `run_month_average`
  - schema v16 三张新表（bill / loan / item_cost_method）
  - Web：`/api/funds/*`、`/api/budget/analysis`、`/api/cost/*` 共 11 个端点 + SPA 三页
  - 桌面：finui「资金管理 / 预算分析 / 成本核算」三视图
- 测试基线：`cargo test --workspace` = fincore 93 + findb 153 + core_loop 21 + ext_loop 7
  + finweb 11 = 285 项全绿
