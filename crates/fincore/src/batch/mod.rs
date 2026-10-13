//! 「把 Excel/WPS 里的一列名单粘进来」—— 解析层，纯函数，不碰数据库也不碰 UI。
//!
//! # 为什么放在 fincore 而不是各自的前端里
//!
//! 桌面端（egui）与 Web 端（浏览器）都需要这套解析：**同一段粘贴文本，在两端
//! 必须得到完全一样的结果**。若各写一份，两边很快就会分叉 —— 桌面端支持了
//! 「名称,编码」倒序而 Web 没支持，用户会得出「这软件有的地方能用有的不能用」。
//!
//! 而且解析本身就是纯计算（切列、认表头、算自动编号、判方向），本来就属于
//! fincore 的职责范围。原实现放在 `finui` 里，Web 端要复用就得让 `finweb`
//! 依赖 `finui` —— 那会把 egui/eframe 整个拖进服务端，只为了调用几个不依赖
//! GUI 的函数。
//!
//! # 两个子模块
//!
//! - [`aux`]：辅助档案（客户/供应商/部门…），字段是「编码 + 名称」
//! - [`begin`]：期初余额，字段是「编码 + 金额（方向自动归位）」
//!
//! 两者共享切列与表头识别的判据，但**行模型故意不合并** —— 跳过原因集合不同
//! （期初多「无此科目」「金额看不懂」），强行合并会得到一堆两边都不适用的枚举变体。

pub mod aux;
pub mod begin; // 占位，下面紧跟文件

/// 按优先级切列：tab > 逗号类 > 连续空白。
///
/// 三者是**互斥**的，先命中先赢，不做混合：Excel 复制两列出来是 `\t`，此时
/// 单元格里就算带逗号（公司名「杭州明远,贸易有限公司」）也不能被当分隔符，
/// 否则一行会被切成三列。
///
/// 优先级不能反。写成「先找逗号、没有再找 tab」的话，Excel 粘出来的
/// `C01\t杭州明远贸易有限公司` 会因为名称里没有逗号而落到空白切分，
/// 而 tab 被 `split_whitespace` 当普通空白吞掉 —— 两列粘成一列，名称和编码
/// 糊在一起，看起来像「只写了名称」，于是自动编号生成一堆错误档案。
pub fn split_cols(line: &str) -> Vec<String> {
    if line.contains('\t') {
        return line.split('\t').map(|s| s.trim().to_string()).collect();
    }
    if line.contains(',') || line.contains('，') || line.contains('、') {
        return line
            .split([',', '，', '、'])
            .map(|s| s.trim().to_string())
            .collect();
    }
    line.split_whitespace().map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod split_tests {
    use super::*;

    /// Excel 复制两列出来就是 tab
    #[test]
    fn tab优先() {
        assert_eq!(split_cols("C01\t杭州明远"), vec!["C01", "杭州明远"]);
    }

    /// tab 存在时，单元格里带逗号也不能被当分隔符（公司名常带逗号）
    #[test]
    fn tab存在时不按逗号切() {
        assert_eq!(
            split_cols("C01\t杭州明远,贸易有限公司"),
            vec!["C01", "杭州明远,贸易有限公司"]
        );
    }

    /// 三种逗号都认
    #[test]
    fn 逗号三兄弟() {
        assert_eq!(split_cols("C01,杭州"), vec!["C01", "杭州"]);
        assert_eq!(split_cols("C01，杭州"), vec!["C01", "杭州"]);
        assert_eq!(split_cols("C01、杭州"), vec!["C01", "杭州"]);
    }

    /// 没分隔符就按空白切（从记事本贴「C01 杭州明远」也能认）
    #[test]
    fn 退化到空白切分() {
        assert_eq!(split_cols("C01 杭州明远"), vec!["C01", "杭州明远"]);
    }

    /// 全角空格（U+3000）在 Rust 的 split_whitespace 里算空白
    #[test]
    fn 全角空格也算空白() {
        assert_eq!(split_cols("C01　杭州明远"), vec!["C01", "杭州明远"]);
    }
}
