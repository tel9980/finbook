// 变异测试：逐条把刚加的前端检查对应的代码改坏，验证检查**真的会失败**。
// 只会通过的检查等于没有检查 —— 必须证明它拦得住。
//
// cases 的每一项是 [要删/改的原文, 替换成什么, 名字]，不用函数 —— 换行归一化
// 需要同时作用在 from 和 to 上，函数形式做不到。
//
// 这个脚本自己踩过两个坑，都写在下面防止重犯：
//   1. **变异串没匹配上源码**（CRLF vs LF）→ 什么都没改 → 检查照样通过 → 被读成
//      「检查放过了」。所以「没生效」必须单独报错，不能混进「放过」。
//   2. **判据钉表象而不是机制**。⑥ 的第一版判据是「autoEmptyRows 里有匹配权限
//      文案的提前 return」——那是猜的机制，真实机制是 renderMain() 没等视图的
//      Promise settle（竞态）。猜错方向的检查永远通过。判据要钉机制。
const fs = require("fs");
const { execFileSync } = require("child_process");
const p = "crates/finweb/static/app.js";
const orig = fs.readFileSync(p, "utf8");

const cases = [
  [
    ".yb = inp.value; paintTotals(); }",
    ".yb = inp.value; }",
    "② 去掉 .bg-yb 的 paintTotals()",
  ],
  [
    ".ad = inp.value; paintTotals(); }",
    ".ad = inp.value; }",
    "② 去掉 .bg-ad 的 paintTotals()",
  ],
  [
    ".ac = inp.value; paintTotals(); }",
    ".ac = inp.value; }",
    "② 去掉 .bg-ac 的 paintTotals()",
  ],
  [
    ".filter((id) => NAV_INDEX[id] && isNavVisible(NAV_INDEX[id]) && favs.indexOf(id) < 0);",
    ".filter((id) => NAV_INDEX[id] && favs.indexOf(id) < 0);",
    "① 去掉 getRecent 的 isNavVisible",
  ],
  [
    'return lsGet(acctKey("nav_fav"), []).filter((id) => NAV_INDEX[id] && isNavVisible(NAV_INDEX[id]));',
    "return lsGet(acctKey('nav_fav'), []).filter((id) => NAV_INDEX[id]);",
    "① 去掉 getFavs 的 isNavVisible",
  ],
  ['return base + "@" + u;', "return base;", "① 去掉账号分区（acctKey 退化成常量）"],
  [
    "const EMPTY_PLACEHOLDERS = /^(暂无数据",
    "const EMPTY_PLACEHOLDERS = /^(加载中|加载中…|加载中\\.\\.\\.|加载|暂无数据",
    "⑥ 把「加载中」放回 EMPTY_PLACEHOLDERS",
  ],
  [
    "    if (LOADING_PLACEHOLDERS.test(txt)) return;\n",
    "",
    "⑥ 去掉 autoEmptyRows 里对「加载中」的跳过",
  ],
  [
    "  Promise.resolve(r).then(\n    () => { try { autoEmptyRows(); } catch (e) {} },\n    () => { try { autoEmptyRows(); } catch (e) {} }\n  );",
    "  try { autoEmptyRows(); } catch (e) {}",
    "⑥ renderMain() 不等 Promise settle 就判空态",
  ],
  [
    "const LOADING_PLACEHOLDERS = /^(加载中|加载中…|加载中\\.\\.\\.|加载)$/;\n",
    "",
    "⑥ 删掉 LOADING_PLACEHOLDERS 常量",
  ],
  [
    '                  ${r.id > 0 ? `<span class="muted" style="font-size:12px">已有</span>` : ""}\n                  <button class="btn ghost sm" data-rm="${i}" title="移除这一行">移除</button>',
    '                  ${r.id > 0 ? `<span class="muted" style="font-size:12px">已有</span>` : `<button class="btn ghost sm" data-rm="${i}">移除</button>`}',
    '④ 已保存的行又变回不可移除（按钮退回三元式 false 分支）',
  ],
  [
    "      orig_code: r.account_code,\n",
    "",
    "④ 不再记录 orig_code（改编码后旧行会留在库里）",
  ],
  [
    'if (r.id > 0 && (r.orig_code || "") !== code) deleteIds.push(r.id);',
    "if (false) deleteIds.push(r.id);",
    "④ 改编码时不删旧行",
  ],
  // --- 建账向导：两道闸门都问、都读、都写进账套参数、提示语都覆盖 ---
  [
    '          <input type="radio" name="set-cash" id="set-cash-on" value="1" ${opts.require_cashier ? "checked" : ""} />',
    "",
    "向导不问「有没有出纳」（控件整个删掉）",
  ],
  [
    '      const require_cashier = ($("#set-cash-on") && $("#set-cash-on").checked) || false;',
    "      const require_cashier = true;",
    "向导把出纳开关写死成 true（控件在但没问用户）",
  ],
  [
    '      notes.push(require_cashier ? "已启用出纳签字" : "未启用出纳签字（无出纳岗）");',
    "",
    "建账提示语只说审核、不说出纳（用户被蒙在鼓里）",
  ],
  // 弹窗走 esc()（HTML 转义）不是 markdown：写了 ** 会让用户看到字面的星号。
  [
    '重置 ${b.dataset.resetDev} 的设备绑定？将解绑该账号「全部」已绑定设备并强制下线，下次登录重新绑定。',
    '重置 ${b.dataset.resetDev} 的设备绑定？将解绑该账号**全部**已绑定设备并强制下线。',
    "弹窗文案里用了 markdown 强调号（用户会看到字面星号）",
  ],  // 已签字时不该还摆着「出纳签字」按钮（和「取消签字」并排，点前者是空操作）
  [
    '    ${can("cashier_sign") && id > 0 && (status === "draft" || status === "audited") && !v.cashier ? `<button class="btn ghost" id="v-sign">出纳签字</button>` : ""}',
    '    ${can("cashier_sign") && id > 0 && (status === "draft" || status === "audited") ? `<button class="btn ghost" id="v-sign">出纳签字</button>` : ""}',
    "已签字的凭证仍显示「出纳签字」按钮（与「取消签字」并排）",
  ],
  // ⑳ 侧栏「最近」是追加不是搬家（2026-10-06 反馈：点过之后就从原分组里消失了）
  // from 串跟着 app.js 实际写法走：精简模式那层把 all 换成了 listed（all 的子集），
  // 谓词本身没变 —— 变的只是数据源。写 from 时要对齐源码，否则这条变异
  // 「匹配 0 次」会静默失效，而 mutate-check 自己会报「失效」——
  // 但没人会因为一条变异失效去看它到底护着什么。
  [
    "    const rest = listed.filter((n) => !favs.includes(n));",
    "    const rest = listed.filter((n) => !favs.includes(n) && !recents.includes(n));",
    "⑳ 最近从原分组搬走（rest 排掉 recents）",
  ],
  [
    'recents.map((n) => itemHtml(n, "", "nav-jump"))',
    'recents.map((n) => itemHtml(n))',
    "⑳ 最近分区用回 nav-item（同一 data-view 两份，E2E 严格模式炸）",
  ],
  [
    'const btn = e.target.closest(".nav-item, .nav-jump");',
    'const btn = e.target.closest(".nav-item");',
    "⑳ 侧栏委托不认 nav-jump（最近那条点了没反应）",
  ],
  // ㉑ 视图快照存下入口跳转哨兵 → 回不到数据导入（2026-10-06 反馈）
  [
    '    if (el.tagName === "SELECT" && el.value.indexOf("__") === 0) return;\n    snap.fields[el.id] = el.value;',
    '    snap.fields[el.id] = el.value;',
    "㉑ saveViewState 存下 __bank 哨兵（进数据导入即被拽去银行对账）",
  ],
];
/** 对换行不敏感地套用变异（源码是 CRLF，变异串按 LF 写） */
function applyMut(src, from, to) {
  for (const nl of ["\n", "\r\n"]) {
    const f = from.replace(/\n/g, nl);
    if (src.includes(f)) return { out: src.replace(f, to.replace(/\n/g, nl)), ok: true };
  }
  return { out: src, ok: false };
}

