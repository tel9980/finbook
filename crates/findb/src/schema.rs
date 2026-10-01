//! 账套表结构
//!
//! 设计取舍：
//! - **金额一律存 TEXT**。SQLite 的 REAL 是 IEEE754 双精度，存钱会出精度事故；
//!   TEXT 存十进制字符串，配合 `Decimal` 解析，分毫不差。
//! - 凭证分录冗余了 `period` 字段，账簿汇总查询只需扫 `voucher_entry` 一张表，
//!   百万级数据量下也能在毫秒级出报表。
//! - 辅助核算维度数量不固定，用 `aux_key`（稳定排序串）+ `aux_json`（完整值）两个字段：
//!   前者用于 GROUP BY，后者用于还原展示。

use rusqlite::Connection;

use crate::DbError;

/// 当前 schema 版本
///
/// v1：核心闭环（科目 / 凭证 / 账簿 / 报表 / 期末）
/// v2：业务闭环与月度自动化（出纳对账 / 往来核销 / 固定资产 / 存货 / 工资 / 报销 /
///     预算 / 自动转账 / 期末调汇 / 附件 / 账户安全）
/// v3：自动转账补"对方科目"，支持计提类（来源科目只取数不转出）
/// v4：自定义报表独立建表（行 × 列 × 公式网格）
/// v5：账号设备绑定（user.device_id / device_name）
/// v6：供应链深化（采购订单 / 销售订单 / BOM / 生产订单）
/// v7：多栏账 / 工艺路线 / MRP / 预算多版本 / 审批流 / 报表附注 / 电子档案
/// v16：资金（票据 / 融资）+ 存货计价配置（全月一次 / 期末结价）
/// v17：用户权限逐项覆盖（user.deny_perms_json）
pub const SCHEMA_VERSION: i64 = 32;

