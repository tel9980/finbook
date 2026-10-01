//! 会计期间
//!
//! 期间用一个 `YYYYMM` 整数表示，例如 `202601`。这样排序、比较、区间查询都天然正确，
//! 存进 SQLite 也只是一个 INTEGER。

use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};
use std::fmt;

use crate::error::FinError;

/// 会计期间，内部为 `YYYYMM`
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Period(i32);

impl Period {
    pub const ZERO: Period = Period(0);

    pub fn new(year: i32, month: u32) -> Result<Self, FinError> {
        if !(1..=12).contains(&month) {
            return Err(FinError::msg(format!("月份非法：{month}")));
        }
        if !(1900..=2999).contains(&year) {
            return Err(FinError::msg(format!("年份非法：{year}")));
        }
        Ok(Period(year * 100 + month as i32))
    }

    /// 不校验构造，用于内部已知合法的换算
    pub fn from_ymm(ymm: i32) -> Self {
        Period(ymm)
    }

    /// 校验构造：非法期间返回错误而非 panic（Web 层非法输入应返 400）。
    pub fn from_ymm_checked(ymm: i32) -> Result<Self, FinError> {
        let y = ymm / 100;
        let m = (ymm % 100) as u32;
        Period::new(y, m).map_err(|_| {
            FinError::msg(format!(
                "非法期间 {ymm}：应为 YYYYMM（年份 1900-2999，月份 1-12）"
            ))
        })
    }

    /// 解析期间。容忍 `202601` / `2026-01` / `2026/01` / `2026年1月` 等写法。
    pub fn parse(s: &str) -> Result<Self, FinError> {
        let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
        let (y, m) = match digits.len() {
            // 202601
            6 => (
                digits[0..4].parse::<i32>().map_err(|_| FinError::msg("年份解析失败"))?,
                digits[4..6].parse::<u32>().map_err(|_| FinError::msg("月份解析失败"))?,
            ),
            // 20261 —— "2026年1月" 去掉非数字后只剩 5 位，补零成 01
            5 => (
                digits[0..4].parse::<i32>().map_err(|_| FinError::msg("年份解析失败"))?,
                digits[4..5].parse::<u32>().map_err(|_| FinError::msg("月份解析失败"))?,
            ),
            _ => return Err(FinError::msg(format!("期间格式应为 YYYYMM，收到：{s}"))),
        };
        Period::new(y, m)
    }

    pub fn from_date(d: NaiveDate) -> Self {
        Period(d.year() * 100 + d.month() as i32)
    }

    #[inline]
    pub fn year(self) -> i32 {
        self.0 / 100
    }
    #[inline]
    pub fn month(self) -> u32 {
        (self.0 % 100) as u32
    }
    #[inline]
    pub fn ymm(self) -> i32 {
        self.0
    }

    /// 该期间在会计年度内的第几期（我国会计年度=自然年度，故即月份）
    #[inline]
    pub fn seq_in_year(self) -> u32 {
        self.month()
    }

    /// 下月，跨年自动进位
    pub fn next(self) -> Period {
        let (y, m) = (self.year(), self.month());
        if m == 12 {
            Period(y * 100 + 100 + 1)
        } else {
            Period(self.0 + 1)
        }
    }

    pub fn prev(self) -> Period {
        let (y, m) = (self.year(), self.month());
        if m == 1 {
            Period((y - 1) * 100 + 12)
        } else {
            Period(self.0 - 1)
        }
    }

    /// 加减 N 个月（可负）
    pub fn add_months(self, n: i32) -> Period {
        let total = (self.year() * 12 + self.month() as i32 - 1) + n;
        let y = total.div_euclid(12);
        let m = total.rem_euclid(12) + 1;
        Period(y * 100 + m)
    }

    /// 该月第一天
    pub fn first_day(self) -> NaiveDate {
        NaiveDate::from_ymd_opt(self.year(), self.month(), 1).expect("期间月份必然合法")
    }

    /// 该月最后一天
    pub fn last_day(self) -> NaiveDate {
        let (y, m) = (self.year(), self.month());
        let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
        NaiveDate::from_ymd_opt(ny, nm, 1)
            .expect("下一月必然合法")
            .pred_opt()
            .expect("必然存在前一天")
    }

    /// `2026年01期`
    pub fn label(self) -> String {
        format!("{}年{:02}期", self.year(), self.month())
    }

    /// `2026-01`
    pub fn code(self) -> String {
        format!("{:04}-{:02}", self.year(), self.month())
    }

    /// 判断某日期是否落在本期间内
    pub fn contains(self, d: NaiveDate) -> bool {
        Period::from_date(d) == self
    }

    /// 生成 [from, to] 的期间序列（闭区间，from <= to）
    pub fn range(from: Period, to: Period) -> Vec<Period> {
        let mut out = Vec::new();
        let mut cur = from;
        while cur <= to {
            out.push(cur);
            cur = cur.next();
            if out.len() > 1200 {
                break;
            }
        }
        out
    }

    /// 本期间是否已期末结账（由调用方传入已结账的最大期间判断）
    pub fn is_closed(self, closed_upto: Period) -> bool {
        self <= closed_upto
    }
}

impl fmt::Display for Period {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.code())
    }
}
impl fmt::Debug for Period {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Period({})", self.code())
    }
}
impl Default for Period {
    fn default() -> Self {
        let now = chrono::Local::now().date_naive();
        Period::from_date(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic() {
        let p = Period::new(2026, 1).unwrap();
        assert_eq!(p.next(), Period::new(2026, 2).unwrap());
        assert_eq!(p.prev(), Period::new(2025, 12).unwrap());
        assert_eq!(p.add_months(13), Period::new(2027, 2).unwrap());
        assert_eq!(p.add_months(-2), Period::new(2025, 11).unwrap());
        assert_eq!(p.code(), "2026-01");
        assert_eq!(p.label(), "2026年01期");
    }

    #[test]
    fn month_ends() {
        let p = Period::new(2024, 2).unwrap();
        assert_eq!(p.last_day(), NaiveDate::from_ymd_opt(2024, 2, 29).unwrap());
        let p = Period::new(2026, 12).unwrap();
        assert_eq!(p.last_day(), NaiveDate::from_ymd_opt(2026, 12, 31).unwrap());
        assert_eq!(p.next(), Period::new(2027, 1).unwrap());
    }

    #[test]
    fn parse_ok() {
        assert_eq!(Period::parse("2026-01").unwrap(), Period::new(2026, 1).unwrap());
        assert_eq!(Period::parse("2026年3月").unwrap(), Period::new(2026, 3).unwrap());
        assert!(Period::parse("202613").is_err());
    }
}
