//! 自定义报表公式引擎
//!
//! 语法沿用用友 UFO 报表那套（会计人员的肌肉记忆在这里，别自创）：
//!
//! | 函数 | 含义 | 示例 |
//! |------|------|------|
//! | `QC("1001")` | 期初余额 | `QC("1001",,"借")` |
//! | `QM("1001")` | 期末余额 | `QM("1001")` |
//! | `FS("6001")` | 本期发生额 | `FS("6001",,"贷")` |
//! | `LFS("6001")` | 本年累计发生额 | `LFS("6001",,"贷")` |
//! | `JE("1001")` | 期末净额（永为正） | `JE("1001")` |
//!
//! 第二个参数是期间偏移（`0` 本期、`-1` 上期、留空同 `0`），
//! 第三个参数是方向（`借` / `贷`，留空取科目默认方向的余额）。
//!
//! 支持 `+ - * /` 和括号。**除法分母为 0 时报错**（L-1 定案，错误信息「公式除零」）：
//! 此前静默返回 0 会把"当期没数据/除数缺失"伪装成合法金额——对凭证金额公式尤其危险；
//! 展示类报表需要"无数据按 0"语义时，应在公式侧显式规避除零。

use rust_decimal::prelude::ToPrimitive;

use crate::money::Money;
use crate::{FinError, Period};

/// 取数上下文
///
/// 取数一律返回 `Result`：余额快照读失败（库被锁、数据行损坏）与"这个科目就是 0"
/// 在报表上是两件完全不同的事。返回裸 `Money` 会让调用方把前者静默变成 0，
/// 于是报表印出一份看着很正常的数字，而底下的取数其实根本没成功——财务上最坏的
/// 失败模式。
pub trait FormulaSource {
    /// 期初余额（带符号：借为正）
    fn qc(&self, code: &str, period: Period, dir: Option<&str>) -> Result<Money, FinError>;
    /// 期末余额（带符号）
    fn qm(&self, code: &str, period: Period, dir: Option<&str>) -> Result<Money, FinError>;
    /// 本期发生额（dir 为"借"/"贷"取单方向，None 取借贷差额）
    fn fs(&self, code: &str, period: Period, dir: Option<&str>) -> Result<Money, FinError>;
    /// 本年累计发生额
    fn lfs(&self, code: &str, period: Period, dir: Option<&str>) -> Result<Money, FinError>;
}

// ---------------- 词法 ----------------

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(Money),
    Str(String),
    Ident(String),
    Plus,
    Minus,
    Star,
    Slash,
    LParen,
    RParen,
    Comma,
    /// 参数为空（连续逗号）时补的占位
    #[allow(dead_code)] // 词法器保留的占位变体
    Blank,
    Eof,
}

fn tokenize(src: &str) -> Result<Vec<Tok>, FinError> {
    let mut out = Vec::new();
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0usize;
    let mut prev_was_value = false;
    while i < chars.len() {
        let c = chars[i];
        match c {
            ' ' | '\t' | '\r' | '\n' => {
                i += 1;
                continue;
            }
            '+' => {
                out.push(Tok::Plus);
                prev_was_value = false;
                i += 1;
            }
            '-' => {
                // 区分减号与负号：前面不是值（数字/字符串/右括号/标识符）就是负号
                if prev_was_value {
                    out.push(Tok::Minus);
                    prev_was_value = false;
                } else {
                    // 一元负号统一交给语法层 factor() 处理，可作用于数字、括号、函数，
                    // 例如 -5、-(a+b)、-FS("6001",,"贷")
                    out.push(Tok::Minus);
                    prev_was_value = true;
                }
                i += 1;
            }
            '*' => {
                out.push(Tok::Star);
                prev_was_value = false;
                i += 1;
            }
            '/' => {
                out.push(Tok::Slash);
                prev_was_value = false;
                i += 1;
            }
            '(' => {
                // 函数调用与括号在这里都是 LParen，靠前一个 token 是不是 Ident 区分
                out.push(Tok::LParen);
                prev_was_value = false;
                i += 1;
            }
            ')' => {
                out.push(Tok::RParen);
                prev_was_value = true;
                i += 1;
            }
            ',' => {
                out.push(Tok::Comma);
                prev_was_value = false;
                i += 1;
            }
            '"' | '\'' => {
                let q = c;
                i += 1;
                let mut s = String::new();
                while i < chars.len() && chars[i] != q {
                    s.push(chars[i]);
                    i += 1;
                }
                if i >= chars.len() {
                    return Err(FinError::msg("字符串缺少右引号"));
                }
                i += 1;
                out.push(Tok::Str(s));
                prev_was_value = true;
            }
            _ if c.is_ascii_digit() || c == '.' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                let s: String = chars[start..i].iter().collect();
                out.push(Tok::Num(Money::parse(&s)?));
                prev_was_value = true;
            }
            _ if c.is_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let s: String = chars[start..i].iter().collect();
                out.push(Tok::Ident(s));
                prev_was_value = true;
            }
            _ => return Err(FinError::msg(format!("无法识别的字符：{c}"))),
        }
    }
    out.push(Tok::Eof);
    Ok(out)
}