/// 建表语句
const DDL: &str = r#"
-- 账套元数据与参数
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- 会计科目
CREATE TABLE IF NOT EXISTS account (
    code        TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    category    TEXT NOT NULL,
    dir         TEXT NOT NULL,
    aux_mask    INTEGER NOT NULL DEFAULT 0,
    unit        TEXT,
    currency    TEXT,
    has_qty     INTEGER NOT NULL DEFAULT 0,
    is_cash     INTEGER NOT NULL DEFAULT 0,
    is_bank     INTEGER NOT NULL DEFAULT 0,
    cf_item     TEXT,
    bs_item     TEXT,
    pl_item     TEXT,
    disabled    INTEGER NOT NULL DEFAULT 0,
    memo        TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_account_prefix ON account(code);

-- 辅助核算档案（客户/供应商/部门/职员/项目/存货/银行账户）
CREATE TABLE IF NOT EXISTS aux_entity (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    kind        TEXT NOT NULL,
    code        TEXT NOT NULL,
    name        TEXT NOT NULL,
    parent_code TEXT,
    disabled    INTEGER NOT NULL DEFAULT 0,
    props_json  TEXT NOT NULL DEFAULT '{}',
    memo        TEXT NOT NULL DEFAULT '',
    UNIQUE(kind, code)
);
CREATE INDEX IF NOT EXISTS idx_aux_kind ON aux_entity(kind);

-- 期间状态（期末结账标记）
CREATE TABLE IF NOT EXISTS period_state (
    period    INTEGER PRIMARY KEY,
    closed    INTEGER NOT NULL DEFAULT 0,
    closed_at TEXT,
    closed_by TEXT
);

-- 期初余额
CREATE TABLE IF NOT EXISTS begin_balance (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    account_code TEXT NOT NULL,
    aux_key      TEXT NOT NULL DEFAULT '',
    aux_json     TEXT NOT NULL DEFAULT '{}',
    year_begin   TEXT NOT NULL DEFAULT '0',
    debit_accum  TEXT NOT NULL DEFAULT '0',
    credit_accum TEXT NOT NULL DEFAULT '0',
    qty_begin    TEXT,
    UNIQUE(account_code, aux_key)
);

-- 记账凭证
CREATE TABLE IF NOT EXISTS voucher (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    period      INTEGER NOT NULL,
    date        TEXT NOT NULL,
    word        TEXT NOT NULL DEFAULT '记',
    no          INTEGER NOT NULL,
    attachments INTEGER NOT NULL DEFAULT 0,
    status      TEXT NOT NULL DEFAULT 'draft',
    prepared_by TEXT NOT NULL DEFAULT '',
    audited_by  TEXT,
    posted_by   TEXT,
    cashier     TEXT,
    source      TEXT NOT NULL DEFAULT 'manual',
    memo        TEXT NOT NULL DEFAULT '',
    created_at  TEXT NOT NULL DEFAULT '',
    updated_at  TEXT NOT NULL DEFAULT '',
    UNIQUE(period, word, no)
);
CREATE INDEX IF NOT EXISTS idx_voucher_period ON voucher(period);
CREATE INDEX IF NOT EXISTS idx_voucher_date   ON voucher(date);
CREATE INDEX IF NOT EXISTS idx_voucher_status ON voucher(status);

-- 凭证分录
CREATE TABLE IF NOT EXISTS voucher_entry (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    voucher_id   INTEGER NOT NULL REFERENCES voucher(id) ON DELETE CASCADE,
    period       INTEGER NOT NULL,
    line         INTEGER NOT NULL,
    summary      TEXT NOT NULL DEFAULT '',
    account_code TEXT NOT NULL,
    aux_key      TEXT NOT NULL DEFAULT '',
    aux_json     TEXT NOT NULL DEFAULT '{}',
    debit        TEXT NOT NULL DEFAULT '0',
    credit       TEXT NOT NULL DEFAULT '0',
    qty          TEXT,
    price        TEXT,
    currency     TEXT,
    rate         TEXT,
    amount_for   TEXT,
    settle_type  TEXT,
    settle_no    TEXT,
    biz_date     TEXT,
    cf_item      TEXT
);
CREATE INDEX IF NOT EXISTS idx_entry_voucher ON voucher_entry(voucher_id);
CREATE INDEX IF NOT EXISTS idx_entry_account ON voucher_entry(account_code);
CREATE INDEX IF NOT EXISTS idx_entry_period  ON voucher_entry(period);
CREATE INDEX IF NOT EXISTS idx_entry_cf      ON voucher_entry(cf_item);

-- 用户
CREATE TABLE IF NOT EXISTS user (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    username      TEXT NOT NULL UNIQUE,
    display_name  TEXT NOT NULL DEFAULT '',
    password_hash TEXT NOT NULL DEFAULT '',
    role          TEXT NOT NULL DEFAULT 'accountant',
    disabled      INTEGER NOT NULL DEFAULT 0,
    extra_perms   TEXT NOT NULL DEFAULT '[]',
    memo          TEXT NOT NULL DEFAULT '',
    roles_json    TEXT NOT NULL DEFAULT '[]'
);

-- 操作日志
CREATE TABLE IF NOT EXISTS audit_log (
    id     INTEGER PRIMARY KEY AUTOINCREMENT,
    ts     TEXT NOT NULL,
    user   TEXT NOT NULL DEFAULT '',
    module TEXT NOT NULL DEFAULT '',
    action TEXT NOT NULL DEFAULT '',
    detail TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_log_ts ON audit_log(ts);

-- 现金流量项目
CREATE TABLE IF NOT EXISTS cash_flow_item (
    code     TEXT PRIMARY KEY,
    name     TEXT NOT NULL,
    grp      TEXT NOT NULL,
    dir      TEXT NOT NULL,
    disabled INTEGER NOT NULL DEFAULT 0
);

-- 自定义报表模板
CREATE TABLE IF NOT EXISTS report_def (
    key         TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    columns_json TEXT NOT NULL,
    lines_json  TEXT NOT NULL
);

-- 常用摘要
CREATE TABLE IF NOT EXISTS summary (
    text       TEXT PRIMARY KEY,
    use_count  INTEGER NOT NULL DEFAULT 0
);

-- 结算方式
CREATE TABLE IF NOT EXISTS settle_type (
    name TEXT PRIMARY KEY,
    sort INTEGER NOT NULL DEFAULT 0
);

-- 常用凭证模板
CREATE TABLE IF NOT EXISTS voucher_template (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    name     TEXT NOT NULL,
    memo     TEXT NOT NULL DEFAULT '',
    entries_json TEXT NOT NULL DEFAULT '[]',
    -- v2 扩展：周期性自动生成（null/'' 表示仅作为手工调用的模板）
    freq        TEXT NOT NULL DEFAULT '',          -- monthly / quarterly / yearly
    start_period INTEGER NOT NULL DEFAULT 0,
    end_period   INTEGER NOT NULL DEFAULT 0,
    last_period  INTEGER NOT NULL DEFAULT 0,
    active      INTEGER NOT NULL DEFAULT 0
);

-- ===========================================================================
-- v2：业务闭环与月度自动化
-- ===========================================================================

-- 凭证附件（扫描件 / 电子发票 / 合同）
CREATE TABLE IF NOT EXISTS attachment (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    voucher_id  INTEGER NOT NULL REFERENCES voucher(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    kind        TEXT NOT NULL DEFAULT '',   -- 扩展名或 MIME 简写
    size        INTEGER NOT NULL DEFAULT 0,
    sha256      TEXT NOT NULL DEFAULT '',
    -- 小文件直接内联存 SQLite，大文件落同目录 .attachments/ 只存相对路径
    inline      INTEGER NOT NULL DEFAULT 1,
    data        BLOB,
    path        TEXT,
    added_by    TEXT NOT NULL DEFAULT '',
    added_at    TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_attach_voucher ON attachment(voucher_id);

-- 银行对账单（出纳模块）
CREATE TABLE IF NOT EXISTS bank_statement (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    period      INTEGER NOT NULL,
    account_code TEXT NOT NULL,      -- 对应的银行存款科目
    biz_date    TEXT NOT NULL,
    summary     TEXT NOT NULL DEFAULT '',
    settle_no   TEXT NOT NULL DEFAULT '',
    debit       TEXT NOT NULL DEFAULT '0',   -- 银行口径：进账
    credit      TEXT NOT NULL DEFAULT '0',   -- 银行口径：支出
    balance     TEXT NOT NULL DEFAULT '0',   -- 对账单上的余额
    -- 勾对上的凭证分录 id。分录被重写/删除时自动置空，避免留下悬空勾对
    entry_id    INTEGER REFERENCES voucher_entry(id) ON DELETE SET NULL,
    matched_at  TEXT,
    matched_by  TEXT
);
CREATE INDEX IF NOT EXISTS idx_stmt_period ON bank_statement(period, account_code);
CREATE INDEX IF NOT EXISTS idx_stmt_entry  ON bank_statement(entry_id);

-- 往来核销记录
CREATE TABLE IF NOT EXISTS settle_record (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    period       INTEGER NOT NULL,
    account_code TEXT NOT NULL,
    aux_key      TEXT NOT NULL DEFAULT '',
    -- 被核销/核销方分录；分录被重写或删除时核销记录随之级联删除，
    -- 否则会留下指向已删分录的"幽灵核销"，让往来重新显示未核销
    from_entry   INTEGER NOT NULL REFERENCES voucher_entry(id) ON DELETE CASCADE,
    to_entry     INTEGER NOT NULL REFERENCES voucher_entry(id) ON DELETE CASCADE,
    amount       TEXT NOT NULL,      -- 本次核销金额（正数）
    settled_by   TEXT NOT NULL DEFAULT '',
    settled_at   TEXT NOT NULL DEFAULT '',
    UNIQUE(from_entry, to_entry)
);
CREATE INDEX IF NOT EXISTS idx_settle_entry ON settle_record(from_entry, to_entry);

-- 固定资产卡片
CREATE TABLE IF NOT EXISTS fixed_asset (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    code          TEXT NOT NULL UNIQUE,
    name          TEXT NOT NULL,
    category      TEXT NOT NULL DEFAULT '',
    spec          TEXT NOT NULL DEFAULT '',
    dept          TEXT NOT NULL DEFAULT '',      -- 使用部门（辅助档案 code）
    asset_account TEXT NOT NULL DEFAULT '1601',  -- 资产科目
    dep_account   TEXT NOT NULL DEFAULT '1602',  -- 累计折旧科目
    expense_account TEXT NOT NULL DEFAULT '6602',-- 折旧费用科目
    original_value TEXT NOT NULL DEFAULT '0',    -- 原值
    residual_rate  TEXT NOT NULL DEFAULT '0.05', -- 残值率
    life_months    INTEGER NOT NULL DEFAULT 60,  -- 预计使用月数
    method         TEXT NOT NULL DEFAULT 'straight', -- straight / ddb / sum_of_years / one_time / fifty_fifty
    start_period   INTEGER NOT NULL,             -- 开始计提期间
    disposed_period INTEGER,                     -- 清理期间
    dispose_amount TEXT,
    status         TEXT NOT NULL DEFAULT 'in_use', -- in_use / idle / disposed
    voucher_id     INTEGER,                      -- 入账凭证
    memo           TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_asset_status ON fixed_asset(status);

-- 折旧明细（每月一条）
CREATE TABLE IF NOT EXISTS asset_depreciation (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    asset_id   INTEGER NOT NULL REFERENCES fixed_asset(id) ON DELETE CASCADE,
    period     INTEGER NOT NULL,
    amount     TEXT NOT NULL,          -- 本期折旧额
    accum      TEXT NOT NULL,          -- 期末累计折旧
    net_value  TEXT NOT NULL,          -- 期末净值
    voucher_id INTEGER,                -- 生成的折旧凭证
    UNIQUE(asset_id, period)
);
CREATE INDEX IF NOT EXISTS idx_dep_period ON asset_depreciation(period);

-- ===========================================================================
-- v13：资产盘点 / 附属设备 / 减值
-- ===========================================================================

-- 资产盘点单
CREATE TABLE IF NOT EXISTS asset_count (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    no          TEXT NOT NULL UNIQUE,
    period      INTEGER NOT NULL,
    date        TEXT NOT NULL,
    status      TEXT NOT NULL DEFAULT 'draft', -- draft / posted
    prepared_by TEXT NOT NULL DEFAULT '',
    memo        TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_ac_period ON asset_count(period);

-- 盘点明细（账面状态 vs 实盘）
CREATE TABLE IF NOT EXISTS asset_count_line (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    ac_id       INTEGER NOT NULL REFERENCES asset_count(id) ON DELETE CASCADE,
    asset_id    INTEGER NOT NULL,
    found       INTEGER NOT NULL DEFAULT 1, -- 1=盘到 0=盘亏
    memo        TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_acl ON asset_count_line(ac_id);

-- 资产附属设备
CREATE TABLE IF NOT EXISTS asset_accessory (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    asset_id    INTEGER NOT NULL REFERENCES fixed_asset(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    spec        TEXT NOT NULL DEFAULT '',
    qty         INTEGER NOT NULL DEFAULT 1,
    memo        TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_acc ON asset_accessory(asset_id);

-- 资产减值
CREATE TABLE IF NOT EXISTS asset_impairment (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    asset_id    INTEGER NOT NULL REFERENCES fixed_asset(id) ON DELETE CASCADE,
    period      INTEGER NOT NULL,
    amount      TEXT NOT NULL DEFAULT '0',
    memo        TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_imp ON asset_impairment(asset_id);

-- 固定资产变更历史（字段级：改前/改后/操作人）
CREATE TABLE IF NOT EXISTS asset_change (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    asset_id    INTEGER NOT NULL REFERENCES fixed_asset(id) ON DELETE CASCADE,
    ts          TEXT NOT NULL DEFAULT '',
    who         TEXT NOT NULL DEFAULT '',
    field       TEXT NOT NULL,
    old_value   TEXT NOT NULL DEFAULT '',
    new_value   TEXT NOT NULL DEFAULT '',
    memo        TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_asset_change_asset ON asset_change(asset_id, id);

-- 汇率表（期末调汇）
CREATE TABLE IF NOT EXISTS fx_rate (
    period      INTEGER NOT NULL,
    currency    TEXT NOT NULL,
    rate        TEXT NOT NULL,   -- 1 外币 = ? 本位币
    PRIMARY KEY (period, currency)
);

-- 自动转账规则（期末一键批量生成凭证）
CREATE TABLE IF NOT EXISTS auto_transfer (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT NOT NULL,
    sort        INTEGER NOT NULL DEFAULT 0,
    active      INTEGER NOT NULL DEFAULT 1,
    -- 转入方（贷方来源）：取数定义
    src_account TEXT NOT NULL DEFAULT '',
    src_aux     TEXT NOT NULL DEFAULT '',
    src_kind    TEXT NOT NULL DEFAULT 'end',  -- begin / debit / credit / end
    src_dir     TEXT NOT NULL DEFAULT 'auto', -- 取该方向的余额：debit / credit / auto
    ratio       TEXT NOT NULL DEFAULT '1',    -- 比例或固定金额
    ratio_mode  TEXT NOT NULL DEFAULT 'ratio',-- ratio 按比例 / amount 固定金额
    -- 转出方
    dst_account TEXT NOT NULL,
    dst_aux     TEXT NOT NULL DEFAULT '',
    dst_dir     TEXT NOT NULL DEFAULT 'debit',
    -- 对方科目：留空则用 src_account（结转类，把来源科目结平）；
    -- 填了则用对方科目（计提类，来源科目只取数不转出）
    offset_account TEXT NOT NULL DEFAULT '',
    summary     TEXT NOT NULL DEFAULT '',
    memo        TEXT NOT NULL DEFAULT ''
);

-- 存货出入库流水
CREATE TABLE IF NOT EXISTS stock_move (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    period      INTEGER NOT NULL,
    biz_date    TEXT NOT NULL,
    kind        TEXT NOT NULL,      -- purchase / sale / other_in / other_out / transfer / adjust
    item        TEXT NOT NULL,      -- 存货档案 code
    warehouse   TEXT NOT NULL DEFAULT '',
    batch_no    TEXT NOT NULL DEFAULT '',  -- v8: 批次号（批次管理）
    qty         TEXT NOT NULL,      -- 正数入库，负数出库
    price       TEXT NOT NULL DEFAULT '0',
    amount      TEXT NOT NULL DEFAULT '0',
    voucher_id  INTEGER,
    memo        TEXT NOT NULL DEFAULT '',
    qc_status   TEXT NOT NULL DEFAULT ''  -- v24: 质检状态：''可用 / pending待检 / quarantine隔离
);
CREATE INDEX IF NOT EXISTS idx_stock_period ON stock_move(period, item);
CREATE INDEX IF NOT EXISTS idx_stock_item ON stock_move(item, biz_date);

-- 仓库主数据（v30；建账/升级自动种默认仓「01 主仓」，见迁移链）
CREATE TABLE IF NOT EXISTS warehouse (
    code        TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    is_default  INTEGER NOT NULL DEFAULT 0,
    disabled    INTEGER NOT NULL DEFAULT 0,
    memo        TEXT NOT NULL DEFAULT ''
);

-- 工资表
CREATE TABLE IF NOT EXISTS payroll (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    period      INTEGER NOT NULL,
    employee    TEXT NOT NULL,          -- 职员档案 code
    dept        TEXT NOT NULL DEFAULT '',
    gross       TEXT NOT NULL DEFAULT '0',  -- 应发合计
    social      TEXT NOT NULL DEFAULT '0',  -- 社保个人部分
    housing     TEXT NOT NULL DEFAULT '0',  -- 公积金个人部分
    deduction   TEXT NOT NULL DEFAULT '0',  -- 其他扣款
    additional  TEXT NOT NULL DEFAULT '0',  -- 专项附加扣除
    tax_base    TEXT NOT NULL DEFAULT '0',  -- 计税基数
    tax         TEXT NOT NULL DEFAULT '0',  -- 个人所得税
    net         TEXT NOT NULL DEFAULT '0',  -- 实发
    -- 企业承担部分
    social_co   TEXT NOT NULL DEFAULT '0',
    housing_co  TEXT NOT NULL DEFAULT '0',
    voucher_id  INTEGER,
    memo        TEXT NOT NULL DEFAULT '',
    UNIQUE(period, employee)
);
CREATE INDEX IF NOT EXISTS idx_payroll_period ON payroll(period);

-- 费用报销单
CREATE TABLE IF NOT EXISTS expense_claim (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    period      INTEGER NOT NULL,
    no          TEXT NOT NULL,
    biz_date    TEXT NOT NULL,
    applicant   TEXT NOT NULL,      -- 申请人（职员 code）
    dept        TEXT NOT NULL DEFAULT '',
    reason      TEXT NOT NULL DEFAULT '',
    amount      TEXT NOT NULL DEFAULT '0',
    status      TEXT NOT NULL DEFAULT 'draft',  -- draft/submitted/approved/rejected/paid
    items_json  TEXT NOT NULL DEFAULT '[]',     -- 明细：[{expense_account,amount,memo}]
    approver    TEXT NOT NULL DEFAULT '',
    approved_at TEXT,
    payer       TEXT NOT NULL DEFAULT '',
    paid_at     TEXT,
    voucher_id  INTEGER,
    created_at  TEXT NOT NULL DEFAULT '',
    UNIQUE(period, no)
);
CREATE INDEX IF NOT EXISTS idx_claim_period ON expense_claim(period, status);

-- 预算
CREATE TABLE IF NOT EXISTS budget (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    period      INTEGER NOT NULL,
    account_code TEXT NOT NULL,
    dept        TEXT NOT NULL DEFAULT '',
    amount      TEXT NOT NULL DEFAULT '0',
    memo        TEXT NOT NULL DEFAULT '',
    version     TEXT NOT NULL DEFAULT '',   -- v7: 预算版本（''=默认/当前）
    UNIQUE(period, account_code, dept, version)
);
CREATE INDEX IF NOT EXISTS idx_budget_period ON budget(period);

-- 用户自定义报表（UFO 风格：单元格 = 公式）
CREATE TABLE IF NOT EXISTS custom_report (
    key         TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    columns_json TEXT NOT NULL DEFAULT '[]',
    lines_json  TEXT NOT NULL DEFAULT '[]',
    updated_at  TEXT NOT NULL DEFAULT ''
);

-- 登录失败记录（账户锁定）
CREATE TABLE IF NOT EXISTS login_attempt (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    username TEXT NOT NULL,
    ts       TEXT NOT NULL,
    ok       INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_attempt_user ON login_attempt(username, ts);

-- 发票管理（进项/销项发票台账）
CREATE TABLE IF NOT EXISTS invoice (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    kind        TEXT NOT NULL DEFAULT 'in',      -- in=进项 / out=销项
    code        TEXT NOT NULL DEFAULT '',        -- 发票代码
    number      TEXT NOT NULL,                   -- 发票号码
    date        TEXT NOT NULL,                   -- 开票日期
    buyer       TEXT NOT NULL DEFAULT '',        -- 购买方名称
    seller      TEXT NOT NULL DEFAULT '',        -- 销售方名称
    amount_tax  TEXT NOT NULL DEFAULT '0',       -- 价税合计
    amount      TEXT NOT NULL DEFAULT '0',       -- 不含税金额
    tax         TEXT NOT NULL DEFAULT '0',       -- 税额
    tax_rate    TEXT NOT NULL DEFAULT '0',       -- 税率
    status      TEXT NOT NULL DEFAULT 'pending', -- pending=待认证 / verified=已认证 / rejected=已作废
    memo        TEXT NOT NULL DEFAULT '',
    attach_id   INTEGER NOT NULL DEFAULT 0,      -- 关联凭证附件（可选）
    created_by  TEXT NOT NULL DEFAULT '',
    created_at  TEXT NOT NULL DEFAULT '',
    updated_at  TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_invoice_kind ON invoice(kind, date);
CREATE INDEX IF NOT EXISTS idx_invoice_number ON invoice(number);

-- 采购订单
CREATE TABLE IF NOT EXISTS purchase_order (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    period          INTEGER NOT NULL,
    no              TEXT NOT NULL UNIQUE,
    date            TEXT NOT NULL,
    supplier_code   TEXT NOT NULL DEFAULT '',
    supplier_name   TEXT NOT NULL DEFAULT '',
    status          TEXT NOT NULL DEFAULT 'draft',
    total_amount    TEXT NOT NULL DEFAULT '0',
    total_tax       TEXT NOT NULL DEFAULT '0',
    received_amount TEXT NOT NULL DEFAULT '0',
    prepared_by     TEXT NOT NULL DEFAULT '',
    memo            TEXT NOT NULL DEFAULT '',
    created_at      TEXT NOT NULL DEFAULT '',
    updated_at      TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_po_period ON purchase_order(period, status);

-- 采购订单行
CREATE TABLE IF NOT EXISTS po_line (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    po_id       INTEGER NOT NULL REFERENCES purchase_order(id) ON DELETE CASCADE,
    item_code   TEXT NOT NULL,
    item_name   TEXT NOT NULL DEFAULT '',
    qty_ordered TEXT NOT NULL DEFAULT '0',
    qty_received TEXT NOT NULL DEFAULT '0',
    unit_price  TEXT NOT NULL DEFAULT '0',
    tax_rate    TEXT NOT NULL DEFAULT '0',
    amount      TEXT NOT NULL DEFAULT '0',
    tax_amount  TEXT NOT NULL DEFAULT '0',
    memo        TEXT NOT NULL DEFAULT ''
);

-- 销售订单
CREATE TABLE IF NOT EXISTS sales_order (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    period          INTEGER NOT NULL,
    no              TEXT NOT NULL UNIQUE,
    date            TEXT NOT NULL,
    customer_code   TEXT NOT NULL DEFAULT '',
    customer_name   TEXT NOT NULL DEFAULT '',
    status          TEXT NOT NULL DEFAULT 'draft',
    total_amount    TEXT NOT NULL DEFAULT '0',
    total_tax       TEXT NOT NULL DEFAULT '0',
    shipped_amount  TEXT NOT NULL DEFAULT '0',
    prepared_by     TEXT NOT NULL DEFAULT '',
    memo            TEXT NOT NULL DEFAULT '',
    created_at      TEXT NOT NULL DEFAULT '',
    updated_at      TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_so_period ON sales_order(period, status);

-- 销售订单行
CREATE TABLE IF NOT EXISTS so_line (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    so_id       INTEGER NOT NULL REFERENCES sales_order(id) ON DELETE CASCADE,
    item_code   TEXT NOT NULL,
    item_name   TEXT NOT NULL DEFAULT '',
    qty_ordered TEXT NOT NULL DEFAULT '0',
    qty_shipped TEXT NOT NULL DEFAULT '0',
    unit_price  TEXT NOT NULL DEFAULT '0',
    tax_rate    TEXT NOT NULL DEFAULT '0',
    amount      TEXT NOT NULL DEFAULT '0',
    tax_amount  TEXT NOT NULL DEFAULT '0',
    memo        TEXT NOT NULL DEFAULT ''
);

-- BOM（物料清单）
CREATE TABLE IF NOT EXISTS bom (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    parent_code TEXT NOT NULL,
    child_code  TEXT NOT NULL,
    version     TEXT NOT NULL DEFAULT '',  -- v9: BOM 版本
    qty         TEXT NOT NULL DEFAULT '1',
    loss_rate   TEXT NOT NULL DEFAULT '0',
    seq         INTEGER NOT NULL DEFAULT 0,
    UNIQUE(parent_code, child_code, version)
);
CREATE INDEX IF NOT EXISTS idx_bom_parent ON bom(parent_code);

-- 生产订单
CREATE TABLE IF NOT EXISTS production_order (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    no              TEXT NOT NULL UNIQUE,
    period          INTEGER NOT NULL,
    date            TEXT NOT NULL,
    item_code       TEXT NOT NULL,
    item_name       TEXT NOT NULL,
    planned_qty     TEXT NOT NULL DEFAULT '0',
    completed_qty   TEXT NOT NULL DEFAULT '0',
    status          TEXT NOT NULL DEFAULT 'draft',
    work_center     TEXT NOT NULL DEFAULT '',
    prepared_by     TEXT NOT NULL DEFAULT '',
    memo            TEXT NOT NULL DEFAULT '',
    order_kind      TEXT NOT NULL DEFAULT 'inhouse', -- inhouse 自制 / outsourcing 委外
    supplier_code   TEXT NOT NULL DEFAULT '',
    supplier_name   TEXT NOT NULL DEFAULT '',
    plan_start      TEXT NOT NULL DEFAULT '',   -- 细排计划开工日（链6）
    plan_end        TEXT NOT NULL DEFAULT '',   -- 细排计划完工日（链6）
    created_at      TEXT NOT NULL DEFAULT '',
    updated_at      TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_prod_period ON production_order(period, status);

-- MPS 主生产计划（链6：成品维度净算建议 —— 需求-现有-在制=计划）
CREATE TABLE IF NOT EXISTS mps_plan (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    run_at     TEXT NOT NULL,
    item_code  TEXT NOT NULL,
    item_name  TEXT NOT NULL DEFAULT '',
    demand     TEXT NOT NULL DEFAULT '0',
    on_hand    TEXT NOT NULL DEFAULT '0',
    wip        TEXT NOT NULL DEFAULT '0',
    planned    TEXT NOT NULL DEFAULT '0',
    source     TEXT NOT NULL DEFAULT '',
    due_date   TEXT NOT NULL DEFAULT '',
    status     TEXT NOT NULL DEFAULT 'open', -- open / converted
    created_at TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_mps_run ON mps_plan(run_at);

-- 往来期初明细（按单据，平台迁移导入 v2）：影子挂账——进账龄展示、不参与 FIFO 核销
CREATE TABLE IF NOT EXISTS arap_opening (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    kind       TEXT NOT NULL,                 -- ar 应收 / ap 应付
    party_code TEXT NOT NULL,
    party_name TEXT NOT NULL DEFAULT '',
    doc_no     TEXT NOT NULL DEFAULT '',
    doc_date   TEXT NOT NULL,
    amount     TEXT NOT NULL,
    memo       TEXT NOT NULL DEFAULT '',
    created_by TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_arap_kind ON arap_opening(kind, party_code);

-- 催款单 / 对账函（应收催收闭环：按客商快照未核销明细 + 期初影子挂账）
CREATE TABLE IF NOT EXISTS dunning (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    no          TEXT NOT NULL DEFAULT '',
    period      INTEGER NOT NULL,
    date        TEXT NOT NULL,
    kind        TEXT NOT NULL DEFAULT 'ar',   -- ar 催款 / ap 对账函
    account     TEXT NOT NULL DEFAULT '',
    party_code  TEXT NOT NULL,
    party_name  TEXT NOT NULL DEFAULT '',
    amount      TEXT NOT NULL DEFAULT '0',
    item_count  INTEGER NOT NULL DEFAULT 0,
    status      TEXT NOT NULL DEFAULT 'draft', -- draft/sent/settled/cancelled
    memo        TEXT NOT NULL DEFAULT '',
    created_by  TEXT NOT NULL DEFAULT '',
    created_at  TEXT NOT NULL DEFAULT '',
    sent_at     TEXT NOT NULL DEFAULT '',
    detail_json TEXT NOT NULL DEFAULT '[]'
);
CREATE INDEX IF NOT EXISTS idx_dunning_party ON dunning(kind, party_code, status);

-- 导出计划任务（v32：每日定时写 CSV 到 books_dir/exports/）
CREATE TABLE IF NOT EXISTS export_schedule (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    kind        TEXT NOT NULL,
    period_mode TEXT NOT NULL DEFAULT 'current', -- current / last
    at_time     TEXT NOT NULL DEFAULT '08:00',   -- 每日执行时刻 HH:MM
    enabled     INTEGER NOT NULL DEFAULT 1,
    last_run    TEXT NOT NULL DEFAULT '',        -- 最近执行日期 YYYY-MM-DD（当日去重）
    memo        TEXT NOT NULL DEFAULT '',
    created_by  TEXT NOT NULL DEFAULT '',
    created_at  TEXT NOT NULL DEFAULT ''
);

-- 生产成本归集表
CREATE TABLE IF NOT EXISTS prod_cost (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    po_id       INTEGER NOT NULL REFERENCES production_order(id) ON DELETE CASCADE,
    cost_type   TEXT NOT NULL,
    amount      TEXT NOT NULL DEFAULT '0',
    memo        TEXT NOT NULL DEFAULT '',
    created_at  TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_pc_po ON prod_cost(po_id);

-- ===========================================================================
-- v9：BOM 增强（替代料 / 变更历史）
-- ===========================================================================

-- BOM 替代料（某子件可被替代料替换）
CREATE TABLE IF NOT EXISTS bom_substitute (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    parent_code  TEXT NOT NULL,
    child_code   TEXT NOT NULL,
    substitute   TEXT NOT NULL,        -- 替代料代码
    ratio        TEXT NOT NULL DEFAULT '1', -- 替代比例（1 份原物料 = ratio 份替代料）
    priority     INTEGER NOT NULL DEFAULT 0,
    UNIQUE(parent_code, child_code, substitute)
);
CREATE INDEX IF NOT EXISTS idx_bom_sub ON bom_substitute(parent_code, child_code);

-- BOM 变更历史（审计追溯）
CREATE TABLE IF NOT EXISTS bom_change_log (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    parent_code TEXT NOT NULL,
    action      TEXT NOT NULL,         -- save / delete / add_sub / del_sub
    detail      TEXT NOT NULL DEFAULT '',
    changed_by  TEXT NOT NULL DEFAULT '',
    changed_at  TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_bom_log ON bom_change_log(parent_code);

-- ===========================================================================
-- v11：采购深化（请购单 / 到货 / 付款 / 历史价格）
-- ===========================================================================

-- 采购请购单
CREATE TABLE IF NOT EXISTS purchase_req (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    no          TEXT NOT NULL UNIQUE,
    period      INTEGER NOT NULL,
    date        TEXT NOT NULL,
    item_code   TEXT NOT NULL,
    item_name   TEXT NOT NULL DEFAULT '',
    qty         TEXT NOT NULL DEFAULT '0',
    status      TEXT NOT NULL DEFAULT 'draft', -- draft / approved / ordered / cancelled
    requester   TEXT NOT NULL DEFAULT '',
    memo        TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_pr_period ON purchase_req(period);

-- 采购到货记录
CREATE TABLE IF NOT EXISTS po_receipt (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    po_id       INTEGER NOT NULL,
    period      INTEGER NOT NULL,
    date        TEXT NOT NULL,
    qty         TEXT NOT NULL DEFAULT '0',
    memo        TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_po_rcpt ON po_receipt(po_id);

-- 采购付款记录
CREATE TABLE IF NOT EXISTS po_payment (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    po_id       INTEGER NOT NULL,
    period      INTEGER NOT NULL,
    date        TEXT NOT NULL,
    amount      TEXT NOT NULL DEFAULT '0',
    memo        TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_po_pay ON po_payment(po_id);

-- 采购历史价格（供应商 × 物料）
CREATE TABLE IF NOT EXISTS price_history (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    item_code     TEXT NOT NULL,
    supplier_code TEXT NOT NULL DEFAULT '',
    unit_price    TEXT NOT NULL DEFAULT '0',
    date          TEXT NOT NULL,
    UNIQUE(item_code, supplier_code, date)
);
CREATE INDEX IF NOT EXISTS idx_ph ON price_history(item_code, supplier_code);

-- ===========================================================================
-- v12：销售深化（报价单 / 发货 / 收款）
-- ===========================================================================

-- 销售报价单
CREATE TABLE IF NOT EXISTS quotation (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    no          TEXT NOT NULL UNIQUE,
    period      INTEGER NOT NULL,
    date        TEXT NOT NULL,
    customer_code TEXT NOT NULL DEFAULT '',
    customer_name TEXT NOT NULL DEFAULT '',
    item_code   TEXT NOT NULL,
    item_name   TEXT NOT NULL DEFAULT '',
    qty         TEXT NOT NULL DEFAULT '0',
    unit_price  TEXT NOT NULL DEFAULT '0',
    status      TEXT NOT NULL DEFAULT 'draft', -- draft / approved / converted / cancelled
    prepared_by TEXT NOT NULL DEFAULT '',
    memo        TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_quo_period ON quotation(period);

-- 销售发货记录
CREATE TABLE IF NOT EXISTS so_shipment (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    so_id       INTEGER NOT NULL,
    period      INTEGER NOT NULL,
    date        TEXT NOT NULL,
    qty         TEXT NOT NULL DEFAULT '0',
    memo        TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_so_ship ON so_shipment(so_id);

-- 销售收款记录
CREATE TABLE IF NOT EXISTS so_payment (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    so_id       INTEGER NOT NULL,
    period      INTEGER NOT NULL,
    date        TEXT NOT NULL,
    amount      TEXT NOT NULL DEFAULT '0',
    memo        TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_so_pay ON so_payment(so_id);

-- ===========================================================================
-- v7：多栏账 / 工艺路线 / MRP / 预算多版本 / 审批流 / 报表附注 / 电子档案
-- ===========================================================================

-- 工艺路线（一个产品一条路线，含多道工序）
CREATE TABLE IF NOT EXISTS routing (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    item_code   TEXT NOT NULL,              -- 产成品存货 code
    version     TEXT NOT NULL DEFAULT '',   -- v10: 工艺版本
    seq         INTEGER NOT NULL DEFAULT 0, -- 工序顺序
    op_code     TEXT NOT NULL DEFAULT '',   -- 工序编码
    op_name     TEXT NOT NULL DEFAULT '',   -- 工序名称
    work_center TEXT NOT NULL DEFAULT '',   -- 工作中心
    std_hours   TEXT NOT NULL DEFAULT '0',  -- 标准工时（小时）
    rate        TEXT NOT NULL DEFAULT '0',  -- 小时费率（人工/制造费用）
    qc_required INTEGER NOT NULL DEFAULT 0, -- v31: 工序检验点（完工前需录工序检验单）
    UNIQUE(item_code, version, seq)
);
CREATE INDEX IF NOT EXISTS idx_routing_item ON routing(item_code, version);

-- 工序检验单（v31：合格/返修/报废/让步接收；报废同步扣减计划量）
CREATE TABLE IF NOT EXISTS prod_qc (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    no          TEXT NOT NULL DEFAULT '',
    prod_id     INTEGER NOT NULL,
    item_code   TEXT NOT NULL,
    qty_insp    TEXT NOT NULL DEFAULT '0',
    qty_pass    TEXT NOT NULL DEFAULT '0',
    qty_fail    TEXT NOT NULL DEFAULT '0',
    disposition TEXT NOT NULL DEFAULT '',   -- rework 返修 / scrap 报废 / concession 让步接收
    result      TEXT NOT NULL DEFAULT 'pass', -- pass/partial/fail
    date        TEXT NOT NULL,
    inspector   TEXT NOT NULL DEFAULT '',
    memo        TEXT NOT NULL DEFAULT '',
    created_at  TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_prod_qc_prod ON prod_qc(prod_id);

-- 生产订单工序进度（报工记录）
CREATE TABLE IF NOT EXISTS prod_op (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    po_id       INTEGER NOT NULL REFERENCES production_order(id) ON DELETE CASCADE,
    routing_id  INTEGER NOT NULL DEFAULT 0,
    op_name     TEXT NOT NULL DEFAULT '',
    work_center TEXT NOT NULL DEFAULT '',
    worker      TEXT NOT NULL DEFAULT '',   -- v10: 派工工人
    qty_done    TEXT NOT NULL DEFAULT '0',  -- 累计完工数量
    hours       TEXT NOT NULL DEFAULT '0',  -- 累计实际工时
    status      TEXT NOT NULL DEFAULT 'pending', -- pending / in_progress / done
    memo        TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_prod_op ON prod_op(po_id);

-- 存货计划参数（MRP 用）
CREATE TABLE IF NOT EXISTS item_plan (
    item_code    TEXT PRIMARY KEY,          -- 存货档案 code
    safety_stock TEXT NOT NULL DEFAULT '0', -- 安全库存
    lead_days    INTEGER NOT NULL DEFAULT 0,-- 采购/生产提前期（天）
    lot_size     TEXT NOT NULL DEFAULT '0'  -- 最小批量（0=按净需求）
);

-- MRP 运算结果快照
CREATE TABLE IF NOT EXISTS mrp_result (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    run_at      TEXT NOT NULL DEFAULT '',   -- 运算时间
    item_code   TEXT NOT NULL,
    item_name   TEXT NOT NULL DEFAULT '',
    level       INTEGER NOT NULL DEFAULT 0, -- BOM 层级（0=产成品）
    gross_req   TEXT NOT NULL DEFAULT '0',  -- 毛需求
    on_hand     TEXT NOT NULL DEFAULT '0',  -- 现有库存
    net_req     TEXT NOT NULL DEFAULT '0',  -- 净需求
    planned_qty TEXT NOT NULL DEFAULT '0',  -- 计划量（套用批量后）
    action      TEXT NOT NULL DEFAULT '',   -- produce / purchase / none
    source      TEXT NOT NULL DEFAULT ''    -- 需求来源说明（如 SO-xxx / MO-xxx）
);
CREATE INDEX IF NOT EXISTS idx_mrp_run ON mrp_result(run_at);

-- 预算版本
CREATE TABLE IF NOT EXISTS budget_version (
    key         TEXT PRIMARY KEY,           -- 版本编码
    name        TEXT NOT NULL,              -- 版本名称（如 2026年初稿/调整版）
    is_current  INTEGER NOT NULL DEFAULT 0, -- 是否当前生效版本
    created_at  TEXT NOT NULL DEFAULT '',
    memo        TEXT NOT NULL DEFAULT ''
);

-- 报表附注
CREATE TABLE IF NOT EXISTS report_note (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    report_key  TEXT NOT NULL,              -- balance_sheet / income_statement / cash_flow
    period      INTEGER NOT NULL,
    seq         INTEGER NOT NULL DEFAULT 0,
    title       TEXT NOT NULL DEFAULT '',
    content     TEXT NOT NULL DEFAULT '',
    updated_by  TEXT NOT NULL DEFAULT '',
    updated_at  TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_note_report ON report_note(report_key, period);

-- 审批流实例（通用单据审批：报销单/采购订单/销售订单/生产订单等）
CREATE TABLE IF NOT EXISTS approval (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    biz_kind    TEXT NOT NULL,              -- claim / po / so / prod
    biz_id      INTEGER NOT NULL,
    title       TEXT NOT NULL DEFAULT '',
    applicant   TEXT NOT NULL DEFAULT '',
    current_node INTEGER NOT NULL DEFAULT 0,-- 当前节点序号（从 1 起）
    status      TEXT NOT NULL DEFAULT 'pending', -- pending / approved / rejected / cancelled
    created_at  TEXT NOT NULL DEFAULT '',
    finished_at TEXT,
    UNIQUE(biz_kind, biz_id)
);
CREATE INDEX IF NOT EXISTS idx_approval_biz ON approval(biz_kind, biz_id);

-- 审批流节点记录
CREATE TABLE IF NOT EXISTS approval_step (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    approval_id INTEGER NOT NULL REFERENCES approval(id) ON DELETE CASCADE,
    seq         INTEGER NOT NULL,
    approver    TEXT NOT NULL DEFAULT '',
    action      TEXT NOT NULL DEFAULT '',   -- approve / reject（空=未处理）
    comment     TEXT NOT NULL DEFAULT '',
    acted_at    TEXT
);
CREATE INDEX IF NOT EXISTS idx_apstep ON approval_step(approval_id);

-- 会计电子档案（凭证/账簿/报表的归档快照，含哈希防篡改）
CREATE TABLE IF NOT EXISTS e_archive (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    period      INTEGER NOT NULL,
    kind        TEXT NOT NULL,              -- voucher / ledger / report / balance
    title       TEXT NOT NULL DEFAULT '',
    file_no     TEXT NOT NULL DEFAULT '',   -- 档案号（如 2026-01-记-001）
    content_hash TEXT NOT NULL DEFAULT '',  -- 内容 SHA-256
    payload     TEXT NOT NULL DEFAULT '',   -- JSON 快照
    archived_by TEXT NOT NULL DEFAULT '',
    archived_at TEXT NOT NULL DEFAULT '',
    sealed      INTEGER NOT NULL DEFAULT 1, -- 归档即封存，不可改
    UNIQUE(period, kind, file_no)
);
CREATE INDEX IF NOT EXISTS idx_archive_period ON e_archive(period, kind);

-- ===========================================================================
-- v8：库存盘点 / 批次管理
-- ===========================================================================

-- 库存盘点单（盘盈盘亏）
CREATE TABLE IF NOT EXISTS stock_count (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    no          TEXT NOT NULL UNIQUE,
    period      INTEGER NOT NULL,
    date        TEXT NOT NULL,
    warehouse   TEXT NOT NULL DEFAULT '',
    status      TEXT NOT NULL DEFAULT 'draft', -- draft / posted
    prepared_by TEXT NOT NULL DEFAULT '',
    memo        TEXT NOT NULL DEFAULT '',
    created_at  TEXT NOT NULL DEFAULT '',
    UNIQUE(no)
);
CREATE INDEX IF NOT EXISTS idx_sc_period ON stock_count(period);

-- 盘点单明细（账面数 vs 实盘数）
CREATE TABLE IF NOT EXISTS stock_count_line (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    sc_id       INTEGER NOT NULL REFERENCES stock_count(id) ON DELETE CASCADE,
    item        TEXT NOT NULL,
    book_qty    TEXT NOT NULL DEFAULT '0',
    count_qty   TEXT NOT NULL DEFAULT '0',
    memo        TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_scl ON stock_count_line(sc_id);

-- ===========================================================================
-- v14：库存深度（序列号 / 多单位换算 / 库存状态）
-- ===========================================================================

-- 存货序列号（一码一物，唯一）
CREATE TABLE IF NOT EXISTS item_serial (
    serial     TEXT PRIMARY KEY,
    item       TEXT NOT NULL,
    batch_no   TEXT NOT NULL DEFAULT '',
    status     TEXT NOT NULL DEFAULT 'in', -- in=在库 / out=已出库 / scrapped=报废
    in_date    TEXT NOT NULL DEFAULT '',
    out_date   TEXT,
    memo       TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_serial_item ON item_serial(item, status);

-- 存货多单位换算（1 主单位 = factor 辅助单位）
CREATE TABLE IF NOT EXISTS item_unit (
    item       TEXT PRIMARY KEY,
    base_unit  TEXT NOT NULL DEFAULT '',   -- 主单位（如 个）
    alt_unit   TEXT NOT NULL DEFAULT '',   -- 辅助单位（如 箱）
    factor     TEXT NOT NULL DEFAULT '1'   -- 1 主单位 = factor 辅助单位
);

-- ===========================================================================
-- v15：采购/销售深度（暂估 / 对账 / 配额 / 订单变更）
-- ===========================================================================

-- 采购暂估（入库先暂估、发票后冲回）
CREATE TABLE IF NOT EXISTS po_estimate (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    po_id       INTEGER NOT NULL,
    period      INTEGER NOT NULL,
    item        TEXT NOT NULL,
    est_amount  TEXT NOT NULL DEFAULT '0', -- 暂估金额
    settled     INTEGER NOT NULL DEFAULT 0 -- 是否已冲回
);
CREATE INDEX IF NOT EXISTS idx_pe_po ON po_estimate(po_id);

-- 供应商配额（配额期间 × 供应商 × 物料）
CREATE TABLE IF NOT EXISTS supplier_quota (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    period        INTEGER NOT NULL,
    supplier_code TEXT NOT NULL,
    item          TEXT NOT NULL,
    quota_qty     TEXT NOT NULL DEFAULT '0',
    used_qty      TEXT NOT NULL DEFAULT '0',
    UNIQUE(period, supplier_code, item)
);

-- 订单变更历史（采购/销售订单共用的追溯日志）
CREATE TABLE IF NOT EXISTS order_change_log (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    order_type  TEXT NOT NULL,             -- po / so
    order_id    INTEGER NOT NULL,
    field       TEXT NOT NULL DEFAULT '',
    old_value   TEXT NOT NULL DEFAULT '',
    new_value   TEXT NOT NULL DEFAULT '',
    changed_by  TEXT NOT NULL DEFAULT '',
    changed_at  TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_ocl ON order_change_log(order_type, order_id);

-- ===========================================================================
-- v16：资金（票据 / 融资）+ 存货计价配置
-- ===========================================================================

-- 票据（应收票据 / 应付票据）
CREATE TABLE IF NOT EXISTS bill (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    kind        TEXT NOT NULL DEFAULT 'receivable', -- receivable=应收 / payable=应付
    no          TEXT NOT NULL DEFAULT '',
    period      INTEGER NOT NULL,
    issue_date  TEXT NOT NULL,             -- 出票日
    due_date    TEXT NOT NULL,             -- 到期日
    counterpart TEXT NOT NULL DEFAULT '',  -- 对方单位
    bank        TEXT NOT NULL DEFAULT '',  -- 承兑银行
    amount      TEXT NOT NULL DEFAULT '0', -- 票面金额
    status      TEXT NOT NULL DEFAULT 'in_hand', -- in_hand/endorsed/discounted/matured/paid
    handled_date TEXT,                     -- 背书/贴现/兑付日期
    memo        TEXT NOT NULL DEFAULT '',
    created_by  TEXT NOT NULL DEFAULT '',
    created_at  TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_bill_kind ON bill(kind, status);

-- 融资（借款台账）
CREATE TABLE IF NOT EXISTS loan (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    kind        TEXT NOT NULL DEFAULT 'borrow', -- borrow=借款 / lend=放款
    no          TEXT NOT NULL DEFAULT '',
    bank        TEXT NOT NULL DEFAULT '',  -- 对方金融机构 / 单位
    principal   TEXT NOT NULL DEFAULT '0', -- 本金
    rate_pct    TEXT NOT NULL DEFAULT '0', -- 年利率（%）
    start_date  TEXT NOT NULL,             -- 起息日
    end_date    TEXT NOT NULL,             -- 到期日
    status      TEXT NOT NULL DEFAULT 'active', -- active / settled
    memo        TEXT NOT NULL DEFAULT '',
    created_by  TEXT NOT NULL DEFAULT '',
    created_at  TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_loan_kind ON loan(kind, status);

-- 现金盘点（出纳：账面 vs 实盘，差异生成盘盈盘亏凭证）
CREATE TABLE IF NOT EXISTS cash_count (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    period       INTEGER NOT NULL,
    date         TEXT NOT NULL,             -- 盘点日期
    account_code TEXT NOT NULL DEFAULT '1001',
    book_amount  TEXT NOT NULL DEFAULT '0', -- 盘点时账面余额快照（借正，仅已记账口径）
    counted      TEXT NOT NULL DEFAULT '0', -- 实盘金额
    diff         TEXT NOT NULL DEFAULT '0', -- 差异 = 实盘 − 账面（正=盘盈 负=盘亏）
    memo         TEXT NOT NULL DEFAULT '',
    voucher_id   INTEGER,                   -- 盘盈盘亏凭证
    created_by   TEXT NOT NULL DEFAULT '',
    created_at   TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_cash_count_period ON cash_count(period, date);

-- 日清标记（出纳日记账：某科目某日已核对）
CREATE TABLE IF NOT EXISTS day_clear (
    account_code TEXT NOT NULL,
    date         TEXT NOT NULL,             -- YYYY-MM-DD
    cleared_by   TEXT NOT NULL DEFAULT '',
    cleared_at   TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (account_code, date)
);

-- 支票登记簿（出纳备查簿：开出/作废；账务由凭证体现，本表不入账）
CREATE TABLE IF NOT EXISTS check_register (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    no           TEXT NOT NULL DEFAULT '',  -- 支票号
    kind         TEXT NOT NULL DEFAULT 'transfer', -- cash=现金支票 / transfer=转账支票
    bank_account TEXT NOT NULL DEFAULT '',  -- 付款银行科目（如 100201）
    payee        TEXT NOT NULL DEFAULT '',  -- 收款人
    amount       TEXT NOT NULL DEFAULT '0',
    issued_date  TEXT NOT NULL DEFAULT '',  -- 开出日期
    status       TEXT NOT NULL DEFAULT 'issued', -- issued=已开出 / void=已作废
    memo         TEXT NOT NULL DEFAULT '',
    created_by   TEXT NOT NULL DEFAULT '',
    created_at   TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_check_status ON check_register(status, issued_date);

-- 员工借支（出纳：预借 → 支付 → 冲账核销）
CREATE TABLE IF NOT EXISTS advance (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    no                TEXT NOT NULL DEFAULT '',
    period            INTEGER NOT NULL,
    date              TEXT NOT NULL,        -- 借支日期
    employee          TEXT NOT NULL DEFAULT '', -- 借支人
    purpose           TEXT NOT NULL DEFAULT '', -- 事由
    amount            TEXT NOT NULL DEFAULT '0', -- 借支金额
    pay_account       TEXT NOT NULL DEFAULT '1001', -- 支付账户
    status            TEXT NOT NULL DEFAULT 'approved', -- approved/paid/settled
    paid_date         TEXT,
    paid_voucher_id   INTEGER,              -- 支付凭证（借 其他应收款 / 贷 资金）
    settle_date       TEXT,
    settle_voucher_id INTEGER,              -- 核销凭证（借 费用 + 退回 / 贷 其他应收款）
    expense_account   TEXT NOT NULL DEFAULT '660201', -- 冲账费用科目
    memo              TEXT NOT NULL DEFAULT '',
    created_by        TEXT NOT NULL DEFAULT '',
    created_at        TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_advance_status ON advance(status, date);

-- 出纳交接班（对标金蝶出纳交接：交班快照现金/银行结存、在库票据、未日清账户，接班人确认）
CREATE TABLE IF NOT EXISTS cash_shift (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    period          INTEGER NOT NULL,
    date            TEXT NOT NULL,
    from_user       TEXT NOT NULL,
    to_user         TEXT NOT NULL DEFAULT '',
    cash_balance    TEXT NOT NULL DEFAULT '0',
    bank_balance    TEXT NOT NULL DEFAULT '0',
    bill_count      INTEGER NOT NULL DEFAULT 0,
    bill_amount     TEXT NOT NULL DEFAULT '0',
    uncleared       INTEGER NOT NULL DEFAULT 0,
    memo            TEXT NOT NULL DEFAULT '',
    status          TEXT NOT NULL DEFAULT 'open',   -- open/confirmed/cancelled
    created_at      TEXT NOT NULL DEFAULT '',
    confirmed_by    TEXT NOT NULL DEFAULT '',
    confirmed_at    TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_cash_shift_date ON cash_shift(date, status);

-- 收付款单（对标金蝶收款单/付款单：出纳资金动作 → 凭证 + 自动核销）
CREATE TABLE IF NOT EXISTS receipt_doc (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    no           TEXT NOT NULL DEFAULT '',
    period       INTEGER NOT NULL,
    date         TEXT NOT NULL,
    kind         TEXT NOT NULL DEFAULT 'receipt', -- receipt=收款 / payment=付款
    fund_account TEXT NOT NULL DEFAULT '100201',
    party        TEXT NOT NULL DEFAULT '',       -- 往来单位辅助编码
    amount       TEXT NOT NULL DEFAULT '0',
    memo         TEXT NOT NULL DEFAULT '',
    voucher_id   INTEGER,
    status       TEXT NOT NULL DEFAULT 'audited', -- draft 待审 / audited 已审（历史单据均有凭证）
    created_by   TEXT NOT NULL DEFAULT '',
    created_at   TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_receipt_doc_period ON receipt_doc(period, kind);

-- 存货盘点单（账面按仓库快照 + 实盘录入 → 应用生成其他入库/出库流水与盘盈盘亏凭证）
CREATE TABLE IF NOT EXISTS inv_count (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    no          TEXT NOT NULL DEFAULT '',
    period      INTEGER NOT NULL,
    date        TEXT NOT NULL,
    warehouse   TEXT NOT NULL DEFAULT '',   -- 空 = 全部仓库合计
    memo        TEXT NOT NULL DEFAULT '',
    status      TEXT NOT NULL DEFAULT 'draft', -- draft 草稿 / applied 已应用
    voucher_id  INTEGER,
    applied_at  TEXT NOT NULL DEFAULT '',
    created_by  TEXT NOT NULL DEFAULT '',
    created_at  TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS inv_count_line (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    count_id  INTEGER NOT NULL,
    item      TEXT NOT NULL,
    batch_no  TEXT NOT NULL DEFAULT '',
    book_qty  TEXT NOT NULL DEFAULT '0',
    count_qty TEXT NOT NULL DEFAULT '0',
    memo      TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_inv_count ON inv_count(period, status);
CREATE INDEX IF NOT EXISTS idx_inv_count_line ON inv_count_line(count_id);

-- 存货批次主数据（对标金蝶批号/保质期管理；批次余额 = stock_move 按 (item,batch_no) 汇总）
CREATE TABLE IF NOT EXISTS stock_batch (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    item            TEXT NOT NULL,
    batch_no        TEXT NOT NULL,
    production_date TEXT NOT NULL DEFAULT '',
    expiry_date     TEXT NOT NULL DEFAULT '',   -- = 生产日期 + 保质期天数（存货档案属性）
    warehouse       TEXT NOT NULL DEFAULT '',
    location        TEXT NOT NULL DEFAULT '',   -- 初始库位（库位主数据 code）
    memo            TEXT NOT NULL DEFAULT '',
    created_by      TEXT NOT NULL DEFAULT '',
    created_at      TEXT NOT NULL DEFAULT '',
    UNIQUE (item, batch_no)
);
-- 库位主数据（对标金蝶货位：存储/拣货/隔离）
CREATE TABLE IF NOT EXISTS stock_location (
    id   INTEGER PRIMARY KEY AUTOINCREMENT,
    code TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    kind TEXT NOT NULL DEFAULT 'storage', -- storage 存储 / pick 拣货 / quarantine 隔离
    memo TEXT NOT NULL DEFAULT ''
);

-- 单据下推勾稽（对标金蝶 源单→目标单 追溯；执行类子单据靠各自外键，不入本表）
CREATE TABLE IF NOT EXISTS doc_link (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    src_type   TEXT NOT NULL,    -- req / po / so / quote ...
    src_id     INTEGER NOT NULL,
    dst_type   TEXT NOT NULL,
    dst_id     INTEGER NOT NULL,
    memo       TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT '',
    UNIQUE (src_type, src_id, dst_type, dst_id)
);
CREATE INDEX IF NOT EXISTS idx_doc_link_dst ON doc_link(dst_type, dst_id);

-- 质检单（对标金蝶来料检验：合格留库，不合格自动按订单单价退货冲减库存）
CREATE TABLE IF NOT EXISTS qc_order (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    po_id      INTEGER NOT NULL,
    item       TEXT NOT NULL DEFAULT '',
    qty_insp   TEXT NOT NULL DEFAULT '0',
    qty_pass   TEXT NOT NULL DEFAULT '0',
    qty_fail   TEXT NOT NULL DEFAULT '0',
    result     TEXT NOT NULL DEFAULT 'pass',  -- pass / fail / partial
    inspector  TEXT NOT NULL DEFAULT '',
    date       TEXT NOT NULL DEFAULT '',
    memo       TEXT NOT NULL DEFAULT '',
    created_by TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_qc_po ON qc_order(po_id);

-- 发货通知单（对标金蝶发货通知：订单确认后的备货指令，出库后自动完成）
CREATE TABLE IF NOT EXISTS ship_notice (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    so_id      INTEGER NOT NULL,
    qty        TEXT NOT NULL,
    date       TEXT NOT NULL DEFAULT '',
    status     TEXT NOT NULL DEFAULT 'pending', -- pending / shipped
    memo       TEXT NOT NULL DEFAULT '',
    created_by TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_sn_so ON ship_notice(so_id);

-- 可视化工作流（对标金蝶审批流设计器）：流程定义 + 节点 + 连线 + 运行实例
CREATE TABLE IF NOT EXISTS workflow_flow (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    name       TEXT NOT NULL,
    biz_type   TEXT NOT NULL,                -- quotation / purchase_req / claim / receipt
    status     TEXT NOT NULL DEFAULT 'draft', -- draft / published
    created_by TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS workflow_node (
    id           TEXT NOT NULL,          -- 客户端节点 id（如 n1）
    flow_id      INTEGER NOT NULL,
    type         TEXT NOT NULL,          -- start / approve / condition / message
    name         TEXT NOT NULL DEFAULT '',
    participants TEXT NOT NULL DEFAULT '[]',  -- 允许角色 code JSON 数组（空 = 有审批权即可）
    strategy     TEXT NOT NULL DEFAULT 'all', -- 会签策略 all/any（v1 单人通过即过，字段存档）
    reject_to    TEXT NOT NULL DEFAULT '',    -- 显式驳回目标节点（优先于 reject 连线）
    seq          INTEGER NOT NULL DEFAULT 0,
    x            REAL NOT NULL DEFAULT 0,
    y            REAL NOT NULL DEFAULT 0,
    PRIMARY KEY (flow_id, id)
);
CREATE TABLE IF NOT EXISTS workflow_edge (
    id        TEXT NOT NULL,             -- 客户端边 id
    flow_id   INTEGER NOT NULL,
    from_node TEXT NOT NULL,
    to_node   TEXT NOT NULL,
    kind      TEXT NOT NULL DEFAULT 'normal', -- normal / reject
    condition TEXT NOT NULL DEFAULT '',
    seq       INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (flow_id, id)
);
CREATE TABLE IF NOT EXISTS workflow_instance (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    flow_id      INTEGER NOT NULL,
    biz_type     TEXT NOT NULL,
    biz_id       INTEGER NOT NULL,
    current_node TEXT NOT NULL DEFAULT '',
    status       TEXT NOT NULL DEFAULT 'running', -- running / approved / rejected
    log_json     TEXT NOT NULL DEFAULT '[]',      -- 运行轨迹 [{node,action,who,at}]
    created_at   TEXT NOT NULL DEFAULT '',
    UNIQUE (biz_type, biz_id)                     -- 一单一个活动实例
);
CREATE INDEX IF NOT EXISTS idx_wf_flow_biz ON workflow_flow(biz_type, status);

-- 存货计价方式配置（按存货档案 code）
CREATE TABLE IF NOT EXISTS item_cost_method (
    item          TEXT PRIMARY KEY,
    method        TEXT NOT NULL DEFAULT 'moving_average', -- moving_average/fifo/specific/standard/month_average
    standard_cost TEXT NOT NULL DEFAULT '0'
);

"#;

/// v1 → v2 需要新增到既有表上的列
///
/// 老账套升级时 `CREATE TABLE IF NOT EXISTS` 不会补列，所以要显式 ALTER。
/// 每条都先探测列是否存在，重复执行安全。
const MIGRATE_V2: &[(&str, &str, &str)] = &[
    ("user", "pwd_changed_at", "TEXT NOT NULL DEFAULT ''"),
    ("user", "must_change_pwd", "INTEGER NOT NULL DEFAULT 0"),
    ("user", "locked_until", "TEXT"),
    ("user", "last_login_at", "TEXT NOT NULL DEFAULT ''"),
    ("user", "data_scope_json", "TEXT NOT NULL DEFAULT '{}'"),
    ("voucher_template", "freq", "TEXT NOT NULL DEFAULT ''"),
    ("voucher_template", "start_period", "INTEGER NOT NULL DEFAULT 0"),
    ("voucher_template", "end_period", "INTEGER NOT NULL DEFAULT 0"),
    ("voucher_template", "last_period", "INTEGER NOT NULL DEFAULT 0"),
    ("voucher_template", "active", "INTEGER NOT NULL DEFAULT 0"),
];

/// v2 → v3：自动转账补对方科目
const MIGRATE_V3: &[(&str, &str, &str)] = &[
    ("auto_transfer", "offset_account", "TEXT NOT NULL DEFAULT ''"),
    ("payroll", "additional", "TEXT NOT NULL DEFAULT '0'"),
];

/// v4 → v5：账号设备绑定
const MIGRATE_V5: &[(&str, &str, &str)] = &[
    ("user", "device_id", "TEXT NOT NULL DEFAULT ''"),
    ("user", "device_name", "TEXT NOT NULL DEFAULT ''"),
];

/// v5 → v6：供应链深化（采购订单 / 销售订单 / BOM / 生产订单）
const MIGRATE_V6: &[(&str, &str, &str)] = &[
    // 采购订单表
    ("purchase_order", "id", "INTEGER PRIMARY KEY AUTOINCREMENT"),
    ("purchase_order", "no", "TEXT NOT NULL UNIQUE"),
    ("purchase_order", "supplier_code", "TEXT NOT NULL DEFAULT ''"),
    ("purchase_order", "supplier_name", "TEXT NOT NULL DEFAULT ''"),
    ("purchase_order", "status", "TEXT NOT NULL DEFAULT 'draft'"),
    ("purchase_order", "total_amount", "TEXT NOT NULL DEFAULT '0'"),
    ("purchase_order", "total_tax", "TEXT NOT NULL DEFAULT '0'"),
    ("purchase_order", "received_amount", "TEXT NOT NULL DEFAULT '0'"),
    ("purchase_order", "prepared_by", "TEXT NOT NULL DEFAULT ''"),
    ("purchase_order", "memo", "TEXT NOT NULL DEFAULT ''"),
    ("purchase_order", "created_at", "TEXT NOT NULL DEFAULT ''"),
    ("purchase_order", "updated_at", "TEXT NOT NULL DEFAULT ''"),
    // 采购订单行表
    ("po_line", "id", "INTEGER PRIMARY KEY AUTOINCREMENT"),
    ("po_line", "po_id", "INTEGER NOT NULL"),
    ("po_line", "item_code", "TEXT NOT NULL"),
    ("po_line", "item_name", "TEXT NOT NULL DEFAULT ''"),
    ("po_line", "qty_ordered", "TEXT NOT NULL DEFAULT '0'"),
    ("po_line", "qty_received", "TEXT NOT NULL DEFAULT '0'"),
    ("po_line", "unit_price", "TEXT NOT NULL DEFAULT '0'"),
    ("po_line", "tax_rate", "TEXT NOT NULL DEFAULT '0'"),
    ("po_line", "amount", "TEXT NOT NULL DEFAULT '0'"),
    ("po_line", "tax_amount", "TEXT NOT NULL DEFAULT '0'"),
    ("po_line", "memo", "TEXT NOT NULL DEFAULT ''"),
    // 销售订单表
    ("sales_order", "id", "INTEGER PRIMARY KEY AUTOINCREMENT"),
    ("sales_order", "no", "TEXT NOT NULL UNIQUE"),
    ("sales_order", "customer_code", "TEXT NOT NULL DEFAULT ''"),
    ("sales_order", "customer_name", "TEXT NOT NULL DEFAULT ''"),
    ("sales_order", "status", "TEXT NOT NULL DEFAULT 'draft'"),
    ("sales_order", "total_amount", "TEXT NOT NULL DEFAULT '0'"),
    ("sales_order", "total_tax", "TEXT NOT NULL DEFAULT '0'"),
    ("sales_order", "shipped_amount", "TEXT NOT NULL DEFAULT '0'"),
    ("sales_order", "prepared_by", "TEXT NOT NULL DEFAULT ''"),
    ("sales_order", "memo", "TEXT NOT NULL DEFAULT ''"),
    ("sales_order", "created_at", "TEXT NOT NULL DEFAULT ''"),
    ("sales_order", "updated_at", "TEXT NOT NULL DEFAULT ''"),
    // 销售订单行表
    ("so_line", "id", "INTEGER PRIMARY KEY AUTOINCREMENT"),
    ("so_line", "so_id", "INTEGER NOT NULL"),
    ("so_line", "item_code", "TEXT NOT NULL"),
    ("so_line", "item_name", "TEXT NOT NULL DEFAULT ''"),
    ("so_line", "qty_ordered", "TEXT NOT NULL DEFAULT '0'"),
    ("so_line", "qty_shipped", "TEXT NOT NULL DEFAULT '0'"),
    ("so_line", "unit_price", "TEXT NOT NULL DEFAULT '0'"),
    ("so_line", "tax_rate", "TEXT NOT NULL DEFAULT '0'"),
    ("so_line", "amount", "TEXT NOT NULL DEFAULT '0'"),
    ("so_line", "tax_amount", "TEXT NOT NULL DEFAULT '0'"),
    ("so_line", "memo", "TEXT NOT NULL DEFAULT ''"),
    // BOM表
    ("bom", "id", "INTEGER PRIMARY KEY AUTOINCREMENT"),
    ("bom", "parent_code", "TEXT NOT NULL"),
    ("bom", "child_code", "TEXT NOT NULL"),
    ("bom", "qty", "TEXT NOT NULL DEFAULT '1'"),
    ("bom", "loss_rate", "TEXT NOT NULL DEFAULT '0'"),
    ("bom", "seq", "INTEGER NOT NULL DEFAULT 0"),
    // 生产订单表
    ("production_order", "id", "INTEGER PRIMARY KEY AUTOINCREMENT"),
    ("production_order", "no", "TEXT NOT NULL UNIQUE"),
    ("production_order", "item_code", "TEXT NOT NULL"),
    ("production_order", "item_name", "TEXT NOT NULL"),
    ("production_order", "planned_qty", "TEXT NOT NULL DEFAULT '0'"),
    ("production_order", "completed_qty", "TEXT NOT NULL DEFAULT '0'"),
    ("production_order", "status", "TEXT NOT NULL DEFAULT 'draft'"),
    ("production_order", "work_center", "TEXT NOT NULL DEFAULT ''"),
    ("production_order", "prepared_by", "TEXT NOT NULL DEFAULT ''"),
    ("production_order", "memo", "TEXT NOT NULL DEFAULT ''"),
    ("production_order", "created_at", "TEXT NOT NULL DEFAULT ''"),
    ("production_order", "updated_at", "TEXT NOT NULL DEFAULT ''"),
];

/// v6 → v7：深度制造（工艺路线 / MRP / 工序报工）+ 管理会计（预算多版本 /
/// 报表附注 / 审批流 / 电子档案）。这些全是新表，DDL 的 IF NOT EXISTS 已覆盖。
/// 唯一需要迁移的是 budget 表：UNIQUE 约束从 (period,account,dept) 扩展为
/// (period,account,dept,version)，SQLite 的 ALTER 改不了约束，必须重建表。
///
/// 其他 v7 新表（routing / prod_op / item_plan / mrp_result / budget_version /
/// report_note / approval / approval_step / e_archive）由 DDL 的
/// `CREATE TABLE IF NOT EXISTS` 在 init 时自动创建。

/// v6 → v7：重建 budget 表以支持多版本
fn migrate_v7(conn: &Connection) -> Result<(), DbError> {
    if column_exists(conn, "budget", "version")? {
        return Ok(()); // 已是新结构
    }
    conn.execute_batch(
        "CREATE TABLE budget_new (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            period      INTEGER NOT NULL,
            account_code TEXT NOT NULL,
            dept        TEXT NOT NULL DEFAULT '',
            amount      TEXT NOT NULL DEFAULT '0',
            memo        TEXT NOT NULL DEFAULT '',
            version     TEXT NOT NULL DEFAULT '',
            UNIQUE(period, account_code, dept, version)
         );
         INSERT INTO budget_new(id,period,account_code,dept,amount,memo,version)
             SELECT id,period,account_code,dept,amount,memo,'' FROM budget;
         DROP TABLE budget;
         ALTER TABLE budget_new RENAME TO budget;
         CREATE INDEX IF NOT EXISTS idx_budget_period ON budget(period);",
    )?;
    Ok(())
}

/// v7 → v8：库存盘点表（新表，DDL 覆盖）+ stock_move 补批次列
const MIGRATE_V8: &[(&str, &str, &str)] = &[
    ("stock_move", "batch_no", "TEXT NOT NULL DEFAULT ''"),
];

/// v16 → v17：用户权限逐项覆盖（deny_perms_json）
const MIGRATE_V17: &[(&str, &str, &str)] = &[
    ("user", "deny_perms_json", "TEXT NOT NULL DEFAULT '[]'"),
];

/// v18 → v19：资金台账挂凭证（票据/融资与总账联动，出纳资金台账可追溯到账务）
const MIGRATE_V19: &[(&str, &str, &str)] = &[
    ("bill", "voucher_id", "INTEGER"),
    ("loan", "voucher_id", "INTEGER"),
    ("loan", "settle_voucher_id", "INTEGER"),
    ("loan", "settle_date", "TEXT"),
];

/// v19 → v20：工资发放/社保缴纳凭证回链（发放状态可见）
const MIGRATE_V20: &[(&str, &str, &str)] = &[
    ("payroll", "paid_voucher_id", "INTEGER"),
    ("payroll", "social_voucher_id", "INTEGER"),
];

/// v20 → v21：兼任岗位（多角色）。有效权限 = 主岗位 + roles_json 并集 − deny。
const MIGRATE_V21: &[(&str, &str, &str)] =
    &[("user", "roles_json", "TEXT NOT NULL DEFAULT '[]'")];

/// v21 → v22：收付款单审核流（历史单据均已生成凭证 → 默认 'audited'）；
/// 存货盘点两张新表由 DDL 直接建（每次 migrate 都会执行 CREATE IF NOT EXISTS）。
const MIGRATE_V22: &[(&str, &str, &str)] =
    &[("receipt_doc", "status", "TEXT NOT NULL DEFAULT 'audited'")];

/// v22 → v23：委外加工（生产订单加 委外类型 与 供应商 两组列）
const MIGRATE_V23: &[(&str, &str, &str)] = &[
    ("production_order", "order_kind", "TEXT NOT NULL DEFAULT 'inhouse'"),
    ("production_order", "supplier_code", "TEXT NOT NULL DEFAULT ''"),
    ("production_order", "supplier_name", "TEXT NOT NULL DEFAULT ''"),
];

/// v23 → v24：来料检验状态机（stock_move.qc_status：''可用 / pending待检 / quarantine隔离）
const MIGRATE_V24: &[(&str, &str, &str)] =
    &[("stock_move", "qc_status", "TEXT NOT NULL DEFAULT ''")];

/// v24 → v25：批次盘点（inv_count_line.batch_no：空 = 整仓口径，向后兼容）
const MIGRATE_V25: &[(&str, &str, &str)] =
    &[("inv_count_line", "batch_no", "TEXT NOT NULL DEFAULT ''")];

/// v30 → v31：工序检验点（routing.qc_required；prod_qc 为纯新表走 DDL）
const MIGRATE_V31: &[(&str, &str, &str)] =
    &[("routing", "qc_required", "INTEGER NOT NULL DEFAULT 0")];

/// v25 → v26：细排计划日期（production_order.plan_start/plan_end）
const MIGRATE_V26: &[(&str, &str, &str)] = &[
    ("production_order", "plan_start", "TEXT NOT NULL DEFAULT ''"),
    ("production_order", "plan_end", "TEXT NOT NULL DEFAULT ''"),
];

/// v8 → v9：BOM 表 UNIQUE 从 (parent,child) 扩展为 (parent,child,version)，
fn migrate_v9(conn: &Connection) -> Result<(), DbError> {
    if column_exists(conn, "bom", "version")? {
        return Ok(());
    }
    conn.execute_batch(
        "CREATE TABLE bom_new (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            parent_code TEXT NOT NULL,
            child_code  TEXT NOT NULL,
            version     TEXT NOT NULL DEFAULT '',
            qty         TEXT NOT NULL DEFAULT '1',
            loss_rate   TEXT NOT NULL DEFAULT '0',
            seq         INTEGER NOT NULL DEFAULT 0,
            UNIQUE(parent_code, child_code, version)
         );
         INSERT INTO bom_new(id,parent_code,child_code,version,qty,loss_rate,seq)
             SELECT id,parent_code,child_code,'',qty,loss_rate,seq FROM bom;
         DROP TABLE bom;
         ALTER TABLE bom_new RENAME TO bom;
         CREATE INDEX IF NOT EXISTS idx_bom_parent ON bom(parent_code);",
    )?;
    Ok(())
}

/// v9 → v10：routing 表 UNIQUE 从 (item_code,seq) 扩展为 (item_code,version,seq)，
/// 需重建；prod_op 补 worker 列。
fn migrate_v10(conn: &Connection) -> Result<(), DbError> {
    if column_exists(conn, "routing", "version")? {
        return Ok(());
    }
    conn.execute_batch(
        "CREATE TABLE routing_new (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            item_code   TEXT NOT NULL,
            version     TEXT NOT NULL DEFAULT '',
            seq         INTEGER NOT NULL DEFAULT 0,
            op_code     TEXT NOT NULL DEFAULT '',
            op_name     TEXT NOT NULL DEFAULT '',
            work_center TEXT NOT NULL DEFAULT '',
            std_hours   TEXT NOT NULL DEFAULT '0',
            rate        TEXT NOT NULL DEFAULT '0',
            UNIQUE(item_code, version, seq)
         );
         INSERT INTO routing_new(id,item_code,version,seq,op_code,op_name,work_center,std_hours,rate)
             SELECT id,item_code,'',seq,op_code,op_name,work_center,std_hours,rate FROM routing;
         DROP TABLE routing;
         ALTER TABLE routing_new RENAME TO routing;
         CREATE INDEX IF NOT EXISTS idx_routing_item ON routing(item_code, version);",
    )?;
    if !column_exists(conn, "prod_op", "worker")? {
        conn.execute("ALTER TABLE prod_op ADD COLUMN worker TEXT NOT NULL DEFAULT ''", [])?;
    }
    Ok(())
}

/// 表上是否已有引用指定列的 FOREIGN KEY（迁移判存用）
fn has_fk(conn: &Connection, table: &str, column: &str) -> Result<bool, DbError> {
    let mut st = conn.prepare(&format!("PRAGMA foreign_key_list({table})"))?;
    let mut rows = st.query([])?;
    while let Some(r) = rows.next()? {
        if r.get::<_, String>(3)? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

/// v17 → v18：给核销与银行勾对补外键。
///
/// `settle_record.from_entry/to_entry`、`bank_statement.entry_id` 指向
/// `voucher_entry(id)`，但此前没有外键：凭证保存会整表 DELETE 再插入分录，
/// 删除凭证会级联删分录——两处引用都会变成指向不存在行的"悬空引用"，
/// 表现为核销静默消失、银行流水显示已勾对但挂空。
/// SQLite 不支持 ALTER 加外键，只能重建表。
fn migrate_v18(conn: &Connection) -> Result<(), DbError> {
    let bank_done = has_fk(conn, "bank_statement", "entry_id")?;
    let settle_done = has_fk(conn, "settle_record", "from_entry")?;
    if bank_done && settle_done {
        return Ok(());
    }
    // 重建前先清掉历史悬空引用，否则带外键的新表会拒绝那些行
    conn.execute_batch(
        "UPDATE bank_statement SET entry_id=NULL, matched_at=NULL, matched_by=NULL
           WHERE entry_id IS NOT NULL
             AND entry_id NOT IN (SELECT id FROM voucher_entry);
         DELETE FROM settle_record
           WHERE from_entry NOT IN (SELECT id FROM voucher_entry)
              OR to_entry NOT IN (SELECT id FROM voucher_entry);",
    )?;
    if !bank_done {
        conn.execute_batch(
            "CREATE TABLE bank_statement_new (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                period       INTEGER NOT NULL,
                account_code TEXT NOT NULL,
                biz_date     TEXT NOT NULL,
                summary      TEXT NOT NULL DEFAULT '',
                settle_no    TEXT NOT NULL DEFAULT '',
                debit        TEXT NOT NULL DEFAULT '0',
                credit       TEXT NOT NULL DEFAULT '0',
                balance      TEXT NOT NULL DEFAULT '0',
                entry_id     INTEGER REFERENCES voucher_entry(id) ON DELETE SET NULL,
                matched_at   TEXT,
                matched_by   TEXT
             );
             INSERT INTO bank_statement_new SELECT * FROM bank_statement;
             DROP TABLE bank_statement;
             ALTER TABLE bank_statement_new RENAME TO bank_statement;
             CREATE INDEX IF NOT EXISTS idx_stmt_period ON bank_statement(period, account_code);
             CREATE INDEX IF NOT EXISTS idx_stmt_entry  ON bank_statement(entry_id);",
        )?;
    }
    if !settle_done {
        conn.execute_batch(
            "CREATE TABLE settle_record_new (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                period       INTEGER NOT NULL,
                account_code TEXT NOT NULL,
                aux_key      TEXT NOT NULL DEFAULT '',
                from_entry   INTEGER NOT NULL REFERENCES voucher_entry(id) ON DELETE CASCADE,
                to_entry     INTEGER NOT NULL REFERENCES voucher_entry(id) ON DELETE CASCADE,
                amount       TEXT NOT NULL,
                settled_by   TEXT NOT NULL DEFAULT '',
                settled_at   TEXT NOT NULL DEFAULT '',
                UNIQUE(from_entry, to_entry)
             );
             INSERT INTO settle_record_new SELECT * FROM settle_record;
             DROP TABLE settle_record;
             ALTER TABLE settle_record_new RENAME TO settle_record;
             CREATE INDEX IF NOT EXISTS idx_settle_entry ON settle_record(from_entry, to_entry);",
        )?;
    }
    Ok(())
}

/// DDL 里声明的全部表名（进程内解析一次并缓存）。
fn ddl_tables() -> &'static [String] {
    static TABLES: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    TABLES.get_or_init(|| {
        const MARK: &str = "CREATE TABLE IF NOT EXISTS ";
        let mut out = Vec::new();
        let mut rest = DDL;
        while let Some(i) = rest.find(MARK) {
            let after = &rest[i + MARK.len()..];
            let name: String = after
                .chars()
                .take_while(|c| !c.is_whitespace() && *c != '(')
                .collect();
            if name.is_empty() {
                break;
            }
            out.push(name.clone());
            rest = &after[name.len()..];
        }
        out
    })
}

/// DDL 声明的表是否都已存在。
///
/// 不能只凭 SCHEMA_VERSION 判断：历史上 `invoice`（5e1efc1）和 `prod_cost`
/// （9b84d95）都是只往 DDL 加表、没升版本号，靠每次 open 跑 DDL 才补上。
/// 这条查询是只读的，不占写锁。
fn tables_complete(conn: &Connection) -> Result<bool, DbError> {
    let mut st = conn.prepare("SELECT name FROM sqlite_master WHERE type='table'")?;
    let have: std::collections::HashSet<String> = st
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    Ok(ddl_tables().iter().all(|t| have.contains(t)))
}

/// 初始化 schema（幂等）
///
/// 快速路径：版本已是最新、且 DDL 声明的表都在，就不开任何写事务直接返回。
/// Web 端每个请求都会 `Db::open` 一次，若每次都跑一遍 DDL（BEGIN IMMEDIATE），
/// 连纯读请求也在抢写锁，并发下会退化成 SQLITE_BUSY。
pub fn init(conn: &Connection) -> Result<(), DbError> {
    // WAL 让服务器上多个进程/多个用户可以同时打开同一个账套文件；
    // busy_timeout 让并发写入时等待而不是立刻报 database is locked。
    // 注意：PRAGMA 不能在事务内执行，必须先单独完成。
    // busy_timeout 必须排在 journal_mode 之前：切 WAL 要拿排他锁，此时若
    // busy_timeout 仍是默认 0，并发的 Db::open 会直接失败而不是等锁。
    conn.execute_batch(
        "PRAGMA busy_timeout = 5000;
         PRAGMA foreign_keys = ON;
         PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;",
    )?;
    if version(conn) >= SCHEMA_VERSION && tables_complete(conn)? {
        return Ok(());
    }
    // DDL + 全部迁移放进一个事务：任一环节失败整体回滚。
    // 否则 migrate_v7/v9/v10 的「建新表 → DROP TABLE → 改名」中途崩溃会把账套表搞丢。
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let res = (|| -> Result<(), DbError> {
        conn.execute_batch(DDL)?;
        let v: i64 = version(conn);
        if v < 2 {
            migrate_v2(conn)?;
        }
        if v < 3 {
            migrate_v3(conn)?;
        }
        if v < SCHEMA_VERSION {
            migrate_generic(conn, MIGRATE_V5)?;
            migrate_generic(conn, MIGRATE_V6)?;
            migrate_v7(conn)?;
            migrate_generic(conn, MIGRATE_V8)?;
            migrate_v9(conn)?;
            migrate_v10(conn)?;
            migrate_generic(conn, MIGRATE_V17)?;
            migrate_v18(conn)?;
            migrate_generic(conn, MIGRATE_V19)?;
            migrate_generic(conn, MIGRATE_V20)?;
            migrate_generic(conn, MIGRATE_V21)?;
            migrate_generic(conn, MIGRATE_V22)?;
            migrate_generic(conn, MIGRATE_V23)?;
            migrate_generic(conn, MIGRATE_V24)?;
            migrate_generic(conn, MIGRATE_V25)?;
            migrate_generic(conn, MIGRATE_V26)?;
            migrate_generic(conn, MIGRATE_V31)?;
            // v30：仓库主数据种默认仓（建表在 DDL；老账套升级即得，幂等）
            conn.execute(
                "INSERT OR IGNORE INTO warehouse(code,name,is_default,disabled,memo)
                 VALUES('01','主仓',1,0,'')",
                [],
            )?;
            conn.execute(
                "INSERT OR REPLACE INTO meta(key,value) VALUES('schema_version', ?1)",
                rusqlite::params![SCHEMA_VERSION.to_string()],
            )?;
        }
        Ok(())
    })();
    match res {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK;");
            Err(e)
        }
    }
}

/// v1 → v2：给既有表补列
fn migrate_v2(conn: &Connection) -> Result<(), DbError> {
    migrate_generic(conn, MIGRATE_V2)
}

/// v2 → v3
fn migrate_v3(conn: &Connection) -> Result<(), DbError> {
    migrate_generic(conn, MIGRATE_V3)
}

/// 按清单补列（幂等）
fn migrate_generic(conn: &Connection, list: &[(&str, &str, &str)]) -> Result<(), DbError> {
    for (table, col, decl) in list {
        if column_exists(conn, table, col)? {
            continue;
        }
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {col} {decl}"),
            [],
        )?;
    }
    Ok(())
}

/// 判断某表是否已存在某列
fn column_exists(conn: &Connection, table: &str, col: &str) -> Result<bool, DbError> {
    let mut st = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = st.query_map([], |r| r.get::<_, String>(1))?;
    for name in rows {
        if name?.eq_ignore_ascii_case(col) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// 读取 schema 版本
pub fn version(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT CAST(value AS INTEGER) FROM meta WHERE key='schema_version'",
        [],
        |r| r.get(0),
    )
    .unwrap_or(0)
}
