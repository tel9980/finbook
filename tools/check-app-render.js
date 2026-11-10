#!/usr/bin/env node
/**
 * 检查 app.js 里的视图函数会不会因为「引用了作用域外变量」而在浏览器里白屏。
 *
 * 为什么需要这个检查：`node --check` 只验语法。下面这种错它一句话都不说 ——
 *
 *     async function viewAux(main) {
 *       const showOff = !!state.x;      // 只在这个函数里存在
 *       renderAux(main, rows, kind);    // 交给另一个函数
 *     }
 *     function renderAux(main, rows, kind) {
 *       main.innerHTML = `<input ${showOff ? "checked" : ""}>`;   // ← ReferenceError
 *     }
 *
 * 语法完全合法，运行到这一行才炸，而炸的是一个 async 函数内部 —— 表现是
 * 整个页面白屏、console 里一行错。v1.8.21 真出过这个 bug（档案页的
 * 「显示停用」勾选框），是 CI 的 Playwright 抓到的：跑一次三十分钟，
 * 且失败时还要去翻 trace 才知道是哪一行。
 *
 * 这个检查不做作用域分析，也不假装是 linter —— 它只是把函数原文抽出来，
 * 给一组最小的桩，然后**真的调用一次**。引用到不存在的名字就会抛
 * ReferenceError，和浏览器里一模一样，一秒钟出结果。
 *
 * 加新用例的方法见下方 CASES。桩要给全（少给一个就报「未定义」，属于误报，
 * 补桩即可）；但**不要**给 Proxy 之类「什么都返回」的万能桩 —— 那会把
 * 这个检查变回假绿。
 */
"use strict";

const fs = require("fs");
const path = require("path");

const ROOT = process.env.FIX_ROOT || path.join(__dirname, "..");
// 前端已拆成 16 份 app*.js（经典 script，按 index.html 顺序加载）。视图函数散落
// 在其中，所以这里用**拼接后的整份源码**做抽取：函数级抽取不受文件边界影响，
// 而只读第一份 app.js 会漏掉绝大部分视图（检查变空转还不报错）。
const APP = () => require("./lib-app-src.js").readAppBundle();

/// 从源码里按大括号配平抽出一个 `function name(...) { ... }` 的完整原文。
function extractFn(src, name) {
  const i = src.indexOf(`function ${name}(`);
  if (i < 0) return null;
  let depth = 0;
  const open = src.indexOf("{", i);
  for (let j = open; j < src.length; j++) {
    const c = src[j];
    if (c === "{") depth++;
    else if (c === "}") {
      depth--;
      if (depth === 0) return src.slice(i, j + 1);
    }
  }
  return null;
}

/**
 * 用例表。args 是传给该函数的参数（行数据尽量给「两种都有」的样子：
 * 既有一条正常行、也有一条停用的，模板里两个分支都会被求值）。
 * stubs 里是这段函数体引用到的外部名字 —— 给**函数**还是给常量都行。
 */
// 「一句话结论」的配色表在前端是模块级常量，函数体里引用它；抽函数时它不在作用域内，
// 所以这里按**原样**给一份（改配色表时这里要同步，否则检查会报「未定义」而不是假绿）。
const VERDICT_CLASS = { unposted: "warn", overdue: "alert", net_debt: "note", no_revenue: "note", ok: "ok" };

// 单价矩阵格子的桩（v49）。`priceIn` 与源码同名函数逻辑一致 —— 另有独立用例守它本身。
const CELL_STUBS = {
  esc: (s) => String(s == null ? "" : s),
  num: (v) => Number(v == null ? 0 : v),
  fmt: (v) => Number(v == null ? 0 : v).toFixed(2),
  priceIn: (v) =>
    String(v == null ? "" : v)
      .replace(/(\.\d*?)0+$/, "$1")
      .replace(/\.$/, ""),
};

// 规格区间那一行的桩（v51）：只用到 esc
const BAND_STUBS = { esc: (s) => String(s == null ? "" : s) };

