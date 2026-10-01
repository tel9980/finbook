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
pub trait FormulaSource {
    /// 期初余额（带符号：借为正）
    fn qc(&self, code: &str, period: Period, dir: Option<&str>) -> Money;
    /// 期末余额（带符号）
    fn qm(&self, code: &str, period: Period, dir: Option<&str>) -> Money;
    /// 本期发生额（dir 为"借"/"贷"取单方向，None 取借贷差额）
    fn fs(&self, code: &str, period: Period, dir: Option<&str>) -> Money;
    /// 本年累计发生额
    fn lfs(&self, code: &str, period: Period, dir: Option<&str>) -> Money;
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
                    v += self.term()?;
                }
                Tok::Minus => {
                    self.bump();
                    v -= self.term()?;
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
                    v = v * self.factor()?.inner();
                }
                Tok::Slash => {
                    self.bump();
                    let d = self.factor()?;
                    // L-1：公式除零不再静默置 0——金额公式会把 0 当合法结果算下去
                    v = v.checked_div(d).ok_or_else(|| FinError::msg("公式除零：除数为 0"))?;
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
        let code = a(0)
            .ok_or_else(|| FinError::msg(format!("{name}() 缺少科目参数")))?;
        // 期间偏移：允许 -1 / -12 这类写法，也容忍历史公式里的 "-1.00" 小数写法
        let offset: i32 = match a(1) {
            Some(s) => {
                let t = s.trim().replace(',', "");
                if t.is_empty() {
                    0
                } else {
                    t.parse::<i32>().ok().or_else(|| {
                        Money::parse(&t)
                            .ok()
                            .and_then(|m| m.inner().trunc().to_i32())
                    })
                    .unwrap_or(0)
                }
            }
            None => 0,
        };
        let period = self.period.add_months(offset);
        let dir = a(2);
        let dir = dir.as_deref().map(|s| s.trim());
        match name.to_ascii_uppercase().as_str() {
            "QC" => Ok(self.src.qc(&code, period, dir)),
            "QM" => Ok(self.src.qm(&code, period, dir)),
            "FS" => Ok(self.src.fs(&code, period, dir)),
            "LFS" => Ok(self.src.lfs(&code, period, dir)),
            "JE" => Ok(self.src.qm(&code, period, dir).abs()),
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
        fn qc(&self, _: &str, _: Period, _: Option<&str>) -> Money {
            Money::ZERO
        }
        fn qm(&self, _: &str, _: Period, _: Option<&str>) -> Money {
            Money::ZERO
        }
        fn fs(&self, _: &str, _: Period, _: Option<&str>) -> Money {
            Money::ZERO
        }
        fn lfs(&self, _: &str, _: Period, _: Option<&str>) -> Money {
            Money::ZERO
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
        fn qc(&self, _: &str, _: Period, _: Option<&str>) -> Money {
            self.0.get("qc").copied().unwrap_or(Money::ZERO)
        }
        fn qm(&self, _: &str, _: Period, _: Option<&str>) -> Money {
            self.0.get("qm").copied().unwrap_or(Money::ZERO)
        }
        fn fs(&self, _: &str, _: Period, _: Option<&str>) -> Money {
            self.0.get("fs").copied().unwrap_or(Money::ZERO)
        }
        fn lfs(&self, _: &str, _: Period, _: Option<&str>) -> Money {
            self.0.get("lfs").copied().unwrap_or(Money::ZERO)
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
        assert_eq!(e("FS(\"6001\")/LFS(\"6001\")*100"), Money::parse("13.33").unwrap());
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
}
