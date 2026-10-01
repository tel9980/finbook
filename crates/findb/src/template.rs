//! 常用凭证模板
//!
//! 两种用法：
//! 1. 手工模板——录凭证时一键调出来，改改金额就存。
//! 2. 周期性分录——房租、摊销、计提这类每月都一样的分录，标了 freq 之后
//!    可以在期末由「自动生成」批量出凭证。
//!
//! 模板分录的金额可以是绝对值，也可以是占位（留空由调用方填）。

use fincore::{Entry, Period, Voucher, VoucherSource};

use rusqlite::OptionalExtension;

use crate::{Db, DbResult};

/// 生成频率
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Freq {
    /// 不自动生成，只在录凭证时手工调用
    #[default]
    Manual,
    Monthly,
    Quarterly,
    Yearly,
}

impl Freq {
    pub fn label(&self) -> &'static str {
        match self {
            Freq::Manual => "仅手工调用",
            Freq::Monthly => "每月",
            Freq::Quarterly => "每季",
            Freq::Yearly => "每年",
        }
    }
    pub fn code(&self) -> &'static str {
        match self {
            Freq::Manual => "",
            Freq::Monthly => "monthly",
            Freq::Quarterly => "quarterly",
            Freq::Yearly => "yearly",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "monthly" => Freq::Monthly,
            "quarterly" => Freq::Quarterly,
            "yearly" => Freq::Yearly,
            _ => Freq::Manual,
        }
    }

    /// 该期间是否该生成
    pub fn due_in(&self, p: Period) -> bool {
        match self {
            Freq::Manual => false,
            Freq::Monthly => true,
            Freq::Quarterly => matches!(p.month(), 3 | 6 | 9 | 12),
            Freq::Yearly => p.month() == 12,
        }
    }
}

/// 模板中的一行（金额留空 = 由调用方填）
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TemplateEntry {
    pub summary: String,
    pub account_code: String,
    /// 借贷方向：debit / credit
    pub dir: String,
    /// 金额文本；空表示待填
    pub amount: String,
    #[serde(default)]
    pub aux: fincore::AuxRef,
}

impl Default for TemplateEntry {
    fn default() -> Self {
        Self {
            summary: String::new(),
            account_code: String::new(),
            dir: "debit".to_string(),
            amount: String::new(),
            aux: fincore::AuxRef::default(),
        }
    }
}

/// 凭证模板
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Template {
    pub id: i64,
    pub name: String,
    pub memo: String,
    pub entries: Vec<TemplateEntry>,
    pub freq: Freq,
    pub start_period: Option<Period>,
    pub end_period: Option<Period>,
    pub last_period: Option<Period>,
    pub active: bool,
}

impl Template {
    pub fn new(name: &str) -> Self {
        Self {
            id: 0,
            name: name.to_string(),
            memo: String::new(),
            entries: Vec::new(),
            freq: Freq::Manual,
            start_period: None,
            end_period: None,
            last_period: None,
            active: false,
        }
    }

    /// 合计借贷（只统计填了金额的）
    pub fn totals(&self) -> (fincore::Money, fincore::Money) {
        let mut d = fincore::Money::ZERO;
        let mut c = fincore::Money::ZERO;
        for e in &self.entries {
            let m = fincore::Money::parse_or_zero(&e.amount);
            if e.dir == "credit" {
                c += m;
            } else {
                d += m;
            }
        }
        (d, c)
    }

    /// 是否所有行都填了金额（可作为周期性分录自动生成的前提）
    pub fn fully_priced(&self) -> bool {
        !self.entries.is_empty()
            && self
                .entries
                .iter()
                .all(|e| !e.amount.trim().is_empty() && !e.account_code.trim().is_empty())
    }

    /// 指定期间是否需要生成
    pub fn due_in(&self, p: Period) -> bool {
        if !self.active || self.freq == Freq::Manual {
            return false;
        }
        if let Some(s) = self.start_period {
            if p.ymm() < s.ymm() {
                return false;
            }
        }
        if let Some(e) = self.end_period {
            if p.ymm() > e.ymm() {
                return false;
            }
        }
        if let Some(l) = self.last_period {
            if p.ymm() <= l.ymm() {
                return false;
            }
        }
        self.freq.due_in(p)
    }

