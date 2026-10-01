//! 金额类型。
//!
//! 财务软件里最不能出错的就是钱。这里统一用 `rust_decimal::Decimal` 做定点十进制，
//! 全程不碰 `f64`，避免 `0.1 + 0.2 != 0.3` 这种经典事故。
//!
//! 约定：
//! - 金额显示默认 2 位小数；
//! - 数量最多 6 位小数（尾随零自动去掉）；
//! - 单价最多 6 位小数；
//! - "借方/贷方发生额" 恒为非负，"余额" 用带符号的 `signed` 表示（正=借、负=贷）。

use rust_decimal::prelude::*;
use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::iter::Sum;
use std::ops::{Add, AddAssign, Mul, Neg, Sub, SubAssign};
use std::str::FromStr;

use crate::error::FinError;

/// 金额标准小数位
pub const MONEY_DP: u32 = 2;
/// 数量/单价最大小数位
pub const QTY_DP: u32 = 6;

/// 强类型金额包装。内部即 `Decimal`，但收敛了格式化与取整策略。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
pub struct Money(pub Decimal);

impl Money {
    pub const ZERO: Money = Money(Decimal::ZERO);
    pub const ONE: Money = Money(Decimal::ONE);

    #[inline]
    pub fn new(d: Decimal) -> Self {
        Self(d)
    }

    /// 由"分"构造，如 `from_cents(1234) == 12.34`
    #[inline]
    pub fn from_cents(cents: i64) -> Self {
        Self(Decimal::new(cents, 2))
    }

    /// 由整数构造
    #[inline]
    pub fn from_i64(v: i64) -> Self {
        Self(Decimal::from(v))
    }

    /// 解析用户输入的金额。容忍千分位逗号、全角字符、百分号后缀、空串。
    pub fn parse(s: &str) -> Result<Self, FinError> {
        let mut t = s.trim().to_string();
        if t.is_empty() {
            return Ok(Money::ZERO);
        }
        // 全角转半角
        t = t
            .replace('，', ",")
            .replace('。', ".")
            .replace('（', "(")
            .replace('）', ")")
            .replace('－', "-")
            .replace('　', "");
        t = t.replace(',', "");
        t = t.replace(['¥', '￥', '$', '%'], "");
        t = t.trim().to_string();

        let negative_paren = t.starts_with('(') && t.ends_with(')');
        if negative_paren {
            t = t[1..t.len() - 1].to_string();
        }
        if t.is_empty() {
            return Ok(Money::ZERO);
        }
        let d = Decimal::from_str_exact(&t).map_err(|_| FinError::msg(format!("金额格式不正确：{s}")))?;
        let v = if negative_paren { -d } else { d };
        Ok(Self(v))
    }

    /// 宽松解析：失败时返回 0。用于表格单元格等不宜弹错的场景。
    #[inline]
    pub fn parse_or_zero(s: &str) -> Self {
        Self::parse(s).unwrap_or(Money::ZERO)
    }

    #[inline]
    pub fn inner(self) -> Decimal {
        self.0
    }

    /// 四舍五入到 2 位（会计惯例，0.005 进位；不用 Decimal 默认的银行家舍入）
    #[inline]
    pub fn round2(self) -> Self {
        Self(round_half_up(self.0, MONEY_DP))
    }

    /// 四舍五入到指定小数位（会计惯例，半值远离零）
    #[inline]
    pub fn round_dp(self, dp: u32) -> Self {
        Self(round_half_up(self.0, dp))
    }

    #[inline]
    pub fn is_zero(self) -> bool {
        self.0.is_zero()
    }

    #[inline]
    pub fn is_negative(self) -> bool {
        self.0.is_sign_negative() && !self.0.is_zero()
    }

    #[inline]
    pub fn is_positive(self) -> bool {
        self.0.is_sign_positive() && !self.0.is_zero()
    }

    #[inline]
    pub fn abs(self) -> Self {
        Self(self.0.abs())
    }

    #[inline]
    pub fn negated(self) -> Self {
        Self(-self.0)
    }

    /// 转为"分"，用于与整数系统对接
    #[inline]
    pub fn cents(self) -> i64 {
        self.round2()
            .0
            .checked_mul(Decimal::from(100))
            .and_then(|d| d.to_i64())
            .unwrap_or(0)
    }

