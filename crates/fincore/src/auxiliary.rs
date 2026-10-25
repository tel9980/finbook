//! 辅助核算档案：客户、供应商、部门、职员、项目、存货、银行账户
//!
//! 各类档案共用一张表（`kind` 区分），差异字段放在 `props` 里，
//! 这样新增档案类型不必改动表结构。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::account::AuxKind;

/// 档案常用属性键
pub mod prop {
    pub const TAX_NO: &str = "tax_no";
    pub const BANK_NAME: &str = "bank_name";
    pub const BANK_ACCOUNT: &str = "bank_account";
    pub const ADDRESS: &str = "address";
    pub const PHONE: &str = "phone";
    pub const CONTACT: &str = "contact";
    pub const CREDIT_LIMIT: &str = "credit_limit";
    /// 信用天数（账期）
    pub const PAY_DAYS: &str = "pay_days";
    /// 期初应收/应付余额
    pub const OPENING: &str = "opening";
    /// 负责人
    pub const MANAGER: &str = "manager";
    /// 岗位
    pub const TITLE: &str = "title";
    /// 所属部门
    pub const DEPT: &str = "dept";
    /// 规格型号
    pub const SPEC: &str = "spec";
    /// 计量单位
    pub const UNIT: &str = "unit";
    /// 计价方法：weighted / fifo / moving
    pub const COST_METHOD: &str = "cost_method";
    /// 参考成本
    pub const REF_COST: &str = "ref_cost";
    /// 开户行账号所属币种
    pub const CURRENCY: &str = "currency";
    /// 客户/供应商专属税点（百分数，如 "3"、"6"、"13"、"0"）
    ///
    /// 现实里同一个老板的不同客户税点经常不一样：有的客户要 13% 专票、
    /// 有的只要普票、有的是小规模代开 3%、有的干脆不要票（0）。
    /// 所以税率不能写死在账套参数里，必须挂在往来单位档案上，一客一率。
    ///
    /// 空字符串 = 未设置，用账套默认税率（见 [`TAX_RATE_POLICY`]）。
    pub const TAX_RATE: &str = "tax_rate";
    /// 开票政策：invoice=每次开票 / sometimes=有时开有时不开 / none=从不开票
    ///
    /// 影响的是**提示**与统计口径，不阻断记账——有没有票都不影响业务入账。
    pub const TAX_RATE_POLICY: &str = "tax_policy";
}

/// 开票政策的存储值 → 中文标签。
///
/// 库里存 `invoice` / `sometimes` / `none` 这类稳定的码，导出的 Excel 里写
/// 中文（老板看得懂、能直接在表里改）。两边必须成对，否则「导出→改→导回」
/// 会把开票情况改回未设置，而人看不出哪里丢了。
pub fn tax_policy_label(v: &str) -> &'static str {
    match v.trim() {
        "invoice" => "每次都开票",
        "sometimes" => "有时开有时不开",
        "none" => "从不开票",
        _ => "",
    }
}

/// 中文标签（或英文码）→ 存储值。认不出来返回 `None`，调用方应当**保留原值**
/// 而不是写空 —— 表里填了「偶尔开」这种自由文字时，静默清空比报错更难查。
pub fn tax_policy_value(label: &str) -> Option<&'static str> {
    let t = label.trim();
    if t.is_empty() {
        return None;
    }
    Some(match t {
        "每次都开票" | "每次开票" | "全开" | "invoice" => "invoice",
        "有时开有时不开" | "有时开" | "部分开" | "sometimes" => "sometimes",
        "从不开票" | "不开票" | "不要票" | "无票" | "none" => "none",
        _ => return None,
    })
}

/// 辅助核算档案
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct AuxEntity {
    pub id: i64,
    pub kind: AuxKind,
    pub code: String,
    pub name: String,
    /// 上级编码（用于部门/存货等分级档案）
    pub parent_code: Option<String>,
    pub disabled: bool,
    /// 扩展属性
    pub props: BTreeMap<String, String>,
    pub memo: String,
}

