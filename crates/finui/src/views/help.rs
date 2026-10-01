//! 帮助系统：用户指南与操作说明

use egui::{Color32, RichText, ScrollArea, Ui};

use crate::state::AppCtx;
use crate::theme;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum HelpTab {
    /// 快速入门
    QuickStart,
    /// 凭证管理
    Voucher,
    /// 科目管理
    Account,
    /// 报表
    Reports,
    /// 固定资产
    Assets,
    /// 存货管理
    Inventory,
    /// 工资管理
    Payroll,
    /// 费用报销
    Claims,
    /// 银行对账
    BankRec,
    /// 往来核销
    Settle,
    /// 期末处理
    PeriodEnd,
    /// 自动化
    Automation,
    /// 用户权限
    Security,
    /// 常见问题
    FAQ,
}

impl HelpTab {
    fn all() -> &'static [HelpTab] {
        &[
            HelpTab::QuickStart,
            HelpTab::Voucher,
            HelpTab::Account,
            HelpTab::Reports,
            HelpTab::Assets,
            HelpTab::Inventory,
            HelpTab::Payroll,
            HelpTab::Claims,
            HelpTab::BankRec,
            HelpTab::Settle,
            HelpTab::PeriodEnd,
            HelpTab::Automation,
            HelpTab::Security,
            HelpTab::FAQ,
        ]
    }

    fn label(&self) -> &'static str {
        match self {
            HelpTab::QuickStart => "快速入门",
            HelpTab::Voucher => "凭证管理",
            HelpTab::Account => "科目管理",
            HelpTab::Reports => "会计报表",
            HelpTab::Assets => "固定资产",
            HelpTab::Inventory => "存货管理",
            HelpTab::Payroll => "工资管理",
            HelpTab::Claims => "费用报销",
            HelpTab::BankRec => "银行对账",
            HelpTab::Settle => "往来核销",
            HelpTab::PeriodEnd => "期末处理",
            HelpTab::Automation => "月末自动化",
            HelpTab::Security => "用户权限",
            HelpTab::FAQ => "常见问题",
        }
    }
}

pub struct HelpView {
    tab: HelpTab,
    search: String,
}

impl Default for HelpView {
    fn default() -> Self {
        Self {
            tab: HelpTab::QuickStart,
            search: String::new(),
        }
    }
}

impl HelpView {
    pub fn invalidate(&mut self) {}