    /// 转为 f64，仅用于绘图/统计，禁止用于账务计算
    #[inline]
    pub fn to_f64(self) -> f64 {
        self.0.to_f64().unwrap_or(0.0)
    }

    /// 恒返回非负（用于发生额）
    #[inline]
    pub fn amount(self) -> Self {
        if self.0.is_sign_negative() {
            Self(-self.0)
        } else {
            self
        }
    }

    /// 2 位小数 + 千分位，如 `1,234.56`
    pub fn fmt_money(self) -> String {
        self.fmt_dp(MONEY_DP)
    }

    /// 指定小数位 + 千分位
    pub fn fmt_dp(self, dp: u32) -> String {
        // 与 round2() 的会计口径一致（半值远离零）；Decimal::round_dp 默认是银行家舍入
        let d = round_half_up(self.0, dp);
        // 四舍五入后若量化为 0（如 -0.004 → -0.00），不应显示负号
        let neg = d.is_sign_negative() && !d.is_zero();
        let s = d.abs().to_string();
        let (ip, fp) = match s.split_once('.') {
            Some((a, b)) => (a, b),
            None => (s.as_str(), ""),
        };
        let mut out = insert_thousands(ip);
        if dp > 0 {
            out.push('.');
            out.push_str(&pad_right(fp, dp as usize));
        }
        if neg {
            out.insert(0, '-');
        }
        out
    }

    /// 数量格式：最多 6 位小数，去掉无意义的尾随零
    pub fn fmt_qty(self) -> String {
        let d = round_half_up(self.0, QTY_DP).normalize();
        let neg = d.is_sign_negative();
        let s = d.abs().to_string();
        let (ip, fp) = match s.split_once('.') {
            Some((a, b)) => (a, b),
            None => (s.as_str(), ""),
        };
        let mut out = insert_thousands(ip);
        if !fp.is_empty() {
            out.push('.');
            out.push_str(fp);
        }
        if neg {
            out.insert(0, '-');
        }
        out
    }

    /// 不含千分位的纯数字，用于导出 CSV/Excel 的数值列
    pub fn fmt_plain(self) -> String {
        rescale_to(self.0, MONEY_DP).to_string()
    }

    /// 满精度、无千分位，用于落库小数位可超过 2 位的字段（数量、单价、汇率、费率）。
    ///
    /// 这类字段不要用 `to_string()` 或 `fmt_plain()` 写库：`Display` 走
    /// `fmt_money()`，既四舍五入到 2 位又插千分位逗号，汇率 7.2345 会变 7.23、
    /// 数量 0.123456 会变 0.12。这里刻意不调 `normalize()`——它会把 100.00
    /// 变成 `1E+2`，而读取侧的 `Decimal::from_str_exact` 不接受科学计数法。
    pub fn fmt_exact(self) -> String {
        self.0.to_string()
    }