// ---------------- 语法（递归下降） ----------------

struct Parser<'a> {
    toks: &'a [Tok],
    pos: usize,
    src: &'a dyn FormulaSource,
    period: Period,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> &Tok {
        self.toks.get(self.pos).unwrap_or(&Tok::Eof)
    }
    fn bump(&mut self) -> Tok {
        let t = self.peek().clone();
        if t != Tok::Eof {
            self.pos += 1;
        }
        t
    }
    fn expect(&mut self, t: Tok) -> Result<(), FinError> {
        if self.peek() == &t {
            self.pos += 1;
            Ok(())
        } else {
            Err(FinError::msg(format!("公式语法错误：期望 {t:?}")))
        }
    }

    pub fn parse(&mut self) -> Result<Money, FinError> {
        let v = self.expr()?;
        if self.peek() != &Tok::Eof {
            return Err(FinError::msg("公式结尾有多余内容"));
        }
        Ok(v)
    }

    fn expr(&mut self) -> Result<Money, FinError> {
        let mut v = self.term()?;
        loop {
            match self.peek() {
                Tok::Plus => {
                    self.bump();
                    let t = self.term()?;
                    v = v
                        .checked_add(t)
                        .ok_or_else(|| FinError::msg("公式运算溢出：加法超出可表示范围"))?;
                }
                Tok::Minus => {
                    self.bump();
                    let t = self.term()?;
                    v = v
                        .checked_sub(t)
                        .ok_or_else(|| FinError::msg("公式运算溢出：减法超出可表示范围"))?;
                }
                _ => break,
            }
        }
        Ok(v)
    }

    fn term(&mut self) -> Result<Money, FinError> {
        let mut v = self.factor()?;
        loop {
            match self.peek() {
                Tok::Star => {
                    self.bump();
                    let f = self.factor()?;
                    // 不能用 `*` 运算符：rust_decimal 乘法溢出会 panic，
                    // 而公式里 `QM("1001")*QM("1002")` 两个大余额相乘就能踩到
                    // （实测 1e15 × 1e15 = 1e30 > Decimal 上限 ~7.9e28）。
                    v = v
                        .checked_mul(f.inner())
                        .ok_or_else(|| FinError::msg("公式运算溢出：乘法超出可表示范围"))?;
                }
                Tok::Slash => {
                    self.bump();
                    let d = self.factor()?;
                    // L-1：公式除零不再静默置 0——金额公式会把 0 当合法结果算下去
                    v = v
                        .checked_div(d)
                        .ok_or_else(|| FinError::msg("公式除零：除数为 0"))?;
                }
                _ => break,
            }
        }
        Ok(v)
    }

    fn factor(&mut self) -> Result<Money, FinError> {
        match self.peek().clone() {
            Tok::Num(n) => {
                self.bump();
                Ok(n)
            }
            Tok::Minus => {
                self.bump();
                Ok(self.factor()?.negated())
            }
            Tok::Plus => {
                self.bump();
                self.factor()
            }
            Tok::LParen => {
                self.bump();
                let v = self.expr()?;
                self.expect(Tok::RParen)?;
                Ok(v)
            }
            Tok::Ident(f) => {
                self.bump();
                self.call(&f)
            }
            _ => Err(FinError::msg(format!("公式语法错误：{:?}", self.peek()))),
        }
    }

    /// 函数调用：`NAME(a1, a2, a3)`，参数可留空
    fn call(&mut self, name: &str) -> Result<Money, FinError> {
        self.expect(Tok::LParen)?;
        let mut args: Vec<Option<String>> = Vec::new();
        if self.peek() == &Tok::RParen {
            self.bump();
            return self.apply(name, &args);
        }
        loop {
            match self.peek().clone() {
                Tok::Comma | Tok::RParen => {
                    args.push(None);
                    if self.bump() == Tok::RParen {
                        break;
                    }
                }
                Tok::Str(s) => {
                    self.bump();
                    args.push(Some(s));
                    match self.bump() {
                        Tok::Comma => {}
                        Tok::RParen => break,
                        _ => return Err(FinError::msg("公式语法错误：参数后应为逗号或右括号")),
                    }
                }
                Tok::Num(n) => {
                    self.bump();
                    // 保留精确数值：`to_string()` 是金额显示格式（2 位小数 + 千分位），
                    // 会让期间偏移 `-1` 变成 `-1.00` 而解析失败
                    args.push(Some(n.fmt_exact()));
                    match self.bump() {
                        Tok::Comma => {}
                        Tok::RParen => break,
                        _ => return Err(FinError::msg("公式语法错误：参数后应为逗号或右括号")),
                    }
                }
                // 参数位置也可以是一个表达式（比如 -1）
                Tok::Minus | Tok::Plus => {
                    let v = self.expr()?;
                    args.push(Some(v.fmt_exact()));
                    match self.bump() {
                        Tok::Comma => {}
                        Tok::RParen => break,
                        _ => return Err(FinError::msg("公式语法错误：参数后应为逗号或右括号")),
                    }
                }
                _ => return Err(FinError::msg("公式语法错误：非法的函数参数")),
            }
        }
        self.apply(name, &args)
    }

    fn apply(&self, name: &str, args: &[Option<String>]) -> Result<Money, FinError> {
        let a = |i: usize| -> Option<String> { args.get(i).cloned().flatten() };
        let code = a(0).ok_or_else(|| FinError::msg(format!("{name}() 缺少科目参数")))?;
        // 期间偏移：允许 -1 / -12 这类写法，也容忍历史公式里的 "-1.00" 小数写法。
        // 解析不出来必须报错——旧的 `unwrap_or(0)` 会把 `QM("1001","abc")` 静默
        // 当成"本期"，报表数字看着正常但口径是错的，财务上比报错难查得多。
        let offset: i32 = match a(1) {
            Some(s) => {
                let t = s.trim().replace(',', "");
                if t.is_empty() {
                    0
                } else {
                    t.parse::<i32>()
                        .ok()
                        .or_else(|| {
                            Money::parse(&t)
                                .ok()
                                .and_then(|m| m.inner().trunc().to_i32())
                        })
                        .ok_or_else(|| {
                            FinError::msg(format!(
                                "{name}() 的期间偏移无法解析：{s}（应为整数，如 0 / -1 / -12）"
                            ))
                        })?
                }
            }
            None => 0,
        };
        let period = self.period.add_months(offset);
        let dir = a(2);
        let dir = dir.as_deref().map(|s| s.trim());
        match name.to_ascii_uppercase().as_str() {
            "QC" => self.src.qc(&code, period, dir),
            "QM" => self.src.qm(&code, period, dir),
            "FS" => self.src.fs(&code, period, dir),
            "LFS" => self.src.lfs(&code, period, dir),
            "JE" => self.src.qm(&code, period, dir).map(|m| m.abs()),
            _ => Err(FinError::msg(format!("不支持的函数：{name}"))),
        }
    }
}