// 调价历史用例用：esc 要真转义（断言里含 <b> 标签），fmt 保持原样不补零
const HIST_STUBS = {
  esc: (s) =>
    s == null
      ? ""
      : String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c])),
  fmt: (s) => (s == null ? "" : String(s)),
};

const CASES = [
  {
    fn: "renderAux",
    args: [
      { innerHTML: "" },
      [
        { id: 1, code: "C01", name: "甲公司", parent_code: null, disabled: false, memo: "" },
        { id: 2, code: "C02", name: "乙公司", parent_code: "C01", disabled: true, memo: "备注" },
      ],
      "customer",
    ],
    stubs: {
      AUX_KINDS: [{ code: "customer", label: "客户" }],
      state: { auxShowDisabled: true, auxKind: "customer" },
      can: () => true,
      esc: (s) => String(s == null ? "" : s),
      $: () => null,
      $all: () => [],
      loadAuxPreset: () => {},
    },
  },
  {
    fn: "renderAccounts",
    args: [
      { innerHTML: "" },
      [
        { code: "1001", name: "库存现金", category: "asset", dir: "debit", disabled: false, aux: 0 },
        { code: "2202", name: "应付账款", category: "liability", dir: "credit", disabled: true, aux: 0 },
      ],
    ],
    stubs: {
      state: {},
      session: { platformAdmin: false },
      can: () => true,
      esc: (s) => String(s == null ? "" : s),
      // 这个函数在渲染完还要挂事件监听，所以 `$` 不能返回 null —— 返回一个
      // 带 value/addEventListener 的假节点。注意它仍是**具体的桩**：模板里
      // 引用了别的没给的名字照样会 ReferenceError。
      $: () => ({ value: "", addEventListener: () => {} }),
      $all: () => [],
      acctCatLabel: () => "资产",
      dirLabel: () => "借",
      auxMaskLabel: () => "",
      downloadUrl: () => {},
    },
  },
  // 首屏「一句话结论」：文案与判定在后端（findb::verdict），前端只负责显示。
  // 这里守两件前端自己的事：颜色/跳转看 kind 不看文字；没权限时不给「去处理」按钮
  // （只读用户点过去只会看到「无权限」，不如把话留给他转告会计）。
  {
    fn: "homeVerdict",
    args: [
      {
        verdict: {
          kind: "unposted",
          text: "⚠ 还有 3 张凭证没记账，下面的 8,000.00 是「已记账部分」的利润，可能偏小。",
          view: "vouchers",
          urgent: true,
        },
      },
    ],
    stubs: { VERDICT_CLASS, esc: (s) => String(s == null ? "" : s), can: () => true },
    expect: ["home-verdict warn", 'data-vd-go="vouchers"', "还有 3 张凭证没记账"],
  },
  {
    fn: "homeVerdict",
    args: [
      { verdict: { kind: "ok", text: "本月净利润 0.00，收支正常，暂无卡住的事。", view: "", urgent: false } },
    ],
    stubs: { VERDICT_CLASS, esc: (s) => String(s == null ? "" : s), can: () => true },
    expect: ["home-verdict ok"],
    absent: ["data-vd-go"],
  },
  {
    fn: "homeVerdict",
    args: [
      { verdict: { kind: "overdue", text: "⚠ 3 家客户的钱超过 90 天没收回。", view: "settle", urgent: true } },
    ],
    stubs: { VERDICT_CLASS, esc: (s) => String(s == null ? "" : s), can: () => false },
    expect: ["home-verdict alert"],
    absent: ["data-vd-go"],
  },
  // 「账有没有记错」：balanced 为 null 时**不能**说「账是平的」——
  // 那是首页最不能出错的一句话。
  {
    fn: "homeTrial",
    args: [{ trial: { balanced: true, diff: "0.00" } }],
    stubs: { esc: (s) => String(s == null ? "" : s) },
    expect: ["home-trial ok", "账是平的"],
  },
  {
    fn: "homeTrial",
    args: [{ trial: { balanced: false, diff: "100.00" } }],
    stubs: { esc: (s) => String(s == null ? "" : s) },
    expect: ["home-trial bad", "100.00", "账不平"],
    absent: ["账是平的"],
  },
  {
    fn: "homeTrial",
    args: [{ trial: { balanced: null, diff: "0.00" } }],
    stubs: { esc: (s) => String(s == null ? "" : s) },
    expect: ["暂时算不出来"],
    absent: ["账是平的", "home-trial ok"],
  },
  // 报价单存完的「和价目表对不对得上」（v1.8.95）。守三条：
  // 出问题时给黄字并带上「价目表折合价」和「差多少」；一致时给绿字；没有价就说空。
  {
    fn: "priceCheckHtml",
    file: "crates/finweb/static/app-docs.js",
    stubs: { esc: (s) => String(s == null ? "" : s) },
    args: [
      {
        price_warn:
          "这单价和价目表对不上：价目表 12.00 元/㎡（折合每 件 0.28 元），这单填 0.15 元，差 -0.13",
        price_check: { kind: "mismatch" },
      },
    ],
    expect: ["⚠", "对不上", "0.28", "-0.13"],
    absent: ["✔"],
  },
  {
    fn: "priceCheckHtml",
    file: "crates/finweb/static/app-docs.js",
    stubs: { esc: (s) => String(s == null ? "" : s) },
    args: [
      {
        price_warn: null,
        price_check: {
          kind: "ok",
          rule_price: "12.00",
          rule_unit: "㎡",
          per_unit: "0.28",
          stock_unit: "件",
        },
      },
    ],
    expect: ["✔", "12.00 元/㎡", "每件 0.28 元"],
    absent: ["⚠"],
  },
  {
    fn: "priceCheckHtml",
    file: "crates/finweb/static/app-docs.js",
    stubs: { esc: (s) => String(s == null ? "" : s) },
    args: [{ price_warn: null, price_check: { kind: "no_rule" } }],
    expect: [],
    absent: ["⚠", "✔"],
  },
  // 「按价目表算」那一行：说得清用的是哪条价、单价多少、合计多少
  {
    fn: "linePriceHtml",
    file: "crates/finweb/static/app-docs.js",
    stubs: { esc: (s) => String(s == null ? "" : s), fmt: (v) => String(v == null ? "" : v) },
    args: [
      {
        rule_name: "MB31 按平方",
        unit_price: "0.2808",
        total: "84.24",
        stock_unit: "件",
        note: "按规格 1250×250×3 折成 7.02 ㎡",
      },
    ],
    expect: ["✔", "MB31 按平方", "0.2808", "件", "84.24", "按规格"],
    absent: ["⚠"],
  },
  {
    fn: "linePriceHtml",
    file: "crates/finweb/static/app-docs.js",
    stubs: { esc: (s) => String(s == null ? "" : s), fmt: (v) => String(v == null ? "" : v) },
    args: [null],
    expect: [],
    absent: ["✔"],
  },
  {
    fn: "linePriceHtml",
    file: "crates/finweb/static/app-docs.js",
    stubs: { esc: (s) => String(s == null ? "" : s), fmt: (v) => String(v == null ? "" : v) },
    args: [
      {
        rule_name: "MB31 按平方",
        unit_price: "12",
        total: "120.00",
        stock_unit: "㎡",
        note: "这条价 2026-12-01 才生效，现在报的是提前预定的价",
      },
    ],
    expect: ["才生效"],
    absent: ["⚠"],
  },
  // 首页「这几天有价要生效」（v1.8.94）。守三条：
  // 空就不占地方；今天生效的要说「今天起」；条数截断了要老实说还剩几条。
  {
    fn: "homePriceSoon",
    args: [{ price_soon: [], price_soon_total: 0 }],
    stubs: HIST_STUBS,
    absent: ["home-soon", "data-vd-go"],
  },
  {
    fn: "homePriceSoon",
    args: [
      {
        price_soon: [
          { date: "2026-11-10", item_code: "MB01", customer_code: "C01", unit: "㎡", process: "", price: "0.15", today: true },
          { date: "2026-11-13", item_code: "MB02", customer_code: "", unit: "件", process: "硬质氧化", price: "0.20", today: false },
        ],
        price_soon_total: 2,
      },
    ],
    stubs: HIST_STUBS,
    expect: ["home-soon", "今天就生效", "今天起", "2026-11-13 起", "0.15", "硬质氧化", 'data-vd-go="price-rules"'],
    absent: ["还有"],
  },
  {
    fn: "homePriceSoon",
    args: [
      {
        price_soon: [
          { date: "2026-11-12", item_code: "MB09", customer_code: "", unit: "㎡", process: "", price: "9.00", today: false },
        ],
        price_soon_total: 7,
      },
    ],
    stubs: HIST_STUBS,
    expect: ["还有 6 条", "调价历史"],
    absent: ["今天就生效"],
  },
  // 单价矩阵里的格子（v49）。守两条最会出事的边界：
  // 没折算系数的那一格**不能**变成「0 元」输入框；有系数没定价的要空着写「未定价」。
  {
    fn: "priceCellHtml",
    args: [
      { item_code: "MB01" },
      { unit: "㎡", rule_id: 0, factor: null, future: false, min_charge: "0.00", effective_from: "", scale: 2 },
      true,
    ],
    stubs: CELL_STUBS,
    expect: ["未设系数 · 点这里录", "data-cell-none", 'placeholder="未定价"'],
    absent: ['value="0"'],
  },
  {
    // 客户发来的价目表刚导进来（价有了、系数还没录）：价必须看得见，
    // 只在下面提一句「未设系数」。早先这一格整块变成「没设系数」三个字，价被藏了。
    fn: "priceCellHtml",
    args: [
      { item_code: "MB09", item_name: "外壳" },
      { unit: "㎡", rule_id: 21, factor: null, future: false, min_charge: "50.00", effective_from: "", scale: 2, unit_price: "20.00" },
      true,
    ],
    stubs: CELL_STUBS,
    expect: ['value="20"', "未设系数 · 点这里录", "起步 50.00"],
    absent: ["没设系数"],
  },
  {
    // 客户发来的价表是按客户报的，矩阵却停在「全部客户」—— 不提一句，
    // 用户会看到满屏「未定价」，以为没导进去
    fn: "importResultHtml",
    args: [
      { created: 2, updated: 0, skipped: 0, warnings: [] },
      "上面的价如果挂在客户名下，右上角「客户编码」填上客户再看，才知道这个客户卖多少。",
    ],
    stubs: CELL_STUBS,
    expect: ["新增 <b>2</b> 条", "右上角「客户编码」"],
    absent: ["一行都没读到"],
  },
  {
    // 「2.5 元一米」+ 两个规格：每件多少钱要一眼看到，还要看到折合量怎么来的
    fn: "specRowsHtml",
    args: [[
      { spec: "1200*80", dims: "1200×80", factor: "1.2000", price: "3.00", note: "" },
      { spec: "920*60", dims: "920×60", factor: "0.9200", price: "2.30", note: "" },
    ]],
    stubs: CELL_STUBS,
    expect: ["1200×80", "1.2", "3.00", "920×60", "2.3", "2.30"],
    absent: ["还没有结果"],
  },
  {
    // 坏规格：整行说要怎么写，金额列打「—」而不是 0 元（0 元看着像免费加工）
    fn: "specRowsHtml",
    args: [[
      { spec: "坏行", dims: "", factor: "0.00", price: "0.00", note: "看不懂这个规格 —— 写成 1250*250*3 这样（毫米）" },
    ]],
    stubs: CELL_STUBS,
    expect: ["看不懂这个规格", "—"],
    absent: ["0.00"],
  },
  {
    fn: "specRowsHtml",
    args: [[]],
    stubs: CELL_STUBS,
    expect: ["还没有结果"],
    absent: ["<table"],
  },
  {
    // 一行都没读到（粘错地方 / 空文本）：必须说人话，不能显示「新增 0 条」让人以为成功了
    fn: "importResultHtml",
    args: [{ created: 0, updated: 0, skipped: 0, warnings: [] }],
    stubs: CELL_STUBS,
    expect: ["一行都没读到", "下载模板"],
    absent: ["新增", "覆盖"],
  },
  {
    // 有跳过行时用黄字，并把每行原因摊出来（前 10 条）
    fn: "importResultHtml",
    args: [{
      created: 12,
      updated: 3,
      skipped: 2,
      warnings: ["第 7 行：单价「abc」不是数字，这一行导不了", "第 9 行：没有计价口径（按㎡还是按只），这一行导不了"],
    }],
    stubs: CELL_STUBS,
    expect: ["新增 <b>12</b> 条", "覆盖 <b>3</b> 条", "跳过 <b>2</b> 行", "第 7 行", "不是数字", "--warn"],
    absent: ["一行都没读到"],
  },
  {
    // 干净导入：绿字，不出现「跳过」和警告列表
    fn: "importResultHtml",
    args: [{ created: 5, updated: 0, skipped: 0, warnings: [] }],
    stubs: CELL_STUBS,
    expect: ["新增 <b>5</b> 条", "--ok"],
    absent: ["跳过", "<ul"],
  },
  {
    // 警告超过 10 条只列前 10，并说明还有多少 —— 否则一次导 500 行会刷屏
    fn: "importResultHtml",
    args: [{ created: 0, updated: 0, skipped: 12, warnings: Array.from({ length: 12 }, (_, i) => `第 ${i + 1} 行：坏的`) }],
    stubs: CELL_STUBS,
    expect: ["跳过 <b>12</b> 行", "第 10 行", "还有 2 行没列出来"],
    absent: ["第 11 行"],
  },
  {
    // 通配行没系数，但价照样能填 —— 否则「任意物料 12 元/㎡」这条规则在矩阵里
    // 只能显示成「没设系数」，等于把已录好的价藏起来。
    fn: "priceCellHtml",
    args: [
      { item_code: "", item_name: "任意物料（通配价）" },
      { unit: "㎡", rule_id: 9, factor: null, future: false, min_charge: "0.00", effective_from: "", scale: 2, unit_price: "18.00" },
      true,
    ],
    stubs: CELL_STUBS,
    expect: ['value="18"', "data-cell-edit"],
    absent: ["没设系数"],
  },
  {
    fn: "priceCellHtml",
    args: [
      { item_code: "MB01" },
      { unit: "㎡", rule_id: 0, factor: "0.0234", future: false, min_charge: "0.00", effective_from: "", scale: 2, unit_price: "0.00" },
      true,
    ],
    stubs: CELL_STUBS,
    expect: ['placeholder="未定价"', 'value=""'],
    absent: ['value="0"'],
  },
  {
    fn: "priceCellHtml",
    args: [
      { item_code: "MB01" },
      { unit: "㎡", rule_id: 7, factor: "0.0234", future: true, min_charge: "30.00", effective_from: "2999-03-01", scale: 2, unit_price: "12.00" },
      true,
    ],
    stubs: CELL_STUBS,
    expect: ['value="12"', "2999-03-01 起", "起步 30.00", "data-cell-edit"],
  },
  {
    // 只读用户：价格能看，但不给按钮、输入框锁死
    fn: "priceCellHtml",
    args: [
      { item_code: "MB01" },
      { unit: "㎡", rule_id: 7, factor: "0.0234", future: false, min_charge: "0.00", effective_from: "", scale: 2, unit_price: "12.00" },
      false,
    ],
    stubs: CELL_STUBS,
    expect: ["disabled", 'value="12"'],
    absent: ["data-cell-edit", "data-cell-none"],
  },
  {
    // 工艺档：这一档还没单独定价，用的是不分档的通用价 —— 必须标出来，
    // 否则用户以为「硬质氧化」已经定价了，报出去就是普通氧化的价
    name: "priceCellHtml 借用通用价要标出来",
    fn: "priceCellHtml",
    args: [
      { item_code: "MB03" },
      { unit: "㎡", rule_id: 31, factor: "1.0000", future: false, min_charge: "0.00", effective_from: "", scale: 2, unit_price: "20.00", fallback: true },
      true,
    ],
    stubs: CELL_STUBS,
    expect: ["用通用价", 'value="20"'],
  },
  {
    name: "priceCellHtml 本档自己的价不标借用",
    fn: "priceCellHtml",
    args: [
      { item_code: "MB03" },
      { unit: "㎡", rule_id: 32, factor: "1.0000", future: false, min_charge: "0.00", effective_from: "", scale: 2, unit_price: "30.00", fallback: false },
      true,
    ],
    stubs: CELL_STUBS,
    expect: ['value="30"'],
    absent: ["用通用价"],
  },
  // 金额进输入框要去掉多余的零，否则格子里写着 7.0200 没人愿意改
  { fn: "priceIn", args: ["7.0200"], stubs: {}, expect: ["7.02"], absent: ["7.0200"] },
  { fn: "priceIn", args: ["0.00"], stubs: {}, expect: ["0"], absent: ["0.00"] },
  { fn: "priceIn", args: ["300.00"], stubs: {}, expect: ["300"], absent: ["300.00"] },
  // 按批量分段的价（v52）。上限含、下限不含 —— 正好 500 件要落在下面那一档，
  // 写成「≥500」会和下一条重复命中，谁先建谁赢，报价会随建单顺序变。
  {
    fn: "qtyRangeHtml",
    args: [{ qty_from: "", qty_to: "500", unit: "件" }],
    stubs: BAND_STUBS,
    expect: ["≤ 500", "件"],
  },
  {
    fn: "qtyRangeHtml",
    args: [{ qty_from: "500", qty_to: "", unit: "㎡" }],
    stubs: BAND_STUBS,
    expect: ["> 500", "㎡"],
    absent: ["≤ 500"],
  },
  {
    fn: "qtyRangeHtml",
    args: [{ qty_from: "500", qty_to: "2000", unit: "件" }],
    stubs: BAND_STUBS,
    expect: ["500–2000"],
  },
  // 调价历史（v53）。这一行文案是客户对账时看的：「0.20 → 0.15」写反了
  // 客户会以为你把价涨回去了；删除行不能画箭头（没有「改成多少」这回事）。
  {
    fn: "histRowsHtml",
    args: [[]],
    stubs: HIST_STUBS,
    expect: ["还没有调价记录"],
  },
  {
    fn: "histRowsHtml",
    args: [
      [
        { at: "2026-11-09 14:03:11", item_code: "MB01", unit: "㎡", action: "改价", old_price: "0.20", new_price: "0.15", who: "李四", memo: "" },
      ],
    ],
    stubs: HIST_STUBS,
    expect: ["0.20 → <b>0.15</b>", "李四", "改价", "MB01"],
  },
  {
    fn: "histRowsHtml",
    args: [
      [
        { at: "2026-11-09 15:00:00", item_code: "MB02", unit: "只", action: "删除", old_price: "0.45", new_price: "", who: "王五", memo: "删除前这条规则还在用" },
      ],
    ],
    stubs: HIST_STUBS,
    expect: ["0.45", "王五"],
    absent: ["→"],
  },
  // 按规格分段的价（v51）。区间说法有三种形态，写错了用户会照着错的范围报价：
  // 「≤200」= 只有上限；「>200」= 只有下限；「200–300」= 两头都有；维度要翻成中文。
  {
    fn: "bandRangeHtml",
    args: [{ band_dim: "max", band_dim_label: "最长边", band_from: "", band_to: "200" }],
    stubs: BAND_STUBS,
    expect: ["最长边", "≤ 200", "cm"],
  },
  {
    fn: "bandRangeHtml",
    args: [{ band_dim: "max", band_dim_label: "最长边", band_from: "200", band_to: "" }],
    stubs: BAND_STUBS,
    expect: ["> 200"],
    absent: ["≤ 200"],
  },
  {
    fn: "bandRangeHtml",
    args: [{ band_dim: "area", band_dim_label: "面积", band_from: "1000", band_to: "2500" }],
    stubs: BAND_STUBS,
    expect: ["面积", "1000–2500", "cm²"],
  },
  {
    // 两头都空 = 不管什么尺寸都用它，别显示成「≤ 」这种半句
    fn: "bandRangeHtml",
    args: [{ band_dim: "max", band_dim_label: "最长边", band_from: "", band_to: "" }],
    stubs: BAND_STUBS,
    expect: ["全部尺寸"],
    absent: ["≤", ">"],
  },
  // 批量调价的预览表（v1.8.97）。用户是靠这张表决定「执行」的，
  // 所以老价、新价、动作三样缺一样都不行。
  {
    fn: "adjustRowsHtml",
    args: [
      [
        { name: "MB01 按㎡", item_code: "MB01", customer_code: "", unit: "㎡", old_price: "12.00", new_price: "12.36", action: "新建", effective_from: "2027-01-01" },
        { name: "C01 按只", item_code: "MB02", customer_code: "C01", unit: "只", old_price: "0.50", new_price: "0.52", action: "覆盖", effective_from: "2027-01-01" },
      ],
      [],
    ],
    stubs: HIST_STUBS,
    expect: ["12.00", "12.36", "新建", "覆盖", "共 2 条", "2027-01-01"],
  },
  {
    // 一条都没调到（比如幅度把价压到 0）+ 有警告：不能显示成空表让人以为成功
    fn: "adjustRowsHtml",
    args: [[], ["「MB03 按件」调完不到 0（原价 0.50）—— 这条跳过"]],
    stubs: HIST_STUBS,
    expect: ["跳过", "没有匹配的价"],
    absent: ["<table"],
  },
];