    pub fn enter(&mut self, _ctx: &mut AppCtx<'_>) {}

    pub fn show(&mut self, _ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        ui.heading(RichText::new("帮助中心").size(20.0));
        ui.separator();

        ui.horizontal(|ui| {
            ui.label("搜索：");
            ui.text_edit_singleline(&mut self.search);
        });

        ui.separator();

        ui.horizontal(|ui| {
            for tab in HelpTab::all() {
                if ui
                    .selectable_label(self.tab == *tab, tab.label())
                    .clicked()
                {
                    self.tab = *tab;
                }
            }
        });

        ui.separator();

        ScrollArea::vertical().show(ui, |ui| {
            match self.tab {
                HelpTab::QuickStart => self.show_quick_start(ui),
                HelpTab::Voucher => self.show_voucher_help(ui),
                HelpTab::Account => self.show_account_help(ui),
                HelpTab::Reports => self.show_reports_help(ui),
                HelpTab::Assets => self.show_assets_help(ui),
                HelpTab::Inventory => self.show_inventory_help(ui),
                HelpTab::Payroll => self.show_payroll_help(ui),
                HelpTab::Claims => self.show_claims_help(ui),
                HelpTab::BankRec => self.show_bank_rec_help(ui),
                HelpTab::Settle => self.show_settle_help(ui),
                HelpTab::PeriodEnd => self.show_period_end_help(ui),
                HelpTab::Automation => self.show_automation_help(ui),
                HelpTab::Security => self.show_security_help(ui),
                HelpTab::FAQ => self.show_faq(ui),
            }
        });
    }

    fn section_title(&self, ui: &mut Ui, title: &str) {
        ui.add_space(8.0);
        ui.label(
            RichText::new(title)
                .size(16.0)
                .color(theme::palette::PRIMARY),
        );
        ui.add_space(4.0);
    }

    fn help_text(&self, ui: &mut Ui, text: &str) {
        ui.label(RichText::new(text).color(Color32::GRAY));
        ui.add_space(4.0);
    }

    fn show_quick_start(&self, ui: &mut Ui) {
        self.section_title(ui, "快速入门指南");

        self.help_text(ui, "欢迎使用 FinBook 财务管理系统！本指南将帮助您快速上手。");

        self.section_title(ui, "1. 初始化设置");
        self.help_text(ui, "• 进入「账套参数」设置公司名称、税号等基本信息");
        self.help_text(ui, "• 进入「会计科目」维护科目表，系统已预置标准科目");
        self.help_text(ui, "• 进入「期初建账」录入各科目的期初余额");

        self.section_title(ui, "2. 日常操作");
        self.help_text(ui, "• 「填制凭证」录入记账凭证");
        self.help_text(ui, "• 「凭证查询」查看、修改、审核凭证");
        self.help_text(ui, "• 「账簿查询」查看明细账、总账、日记账");

        self.section_title(ui, "3. 月末处理");
        self.help_text(ui, "• 「月末自动化」执行期末调汇、自动转账");
        self.help_text(ui, "• 「期末处理」结转损益、月末结账");
        self.help_text(ui, "• 「会计报表」生成资产负债表、利润表、现金流量表");

        self.section_title(ui, "4. 辅助功能");
        self.help_text(ui, "• 「固定资产」管理资产卡片、计提折旧");
        self.help_text(ui, "• 「银行对账」导入银行流水、自动对账");
        self.help_text(ui, "• 「往来核销」管理应收应付、账龄分析");
    }

    fn show_voucher_help(&self, ui: &mut Ui) {
        self.section_title(ui, "凭证管理");

        self.section_title(ui, "填制凭证");
        self.help_text(ui, "1. 选择凭证字（记/收/付/转）");
        self.help_text(ui, "2. 输入日期和附件张数");
        self.help_text(ui, "3. 录入分录行：科目编码、摘要、借方/贷方金额");
        self.help_text(ui, "4. 系统自动校验借贷平衡");
        self.help_text(ui, "5. 保存后凭证状态为「已审核」");

        self.section_title(ui, "审核凭证");
        self.help_text(ui, "• 凭证必须审核后才能记账");
        self.help_text(ui, "• 审核人不能是制单人");
        self.help_text(ui, "• 已审核凭证不能直接修改，需先取消审核");

        self.section_title(ui, "记账");
        self.help_text(ui, "• 记账后凭证状态变为「已记账」");
        self.help_text(ui, "• 记账后数据将更新科目余额");
        self.help_text(ui, "• 已记账凭证不能修改或删除");

        self.section_title(ui, "作废凭证");
        self.help_text(ui, "• 作废凭证保留但不参与记账");
        self.help_text(ui, "• 作废凭证可以恢复为正常状态");

        self.section_title(ui, "常用功能");
        self.help_text(ui, "• 自动平衡：点击「补平」按钮自动补齐差额");
        self.help_text(ui, "• 凭证模板：保存常用凭证为模板，快速调用");
        self.help_text(ui, "• 辅助核算：客户、供应商、部门、项目等维度");
    }

    fn show_account_help(&self, ui: &mut Ui) {
        self.section_title(ui, "科目管理");

        self.section_title(ui, "科目结构");
        self.help_text(ui, "• 科目编码采用分级定长（默认4-2-2-2）");
        self.help_text(ui, "• 一级科目：4位（如1001库存现金）");
        self.help_text(ui, "• 二级科目：6位（如100201工行基本户）");
        self.help_text(ui, "• 末级科目才能直接记账");

        self.section_title(ui, "科目类别");
        self.help_text(ui, "• 资产类（1xxx）：借方余额");
        self.help_text(ui, "• 负债类（2xxx）：贷方余额");
        self.help_text(ui, "• 权益类（4xxx）：贷方余额");
        self.help_text(ui, "• 成本类（5xxx）：借方余额");
        self.help_text(ui, "• 损益类（6xxx）：收入贷方/费用借方");

        self.section_title(ui, "辅助核算");
        self.help_text(ui, "• 客户：应收账款、预收账款等");
        self.help_text(ui, "• 供应商：应付账款、预付账款等");
        self.help_text(ui, "• 部门：管理费用、销售费用等");
        self.help_text(ui, "• 项目：在建工程、研发支出等");
        self.help_text(ui, "• 存货：原材料、库存商品等（支持数量核算）");

        self.section_title(ui, "科目属性");
        self.help_text(ui, "• 现金科目：库存现金等");
        self.help_text(ui, "• 银行科目：银行存款等");
        self.help_text(ui, "• 外币核算：支持多币种");
        self.help_text(ui, "• 数量核算：支持数量金额式");
    }

    fn show_reports_help(&self, ui: &mut Ui) {
        self.section_title(ui, "会计报表");

        self.section_title(ui, "内置报表");
        self.help_text(ui, "• 资产负债表：反映企业财务状况");
        self.help_text(ui, "• 利润表：反映企业经营成果");
        self.help_text(ui, "• 现金流量表：反映现金流入流出");

        self.section_title(ui, "报表操作");
        self.help_text(ui, "1. 选择会计期间范围");
        self.help_text(ui, "2. 点击「刷新」生成报表");
        self.help_text(ui, "3. 支持导出为Excel或CSV");

        self.section_title(ui, "自定义报表");
        self.help_text(ui, "• 支持自定义行和列");
        self.help_text(ui, "• 公式支持科目取数、行间引用、常量");
        self.help_text(ui, "• 可保存为模板复用");

        self.section_title(ui, "现金流量表");
        self.help_text(ui, "• 经营活动：销售收款、采购付款等");
        self.help_text(ui, "• 投资活动：投资收益、购置资产等");
        self.help_text(ui, "• 筹资活动：借款、还款、分红等");
        self.help_text(ui, "• 系统自动归集现金流量项目");
    }

    fn show_assets_help(&self, ui: &mut Ui) {
        self.section_title(ui, "固定资产管理");

        self.section_title(ui, "资产卡片");
        self.help_text(ui, "• 录入资产信息：名称、类别、规格、使用部门");
        self.help_text(ui, "• 设置原值、残值率、使用年限、折旧方法");
        self.help_text(ui, "• 支持多种折旧方法：直线法、双倍余额递减法等");

        self.section_title(ui, "折旧计提");
        self.help_text(ui, "• 每月自动计算折旧额");
        self.help_text(ui, "• 生成折旧凭证");
        self.help_text(ui, "• 支持批量计提");

        self.section_title(ui, "资产变动");
        self.help_text(ui, "• 原值变动：改良、减值");
        self.help_text(ui, "• 部门转移");
        self.help_text(ui, "• 资产清理：报废、出售");

        self.section_title(ui, "查询统计");
        self.help_text(ui, "• 资产清单查询");
        self.help_text(ui, "• 折旧明细查询");
        self.help_text(ui, "• 按部门、类别统计");
    }

    fn show_inventory_help(&self, ui: &mut Ui) {
        self.section_title(ui, "存货管理");

        self.section_title(ui, "出入库管理");
        self.help_text(ui, "• 采购入库：关联采购订单");
        self.help_text(ui, "• 销售出库：关联销售订单");
        self.help_text(ui, "• 其他入库：盘盈、调拨入库等");
        self.help_text(ui, "• 其他出库：盘亏、调拨出库等");

        self.section_title(ui, "成本核算");
        self.help_text(ui, "• 支持先进先出法");
        self.help_text(ui, "• 支持加权平均法");
        self.help_text(ui, "• 自动计算发出成本");

        self.section_title(ui, "库存查询");
        self.help_text(ui, "• 实时库存查询");
        self.help_text(ui, "• 按仓库、类别查询");
        self.help_text(ui, "• 库存预警设置");
    }

    fn show_payroll_help(&self, ui: &mut Ui) {
        self.section_title(ui, "工资管理");

        self.section_title(ui, "工资项目");
        self.help_text(ui, "• 基本工资、绩效工资、奖金");
        self.help_text(ui, "• 社保、公积金、个税");
        self.help_text(ui, "• 支持自定义项目");

        self.section_title(ui, "工资计算");
        self.help_text(ui, "• 自动计算应发工资");
        self.help_text(ui, "• 自动计算代扣款项");
        self.help_text(ui, "• 自动计算实发工资");

        self.section_title(ui, "个税计算");
        self.help_text(ui, "• 支持累计预扣法");
        self.help_text(ui, "• 自动计算专项附加扣除");
        self.help_text(ui, "• 生成个税申报表");

        self.section_title(ui, "工资发放");
        self.help_text(ui, "• 生成工资条");
        self.help_text(ui, "• 生成发放凭证");
        self.help_text(ui, "• 银行代发");
    }

    fn show_claims_help(&self, ui: &mut Ui) {
        self.section_title(ui, "费用报销");

        self.section_title(ui, "报销流程");
        self.help_text(ui, "1. 员工提交报销单");
        self.help_text(ui, "2. 部门主管审批");
        self.help_text(ui, "3. 财务审核");
        self.help_text(ui, "4. 出纳付款");
        self.help_text(ui, "5. 生成会计凭证");

        self.section_title(ui, "费用类型");
        self.help_text(ui, "• 差旅费：交通、住宿、餐饮");
        self.help_text(ui, "• 业务招待费：餐饮、礼品");
        self.help_text(ui, "• 办公费：办公用品、耗材");
        self.help_text(ui, "• 其他费用：自定义类型");

        self.section_title(ui, "附件管理");
        self.help_text(ui, "• 支持上传发票、收据等附件");
        self.help_text(ui, "• 支持拍照上传");
        self.help_text(ui, "• 附件与报销单关联");
    }

    fn show_bank_rec_help(&self, ui: &mut Ui) {
        self.section_title(ui, "银行对账");

        self.section_title(ui, "导入银行流水");
        self.help_text(ui, "• 支持CSV格式导入");
        self.help_text(ui, "• 自动识别日期、摘要、金额");
        self.help_text(ui, "• 支持多家银行格式");

        self.section_title(ui, "对账操作");
        self.help_text(ui, "• 自动对账：系统自动匹配");
        self.help_text(ui, "• 手动对账：人工选择匹配");
        self.help_text(ui, "• 一对多、多对一匹配");

        self.section_title(ui, "余额调节表");
        self.help_text(ui, "• 自动生成余额调节表");
        self.help_text(ui, "• 显示未达账项");
        self.help_text(ui, "• 核对银行余额与账面余额");
    }

    fn show_settle_help(&self, ui: &mut Ui) {
        self.section_title(ui, "往来核销");

        self.section_title(ui, "应收应付");
        self.help_text(ui, "• 按客户/供应商管理往来款项");
        self.help_text(ui, "• 支持预收预付");
        self.help_text(ui, "• 自动生成对账单");

        self.section_title(ui, "账龄分析");
        self.help_text(ui, "• 按账龄区间分析");
        self.help_text(ui, "• 支持自定义账龄区间");
        self.help_text(ui, "• 计提坏账准备");

        self.section_title(ui, "核销管理");
        self.help_text(ui, "• 手动核销：选择应收应付进行核销");
        self.help_text(ui, "• 自动核销：系统自动匹配");
        self.help_text(ui, "• 核销记录查询");
    }

    fn show_period_end_help(&self, ui: &mut Ui) {
        self.section_title(ui, "期末处理");

        self.section_title(ui, "期末调汇");
        self.help_text(ui, "• 外币科目汇率调整");
        self.help_text(ui, "• 自动生成汇兑损益凭证");

        self.section_title(ui, "自动转账");
        self.help_text(ui, "• 设置转账模板");
        self.help_text(ui, "• 一键生成转账凭证");

        self.section_title(ui, "损益结转");
        self.help_text(ui, "• 将收入费用科目余额转入本年利润");
        self.help_text(ui, "• 自动生成结转凭证");
        self.help_text(ui, "• 结转后损益科目余额为零");

        self.section_title(ui, "月末结账");
        self.help_text(ui, "• 检查本期凭证是否全部记账");
        self.help_text(ui, "• 检查损益是否已结转");
        self.help_text(ui, "• 锁定本期，进入下期");
    }

    fn show_automation_help(&self, ui: &mut Ui) {
        self.section_title(ui, "月末自动化");

        self.section_title(ui, "自动转账");
        self.help_text(ui, "• 预设转账模板");
        self.help_text(ui, "• 支持按比例、按余额转账");
        self.help_text(ui, "• 一键生成凭证");

        self.section_title(ui, "期末调汇");
        self.help_text(ui, "• 设置外币汇率");
        self.help_text(ui, "• 自动计算汇兑损益");
        self.help_text(ui, "• 生成调汇凭证");

        self.section_title(ui, "月度检查");
        self.help_text(ui, "• 检查凭证是否平衡");
        self.help_text(ui, "• 检查科目余额是否异常");
        self.help_text(ui, "• 检查是否有未审核凭证");
    }

    fn show_security_help(&self, ui: &mut Ui) {
        self.section_title(ui, "用户与权限");

        self.section_title(ui, "用户管理");
        self.help_text(ui, "• 创建用户账号");
        self.help_text(ui, "• 分配角色（管理员/会计/出纳等）");
        self.help_text(ui, "• 设置数据范围");

        self.section_title(ui, "权限控制");
        self.help_text(ui, "• 功能权限：控制可访问的模块");
        self.help_text(ui, "• 数据权限：控制可查看的数据范围");
        self.help_text(ui, "• 操作权限：控制可执行的操作");

        self.section_title(ui, "安全策略");
        self.help_text(ui, "• 密码复杂度要求");
        self.help_text(ui, "• 登录失败锁定");
        self.help_text(ui, "• 会话超时自动退出");
        self.help_text(ui, "• 操作日志审计");
    }

    fn show_faq(&self, ui: &mut Ui) {
        self.section_title(ui, "常见问题");

        self.section_title(ui, "Q: 凭证保存提示借贷不平衡？");
        self.help_text(ui, "A: 请检查借方合计是否等于贷方合计。可以使用「补平」功能自动补齐差额。");

        self.section_title(ui, "Q: 无法修改已审核的凭证？");
        self.help_text(ui, "A: 已审核凭证需要先取消审核才能修改。在凭证查询界面选中凭证，点击「取消审核」。");

        self.section_title(ui, "Q: 资产负债表不平？");
        self.help_text(ui, "A: 请检查：1. 期初余额是否正确；2. 本期凭证是否全部记账；3. 损益是否已结转。");

        self.section_title(ui, "Q: 如何导出报表？");
        self.help_text(ui, "A: 在报表界面点击「导出」按钮，选择Excel或CSV格式即可。");

        self.section_title(ui, "Q: 如何备份数据？");
        self.help_text(ui, "A: 进入「备份恢复」界面，点击「备份」按钮，选择保存路径即可。");

        self.section_title(ui, "Q: 如何设置自动备份？");
        self.help_text(ui, "A: 在「账套参数」中设置自动备份路径和频率，系统将自动备份。");

        self.section_title(ui, "Q: 如何导入银行流水？");
        self.help_text(ui, "A: 在「银行对账」界面点击「导入」，选择银行导出的CSV文件即可。");

        self.section_title(ui, "Q: 如何计提折旧？");
        self.help_text(ui, "A: 在「固定资产」界面点击「计提折旧」，系统自动计算并生成凭证。");
    }
}
