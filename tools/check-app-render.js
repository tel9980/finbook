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
const APP = path.join(ROOT, "crates/finweb/static/app.js");

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
];

function main() {
  if (!fs.existsSync(APP)) {
    console.log(`FAIL 找不到 ${APP}`);
    return 1;
  }
  const src = fs.readFileSync(APP, "utf8");
  let bad = 0;
  for (const c of CASES) {
    const body = extractFn(src, c.fn);
    if (!body) {
      // 函数改名/删掉了：用例失效必须报出来。静默跳过就是「看着在守，
      // 实际没守」—— 本仓库最忌讳的失效模式。
      console.log(`FAIL app.js 里找不到函数 ${c.fn}，检查用例已失效，请同步更新 tools/check-app-render.js`);
      bad++;
      continue;
    }
    const names = Object.keys(c.stubs);
    const decl = names.map((k) => `const ${k} = __stubs[${JSON.stringify(k)}];`).join("\n");
    try {
      // eslint-disable-next-line no-new-func
      const factory = new Function("__stubs", `${decl}\n${body}\nreturn ${c.fn};`);
      const f = factory(c.stubs);
      f(...JSON.parse(JSON.stringify(c.args)));
      console.log(`OK   ${c.fn} 渲染通过`);
    } catch (e) {
      console.log(`FAIL ${c.fn} 渲染抛错：${e.constructor.name}: ${e.message}`);
      bad++;
    }
  }
  return bad === 0 ? 0 : 1;
}

process.exit(main());