impl AuxEntity {
    pub fn new(kind: AuxKind, code: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: 0,
            kind,
            code: code.into(),
            name: name.into(),
            parent_code: None,
            disabled: false,
            props: BTreeMap::new(),
            memo: String::new(),
        }
    }

    pub fn prop(&self, key: &str) -> Option<&String> {
        self.props.get(key)
    }
    pub fn prop_or<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.props.get(key).map(|s| s.as_str()).unwrap_or(default)
    }
    pub fn set_prop(&mut self, key: &str, value: impl Into<String>) {
        self.props.insert(key.to_string(), value.into());
    }
    /// 展示用的一行摘要，列表界面直接显示
    pub fn summary(&self) -> String {
        match self.kind {
            AuxKind::Customer | AuxKind::Supplier => {
                let tax = self.prop(prop::TAX_NO).cloned().unwrap_or_default();
                if tax.is_empty() {
                    String::new()
                } else {
                    format!("税号 {tax}")
                }
            }
            AuxKind::Employee => format!(
                "{} {}",
                self.prop_or(prop::DEPT, ""),
                self.prop_or(prop::TITLE, "")
            )
            .trim()
            .to_string(),
            AuxKind::Item => format!(
                "{} {}",
                self.prop_or(prop::SPEC, ""),
                self.prop_or(prop::UNIT, "")
            )
            .trim()
            .to_string(),
            AuxKind::Bank => format!(
                "{} {}",
                self.prop_or(prop::BANK_NAME, ""),
                self.prop_or(prop::BANK_ACCOUNT, "")
            )
            .trim()
            .to_string(),
            _ => self.memo.clone(),
        }
    }
}

/// 档案列表筛选条件
#[derive(Clone, Default, Debug)]
pub struct AuxQuery {
    pub kind: Option<AuxKind>,
    pub keyword: Option<String>,
    pub include_disabled: bool,
    pub parent_code: Option<String>,
}

impl AuxQuery {
    pub fn kind(k: AuxKind) -> Self {
        Self {
            kind: Some(k),
            keyword: None,
            include_disabled: false,
            parent_code: None,
        }
    }
    pub fn with_keyword(mut self, kw: &str) -> Self {
        let kw = kw.trim().to_string();
        self.keyword = if kw.is_empty() { None } else { Some(kw) };
        self
    }
    pub fn with_disabled(mut self, on: bool) -> Self {
        self.include_disabled = on;
        self
    }
}

/// 校验档案：编码必填、同类内唯一、名称必填
pub fn validate_aux(e: &AuxEntity, existing_codes: &[String]) -> Vec<String> {
    let mut v = Vec::new();
    if e.code.trim().is_empty() {
        v.push("编码不能为空".to_string());
    }
    if e.name.trim().is_empty() {
        v.push("名称不能为空".to_string());
    }
    if existing_codes.iter().any(|c| c == &e.code) {
        v.push(format!("编码 {} 已存在", e.code));
    }
    if e.kind == AuxKind::Item && e.prop(prop::UNIT).map(|s| s.is_empty()).unwrap_or(true) {
        v.push("存货档案必须指定计量单位".to_string());
    }
    v
}

/// 一键预置的常用档案条目（纯数据，不含存储）
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct AuxPreset {
    pub kind: AuxKind,
    pub code: &'static str,
    pub name: &'static str,
    /// 备注：写清这条是「通用占位」，提醒用户按自己情况改
    pub memo: &'static str,
}

/// 某个类别可预置的常用档案。返回空 Vec 表示该类别不预置。
///
/// **只预置「任何公司都成立」的条目**，见 `all_preset_aux` 的注释。
pub fn preset_aux(kind: AuxKind) -> Vec<AuxPreset> {
    match kind {
        // 6601 销售费用 / 6602 管理费用 / 6603 财务费用 三个科目的
        // aux 都是 Dept，validate_entry 又要求「必须填写」——
        // 没有部门档案就一行费用都记不了。这三个部门任何公司都有。
        AuxKind::Dept => vec![
            AuxPreset {
                kind: AuxKind::Dept,
                code: "D01",
                name: "销售部",
                memo: "预置的通用部门，可改名或删除后填自己的",
            },
            AuxPreset {
                kind: AuxKind::Dept,
                code: "D02",
                name: "管理部",
                memo: "预置的通用部门，可改名或删除后填自己的",
            },
            AuxPreset {
                kind: AuxKind::Dept,
                code: "D03",
                name: "财务部",
                memo: "预置的通用部门，可改名或删除后填自己的",
            },
        ],
        // 100201~100207 全部 aux=Bank，同样必填。
        // 只给两个：基本户（对应 100201）+ 一般户（对应 100202），
        // 多了是猜用户开几家银行。
        AuxKind::Bank => vec![
            AuxPreset {
                kind: AuxKind::Bank,
                code: "B01",
                name: "基本户",
                memo: "预置的通用账户，请点开补填开户行和账号",
            },
            AuxPreset {
                kind: AuxKind::Bank,
                code: "B02",
                name: "一般户",
                memo: "预置的通用账户，请点开补填开户行和账号",
            },
        ],
        // 客户/供应商/存货/职员/项目**一律不预置**：这些名字没有通用值。
        // 预置一个「待填客户」会让用户以为它是真客户，凭证里带出去
        // 变成脏数据，而且他自己想不起来要回来改 —— 比空着更糟。
        _ => Vec::new(),
    }
}

