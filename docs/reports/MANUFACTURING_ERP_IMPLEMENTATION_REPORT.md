# FinBook 制造业ERP功能实现报告

## 执行摘要

已完成 **Phase 1-2** 核心制造业功能，实现采购订单、销售订单、BOM管理、生产订单四大基础模块。

**当前进度**: Phase 1✅ | Phase 2✅ | Phase 3⏳

---

## 一、已完成功能

### 1.1 采购订单模块 (PurchaseOrder)

| 功能 | 状态 | 说明 |
|------|------|------|
| 订单创建 | ✅ | 自动生成订单号（CG前缀+年月+流水） |
| 订单行管理 | ✅ | 物料、数量、单价、税率、金额 |
| 订单状态 | ✅ | Draft → Confirmed → PartialIn → Completed |
| 删除订单 | ✅ | 级联删除订单行 |
| 订单列表 | ✅ | 按期间/状态筛选 |

**数据库表**: `purchase_order`, `po_line`

**API接口**:
```rust
pub fn po_next_no(db: &Db, period: Period) -> DbResult<String>
pub fn po_save(db: &Db, po: &mut PurchaseOrder) -> DbResult<i64>
pub fn po_delete(db: &Db, id: i64) -> DbResult<()>
pub fn po_list(db: &Db, period: Period, status: Option<PoStatus>) -> DbResult<Vec<PurchaseOrder>>
```

### 1.2 销售订单模块 (SalesOrder)

| 功能 | 状态 | 说明 |
|------|------|------|
| 订单创建 | ✅ | 自动生成订单号（XS前缀+年月+流水） |
| 订单行管理 | ✅ | 物料、数量、单价、税率、金额 |
| 订单状态 | ✅ | Draft → Confirmed → PartialShip → Completed |
| 删除订单 | ✅ | 级联删除订单行 |
| 订单列表 | ✅ | 按期间/状态筛选 |

**数据库表**: `sales_order`, `so_line`

### 1.3 BOM管理模块

| 功能 | 状态 | 说明 |
|------|------|------|
| BOM查询 | ✅ | 按父物料查询子项列表 |
| BOM保存 | ✅ | 替换式保存（删除旧+插入新） |
| 损耗率支持 | ✅ | 每个子项可配置损耗率 |
| 排序支持 | ✅ | seq字段控制展开顺序 |

**数据库表**: `bom`

### 1.4 生产订单模块 (ProductionOrder)

| 功能 | 状态 | 说明 |
|------|------|------|
| 订单创建 | ✅ | 自动生成订单号（SC前缀+年月+流水） |
| 订单状态 | ✅ | Draft → Released → InProgress → Completed |
| 计划/完工数量 | ✅ | 跟踪生产进度 |
| 工作中心 | ✅ | 关联生产资源 |

**数据库表**: `production_order`

---

## 二、数据库Schema更新

### v6迁移新增表

```sql
-- 采购订单相关
purchase_order      -- 采购订单主表
po_line            -- 采购订单行表

-- 销售订单相关
sales_order        -- 销售订单主表
so_line            -- 销售订单行表

-- BOM相关
bom                -- 物料清单表

-- 生产订单相关
production_order   -- 生产订单表
```

**Schema版本**: v5 → v6

---

## 三、测试覆盖

```bash
cargo test -p findb --lib scm
# running 4 tests
# test scm::prod_tests::prod_crud ... ok
# test scm::tests::bom_crud ... ok
# test scm::tests::po_crud ... ok
# test scm::tests::so_crud ... ok
# test result: ok. 4 passed; 0 failed
```

---

## 四、待实现功能（Phase 3-4）

### 4.1 Phase 3: 成本深化（预计2周）

| 功能 | 优先级 | 工作量 | 说明 |
|------|--------|--------|------|
| 制造费用归集 | P1 | 中 | 归集人工/制造费用到生产订单 |
| 成本分配规则 | P1 | 大 | 按工时/产量多维度分摊 |
| 完工成本计算 | P1 | 大 | BOM展开+领料成本汇总 |
| WIP核算 | P2 | 大 | 在制品 valuation |
| 成本差异分析 | P3 | 中 | 标准vs实际差异 |

### 4.2 Phase 4: MRP与高级功能（预计4周）

| 功能 | 优先级 | 工作量 |
|------|--------|--------|
| MRP物料需求运算 | P2 | 大 |
| 委外加工管理 | P3 | 中 |
| 标准成本体系 | P3 | 大 |
| 工序报工 | P3 | 大 |
| 车间看板 | P3 | 中 |

---

## 五、架构设计

### 5.1 新增模块结构

```
crates/findb/src/
├── scm.rs              # 供应链管理（新增）
│   ├── PoStatus/SoStatus/ProdStatus
│   ├── PurchaseOrder/SalesOrder/ProductionOrder
│   ├── BomItem
│   └── CRUD函数
├── schema.rs           # 已更新v6迁移
└── business.rs         # 现有（保留）
```

### 5.2 数据模型

```
采购订单流程:
  PurchaseOrder → PoLine
       ↓
  StockMove (kind=Purchase)
       ↓
  Voucher (凭证自动生成)

销售订单流程:
  SalesOrder → SoLine
       ↓
  StockMove (kind=Sale)
       ↓
  Voucher (凭证自动生成)

生产订单流程:
  ProductionOrder
       ├── BOM展开 → ProdMaterialIssue
       ├── 领料 → StockMove (OtherOut)
       └── 完工入库 → StockMove (OtherIn) + Cost Voucher
```

---

## 六、与金蝶/用友对比

| 功能域 | FinBook现状 | 金蝶云星空 | 用友T+ | 差距 |
|--------|------------|------------|--------|------|
| 采购订单 | ✅ 基础版 | ✅ 完整 | ✅ 完整 | 小 |
| 销售订单 | ✅ 基础版 | ✅ 完整 | ✅ 完整 | 小 |
| BOM管理 | ✅ 单层 | ✅ 多层 | ✅ 多层 | 中 |
| 生产订单 | ✅ 基础版 | ✅ 完整 | ✅ 完整 | 中 |
| 工序报工 | ❌ 缺失 | ✅ | ✅ | 大 |
| MRP运算 | ❌ 缺失 | ✅ | ✅ | 大 |
| 成本核算 | 🟡 简单 | ✅ 精细 | ✅ 精细 | 大 |

---

## 七、下一步计划

### 短期（1-2周）
1. 完善生产订单与BOM联动（领料自动展开BOM）
2. 实现完工入库凭证自动生成
3. 添加库存预警（安全库存）

### 中期（2-4周）
1. 制造费用归集与分配
2. 完工成本计算
3. WIP在制品核算

### 长期（1-3月）
1. MRP物料需求计划
2. 标准成本体系
3. 作业成本法(ABC)

---

## 八、技术债务

1. **订单号生成**: 当前使用COALESCE+MAX，高并发下可能有冲突，需加分布式锁
2. **BOM递归展开**: 当前仅支持单层，多层BOM需递归或CTE
3. **凭证自动生成**: 订单状态变更时自动生成凭证的逻辑未实现
4. **权限控制**: 新模块未接入DataScope权限体系

---

**实现时间**: 2026-09-02  
**代码仓库**: https://github.com/rongyaoidea/finbook  
**当前分支**: main  
**提交记录**: a3e4f0c (Phase 1), 待推送 (Phase 2)
