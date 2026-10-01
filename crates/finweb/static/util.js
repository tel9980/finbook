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

// ===========================================================================
// 通用粘贴解析器（无 DOM 依赖，纯函数 —— 浏览器与 Node 均可测）
//
// 背景：期初建账、凭证录入都需要"从 Excel 复制一片区域直接粘进来"，
// 但两者的列含义完全不同。此前期初页自己写了一份解析，凭证页没有。
// 这里抽出三层：parseGrid（切格子）→ parseBeginText / parseVoucherText（认列）。
// 以后再加出入库单/往来单据，直接复用同一套。
// ===========================================================================

// 把剪贴板文本切成二维数组：优先按制表符（Excel），否则按逗号（CSV）。
// 自动去掉千分位逗号与货币符号，但保留数字内部的负号与小数点。
function parseGrid(text) {
  const src = String(text == null ? "" : text).replace(/\r/g, "");
  const lines = src.split("\n").filter((l) => l.trim() !== "");
  if (!lines.length) return [];
  const delim = lines[0].indexOf("\t") >= 0 ? "\t" : ",";
  return lines.map((l) => l.split(delim).map((c) => String(c).trim()));
}

// 金额归一：去千分位/货币符号/全角空格；空串返回 null（区别于 0）
function normNum(s) {
  if (s == null) return null;
  const t = String(s).replace(/[,\s¥￥$€]/g, "").trim();
  if (t === "") return null;
  if (!/^-?\d+(\.\d+)?$/.test(t)) return NaN; // 非法数字 → NaN，让调用方报错
  return Number(t);
}

const DIR_WORDS = { "借": "debit", "贷": "credit", "debit": "debit", "credit": "credit" };

// 期初余额表解析
//   A) 6~7 列：科目编码, 科目名称, 方向, 年初余额, 借方累计, 贷方累计, [数量]
//   B) 3~5 列：科目编码, 方向, 年初余额, [借方累计], [贷方累计]
// 返回 { rows, bad }：rows 为可写入的行；bad 为 [{line, code, why}] 逐行问题。
function parseBeginText(text, accounts) {
  const accMap = new Map((accounts || []).map((a) => [a.code, a]));
  const nameOf = (code) => { const a = accMap.get(code); return a ? a.name : ""; };
  const isUsable = (code) => { const a = accMap.get(code); return !!(a && !a.disabled); };
  const grid = parseGrid(text);
  const rows = [], bad = [];

  grid.forEach((cells, li) => {
    if (cells.length < 2) return;
    const code = cells[0].replace(/[,\s]/g, "");
    // 表头/说明行：首列不像科目编码就跳过（不计入 bad）
    if (!/^\d/.test(code)) return;

    let name = "", dirRaw = "", ybRaw = "", adRaw = "", acRaw = "", qtyRaw = "";
    if (cells.length >= 4) {
      const c1 = cells[1];
      if (DIR_WORDS[c1.toLowerCase()] != null) {      // B 格式：第 2 列就是方向
        dirRaw = c1; ybRaw = cells[2]; adRaw = cells[3]; acRaw = cells[4];
      } else {                                          // A 格式：第 2 列是科目名称
        name = c1; dirRaw = cells[2]; ybRaw = cells[3]; adRaw = cells[4]; acRaw = cells[5]; qtyRaw = cells[6];
      }
    } else {
      dirRaw = cells[1]; ybRaw = cells[2];
    }

    const yb = normNum(ybRaw);
    if (yb == null) return;
    if (isNaN(yb)) { bad.push({ line: li + 1, code, why: `金额「${ybRaw}」不是数字` }); return; }
    if (!isUsable(code)) {
      bad.push({ line: li + 1, code, why: nameOf(code) ? "科目已停用" : "科目不存在" });
    }
    // 方向列空且金额为负 → 视为贷方
    const dir = DIR_WORDS[String(dirRaw).toLowerCase()]
      || (yb < 0 ? "credit" : "debit");
    const ad = normNum(adRaw), ac = normNum(acRaw), qty = normNum(qtyRaw);
    rows.push({
      account_code: code, name, dir,
      yb: fmt(String(Math.abs(yb))),
      ad: ad != null && !isNaN(ad) ? fmt(String(ad)) : "",
      ac: ac != null && !isNaN(ac) ? fmt(String(ac)) : "",
      qty: qty != null && !isNaN(qty) ? fmt(String(qty)) : "",
    });
  });
  return { rows, bad };
}

// 凭证分录解析
//   A) 金蝶式：日期, 凭证字, 凭证号, 摘要, 科目编码, 科目名称, 借方, 贷方
//   B) 通用式：摘要, 科目编码, 借方, 贷方      （日期/凭证字取编辑器头部）
//   C) 精简式：科目编码, 借方, 贷方
// 同一张凭证的日期/凭证字从首行取（后续行留空，保存时沿用头部）。
// 返回 { rows, bad, date, word }。
function parseVoucherText(text, accounts) {
  const accMap = new Map((accounts || []).map((a) => [a.code, a]));
  const nameOf = (code) => { const a = accMap.get(code); return a ? a.name : ""; };
  const isLeaf = (code) => { const a = accMap.get(code); return !!(a && !a.disabled); };
  const isDate = (s) => /^\d{4}-\d{1,2}-\d{1,2}$/.test(String(s).trim());
  const grid = parseGrid(text);
  const rows = [], bad = [];
  let date = "", word = "";

  grid.forEach((cells, li) => {
    if (!cells.length) return;
    let summary = "", code = "", dRaw = "", cRaw = "";
    if (isDate(cells[0])) {
      // A 格式：日期, 凭证字, [凭证号], 摘要, 科目编码, [科目名称], 借, 贷
      date = date || cells[0].trim();
      word = word || (cells[1] || "").trim();
      const hasNo = /^\d+$/.test(String(cells[2] || "").trim());
      const off = hasNo ? 3 : 2;
      summary = cells[off] || ""; code = cells[off + 1] || ""; dRaw = cells[off + 2] || ""; cRaw = cells[off + 3] || "";
    } else if (cells.length >= 4) {
      // B 格式：摘要, 科目编码, 借, 贷
      summary = cells[0]; code = cells[1]; dRaw = cells[2]; cRaw = cells[3];
    } else if (cells.length === 3) {
      // C 格式：科目编码, 借, 贷
      code = cells[0]; dRaw = cells[1]; cRaw = cells[2];
    } else {
      return;
    }
    code = String(code).replace(/[,\s]/g, "");
    if (!/^\d/.test(code)) return;                 // 表头行

    const d = normNum(dRaw), c = normNum(cRaw);
    if ((d == null || isNaN(d)) && (c == null || isNaN(c))) {
      bad.push({ line: li + 1, code, why: "借贷金额都不是数字" });
      return;
    }
    if (!isLeaf(code)) bad.push({ line: li + 1, code, why: nameOf(code) ? "科目已停用或非末级" : "科目不存在" });
    rows.push({
      summary: String(summary).trim(),
      account_code: code,
      // 凭证金额统一两位小数（金额语义；数量/单价不在此路径）
      debit: d != null && !isNaN(d) ? fmtMoney(String(d)) : "0.00",
      credit: c != null && !isNaN(c) ? fmtMoney(String(c)) : "0.00",
    });
  });
  return { rows, bad, date, word };
}

// Node 测试导出
if (typeof module !== "undefined" && module.exports) {
  module.exports = { esc, fmt, fmtMoney, ymm, parseGrid, normNum, parseBeginText, parseVoucherText };
}
