// FinBook Web 纯工具函数（无 DOM 依赖，可在浏览器与 Node 中复用/测试）
//
// 浏览器：以普通 <script> 加载，函数声明提升为全局（window.esc / fmt / fmtMoney / ymm）。
// Node 测试：末尾 module.exports 导出，供 node:test 引用。

function esc(s) {
  if (s == null) return "";
  return String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
}

// 金额/数量格式化：字符串层加千分位，保留原小数位数，绝不 parseFloat（避免浮点误差）。
function fmt(s) {
  if (s == null) return "";
  let str = String(s).trim();
  if (str === "") return "";
  const neg = str.startsWith("-");
  if (neg) str = str.slice(1);
  const parts = str.split(".");
  parts[0] = parts[0].replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  return (neg ? "-" : "") + parts.join(".");
}

// 金额专用：强制两位小数 + 千分位
function fmtMoney(s) {
  if (s == null || s === "") return "";
  let str = String(s).trim();
  const neg = str.startsWith("-");
  if (neg) str = str.slice(1);
  let parts = str.split(".");
  const int = parts[0].replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  const dec = parts[1] != null ? parts[1] : "0";
  const tail = (dec + "00").slice(0, 2);
  return (neg ? "-" : "") + int + "." + tail;
}

// "2026-01" 或 "202601" -> 202601
function ymm(s) {
  const str = String(s).trim();
  if (/^\d{6}$/.test(str)) return parseInt(str, 10);
  const [y, m] = str.split("-");
  return parseInt(y, 10) * 100 + parseInt(m, 10);
}

// Node 测试导出（浏览器中 typeof module === "undefined"，此分支不执行）
if (typeof module !== "undefined" && module.exports) {
  module.exports = { esc, fmt, fmtMoney, ymm };
}
