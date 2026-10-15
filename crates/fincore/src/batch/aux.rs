//! 辅助档案批量粘贴解析。
//!
//! 为什么需要它：一家公司 50 个客户 + 30 个供应商 + 20 个职员，按「新增」按钮
//! 一个个敲是 100 次点开、填编码、填名称、保存。而财务手上几乎一定已经有一份
//! Excel/WPS 的名单 —— 让他们把两列复制过来粘一次，比让他们重打一遍便宜得多。
//!
//! 支持两种写法（都能从 Excel 直接复制）：
//!   1. `编码,名称` 两列 —— 有自己的编码体系时用
//!   2. 只写名称       —— 编码按类型前缀自动递增（C01/C02…、S01/S02…），
//!      小白多半不知道怎么给档案编编码，让他自己填反而是个坎
//!
//! 分隔符见 [`super::split_cols`]；标题行（含「编码」「名称」等）自动跳过。

use crate::{AuxEntity, AuxKind};

use super::split_cols;

/// 解析结果里的一行
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedRow {
    pub code: String,
    pub name: String,
    /// 该行被跳过的原因（None = 可导入）
    pub skip: Option<SkipReason>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SkipReason {
    /// 与本批次里前面的行重复
    DuplicateInBatch,
    /// 库里已有这个编码
    ExistsInBook,
}

impl SkipReason {
    /// 界面上给用户看的中文说明。两个端点共用，避免两边文案不一致
    /// （Web 端与桌面端的文案曾经就漂移过）。
    pub fn label(&self) -> &'static str {
        match self {
            SkipReason::DuplicateInBatch => "跳过：本批内重复",
            SkipReason::ExistsInBook => "跳过：编码已存在",
        }
    }
}

/// 解析结果
#[derive(Debug, Clone, Default)]
pub struct Parsed {
    pub rows: Vec<ParsedRow>,
}

impl Parsed {
    /// 可导入的行
    pub fn importable(&self) -> Vec<ParsedRow> {
        self.rows
            .iter()
            .filter(|r| r.skip.is_none())
            .cloned()
            .collect()
    }
    /// 一条都没解析出来（整段是垃圾，或全空）
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// 各类档案的编码前缀。与项目里既有惯例一致（示例账套就是 C01/S01/D01/E01/B01）。
pub fn code_prefix(kind: AuxKind) -> &'static str {
    match kind {
        AuxKind::Customer => "C",
        AuxKind::Supplier => "S",
        AuxKind::Dept => "D",
        AuxKind::Employee => "E",
        AuxKind::Item => "I",
        AuxKind::Bank => "B",
        _ => "X",
    }
}

/// 表头词表。**大小写敏感、精确全等**：`Code`/`CODE` 不算表头 —— 宁可把一行
/// 真实数据当成表头跳过（用户看得见少了哪行），也不要把标题行当数据写进去
/// （那是静默的脏数据）。
const HEADER_WORDS: &[&str] = &["编码", "代码", "编号", "code", "名称", "name"];

fn is_header_row(cols: &[&str]) -> bool {
    // 只在首行判（调用方保证 lineno == 0），且要求**整行全是**表头词。
    // 不能用 any：一行真实数据 `C01,编码` 里也有个「编码」，any 会把它整行吞掉。
    // aux 这边名称列是纯中文，any/all 在此本等价，但用 all 是为了和
    // begin_batch 保持同一判据，将来加新写法时不至于两处分叉。
    cols.iter().all(|c| {
        let c = c.trim();
        HEADER_WORDS.contains(&c)
    })
}

/// 「像不像一个编码」的启发式判据。
///
/// 要求**首字符是 ASCII 字母**：所以 `C01`/`KH-2024` 像编码，而纯数字的
/// `1001`、中文的「杭州明远」都不像。这是有意的 —— 辅助档案的编码都是人定的
/// （C01/S01），纯数字更可能是名称的一部分；而期初页的科目编码恰恰是纯数字，
/// 那边的判据是「含不含中文」，两套不要混用。
fn looks_like_code(s: &str) -> bool {
    !s.is_empty()
        && s.chars().count() <= 12
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && s.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
}