    /// 套用模板生成一张草稿凭证（金额为待填行使用 0，由调用方补）
    pub fn to_voucher(
        &self,
        p: Period,
        date: chrono::NaiveDate,
        word: &str,
        no: i64,
        who: &str,
    ) -> Result<Voucher, fincore::FinError> {
        let mut v = Voucher::new(p, date, word, no as i32);
        v.source = VoucherSource::Template;
        v.prepared_by = who.to_string();
        for (i, e) in self.entries.iter().enumerate() {
            let amt = fincore::Money::parse_or_zero(&e.amount);
            let mut ent = Entry::new((i + 1) as i32, e.account_code.clone(), e.summary.clone());
            if e.dir == "credit" {
                ent.credit = amt;
            } else {
                ent.debit = amt;
            }
            ent.aux = e.aux.clone();
            v.push_entry(ent);
        }
        Ok(v)
    }
}

fn map(r: &rusqlite::Row) -> rusqlite::Result<Template> {
    let entries_s: String = r.get(3)?;
    let freq_s: String = r.get(4)?;
    Ok(Template {
        id: r.get(0)?,
        name: r.get(1)?,
        memo: r.get(2)?,
        entries: serde_json::from_str::<Vec<TemplateEntry>>(&entries_s).unwrap_or_default(),
        freq: Freq::parse(&freq_s),
        // 写入端用 0 表示"未设置"，读回时把 0 过滤掉
        start_period: r
            .get::<_, Option<i64>>(5)?
            .filter(|v| *v > 0)
            .map(|v| Period::from_ymm(v as i32)),
        end_period: r
            .get::<_, Option<i64>>(6)?
            .filter(|v| *v > 0)
            .map(|v| Period::from_ymm(v as i32)),
        last_period: r
            .get::<_, Option<i64>>(7)?
            .filter(|v| *v > 0)
            .map(|v| Period::from_ymm(v as i32)),
        active: r.get::<_, i64>(8)? != 0,
    })
}

/// 注意：`start_period/end_period/last_period` 允许为 NULL（0 视为未设置）
const COLS: &str = "id,name,memo,entries_json,freq,start_period,end_period,last_period,active";

pub fn list(db: &Db) -> DbResult<Vec<Template>> {
    let mut st = db
        .conn()
        .prepare(&format!("SELECT {COLS} FROM voucher_template ORDER BY name"))?;
    let rows = st.query_map([], map)?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 只列出启用了周期性生成的
pub fn list_auto(db: &Db) -> DbResult<Vec<Template>> {
    Ok(list(db)?.into_iter().filter(|t| t.active).collect())
}

pub fn get(db: &Db, id: i64) -> DbResult<Option<Template>> {
    db.conn()
        .query_row(
            &format!("SELECT {COLS} FROM voucher_template WHERE id=?1"),
            rusqlite::params![id],
            map,
        )
        .optional()
        .map_err(Into::into)
}

pub fn insert(db: &Db, t: &Template) -> DbResult<i64> {
    db.conn().execute(
        "INSERT INTO voucher_template(name,memo,entries_json,freq,start_period,end_period,last_period,active)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        rusqlite::params![
            t.name,
            t.memo,
            serde_json::to_string(&t.entries)?,
            t.freq.code(),
            t.start_period.map(|p| p.ymm()).unwrap_or(0),
            t.end_period.map(|p| p.ymm()).unwrap_or(0),
            t.last_period.map(|p| p.ymm()).unwrap_or(0),
            t.active as i64
        ],
    )?;
    Ok(db.conn().last_insert_rowid())
}