/// 求值一条公式
pub fn eval(src: &str, ctx: &dyn FormulaSource, period: Period) -> Result<Money, FinError> {
    let t = src.trim();
    if t.is_empty() {
        return Ok(Money::ZERO);
    }
    // 纯数字（含负号）走快路径，不必进语法分析
    if let Ok(n) = Money::parse(t) {
        return Ok(n);
    }
    let toks = tokenize(t)?;
    let mut p = Parser {
        toks: &toks,
        pos: 0,
        src: ctx,
        period,
    };
    // 中间过程不取整，避免 (a/b)*100 这类式子被逐步舍入吃掉精度
    Ok(p.parse()?.round2())
}

/// 公式语法检查（只解析不取数，用于在报表设计器里即时报错）
pub fn check(src: &str) -> Result<(), FinError> {
    struct Null;
    impl FormulaSource for Null {
        fn qc(&self, _: &str, _: Period, _: Option<&str>) -> Result<Money, FinError> {
            Ok(Money::ZERO)
        }
        fn qm(&self, _: &str, _: Period, _: Option<&str>) -> Result<Money, FinError> {
            Ok(Money::ZERO)
        }
        fn fs(&self, _: &str, _: Period, _: Option<&str>) -> Result<Money, FinError> {
            Ok(Money::ZERO)
        }
        fn lfs(&self, _: &str, _: Period, _: Option<&str>) -> Result<Money, FinError> {
            Ok(Money::ZERO)
        }
    }
    let t = src.trim();
    if t.is_empty() {
        return Ok(());
    }
    let toks = tokenize(t)?;
    let mut p = Parser {
        toks: &toks,
        pos: 0,
        src: &Null,
        period: Period::from_ymm(202601),
    };
    p.parse()?;
    Ok(())
}

