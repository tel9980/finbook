//! 数据导出：CSV 构建（供手动导出与「导出计划任务」共用）+ 计划任务 CRUD。
//!
//! - 计划任务按账套存表 `export_schedule`（kind / period_mode / at_time / enabled / last_run）；
//! - `sched_due` 由 Web 端后台任务每 60s 轮询，到期写 `books_dir/exports/<prefix>_<时间戳>.csv`；
//! - 全部金额 Rust 侧 Decimal 格式化，CSV 带 BOM（Excel 直开）。

use fincore::Period;
use rusqlite::OptionalExtension;

use crate::{Db, DbResult};

/// 获取期间的起止日期（用于按日期过滤没有 period 字段的表，如 dunning）
fn period_range(p: Period) -> (String, String) {
    (p.first_day().format("%Y-%m-%d").to_string(), p.last_day().format("%Y-%m-%d").to_string())
}

/// 构建导出 CSV（kind: vouchers / trial / payroll / claims / dunning_ar / dunning_ap）→ (文件名前缀, 行集)
pub fn build_csv(db: &Db, kind: &str, period: Period) -> DbResult<(String, Vec<Vec<String>>)> {
    match kind {
        "vouchers" => {
            let mut rows = vec![header(&["日期", "凭证字", "号", "摘要", "制单", "状态"])];
            let mut st = db.conn().prepare(
                "SELECT date,word,no,memo,prepared_by,status FROM voucher
                 WHERE period=?1 ORDER BY no, id",
            )?;
            let it = st.query_map([period.ymm()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                ))
            })?;
            for r in it {
                let (d, w, n, m, p, s) = r?;
                rows.push(vec![d, w, n.to_string(), m, p, s]);
            }
            Ok((format!("vouchers_{}", period.ymm()), rows))
        }
        "trial" => {
            let bq = crate::balances::BalanceQuery::period(period);
            let snap = crate::balances::BalanceSnapshot::load(db, &bq)?;
            let chart = crate::accounts::chart(db)?;
            let mut rows = vec![header(&["科目", "名称", "期初", "借方", "贷方", "期末"])];
            for r in snap.account_table(&chart, &bq) {
                let end = r.end();
                rows.push(vec![
                    r.account_code,
                    r.account_name,
                    r.begin.fmt_plain(),
                    r.debit.fmt_plain(),
                    r.credit.fmt_plain(),
                    end.fmt_plain(),
                ]);
            }
            Ok((format!("trial_{}", period.ymm()), rows))
        }
        "payroll" => {
            let mut rows = vec![header(&[
                "员工",
                "部门",
                "应发",
                "社保",
                "公积金",
                "其他扣除",
                "专项附加",
                "计税基数",
                "个税",
                "实发",
            ])];
            for p in crate::business::payroll_list(db, period)? {
                rows.push(vec![
                    p.employee,
                    p.dept,
                    p.gross.fmt_plain(),
                    p.social.fmt_plain(),
                    p.housing.fmt_plain(),
                    p.deduction.fmt_plain(),
                    p.additional.fmt_plain(),
                    p.tax_base.fmt_plain(),
                    p.tax.fmt_plain(),
                    p.net.fmt_plain(),
                ]);
            }
            Ok((format!("payroll_{}", period.ymm()), rows))
        }
        "claims" => {
            let mut rows = vec![header(&["单号", "申请人", "金额", "状态"])];
            let mut st = db.conn().prepare(
                "SELECT no, applicant, amount, status FROM claim WHERE period=?1 ORDER BY id",
            )?;
            let it = st.query_map([period.ymm()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?;
            for r in it {
                let (no, who, amt, stt) = r?;
                rows.push(vec![no, who, amt, stt]);
            }
            Ok((format!("claims_{}", period.ymm()), rows))
        }
        k if k.starts_with("dunning_") => {
            let kind = &k["dunning_".len()..];
            let mut rows = vec![header(&["催款单号", "日期", "往来单位", "往来单位名称", "金额", "状态", "备注"])];
            let mut st = db.conn().prepare(
                "SELECT no, date, party_code, party_name, amount, status, memo FROM dunning
                 WHERE kind=?1 AND date >= ?2 AND date <= ?3 ORDER BY date DESC, id DESC",
            )?;
            let (from, to) = period_range(period);
            let it = st.query_map(rusqlite::params![kind, from, to], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                ))
            })?;
            for r in it {
                let (no, date, party_code, party_name, amount, status, memo) = r?;
                rows.push(vec![no, date, party_code, party_name, amount, status, memo]);
            }
            Ok((format!("dunning_{}_{}", kind, period.ymm()), rows))
        }
        _ => Err(fincore::FinError::msg("未知导出类型（vouchers/trial/payroll/claims/dunning_ar/dunning_ap）").into()),
    }
}

