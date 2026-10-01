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
        assert!(validate_aux(&item, &[]).iter().any(|s| s.contains("计量单位")));
    }
}