// --list：只把变异清单打成 JSON，不执行。让 tools/check-ci.js 检查「清单是否已过期」
// （源码改了但 from 串还对不上 = 这条变异形同虚设）。
//
// 为什么不让 check-ci.js 自己解析本文件：它得把 JS 字符串字面量再解一遍转义，
// 两层转义（`\\\\.`）必错——第一版就是这么写的，把一条有效变异误报成失效。
// 让脚本自己导出真值，别在外部猜。
if (process.argv.includes("--list")) {
  process.stdout.write(JSON.stringify({ file: p, cases }, null, 1));
  process.exit(0);
}

let escaped = 0;
for (const [from, to, name] of cases) {
  const { out: mod, ok } = applyMut(orig, from, to);
  if (!ok) {
    console.log("!! 变异未生效（源码已变，检查已失效）：" + name);
    escaped++;
    continue;
  }
  fs.writeFileSync(p, mod);
  let out = "";
  try {
    out = execFileSync(process.execPath, ["tools/check-js.js"], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });
  } catch (e) {
    out = (e.stdout || "") + (e.stderr || "");
  }
  const caught = out.includes("FAIL");
  if (!caught) escaped++;
  console.log((caught ? "PASS 拦下了  " : "FAIL 放过了  ") + name);
  if (caught) {
    const line = out.split("\n").find((l) => l.startsWith("FAIL"));
    console.log("        " + (line || "").slice(0, 140));
  }
}
fs.writeFileSync(p, orig);
console.log(
  escaped === 0
    ? `\n全部 ${cases.length} 个变异都被拦下，app.js 已还原`
    : `\n${escaped} 个变异没被拦下或没生效（共 ${cases.length} 个）`
);
process.exit(escaped === 0 ? 0 : 1);