/// 自动编码计数器。抽成结构体是为了让「每次发号都必须记账」变成**类型上的
/// 要求**而不是靠人记得在每个分支里 `next_num += 1`。
///
/// 原实现是一个裸的 `next_num: u32` 加一个 `auto_taken: Vec<String>`，两者要
/// 手动同步。结果兜底分支（编码为空时）只用了 `next_num` 却**没登记到
/// `auto_taken`** —— 于是后面单列行会发出**同一个编码**，两条档案撞码，
/// 而 `insert_many_skip_existing` 认编码不认名，第二条会被静默跳过：
/// 用户粘 10 行进来，只进了 9 条，且界面上看不出哪条没进。
struct CodeGen<'a> {
    prefix: &'static str,
    existing: &'a [String],
    /// 本批已发出的编码（含自动与显式写入的），发号时避开
    taken: Vec<String>,
    next: u32,
}

impl<'a> CodeGen<'a> {
    fn new(prefix: &'static str, existing: &'a [String]) -> Self {
        // 从「库里最大号 +1」接着走，避免撞上已有的 C01
        let next = 1 + existing
            .iter()
            .filter_map(|c| {
                let rest = c.strip_prefix(prefix)?;
                rest.parse::<u32>().ok()
            })
            .max()
            .unwrap_or(0);
        Self {
            prefix,
            existing,
            taken: Vec::new(),
            next,
        }
    }

    /// 记下一个**显式写入**的编码，让自动发号避开它
    fn reserve(&mut self, code: &str) {
        self.taken.push(code.to_string());
        // 显式编码也推进计数器：`C09` 之后接单列行应该发 C10，不是 C09
        if let Some(rest) = code.strip_prefix(self.prefix) {
            if let Ok(n) = rest.parse::<u32>() {
                self.next = self.next.max(n + 1);
            }
        }
    }

    /// 发下一个自动编码：跳过库里已有的和本批已占用的
    fn next_code(&mut self) -> String {
        loop {
            let code = format!("{}{:02}", self.prefix, self.next);
            self.next += 1;
            if self.taken.contains(&code) || self.existing.contains(&code) {
                continue;
            }
            self.taken.push(code.clone());
            return code;
        }
    }
}

/// 解析粘贴文本。
///
/// `existing` 是库里已有的编码集合，用来标出「这个编码已经有了」；
/// 传空集合就只做批次内去重（便于纯解析的单元测试）。
pub fn parse(text: &str, kind: AuxKind, existing: &[String]) -> Parsed {
    let mut cg = CodeGen::new(code_prefix(kind), existing);
    let mut rows: Vec<ParsedRow> = Vec::new();
    let mut seen_in_batch: Vec<String> = Vec::new();

    for (lineno, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let cols = split_cols(line);
        let cols: Vec<&str> = cols.iter().map(|s| s.as_str()).collect();
        if cols.is_empty() {
            continue;
        }
        if lineno == 0 && is_header_row(&cols) {
            continue;
        }

        // 只有一列 → 名称，编码自动生成
        // 两列 → 编码,名称（容忍顺序反了：像编码的那列才是编码）
        let (code, name) = match cols.len() {
            1 => {
                let name = cols[0].to_string();
                (cg.next_code(), name)
            }
            _ => {
                let (a, b) = (cols[0], cols[1]);
                if looks_like_code(a) && !looks_like_code(b) {
                    (a.to_string(), b.to_string())
                } else if looks_like_code(b) && !looks_like_code(a) {
                    (b.to_string(), a.to_string())
                } else {
                    // 两列都不是编码形状 → 当成「名称,名称」，仍自动编号。
                    // 名称取**前两列**，不再 `cols.join(" ")`：三列以上（比如从
                    // Excel 多带了一列备注）时，旧写法会把编码也拼进名称，
                    // 生成「C01 客户名 备注」这种没人会要的档案。
                    (String::new(), cols[..2.min(cols.len())].join(" "))
                }
            }
        };

        if name.trim().is_empty() {
            continue;
        }
        let code = if code.trim().is_empty() {
            cg.next_code()
        } else {
            code.trim().to_string()
        };

        let skip = if seen_in_batch.contains(&code) {
            Some(SkipReason::DuplicateInBatch)
        } else if existing.contains(&code) {
            Some(SkipReason::ExistsInBook)
        } else {
            seen_in_batch.push(code.clone());
            cg.reserve(&code);
            None
        };

        rows.push(ParsedRow {
            code,
            name: name.trim().to_string(),
            skip,
        });
    }

    Parsed { rows }
}