    /// 大写金额（人民币），用于票据/打印
    pub fn to_capital(self) -> String {
        let v = self.round2();
        let neg = v.0.is_sign_negative();
        // Decimal 乘法溢出会 panic；超出 i64 分范围的极端值按上限处理，绝不崩溃
        let cents = v
            .0
            .abs()
            .checked_mul(Decimal::from(100))
            .and_then(|d| d.to_i64())
            .unwrap_or(i64::MAX);
        let yuan = cents / 100;
        let jiao = (cents % 100) / 10;
        let fen = cents % 10;

        const DIGITS: [&str; 10] = ["零", "壹", "贰", "叁", "肆", "伍", "陆", "柒", "捌", "玖"];
        // digits 顺序为 [千,百,十,个]
        const UNITS: [&str; 4] = ["仟", "佰", "拾", ""];
        // groups[i] 对应的级名
        const GROUPS: [&str; 4] = ["", "万", "亿", "兆"];

        if yuan == 0 && jiao == 0 && fen == 0 {
            return "零元整".to_string();
        }

        // 按 4 位分组
        let mut groups: Vec<u32> = Vec::new();
        let mut n = yuan as u64;
        if n == 0 {
            groups.push(0);
        }
        while n > 0 {
            groups.push((n % 10000) as u32);
            n /= 10000;
        }

        let mut out = String::new();
        let mut need_zero = false; // 组内是否需要在下一位前补零
        for (gi, &g) in groups.iter().enumerate().rev() {
            let mut gstr = String::new();
            let digits = [
                g / 1000,
                (g % 1000) / 100,
                (g % 100) / 10,
                g % 10,
            ];
            let mut wrote_any = false;
            for (i, &d) in digits.iter().enumerate() {
                if d == 0 {
                    if wrote_any {
                        need_zero = true;
                    }
                    continue;
                }
                if need_zero {
                    gstr.push('零');
                    need_zero = false;
                }
                gstr.push_str(DIGITS[d as usize]);
                gstr.push_str(UNITS[i]);
                wrote_any = true;
            }
            if wrote_any {
                out.push_str(&gstr);
                out.push_str(GROUPS.get(gi).copied().unwrap_or(""));
                need_zero = true;
            } else if !out.is_empty() {
                need_zero = true;
            }
        }

        if out.is_empty() {
            out.push('零');
        }
        out.push('元');

        if jiao == 0 && fen == 0 {
            out.push('整');
        } else {
            if jiao > 0 {
                out.push_str(DIGITS[jiao as usize]);
                out.push('角');
            } else if fen > 0 {
                out.push('零');
            }
            if fen > 0 {
                out.push_str(DIGITS[fen as usize]);
                out.push('分');
            } else {
                // 角位有值、分位为零：票据规则要求以"整"结尾
                out.push('整');
            }
        }
        if neg {
            out.insert(0, '负');
        }
        out
    }
}

/// 会计口径的舍入：半值远离零（即常说的四舍五入）。
///
/// `rust_decimal` 的 `round_dp` 默认是 `MidpointNearestEven`（银行家舍入），
/// 0.005 → 0.00、0.125 → 0.12，与财务惯例及本文档/界面的口径都不一致。
fn round_half_up(d: Decimal, dp: u32) -> Decimal {
    d.round_dp_with_strategy(dp, RoundingStrategy::MidpointAwayFromZero)
}

/// 四舍五入到指定小数位，并**补足**小数位数。
/// `Decimal::round_dp` 在小数位已足够时不会补零（100.round_dp(2) 仍是 100），
/// 而财务报表要求固定两位小数，所以需要额外 rescale。
fn rescale_to(d: Decimal, dp: u32) -> Decimal {
    let mut d = round_half_up(d, dp);
    if d.scale() < dp {
        d.rescale(dp);
    }
    d
}