pub fn update(db: &Db, t: &Template) -> DbResult<()> {
    db.conn().execute(
        "UPDATE voucher_template SET name=?2,memo=?3,entries_json=?4,freq=?5,
            start_period=?6,end_period=?7,last_period=?8,active=?9
         WHERE id=?1",
        rusqlite::params![
            t.id,
            t.name,
            t.memo,
            serde_json::to_string(&t.entries)?,
            t.freq.code(),
            t.start_period.map(|p| p.ymm()).unwrap_or(0),
            t.end_period.map(|p| p.ymm()).unwrap_or(0),
            t.last_period.map(|p| p.ymm()).unwrap_or(0),
            t.active as i64
        ],
    )?;
    Ok(())
}

pub fn delete(db: &Db, id: i64) -> DbResult<()> {
    db.conn()
        .execute("DELETE FROM voucher_template WHERE id=?1", rusqlite::params![id])?;
    Ok(())
}

/// 标记某期间已经生成过（防止重复出凭证）
pub fn mark_generated(db: &Db, id: i64, p: Period) -> DbResult<()> {
    db.conn().execute(
        "UPDATE voucher_template SET last_period=?2 WHERE id=?1",
        rusqlite::params![id, p.ymm()],
    )?;
    Ok(())
}

/// 某期间待生成的周期性模板
pub fn due_list(db: &Db, p: Period) -> DbResult<Vec<Template>> {
    Ok(list(db)?.into_iter().filter(|t| t.due_in(p)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;

    fn sample() -> Template {
        let mut t = Template::new("计提房租");
        t.entries = vec![
            TemplateEntry {
                summary: "计提房租".into(),
                account_code: "660201".into(),
                dir: "debit".into(),
                amount: "5000".into(),
                aux: fincore::AuxRef::default(),
            },
            TemplateEntry {
                summary: "计提房租".into(),
                account_code: "2203".into(),
                dir: "credit".into(),
                amount: "5000".into(),
                aux: fincore::AuxRef::default(),
            },
        ];
        t
    }

    #[test]
    fn roundtrip() {
        let db = mem();
        let mut t = sample();
        let id = insert(&db, &t).unwrap();
        t.id = id;
        let got = get(&db, id).unwrap().unwrap();
        assert_eq!(got.name, "计提房租");
        assert_eq!(got.entries.len(), 2);
        let (d, c) = got.totals();
        assert_eq!(d.to_string(), c.to_string());
        assert!(got.fully_priced());
    }

    #[test]
    fn due_respects_freq_and_window() {
        let db = mem();
        let mut t = sample();
        t.freq = Freq::Quarterly;
        t.active = true;
        t.start_period = Some(Period::new(2026, 1).unwrap());
        t.end_period = Some(Period::new(2026, 12).unwrap());
        let id = insert(&db, &t).unwrap();
        t.id = id;

        assert!(t.due_in(Period::new(2026, 3).unwrap()));
        assert!(!t.due_in(Period::new(2026, 4).unwrap()));
        assert!(!t.due_in(Period::new(2025, 12).unwrap()), "早于生效期");
        assert!(!t.due_in(Period::new(2027, 3).unwrap()), "晚于失效期");

        // 标记已生成后不再重复
        mark_generated(&db, id, Period::new(2026, 3).unwrap()).unwrap();
        let t2 = get(&db, id).unwrap().unwrap();
        assert!(!t2.due_in(Period::new(2026, 3).unwrap()));
        assert!(t2.due_in(Period::new(2026, 6).unwrap()));

        assert_eq!(due_list(&db, Period::new(2026, 6).unwrap()).unwrap().len(), 1);
    }

    #[test]
    fn to_voucher_shapes_entries() {
        let t = sample();
        let v = t
            .to_voucher(
                Period::new(2026, 3).unwrap(),
                Period::new(2026, 3).unwrap().last_day(),
                "记",
                1,
                "tester",
            )
            .unwrap();
        assert_eq!(v.entries.len(), 2);
        assert_eq!(v.source, VoucherSource::Template);
        assert_eq!(v.debit_total(), fincore::Money::parse("5000").unwrap());
        assert_eq!(v.credit_total(), fincore::Money::parse("5000").unwrap());
    }
}