/// 把可导入的行转成待写入的实体
pub fn to_entities(kind: AuxKind, rows: &[ParsedRow]) -> Vec<AuxEntity> {
    rows.iter()
        .filter(|r| r.skip.is_none())
        .map(|r| AuxEntity::new(kind, r.code.clone(), r.name.clone()))
        .collect()
}

/// 辅助档案的 Excel 导入模板（桌面端「下载模板」用）。
///
/// 布局必须是 [`parse`] 认得的写法 —— 「编码,名称」两列；示例编码按该类别的
/// 前缀发号（客户 C01…、供应商 S01…），用户照着填就行。
pub fn template(kind: AuxKind) -> super::Template {
    let p = code_prefix(kind);
    let label = kind.label();
    super::Template {
        headers: vec!["编码".to_string(), "名称".to_string()],
        examples: vec![
            vec![format!("{p}01"), format!("示例{label}甲")],
            vec![format!("{p}02"), format!("示例{label}乙")],
        ],
        help: vec![
            "第一页第一行是表头，从第二行开始填你自己的数据。".to_string(),
            "先把两行示例删掉，再填。".to_string(),
            "只写名称也行（不用填编码）：编码会自动编号，不会撞上已有的。".to_string(),
            "编码已存在的那行会被跳过，不会覆盖你原来的档案。".to_string(),
            "填完后回软件：点「从 Excel 文件载入」直接读进来，或复制粘贴到输入框。".to_string(),
            "载入后先预览，确认条数对了再点「确认导入」。".to_string(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(p: &Parsed) -> Vec<&str> {
        p.rows.iter().map(|r| r.code.as_str()).collect()
    }
    fn names(p: &Parsed) -> Vec<&str> {
        p.rows.iter().map(|r| r.name.as_str()).collect()
    }

    #[test]
    fn 两列编码名称() {
        let p = parse(
            "C01,杭州明远贸易有限公司\nC02,宁波华信电子有限公司",
            AuxKind::Customer,
            &[],
        );
        assert_eq!(codes(&p), vec!["C01", "C02"]);
        assert_eq!(names(&p)[0], "杭州明远贸易有限公司");
        assert_eq!(p.importable().len(), 2);
    }

    #[test]
    fn excel制表符() {
        let p = parse("C01\t杭州明远\nC02\t宁波华信", AuxKind::Customer, &[]);
        assert_eq!(codes(&p), vec!["C01", "C02"]);
        assert_eq!(names(&p)[0], "杭州明远");
    }

    /// 只给名称时编码自动编号 —— 小白不知道怎么编编码
    #[test]
    fn 只给名称自动编号() {
        let p = parse("杭州明远\n宁波华信\n苏州精诚", AuxKind::Customer, &[]);
        assert_eq!(codes(&p), vec!["C01", "C02", "C03"]);
        assert_eq!(names(&p), vec!["杭州明远", "宁波华信", "苏州精诚"]);
    }

    /// 自动编号从库里最大号 +1 接着走，不撞已有编码
    #[test]
    fn 编号接库中最大号() {
        let p = parse(
            "甲公司\n乙公司",
            AuxKind::Customer,
            &["C01".into(), "C05".into()],
        );
        assert_eq!(codes(&p), vec!["C06", "C07"]);
    }

    #[test]
    fn 各类别前缀不同() {
        assert_eq!(code_prefix(AuxKind::Customer), "C");
        assert_eq!(code_prefix(AuxKind::Supplier), "S");
        assert_eq!(code_prefix(AuxKind::Dept), "D");
        assert_eq!(code_prefix(AuxKind::Employee), "E");
        assert_eq!(code_prefix(AuxKind::Bank), "B");
    }

    #[test]
    fn 跳过表头() {
        let p = parse("编码,名称\nC01,杭州明远", AuxKind::Customer, &[]);
        assert_eq!(codes(&p), vec!["C01"], "表头不该进结果");
    }

    /// 表头必须**整行**都是表头词才跳。一行真数据里带个「编码」不能被吞掉
    #[test]
    fn 半行表头不跳() {
        let p = parse("C01,编码供应商", AuxKind::Customer, &[]);
        assert_eq!(codes(&p), vec!["C01"], "真实数据不能被当表头丢掉");
    }

    #[test]
    fn 库中已有标跳过() {
        let p = parse(
            "C01,杭州明远\nC02,宁波华信",
            AuxKind::Customer,
            &["C01".into()],
        );
        assert_eq!(p.rows.len(), 2);
        assert_eq!(p.rows[0].skip, Some(SkipReason::ExistsInBook));
        assert_eq!(p.rows[1].skip, None);
        assert_eq!(p.importable().len(), 1, "已有的不覆盖");
    }

    #[test]
    fn 批次内重复标出() {
        let p = parse("C01,甲\nC01,乙", AuxKind::Customer, &[]);
        assert_eq!(p.rows[1].skip, Some(SkipReason::DuplicateInBatch));
    }

    #[test]
    fn 清理空行与空白() {
        let p = parse(
            "\n\nC01,杭州明远\n\n   \nC02,宁波华信\n",
            AuxKind::Customer,
            &[],
        );
        assert_eq!(codes(&p), vec!["C01", "C02"]);
    }

    #[test]
    fn 全空输入不报错() {
        let p = parse("", AuxKind::Customer, &[]);
        assert!(p.is_empty());
        let p2 = parse("\n\n  \n", AuxKind::Customer, &[]);
        assert!(p2.is_empty());
    }

    /// 回归：显式编码 C09 之后，自动编号要接着发 C10，不能又发 C09
    #[test]
    fn 显式编码推进自动编号() {
        let p = parse("C09,甲\n乙\n丙", AuxKind::Customer, &[]);
        assert_eq!(codes(&p), vec!["C09", "C10", "C11"]);
    }

    /// 回归：兜底分支（原「两列都不像编码」）发号后，后续单列行不能拿到同一个码。
    ///
    /// 旧实现里兜底分支用了 `next_num` 却没登记 `auto_taken`、也没递增，
    /// 于是紧接着的单列行会发出**完全相同**的编码 —— 两条档案撞码，
    /// `insert_many_skip_existing` 会静默丢掉第二条，用户只看到「粘了 3 行进了 2 条」。
    #[test]
    fn 兜底发号不与后续撞码() {
        let p = parse(
            "1001,杭州明远\n1002,宁波华信\n苏州精诚",
            AuxKind::Customer,
            &[],
        );
        let cs = codes(&p);
        let mut uniq: Vec<&str> = cs.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(
            uniq.len(),
            cs.len(),
            "编码撞车了：{cs:?} —— 粘 3 行只能进 3 条，少一条是静默丢数据"
        );
        assert!(
            p.rows.iter().all(|r| r.skip.is_none()),
            "不该有跳过行：{cs:?}"
        );
    }

    /// 三列以上时不能把编码拼进名称（从 Excel 多带一列备注时）
    #[test]
    fn 多余列不混进名称() {
        let p = parse("C01,杭州明远,备注列", AuxKind::Customer, &[]);
        assert_eq!(codes(&p), vec!["C01"]);
        assert_eq!(names(&p)[0], "杭州明远", "备注不该混进名称");
    }

    #[test]
    fn 跳过原因有中文文案() {
        assert!(!SkipReason::DuplicateInBatch.label().is_empty());
        assert!(!SkipReason::ExistsInBook.label().is_empty());
    }

    /// to_entities 只给可导入的行转实体，且带上类别
    #[test]
    fn 转实体只含可导入行() {
        let p = parse("C01,甲\nC02,乙", AuxKind::Supplier, &["C01".into()]);
        let ents = to_entities(AuxKind::Supplier, &p.importable());
        assert_eq!(ents.len(), 1);
        assert_eq!(ents[0].code, "C02");
        assert_eq!(ents[0].kind, AuxKind::Supplier);
    }

    /// 模板出去的列，解析必须能认回来 —— 否则用户填完表导不进来。
    #[test]
    fn 模板列能被自家解析认回来() {
        for kind in [AuxKind::Customer, AuxKind::Supplier, AuxKind::Employee] {
            let t = template(kind);
            let mut text = t.headers.join(",");
            for r in &t.examples {
                text.push('\n');
                text.push_str(&r.join(","));
            }
            let p = parse(&text, kind, &[]);
            assert_eq!(p.rows.len(), t.examples.len(), "{kind:?}：表头应被跳过");
            assert_eq!(
                p.importable().len(),
                t.examples.len(),
                "{kind:?}：示例行都该可导入"
            );
            assert!(p.rows[0].code.starts_with(code_prefix(kind)));
        }
    }
}