fn header(cols: &[&str]) -> Vec<String> {
    cols.iter().map(|s| s.to_string()).collect()
}

fn csv_text(rows: &[Vec<String>]) -> String {
    fn esc(s: &str) -> String {
        if s.contains([',', '"', '\n', '\r']) {
            format!("\"{}\"", s.replace('"', "\"\""))
        } else {
            s.to_string()
        }
    }
    let mut out = String::from("\u{feff}");
    for r in rows {
        out.push_str(&r.iter().map(|c| esc(c)).collect::<Vec<_>>().join(","));
        out.push_str("\r\n");
    }
    out
}

// ===========================================================================
// 导出计划任务
// ===========================================================================

#[derive(Clone, Debug, serde::Serialize)]
pub struct ExportSchedule {
    pub id: i64,
    pub kind: String,
    /// current = 当前期间；last = 上一期间
    pub period_mode: String,
    /// 每日执行时刻 HH:MM
    pub at_time: String,
    pub enabled: bool,
    pub last_run: String,
    pub memo: String,
    pub created_by: String,
    pub created_at: String,
}

fn map_sched(r: &rusqlite::Row) -> rusqlite::Result<ExportSchedule> {
    Ok(ExportSchedule {
        id: r.get(0)?,
        kind: r.get(1)?,
        period_mode: r.get(2)?,
        at_time: r.get(3)?,
        enabled: r.get::<_, i64>(4)? != 0,
        last_run: r.get(5)?,
        memo: r.get(6)?,
        created_by: r.get(7)?,
        created_at: r.get(8)?,
    })
}

const S_COLS: &str = "id,kind,period_mode,at_time,enabled,last_run,memo,created_by,created_at";