function main() {
  let src;
  try {
    src = APP();
  } catch (e) {
    console.log(`FAIL 读不到前端脚本：${e.message}`);
    return 1;
  }
  let bad = 0;
  for (const c of CASES) {
    const body = extractFn(src, c.fn);
    if (!body) {
      // 函数改名/删掉了：用例失效必须报出来。静默跳过就是「看着在守，
      // 实际没守」—— 本仓库最忌讳的失效模式。
      console.log(
        `FAIL 前端脚本（app*.js 拼接）里找不到函数 ${c.fn}，检查用例已失效，请同步更新 tools/check-app-render.js`
      );
      bad++;
      continue;
    }
    const names = Object.keys(c.stubs);
    const decl = names.map((k) => `const ${k} = __stubs[${JSON.stringify(k)}];`).join("\n");
    try {
      // eslint-disable-next-line no-new-func
      const factory = new Function("__stubs", `${decl}\n${body}\nreturn ${c.fn};`);
      const f = factory(c.stubs);
      const out = f(...JSON.parse(JSON.stringify(c.args)));
      // 只验「不抛错」是不够的：模板里少写一个 class 或少了权限判断，照样不抛错。
      // 需要守具体输出的用例，用 expect/absent 写明必须出现/必须不出现的片段。
      const html = typeof out === "string" ? out : "";
      const missing = (c.expect || []).filter((t) => !html.includes(t));
      const leaked = (c.absent || []).filter((t) => html.includes(t));
      if (missing.length || leaked.length) {
        if (missing.length) console.log(`FAIL ${c.fn} 输出里缺少：${missing.join(" / ")}`);
        if (leaked.length) console.log(`FAIL ${c.fn} 输出里不该出现：${leaked.join(" / ")}`);
        bad++;
        continue;
      }
      console.log(`OK   ${c.fn} 渲染通过`);
    } catch (e) {
      console.log(`FAIL ${c.fn} 渲染抛错：${e.constructor.name}: ${e.message}`);
      bad++;
    }
  }
  return bad === 0 ? 0 : 1;
}

process.exit(main());
