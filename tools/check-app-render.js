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
    expect: ["没设系数", "data-cell-none"],
    absent: ["<input", "未定价"],
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
  // 金额进输入框要去掉多余的零，否则格子里写着 7.0200 没人愿意改
  { fn: "priceIn", args: ["7.0200"], stubs: {}, expect: ["7.02"], absent: ["7.0200"] },
  { fn: "priceIn", args: ["0.00"], stubs: {}, expect: ["0"], absent: ["0.00"] },
  { fn: "priceIn", args: ["300.00"], stubs: {}, expect: ["300"], absent: ["300.00"] },
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