pub fn sched_list(db: &Db) -> DbResult<Vec<ExportSchedule>> {
    let mut st = db
        .conn()
        .prepare(&format!("SELECT {S_COLS} FROM export_schedule ORDER BY id"))?;
    let rows = st
        .query_map([], map_sched)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn sched_get(db: &Db, id: i64) -> DbResult<Option<ExportSchedule>> {
    db.conn()
        .query_row(
            &format!("SELECT {S_COLS} FROM export_schedule WHERE id=?1"),
            rusqlite::params![id],
            map_sched,
        )
        .optional()
        .map_err(Into::into)
}

/// 新增/修改计划任务（校验类型与 HH:MM 时刻）
pub fn sched_save(db: &Db, s: &ExportSchedule, who: &str) -> DbResult<i64> {
    if !matches!(s.kind.as_str(), "vouchers" | "trial" | "payroll" | "claims") {
        return Err(fincore::FinError::msg("未知导出类型（vouchers/trial/payroll/claims）").into());
    }
    let mode = if s.period_mode == "last" {
        "last"
    } else {
        "current"
    };
    let at = s.at_time.trim();
    let valid = at.len() == 5
        && at.as_bytes()[2] == b':'
        && at[..2].parse::<u32>().map(|h| h < 24).unwrap_or(false)
        && at[3..].parse::<u32>().map(|m| m < 60).unwrap_or(false);
    if !valid {
        return Err(fincore::FinError::msg("执行时刻格式应为 HH:MM").into());
    }
    let id = if s.id > 0 {
        db.conn().execute(
            "UPDATE export_schedule SET kind=?2,period_mode=?3,at_time=?4,enabled=?5,memo=?6 WHERE id=?1",
            rusqlite::params![s.id, s.kind, mode, at, s.enabled as i64, s.memo],
        )?;
        s.id
    } else {
        db.conn().execute(
            "INSERT INTO export_schedule(kind,period_mode,at_time,enabled,last_run,memo,created_by,created_at)
             VALUES(?1,?2,?3,?4,'',?5,?6,?7)",
            rusqlite::params![
                s.kind,
                mode,
                at,
                s.enabled as i64,
                s.memo,
                who,
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
            ],
        )?;
        db.conn().last_insert_rowid()
    };
    Ok(id)
}

pub fn sched_delete(db: &Db, id: i64) -> DbResult<()> {
    db.conn()
        .execute("DELETE FROM export_schedule WHERE id=?1", [id])?;
    Ok(())
}

/// 到期任务（enabled && last_run != today && at_time <= now_hhmm）
pub fn sched_due(db: &Db, now_hhmm: &str, today: &str) -> DbResult<Vec<ExportSchedule>> {
    Ok(sched_list(db)?
        .into_iter()
        .filter(|s| s.enabled && s.last_run != today && s.at_time.as_str() <= now_hhmm)
        .collect())
}

/// 立即执行：写 CSV 到 `dir`，更新 last_run，返回文件路径
pub fn sched_run(db: &Db, id: i64, dir: &std::path::Path) -> DbResult<String> {
    let s = sched_get(db, id)?.ok_or_else(|| fincore::FinError::not_found("导出计划任务"))?;
    let today = chrono::Local::now().date_naive();
    let period = if s.period_mode == "last" {
        Period::from_date(today).prev()
    } else {
        Period::from_date(today)
    };
    let (prefix, rows) = build_csv(db, &s.kind, period)?;
    std::fs::create_dir_all(dir)
        .map_err(|e| fincore::FinError::msg(format!("创建导出目录失败：{e}")))?;
    let fname = format!(
        "{prefix}_{}.csv",
        chrono::Local::now().format("%Y%m%d_%H%M%S")
    );
    let path = dir.join(&fname);
    std::fs::write(&path, csv_text(&rows))
        .map_err(|e| fincore::FinError::msg(format!("写导出文件失败：{e}")))?;
    db.conn().execute(
        "UPDATE export_schedule SET last_run=?2 WHERE id=?1",
        rusqlite::params![id, today.format("%Y-%m-%d").to_string()],
    )?;
    Ok(path.to_string_lossy().to_string())
}


/// 导出催款记录为 Excel（多 Sheet：催款单、对账单）
///
/// `kind` = "ar" 或 "ap"；`period` 用于按日期过滤。
/// 返回 (文件名, Excel 二进制数据)。
pub fn build_dunning_excel(
    db: &Db,
    kind: &str,
    period: Period,
) -> DbResult<(String, Vec<u8>)> {
    use rust_xlsxwriter::{Workbook, Format, XlsxAlign, XlsxColor, XlsxBorder, XlsxError};

    if kind != "ar" && kind != "ap" {
        return Err(fincore::FinError::msg("kind 必须是 ar 或 ap").into());
    }

    let (from, to) = period_range(period);
    let (from_date, to_date) = (from.clone(), to.clone());

    // Sheet 1: 催款单
    let mut dunning_rows = Vec::new();
    {
        let mut st = db.conn().prepare(
            "SELECT no, date, party_code, party_name, amount, status, memo FROM dunning
             WHERE kind=?1 AND date >= ?2 AND date <= ?3 ORDER BY date DESC, id DESC",
        )?;
        let it = st.query_map(rusqlite::params![kind, from_date, to_date], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
            ))
        })?;
        for r in it {
            let (no, date, party_code, party_name, amount, status, memo) = r?;
            dunning_rows.push(vec![no, date, party_code, party_name, amount, status, memo]);
        }
    }

    // Sheet 2: 对账单
    let mut statement_rows = Vec::new();
    {
        let mut st = db.conn().prepare(
            "SELECT no, kind, party_code, party_name, period_from, period_to,
                    begin_balance, period_increase, period_decrease, end_balance,
                    line_count, status, memo, created_by, created_at, sent_at, confirmed_by, confirmed_at
             FROM ar_statement WHERE kind=?1 AND period_to >= ?2 AND period_to <= ?3
             ORDER BY period_to DESC, id DESC",
        )?;
        let it = st.query_map(rusqlite::params![kind, from, to], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(6)?,
                r.get::<_, i64>(10)?,
                r.get::<_, String>(11)?,
                r.get::<_, String>(12)?,
                r.get::<_, String>(13)?,
                r.get::<_, String>(15)?,
                r.get::<_, String>(16)?,
                r.get::<_, String>(17)?,
            ))
        })?;
        for r in it {
            let (no, kind_s, party_code, party_name, period_from, period_to,
                 begin_balance, period_increase, period_decrease, end_balance,
                 line_count, status, memo, created_by, created_at, sent_at, confirmed_by, confirmed_at) = r?;
            statement_rows.push(vec![
                no, kind_s, party_code, party_name,
                period_from, period_to,
                begin_balance, period_increase, period_decrease, end_balance,
                line_count.to_string(), status.to_string(), memo,
                created_by, created_at, sent_at, confirmed_by, confirmed_at,
            ]);
        }
    }

    let mut workbook = Workbook::new();
    let header_fmt = Format::new()
        .set_bold()
        .set_background_color(XlsxColor::RGB(0x4472C4))
        .set_font_color(XlsxColor::White)
        .set_align(XlsxAlign::Center)
        .set_border(XlsxBorder::Thin);

    let money_fmt = Format::new()
        .set_num_format("#,##0.00")
        .set_border(XlsxBorder::Thin)
        .set_align(XlsxAlign::Right);

    let text_fmt = Format::new()
        .set_border(XlsxBorder::Thin)
        .set_align(XlsxAlign::Left);

    // Sheet 1: 催款单
    let sheet1 = workbook.add_worksheet();
    sheet1.set_name("催款单")?;
    let dunning_headers = ["催款单号", "日期", "往来单位", "往来单位名称", "金额", "状态", "备注"];
    for (col, h) in dunning_headers.iter().enumerate() {
        sheet1.write_string(0, col as u16, *h, &header_fmt)?;
    }
    for (row_idx, row) in dunning_rows.iter().enumerate() {
        for (col_idx, cell) in row.iter().enumerate() {
            let fmt = if col_idx == 4 { &money_fmt } else { &text_fmt };
            if col_idx == 4 {
                let val = cell.parse::<f64>().unwrap_or(0.0);
                sheet1.write_number(row_idx as u32 + 1, col_idx as u16, val, &fmt)?;
            } else {
                sheet1.write_string(row_idx as u32 + 1, col_idx as u16, cell, &fmt)?;
            }
        }
    }
    sheet1.set_column_width(0, 18)?;
    sheet1.set_column_width(1, 12)?;
    sheet1.set_column_width(2, 15)?;
    sheet1.set_column_width(3, 20)?;
    sheet1.set_column_width(4, 14)?;
    sheet1.set_column_width(5, 10)?;
    sheet1.set_column_width(6, 25)?;

    // Sheet 2: 对账单
    let sheet2 = workbook.add_worksheet();
    sheet2.set_name("对账单")?;
    let statement_headers = [
        "单号", "类型", "往来单位", "往来单位名称",
        "起始期间", "截止期间",
        "期初余额", "本期增", "本期减", "期末余额",
        "行数", "状态", "备注",
        "制单人", "制单时间", "发出时间", "确认人", "确认时间",
    ];
    for (col, h) in statement_headers.iter().enumerate() {
        sheet2.write_string(0, col as u16, *h, &header_fmt)?;
    }
    for (row_idx, row) in statement_rows.iter().enumerate() {
        for (col_idx, cell) in row.iter().enumerate() {
            let fmt = if matches!(col_idx, 6 | 7 | 8 | 9) { &money_fmt } else { &text_fmt };
            if matches!(col_idx, 6 | 7 | 8 | 9) {
                let val = cell.parse::<f64>().unwrap_or(0.0);
                sheet2.write_number(row_idx as u32 + 1, col_idx as u16, val, &fmt)?;
            } else {
                sheet2.write_string(row_idx as u32 + 1, col_idx as u16, cell, &fmt)?;
            }
        }
    }
    sheet2.set_column_width(0, 18)?;
    sheet2.set_column_width(1, 10)?;
    sheet2.set_column_width(2, 15)?;
    sheet2.set_column_width(3, 20)?;
    sheet2.set_column_width(4, 12)?;
    sheet2.set_column_width(5, 12)?;
    sheet2.set_column_width(6, 14)?;
    sheet2.set_column_width(7, 14)?;
    sheet2.set_column_width(8, 14)?;
    sheet2.set_column_width(9, 14)?;
    sheet2.set_column_width(10, 8)?;
    sheet2.set_column_width(11, 10)?;
    sheet2.set_column_width(12, 25)?;
    sheet2.set_column_width(13, 15)?;
    sheet2.set_column_width(14, 20)?;
    sheet2.set_column_width(15, 15)?;
    sheet2.set_column_width(16, 20)?;

    let data = workbook.save_to_buffer()?;
    let fname = format!("dunning_{}_{}.xlsx", kind, period.ymm());
    Ok((fname, data))
}