fn insert_thousands(int_part: &str) -> String {
    let mut out = String::with_capacity(int_part.len() + int_part.len() / 3 + 1);
    let n = int_part.len();
    for (i, ch) in int_part.chars().enumerate() {
        if i > 0 && (n - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn pad_right(s: &str, width: usize) -> String {
    let mut r = s.to_string();
    while r.len() < width {
        r.push('0');
    }
    if r.len() > width {
        r.truncate(width);
    }
    r
}

// ---------- 运算符 ----------
impl Add for Money {
    type Output = Money;
    fn add(self, rhs: Money) -> Money {
        Money(self.0 + rhs.0)
    }
}
impl Sub for Money {
    type Output = Money;
    fn sub(self, rhs: Money) -> Money {
        Money(self.0 - rhs.0)
    }
}
impl Neg for Money {
    type Output = Money;
    fn neg(self) -> Money {
        Money(-self.0)
    }
}
impl Mul<Decimal> for Money {
    type Output = Money;
    fn mul(self, rhs: Decimal) -> Money {
        Money(self.0 * rhs)
    }
}
/// Money × Money
///
/// 维度上不严谨，但本项目把**比率**（残值率、税率、折旧率）和**计数**（月数、数量）
/// 也统一用 Money 承载，避免为标量再引入一套类型。典型用法：
/// `原值 * 残值率`、`数量 * 单价`。
impl Mul<Money> for Money {
    type Output = Money;
    fn mul(self, rhs: Money) -> Money {
        Money(self.0 * rhs.0)
    }
}
/// 整数倍率（月数、份数），避免调用处手写 `Money::from_i64(n)`
impl Mul<i64> for Money {
    type Output = Money;
    fn mul(self, rhs: i64) -> Money {
        Money(self.0 * Decimal::from(rhs))
    }
}

/// 除数抽象：统一 `Money` 的三种除数（`Money` / `Decimal` / `i64`），
/// 仅作为 [`Money::checked_div`] 的参数约束使用（inherent 方法，调用点无需引入本 trait）。
pub trait Divisor {
    fn divisor_dec(self) -> Decimal;
}
impl Divisor for Money {
    fn divisor_dec(self) -> Decimal {
        self.0
    }
}
impl Divisor for Decimal {
    fn divisor_dec(self) -> Decimal {
        self
    }
}
impl Divisor for i64 {
    fn divisor_dec(self) -> Decimal {
        Decimal::from(self)
    }
}

impl Money {
    /// 除法（L-1 定案）：**不再静默返回 0**——除数为 0 返回 `None`。
    ///
    /// 语义决策下放到每个调用点：
    /// - 数据入口/计算链 → 传播错误（`?` / `ok_or`）；
    /// - 展示类（比率、均值）→ 显式 `.unwrap_or(Money::ZERO)`（"无数据按 0 显示"）；
    /// - 能前置校验的除数（月数、份数）→ 在录入/校验处拦截。
    ///
    /// 原先 `a / 0 == 0` 会把"除数缺失"伪装成合法金额，是金额类静默错误
    /// 的高危来源（SECURITY_TODO L-1）。`{Money, Decimal, i64} / …` 的除法
    /// 一律改用本方法；`Money * …` 仍是运算符。
    pub fn checked_div<R: Divisor>(self, rhs: R) -> Option<Money> {
        let d = rhs.divisor_dec();
        if d.is_zero() {
            None
        } else {
            Some(Money(self.0 / d))
        }
    }
}
impl AddAssign for Money {
    fn add_assign(&mut self, rhs: Money) {
        self.0 += rhs.0
    }
}
impl SubAssign for Money {
    fn sub_assign(&mut self, rhs: Money) {
        self.0 -= rhs.0
    }
}
impl Sum for Money {
    fn sum<I: Iterator<Item = Money>>(iter: I) -> Money {
        iter.fold(Money::ZERO, |a, b| a + b)
    }
}
impl<'a> Sum<&'a Money> for Money {
    fn sum<I: Iterator<Item = &'a Money>>(iter: I) -> Money {
        iter.fold(Money::ZERO, |a, b| a + *b)
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.fmt_money())
    }
}
impl fmt::Debug for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Money({})", self.fmt_plain())
    }
}
impl From<Decimal> for Money {
    fn from(d: Decimal) -> Self {
        Money(d)
    }
}
impl From<i64> for Money {
    fn from(v: i64) -> Self {
        Money(Decimal::from(v))
    }
}
impl FromStr for Money {
    type Err = FinError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Money::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_div_zero_is_none() {
        // L-1：除零不再静默返 0——三种除数一律 None；正常值语义与原 / 运算符一致
        let a = Money::parse("10").unwrap();
        assert!(a.checked_div(Money::ZERO).is_none());
        assert!(a.checked_div(Decimal::ZERO).is_none());
        assert!(a.checked_div(0_i64).is_none());
        assert_eq!(
            a.checked_div(Money::parse("4").unwrap()).unwrap(),
            Money::parse("2.5").unwrap()
        );
        assert_eq!(
            a.checked_div(Decimal::from(100)).unwrap(),
            Money::parse("0.1").unwrap()
        );
        assert_eq!(a.checked_div(4_i64).unwrap(), Money::parse("2.5").unwrap());
    }

    #[test]
    fn no_float_error() {
        let a = Money::parse("0.1").unwrap();
        let b = Money::parse("0.2").unwrap();
        assert_eq!((a + b).fmt_plain(), "0.30");
    }

    #[test]
    fn thousands_and_dp() {
        assert_eq!(Money::parse("1234567.891").unwrap().fmt_money(), "1,234,567.89");
        assert_eq!(Money::parse("-1234.5").unwrap().fmt_money(), "-1,234.50");
        assert_eq!(Money::ZERO.fmt_money(), "0.00");
        assert_eq!(Money::parse("100").unwrap().fmt_plain(), "100.00");
    }