/// 把公式里用到的科目代码抽出来（用于"这个报表依赖哪些科目"的提示）
pub fn referenced_accounts(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let toks = match tokenize(src) {
        Ok(t) => t,
        Err(_) => return out,
    };
    for w in toks.windows(3) {
        if let (Tok::Ident(_), Tok::LParen, Tok::Str(s)) = (&w[0], &w[1], &w[2]) {
            if !s.is_empty() && !out.contains(s) {
                out.push(s.clone());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct Fake(HashMap<&'static str, Money>);
    impl FormulaSource for Fake {
        fn qc(&self, _: &str, _: Period, _: Option<&str>) -> Result<Money, FinError> {
            Ok(self.0.get("qc").copied().unwrap_or(Money::ZERO))
        }
        fn qm(&self, _: &str, _: Period, _: Option<&str>) -> Result<Money, FinError> {
            Ok(self.0.get("qm").copied().unwrap_or(Money::ZERO))
        }
        fn fs(&self, _: &str, _: Period, _: Option<&str>) -> Result<Money, FinError> {
            Ok(self.0.get("fs").copied().unwrap_or(Money::ZERO))
        }
        fn lfs(&self, _: &str, _: Period, _: Option<&str>) -> Result<Money, FinError> {
            Ok(self.0.get("lfs").copied().unwrap_or(Money::ZERO))
        }
    }

    fn ctx() -> Fake {
        let mut m = HashMap::new();
        m.insert("qc", Money::parse("100").unwrap());
        m.insert("qm", Money::parse("300").unwrap());
        m.insert("fs", Money::parse("200").unwrap());
        m.insert("lfs", Money::parse("1500").unwrap());
        Fake(m)
    }
    fn p() -> Period {
        Period::new(2026, 1).unwrap()
    }
    fn e(s: &str) -> Money {
        eval(s, &ctx(), p()).unwrap()
    }

    #[test]
    fn arithmetic() {
        assert_eq!(e("1+2*3"), Money::parse("7").unwrap());
        assert_eq!(e("(1+2)*3"), Money::parse("9").unwrap());
        assert_eq!(e("10/4"), Money::parse("2.5").unwrap());
        assert_eq!(e("-5+8"), Money::parse("3").unwrap());
        assert_eq!(e("100"), Money::parse("100").unwrap());
        assert_eq!(e("   "), Money::ZERO);
    }

    #[test]
    fn functions() {
        assert_eq!(e("QC(\"1001\")"), Money::parse("100").unwrap());
        assert_eq!(e("QM(\"1001\")"), Money::parse("300").unwrap());
        assert_eq!(e("FS(\"6001\",,\"贷\")"), Money::parse("200").unwrap());
        assert_eq!(e("LFS(\"6001\")"), Money::parse("1500").unwrap());
        assert_eq!(e("QM(\"1001\")-QC(\"1001\")"), Money::parse("200").unwrap());
        assert_eq!(
            e("FS(\"6001\")/LFS(\"6001\")*100"),
            Money::parse("13.33").unwrap()
        );
    }

    #[test]
    fn div_by_zero_is_error() {
        // L-1：公式除零不再静默返回 0——金额公式会把 0 当合法结果算下去
        let err = eval("100/(3-3)", &ctx(), p()).unwrap_err();
        assert!(err.to_string().contains("除零"), "应报除零错误：{err}");
    }

    #[test]
    fn syntax_errors_reported() {
        assert!(eval("1+", &ctx(), p()).is_err());
        assert!(eval("(1+2", &ctx(), p()).is_err());
        assert!(eval("XX(\"1001\")", &ctx(), p()).is_err());
        assert!(eval("QC()", &ctx(), p()).is_err());
        assert!(eval("1 & 2", &ctx(), p()).is_err());
    }

    #[test]
    fn check_and_refs() {
        assert!(check("QM(\"1001\")+QM(\"1002\")").is_ok());
        assert!(check("QM(\"1001\"").is_err());
        let r = referenced_accounts("QM(\"1001\")+QM(\"1002\")-FS(\"6001\",,\"贷\")");
        assert_eq!(r, vec!["1001", "1002", "6001"]);
    }

    /// 记录每次取数落在哪个期间，用来验证期间偏移
    struct Spy {
        seen: std::cell::RefCell<Vec<i32>>,
        val: Money,
    }
    impl FormulaSource for Spy {
        fn qc(&self, _c: &str, p: Period, _d: Option<&str>) -> Result<Money, FinError> {
            self.seen.borrow_mut().push(p.ymm());
            Ok(self.val)
        }
        fn qm(&self, c: &str, p: Period, d: Option<&str>) -> Result<Money, FinError> {
            self.qc(c, p, d)
        }
        fn fs(&self, c: &str, p: Period, d: Option<&str>) -> Result<Money, FinError> {
            self.qc(c, p, d)
        }
        fn lfs(&self, c: &str, p: Period, d: Option<&str>) -> Result<Money, FinError> {
            self.qc(c, p, d)
        }
    }

    /// 乘法溢出必须报错而不是 panic。
    /// 回归：`QM("1001")*QM("1002")` 两个 1e15 余额相乘 = 1e30 > Decimal 上限
    /// ~7.9e28，`rust_decimal` 的 `*` 会 panic（Web 端 500、桌面端崩进程）。
    #[test]
    fn mul_overflow_is_error_not_panic() {
        struct Big;
        impl FormulaSource for Big {
            fn qc(&self, _: &str, _: Period, _: Option<&str>) -> Result<Money, FinError> {
                Ok(Money::parse("999999999999999.99").unwrap())
            }
            fn qm(&self, c: &str, p: Period, d: Option<&str>) -> Result<Money, FinError> {
                self.qc(c, p, d)
            }
            fn fs(&self, c: &str, p: Period, d: Option<&str>) -> Result<Money, FinError> {
                self.qc(c, p, d)
            }
            fn lfs(&self, c: &str, p: Period, d: Option<&str>) -> Result<Money, FinError> {
                self.qc(c, p, d)
            }
        }
        let p = Period::new(2026, 1).unwrap();
        // 确认在 unchecked 路径上确实会 panic（证明本测试不是空转）
        let raw = Money::parse("999999999999999.99").unwrap();
        assert!(
            std::panic::catch_unwind(|| raw * raw.0).is_err(),
            "rust_decimal 乘法仍应溢出 panic，说明 overflow 前提成立"
        );
        for f in [
            "QM(\"1001\")*QM(\"1002\")",
            "(QM(\"1001\")*QM(\"1002\"))*QM(\"1003\")",
        ] {
            let err = eval(f, &Big, p).unwrap_err();
            assert!(err.to_string().contains("溢出"), "{f} 应报溢出：{err}");
        }
        // 未溢出的乘法照常工作
        assert_eq!(e("2*3"), Money::parse("6").unwrap());
    }

    #[test]
    fn add_overflow_is_error() {
        struct Big;
        impl FormulaSource for Big {
            fn qc(&self, _: &str, _: Period, _: Option<&str>) -> Result<Money, FinError> {
                // 7e28，两项相加即 1.4e29 > Decimal 上限 ~7.9e28
                Ok(Money::parse("70000000000000000000000000000").unwrap())
            }
            fn qm(&self, c: &str, p: Period, d: Option<&str>) -> Result<Money, FinError> {
                self.qc(c, p, d)
            }
            fn fs(&self, c: &str, p: Period, d: Option<&str>) -> Result<Money, FinError> {
                self.qc(c, p, d)
            }
            fn lfs(&self, c: &str, p: Period, d: Option<&str>) -> Result<Money, FinError> {
                self.qc(c, p, d)
            }
        }
        let err = eval(
            "QM(\"1001\")+QM(\"1002\")",
            &Big,
            Period::new(2026, 1).unwrap(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("溢出"), "{err}");
    }

    /// 期间偏移解析不出来必须报错。
    /// 回归：旧的 `unwrap_or(0)` 让 `QM("1001","abc")` 静默取本期数据。
    #[test]
    fn bad_period_offset_is_error() {
        let spy = Spy {
            seen: std::cell::RefCell::new(Vec::new()),
            val: Money::ZERO,
        };
        let err = eval("QM(\"1001\",\"abc\")", &spy, p()).unwrap_err();
        assert!(
            err.to_string().contains("期间偏移"),
            "应报期间偏移解析失败：{err}"
        );
        assert!(spy.seen.borrow().is_empty(), "解析失败时不应取数");

        // 合法偏移仍然照常工作
        let spy2 = Spy {
            seen: std::cell::RefCell::new(Vec::new()),
            val: Money::ZERO,
        };
        eval("QM(\"1001\",-1)", &spy2, p()).unwrap();
        eval("QM(\"1001\",,)", &spy2, p()).unwrap();
        assert_eq!(spy2.seen.borrow().as_slice(), &[202512, 202601]);
    }

    /// 取数失败必须一路冒泡成错误，不能变成 0。
    /// 回归：旧的 `FormulaSource` 返回裸 `Money`，余额快照读失败（库被锁、行损坏）
    /// 与"这个科目就是 0"在报表上完全一样——印出一份看着正常的数字，底下的取数
    /// 其实根本没成功。
    #[test]
    fn source_error_propagates_instead_of_zero() {
        struct Broken;
        impl FormulaSource for Broken {
            fn qc(&self, _: &str, _: Period, _: Option<&str>) -> Result<Money, FinError> {
                Err(FinError::db("余额快照读取失败"))
            }
            fn qm(&self, c: &str, p: Period, d: Option<&str>) -> Result<Money, FinError> {
                self.qc(c, p, d)
            }
            fn fs(&self, c: &str, p: Period, d: Option<&str>) -> Result<Money, FinError> {
                self.qc(c, p, d)
            }
            fn lfs(&self, c: &str, p: Period, d: Option<&str>) -> Result<Money, FinError> {
                self.qc(c, p, d)
            }
        }
        for f in [
            "QC(\"1001\")",
            "QM(\"1001\")",
            "FS(\"6001\")",
            "LFS(\"6001\")",
            "JE(\"1001\")",
            "QM(\"1001\")+1",
        ] {
            let err = eval(f, &Broken, p()).unwrap_err();
            assert!(
                err.to_string().contains("余额快照读取失败"),
                "{f} 应把取数错误抛出来，而不是当成 0：{err}"
            );
        }
        // 纯数字与空公式仍照常返回 0（无取数可失败）
        assert_eq!(
            eval("100", &Broken, p()).unwrap(),
            Money::parse("100").unwrap()
        );
        assert_eq!(eval("   ", &Broken, p()).unwrap(), Money::ZERO);
        // check() 用的 Null 取数实现应返回 Ok，纯语法检查不受影响
        assert!(check("QM(\"1001\")+1").is_ok());
    }
}