/// 全部可预置条目（新建账套时一次性灌进去用）
pub fn all_preset_aux() -> Vec<AuxPreset> {
    AuxKind::ALL.iter().flat_map(|k| preset_aux(*k)).collect()
}

/// 该类别有没有可预置的常用档案（界面据此隐藏按钮）
pub fn has_preset(kind: AuxKind) -> bool {
    !preset_aux(kind).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn props_and_summary() {
        let mut c = AuxEntity::new(AuxKind::Customer, "C001", "华东商贸有限公司");
        c.set_prop(prop::TAX_NO, "91310115MA1K3XXXXX");
        assert_eq!(c.prop(prop::TAX_NO).unwrap(), "91310115MA1K3XXXXX");
        assert!(c.summary().contains("91310115MA1K3XXXXX"));
        assert_eq!(c.prop_or("nope", "-"), "-");
    }

    #[test]
    fn validate() {
        let c = AuxEntity::new(AuxKind::Customer, "C001", "甲公司");
        assert!(validate_aux(&c, &[]).is_empty());
        assert!(!validate_aux(&c, &["C001".to_string()]).is_empty());
        let bad = AuxEntity::new(AuxKind::Customer, "", "");
        assert_eq!(validate_aux(&bad, &[]).len(), 2);
        let item = AuxEntity::new(AuxKind::Item, "I001", "钢板");
        assert!(validate_aux(&item, &[])
            .iter()
            .any(|s| s.contains("计量单位")));
    }

    /// 只预置部门 + 银行账户：这两类的名字有通用值，其余的是用户自己的数据
    #[test]
    fn 只预置有通用值的类别() {
        assert_eq!(preset_aux(AuxKind::Dept).len(), 3);
        assert_eq!(preset_aux(AuxKind::Bank).len(), 2);
        for k in [
            AuxKind::Customer,
            AuxKind::Supplier,
            AuxKind::Item,
            AuxKind::Employee,
            AuxKind::Project,
            AuxKind::CashFlow,
        ] {
            assert!(
                preset_aux(k).is_empty(),
                "{} 没有通用档案名，不该预置 —— 预置假客户会变成脏数据",
                k.label()
            );
            assert!(!has_preset(k));
        }
        assert!(has_preset(AuxKind::Dept) && has_preset(AuxKind::Bank));
    }

    /// all_preset_aux 要等于各类别之和，且编码在本类别内不重复
    #[test]
    fn 全量清单与分类一致且编码不重复() {
        let all = all_preset_aux();
        assert_eq!(all.len(), 5, "3 个部门 + 2 个银行账户");
        for k in AuxKind::ALL {
            let sub = preset_aux(*k);
            assert_eq!(
                all.iter().filter(|p| p.kind == *k).count(),
                sub.len(),
                "{} 数量对不上",
                k.label()
            );
            let mut codes: Vec<&str> = sub.iter().map(|p| p.code).collect();
            let n = codes.len();
            codes.sort_unstable();
            codes.dedup();
            assert_eq!(codes.len(), n, "{} 内部编码重复了", k.label());
        }
    }

    /// 预置条目本身要能通过档案校验（编码/名称非空）
    #[test]
    fn 预置条目合法() {
        for p in all_preset_aux() {
            assert!(!p.code.trim().is_empty() && !p.name.trim().is_empty());
            let e = AuxEntity::new(p.kind, p.code, p.name);
            assert!(
                validate_aux(&e, &[]).is_empty(),
                "{} {} 没过自身校验",
                p.code,
                p.name
            );
            assert!(
                !p.memo.is_empty(),
                "预置条目必须带说明，提醒用户这是通用占位"
            );
        }
    }
}
