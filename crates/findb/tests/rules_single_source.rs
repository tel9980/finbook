//! 跨端口径：业务规则只许有一份。
//!
//! 这套账有桌面端（`finui`）和 Web 端（`finweb`）两个界面，将来还可能多一个。
//! 两端各自写一遍「含税怎么拆」「账龄怎么分档」，短期看不出问题，长期必然漂移：
//! 桌面端按 13% 拆、Web 端某次改动后按 ÷1.13 少了一分钱，客户对账才会发现。
//!
//! 所以这里立两条：
//!
//! 1. **口径表驱动**：把「含税 10,000 拆成不含税与税额」这类金额规则列成表，
//!    逐行断言 —— 漂了立刻红，而不是等月底对不上；
//! 2. **静态巡检**：`finui` / `finweb` 的 Rust 与前端脚本里**不许出现**含税拆分
//!    算式（`1.13`、`1.03`、`1.09` 之类的除法/乘法）。要用就到 `findb` 里调函数。
//!
//! 第二条是这次加的：它拦的不是「算错了」，而是「另起炉灶」。
//! 一旦有人在界面层手写 `/ 1.13`，哪怕当时算对了，下一次税率调整就会漏掉那一处。

use std::path::{Path, PathBuf};

use fincore::Money;
use findb::outsource::{self, Ticket};

fn m(s: &str) -> Money {
    Money::parse(s).unwrap()
}

/// 一份金额规则：输入含税额、票种，期望的不含税 / 税额
struct SplitCase {
    ticket: Ticket,
    gross: &'static str,
    net: &'static str,
    tax: &'static str,
}

/// 三种票的含税拆分口径（演示账里那三种：13% 专票 / 3% 征收率 / 无票）
#[test]
fn tax_split_table_is_the_single_source_of_truth() {
    let cases = [
        SplitCase {
            ticket: Ticket::Special13,
            gross: "10000",
            net: "8849.56",
            tax: "1150.44",
        },
        SplitCase {
            ticket: Ticket::Special13,
            gross: "11300",
            net: "10000.00",
            tax: "1300.00",
        },
        SplitCase {
            ticket: Ticket::Special13,
            gross: "113",
            net: "100.00",
            tax: "13.00",
        },
        // 一分钱也要分得干净：0.01 含税 → 不含税 0.01、税 0
        SplitCase {
            ticket: Ticket::Special13,
            gross: "0.01",
            net: "0.01",
            tax: "0.00",
        },
        SplitCase {
            ticket: Ticket::Rate3,
            gross: "10000",
            net: "9708.74",
            tax: "291.26",
        },
        SplitCase {
            ticket: Ticket::Rate3,
            gross: "103",
            net: "100.00",
            tax: "3.00",
        },
        SplitCase {
            ticket: Ticket::Plain,
            gross: "10000",
            net: "10000.00",
            tax: "0.00",
        },
        SplitCase {
            ticket: Ticket::Plain,
            gross: "0.01",
            net: "0.01",
            tax: "0.00",
        },
    ];
    for c in &cases {
        let s = outsource::split(c.ticket, m(c.gross)).unwrap();
        assert_eq!(
            (s.net, s.tax),
            (m(c.net), m(c.tax)),
            "票种 {} 含税 {} 拆分不对",
            c.ticket.label(),
            c.gross
        );
        // 不管怎么拆，不含税 + 税额必须等于含税 —— 差一分钱就是账不平
        assert_eq!(
            s.net + s.tax,
            m(c.gross),
            "票种 {} 含税 {} 拆完不平",
            c.ticket.label(),
            c.gross
        );
    }
}

/// 三票对比的顺序固定：13% → 3% → 无票（界面上「越往下越贵」要稳定）
#[test]
fn compare_table_keeps_its_order() {
    let rows = outsource::compare(m("10000")).unwrap();
    let labels: Vec<&str> = rows.iter().map(|r| r.ticket.label()).collect();
    assert_eq!(labels, vec!["13% 专用发票", "3% 征收率发票", "无票"]);
    // 真实成本递增：无票最贵（不能抵进项，还可能不能税前扣除）
    assert!(rows[0].real_cost() < rows[1].real_cost());
    assert!(rows[1].real_cost() < rows[2].real_cost());
}

/// 界面层不许另起炉灶：
///
/// 扫 `finui`/`finweb` 的 Rust 源码与前端脚本，出现含税拆分算式就算失败。
/// 例外（白名单）只有两类：注释里讲原理、以及 `findb` 自己传下来的税率字符串常量。
#[test]
fn ui_layers_do_not_reimplement_tax_split() {
    let roots = [crate_dir("finui", "src"), crate_dir("finweb", "src")];
    // 前端脚本也要扫：界面在浏览器里，最容易「顺手除一下」
    let static_dir = crate_dir("finweb", "static");

    let mut files: Vec<PathBuf> = Vec::new();
    for r in &roots {
        collect(r, "rs", &mut files);
    }
    collect(&static_dir, "js", &mut files);
    assert!(!files.is_empty(), "巡检路径没扫到文件，测试本身写错了");

    // 含税拆分只允许叫 findb 的函数名，不允许写算式
    const BAD: [(&str, &str); 6] = [
        (
            "/ 1.13",
            "含税转不含税要调 findb::outsource / meter / invoices，不要手写除法",
        ),
        ("/ 1.03", "3% 征收率同理，别在界面层手写"),
        ("/ 1.09", "9% 税率同理"),
        ("* 1.13", "不含税转含税同理"),
        ("* 1.03", "3% 征收率同理"),
        ("* 1.09", "9% 税率同理"),
    ];

    let mut bad: Vec<String> = Vec::new();
    for f in &files {
        let src = std::fs::read_to_string(f).unwrap_or_default();
        for (line_no, line) in src.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            for (pat, why) in BAD {
                if code.contains(pat) {
                    bad.push(format!(
                        "{}:{} 出现「{}」—— {}",
                        f.display(),
                        line_no + 1,
                        pat,
                        why
                    ));
                }
            }
        }
    }
    assert!(
        bad.is_empty(),
        "界面层自己算税了，规则会漂：\n{}",
        bad.join("\n")
    );
}

fn crate_dir(name: &str, sub: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(name)
        .join(sub)
}

fn collect(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            // 跳过构建产物与测试数据
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name == "target" || name == "node_modules" {
                continue;
            }
            collect(&p, ext, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some(ext) {
            out.push(p);
        }
    }
}
