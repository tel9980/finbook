# 历史文档归档

这里放的是**项目各阶段的一次性报告**。它们记录「当时做了什么、当时怎么判断」，
有追溯价值；但**结论一律已过时**，不要当现状看。

## 现状去哪看

| 你想知道 | 看这个 |
|---|---|
| 现在有什么功能 | [../功能参考.md](../功能参考.md) |
| 怎么装、怎么跑、怎么测 | [../../README.md](../../README.md) |
| 记账怎么记（会计/老板娘） | [../日常记账手册.md](../日常记账手册.md) |
| 部署到服务器 | [../../DEPLOY.md](../../DEPLOY.md) |
| 还有哪些安全待办没做 | [../../SECURITY_TODO.md](../../SECURITY_TODO.md)（这一份是**在办**的，不是历史） |
| 每个版本改了什么 | [../releases/](../releases/) |

## 归档清单

### 制造业 ERP 补齐阶段（2026-09）

| 文件 | 内容 |
|---|---|
| [COMPLETION_SUMMARY.md](COMPLETION_SUMMARY.md) | 阶段完成总结：当时认为「全部完成」 |
| [MANUFACTURING_ERP_IMPLEMENTATION_REPORT.md](MANUFACTURING_ERP_IMPLEMENTATION_REPORT.md) | ERP 实现报告：模块与进度 |
| [ERP_MANUFACTURING_GAP_ANALYSIS.md](ERP_MANUFACTURING_GAP_ANALYSIS.md) | 与金蝶/用友的差距分析 |
| [GAP_RESEARCH_REPORT.md](GAP_RESEARCH_REPORT.md) | 功能对齐调研（2026-09 重写版） |
| [COMPREHENSIVE_COMPARISON.md](COMPREHENSIVE_COMPARISON.md) | 逐项对标总表 |
| [FINAL_MANUFACTURING_ERP_REPORT.md](FINAL_MANUFACTURING_ERP_REPORT.md) | 收尾报告 |
| [FINAL_VERIFICATION.md](FINAL_VERIFICATION.md) | 一次最终验证记录 |

**这批文档最典型的过时点**：把 MRP 运算、工序报工、BOM、委外加工、生产管理、
标准成本/成本差异都标成 ❌ 未实现 —— 这些后来**全部实现了**，有 e2e 用例与
集成测试守着（`mrp-procure-sales-cost`、`outsourcing_flow`、`mps_schedule_flow`、
`cost_variance`、`landed_cost` 系列）。

### 安全与交付（2026-09）

| 文件 | 内容 |
|---|---|
| [SECURITY_FIX_SUMMARY.md](SECURITY_FIX_SUMMARY.md) | 20 项安全修复的处理清单 |
| [FINAL_SECURITY_REPORT.md](FINAL_SECURITY_REPORT.md) | 安全收尾报告 |
| [DELIVERY_CHECKLIST.md](DELIVERY_CHECKLIST.md) | 交付物清单 |
| [code_review_report.html](code_review_report.html) | 一次性代码审计产物（HTML） |

> 安全项里**仍有未做完的**低优先级条目，跟踪在
> [../../SECURITY_TODO.md](../../SECURITY_TODO.md)，那份留在根目录保持更新。

## 为什么归档而不是删掉

删掉是另一种信息损失 —— 「当时为什么这么判断」在复盘时会用到。
真正的问题从来不是「文档旧」，而是「旧文档读起来像现状」。
所以每份归档文件开头都有一段 `⚠️ 本文是历史快照，结论已过时` 的声明，
写明文中哪个说法已被推翻、现状去哪看。

这条约定由 `tools/check-ci.js` 的第 2.8 项守着：归档目录里凡是含
「对标竞品覆盖度」或「测试条数」字样的文档，缺声明就让 CI 红。
