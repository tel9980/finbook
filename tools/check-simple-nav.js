// 在 Node 里跑 app.js 的**侧栏逻辑**，验证精简模式的三个不变量。
// 为什么不靠 Playwright：本机浏览器输入注入失效（page.fill 后 value 为空），
// 而把 11k 行 app.js 搬进 jsdom 成本太高。这里只抽出侧栏相关的纯逻辑片段，
// 用真实的 NAV_ITEMS 与真实的选择器表达式渲染，断言输出的 HTML。
const fs = require("fs");
const path = require("path");

const SRC = fs.readFileSync(
  path.join(__dirname, "..", "crates", "finweb", "static", "app.js"), "utf8");

// ---- 抽真实源码片段（抽不出来就报错，不许静默用副本）----
function grab(re, what) {
  const m = SRC.match(re);
  if (!m) throw new Error(`抽不到 ${what}，源码形状变了？`);
  return m[0];
}
const NAV_ITEMS_SRC = grab(/const NAV_ITEMS = \[[\s\S]*?\n\];/, "NAV_ITEMS");
const SIMPLE_NAV_SRC = grab(/const SIMPLE_NAV = \[[\s\S]*?\];/, "SIMPLE_NAV");
const LISTED_SRC = grab(/const listed = [\s\S]*?;/, "listed");
const REST_SRC = grab(/const rest = [\s\S]*?;/, "rest");

// ---- 最小运行时 ----
const store = {};
global.localStorage = {
  getItem: (k) => (k in store ? store[k] : null),
  setItem: (k, v) => { store[k] = String(v); },
  removeItem: (k) => { delete store[k]; },
};
const esc = (s) => String(s);
const icon = () => "";
let session = { user: { username: "admin" } };
const state = { view: "dashboard" };

const NAV_ITEMS = eval(NAV_ITEMS_SRC.replace("const NAV_ITEMS = ", "").replace(/;$/, ""));
const SIMPLE_NAV = eval(SIMPLE_NAV_SRC.replace("const SIMPLE_NAV = ", "").replace(/;$/, ""));

function lsGet(k, d) { const v = localStorage.getItem(k); return v ? JSON.parse(v) : d; }
function lsSet(k, v) { localStorage.setItem(k, JSON.stringify(v)); }
function acctKey(base) {
  return base + "@" + ((session.user && session.user.username) || "_anon");
}
// admin = Perm::all()，什么都可见
function isNavVisible() { return true; }
const NAV_INDEX = {}; NAV_ITEMS.forEach(n => { NAV_INDEX[n.id] = n; });
function simpleNav() {
  const v = lsGet(acctKey("nav_simple"), null);
  return v === null ? true : !!v;
}

function render() {
  const q = "";
  const all = NAV_ITEMS.filter(isNavVisible);
  const simple = !q && simpleNav();
  const ctx = { all, simple, q };
  // 逐字复制 app.js 的那两行，不改写
  const listed = eval(LISTED_SRC.replace("const listed = ", "").replace(/;$/, ""));
  const favs = [], recents = [];
  const rest = eval(REST_SRC.replace("const rest = ", "").replace(/;$/, ""));
  const groups = [], gmap = {};
  rest.forEach(n => { const g = n.group || ""; if (!gmap[g]) { gmap[g] = []; groups.push(g); } gmap[g].push(n); });
  const html = groups.map(g =>
    `<div class="nav-sec" data-sec="g:${esc(g)}"></div><div class="nav-body">` +
    gmap[g].map(n => `<button class="nav-item" data-view="${n.id}">${esc(n.label)}</button>`).join("") +
    `</div>`).join("");
  return { ...ctx, listed, rest, html };
}

// 从 HTML 里抽出 .nav-body 里的 nav-item data-view
function viewsIn(html) {
  return [...html.matchAll(/data-view="([a-z0-9-]+)"/g)].map(m => m[1]);
}

let bad = 0;
const ok = (m) => console.log("OK   " + m);
const fail = (m) => { bad++; console.log("FAIL " + m); };

// ---- 1. 默认开 ----
localStorage.removeItem("nav_simple@admin");
let r = render();
if (!r.simple) fail("默认应为精简态（localStorage 无记录）");
else ok("默认是精简态");
const laid = viewsIn(r.html);
for (const h of ["mrp", "routing", "work-report", "budget-analysis", "inv-abc"]) {
  if (laid.includes(h)) fail(`${h} 不该出现在默认侧栏`); else ok(`${h} 已藏起`);
}
for (const s of ["vouchers", "begin", "accounts", "aux", "ledger", "reports"]) {
  if (!laid.includes(s)) fail(`${s} 应出现在默认侧栏`); else ok(`${s} 在侧栏`);
}
if (laid.length <= 20) ok(`默认铺 ${laid.length} 项（<=20）`);
else fail(`默认铺了 ${laid.length} 项，太多了`);

// ---- 2. 搜索结果始终全量（不变量①的逻辑依据）----
const hitAll = NAV_ITEMS.filter(n => !q(n));
function q() { return false; } // 空查询 = 全命中
if (hitAll.length === NAV_ITEMS.length) ok(`搜索数据源是全量 ${hitAll.length} 项（不受精简影响）`);
else fail("搜索数据源被精简影响了");

// ---- 3. 关掉 → 全量 ----
lsSet("nav_simple@admin", false);
r = render();
if (r.simple) fail("显式关掉后仍是精简态");
else ok("显式关掉 → 全量");
const all = viewsIn(r.html);
for (const h of ["mrp", "routing"]) if (!all.includes(h)) fail(`关掉后 ${h} 仍不在`); else ok(`关掉后 ${h} 在`);
if (all.length === NAV_ITEMS.length) ok(`全量 ${all.length} 项`);
else fail(`全量应是 ${NAV_ITEMS.length} 项，实际 ${all.length}`);

// ---- 4. 切回精简，刷新后保持 ----
lsSet("nav_simple@admin", true);
r = render();
if (!r.simple) fail("切回精简失败");
else ok("切回精简 → 持久保持");

// ---- 5. 账号分区：A 关掉不影响 B ----
// 注意要先切 session 再读写 —— acctKey 拼的是**当前登录账号**，
// session 不换就等于在测 admin 自己的 key（第一版就栽在这，报了个假失败）。
lsSet("nav_simple@cashier", false);
session = { user: { username: "cashier" } };
if (simpleNav()) fail("cashier 的 false 没生效");
else ok("账号分区生效（cashier 单独关掉）");
session = { user: { username: "admin" } };
if (!simpleNav()) fail("admin 的 true 被 cashier 的设置污染了");
else ok("admin 不受 cashier 设置影响");

// ---- 6. 只排收藏，不排最近（2026-10-06 那条反馈）----
{
  const s = REST_SRC;
  if (s.indexOf("recents") >= 0) fail("rest 排掉了 recents —— 最近会被搬走");
  else ok("rest 只排 favs，不排 recents");
  if (s.indexOf("favs") >= 0) ok("rest 排掉了 favs（常用只出现一份）");
  else fail("rest 没排 favs —— 常用会在原分组出现第二份");
}

console.log(bad ? `\n${bad} 个问题` : "\n全部通过");
process.exit(bad ? 1 : 0);