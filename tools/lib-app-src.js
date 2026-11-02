// 前端源码聚合器：前端从单个 app.js（14k 行、295 个顶层函数）拆成了
// `app.js` + `app-*.js` 共 16 份，按 `index.html` 里的 <script> 顺序加载
// （经典 script，共享全局作用域，不用 import/export）。
//
// 为什么要有这个文件：拆分之前，tools/ 里**有 5 个工具各自硬编码**
// `crates/finweb/static/app.js`。拆分当天它们不会报错，只会安静地少检查
// 15 个文件 —— 检查照样打印"通过"，实际等于全没查。所以「前端有哪些脚本、
// 按什么顺序拼」只能有一处定义，其余工具全部从这里取。
//
// 单一来源就是 `index.html` 本身：页面引用了什么、以什么顺序引用，就是浏览器
// 实际执行的顺序。这里做的只是把它读出来，不维护第二份清单。
const fs = require("fs");
const path = require("path");

const STATIC = path.join(__dirname, "..", "crates", "finweb", "static");

/** index.html 里按顺序引用的 app*.js 文件名（如 ["app.js", "app-auth.js", …]） */
function appFileNames() {
  const html = fs.readFileSync(path.join(STATIC, "index.html"), "utf8");
  const names = [];
  const re = /<script[^>]*\ssrc="\/([^"?]+\.js)/g;
  let m;
  while ((m = re.exec(html))) {
    const name = m[1];
    // util.js / icons.js 是独立的小工具，不参与 app 的检查与拼接
    if (!/^app.*\.js$/.test(name)) continue;
    if (!names.includes(name)) names.push(name);
  }
  if (names.length === 0) {
    throw new Error(
      "index.html 里找不到任何 app*.js 的 <script> 引用 —— 前端聚合器失效，" +
        "所有前端检查会变成空转，必须先修好再继续"
    );
  }
  return names;
}

/** [{ name, file, src }]，按浏览器加载顺序 */
function readAppFiles() {
  return appFileNames().map((name) => {
    const file = path.join(STATIC, name);
    return { name, file, src: fs.readFileSync(file, "utf8") };
  });
}

/** 全部 app*.js 拼接成一份源码（顺序 = 浏览器加载顺序） */
function readAppBundle() {
  return readAppFiles()
    .map((f) => f.src)
    .join("\n");
}

module.exports = { STATIC, appFileNames, readAppFiles, readAppBundle };