    /// 落库精度：fmt_exact 保留满精度、无千分位，且可被 Money::parse 原样读回。
    /// `to_string()`（Display）会截到 2 位并加逗号，所以数量/单价/汇率不能用它落库。
    #[test]
    fn fmt_exact_keeps_precision_for_storage() {
        let rate = Money::parse("7.2345").unwrap();
        assert_eq!(rate.to_string(), "7.23"); // 反例：旧落库方式丢精度
        assert_eq!(rate.fmt_exact(), "7.2345");

        let qty = Money::parse("0.123456").unwrap();
        assert_eq!(qty.fmt_exact(), "0.123456");
        assert_eq!(qty.to_string(), "0.12");

        let tax = Money::parse("0.095").unwrap(); // 9.5% 税率
        assert_eq!(tax.fmt_exact(), "0.095");
        assert_eq!(tax.to_string(), "0.10");

        // 整值保留原 scale，不会变成 normalize() 那种科学计数法（1E+2），
        // 因为 from_str_exact 读不回科学计数法
        assert_eq!(Money::parse("100").unwrap().fmt_exact(), "100");
        assert_eq!(Money::new(Decimal::new(10000, 2)).fmt_exact(), "100.00");
        assert_eq!(Money::parse("1234567.89").unwrap().fmt_exact(), "1234567.89");

        for s in ["7.2345", "0.123456", "0.095", "100", "1234567.89", "-0.5"] {
            let m = Money::parse(s).unwrap();
            assert_eq!(Money::parse(&m.fmt_exact()).unwrap(), m, "回读不一致：{s}");
        }
    }

    #[test]
    fn parse_tolerant() {
        assert_eq!(Money::parse("1,234.56").unwrap().fmt_plain(), "1234.56");
        assert_eq!(Money::parse("（12.30）").unwrap().fmt_plain(), "-12.30");
        assert_eq!(Money::parse("¥ 88").unwrap().fmt_plain(), "88.00");
        assert_eq!(Money::parse("").unwrap(), Money::ZERO);
        assert!(Money::parse("abc").is_err());
    }

    /// 会计惯例是四舍五入（半值远离零），不是 Decimal 默认的银行家舍入
    #[test]
    fn round_half_up_midpoints() {
        assert_eq!(Money::parse("0.005").unwrap().round2().fmt_plain(), "0.01");
        assert_eq!(Money::parse("0.015").unwrap().round2().fmt_plain(), "0.02");
        assert_eq!(Money::parse("0.125").unwrap().round2().fmt_plain(), "0.13");
        assert_eq!(Money::parse("-0.005").unwrap().round2().fmt_plain(), "-0.01");
        assert_eq!(Money::parse("2.675").unwrap().round2().fmt_plain(), "2.68");
        // 非中点保持最近舍入
        assert_eq!(Money::parse("0.004").unwrap().round2().fmt_plain(), "0.00");
        assert_eq!(Money::parse("0.006").unwrap().round2().fmt_plain(), "0.01");
    }

    #[test]
    fn capital() {
        assert_eq!(Money::parse("1001.00").unwrap().to_capital(), "壹仟零壹元整");
        assert_eq!(Money::parse("100000000").unwrap().to_capital(), "壹亿元整");
        assert_eq!(Money::parse("0.05").unwrap().to_capital(), "零元零伍分");
        assert_eq!(Money::parse("1234.56").unwrap().to_capital(), "壹仟贰佰叁拾肆元伍角陆分");
        assert_eq!(Money::parse("10.40").unwrap().to_capital(), "壹拾元肆角整");
        assert_eq!(Money::parse("10001").unwrap().to_capital(), "壹万零壹元整");
        assert_eq!(Money::parse("100000001").unwrap().to_capital(), "壹亿零壹元整");
        assert_eq!(Money::parse("-8.20").unwrap().to_capital(), "负捌元贰角整");
    }

    #[test]
    fn qty_format() {
        assert_eq!(Money::parse("12.500000").unwrap().fmt_qty(), "12.5");
        assert_eq!(Money::parse("0.000001").unwrap().fmt_qty(), "0.000001");
    }
}
