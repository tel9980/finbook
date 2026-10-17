// 把「检查脚本不得含机器相关的东西」变成常驻检查。
//
// 为什么值得：这一轮 Test job 红的原因就是 selftest 写死了本机绝对路径
// `C:\Users\Administrator\...` —— 本地跑**完全正常**（路径就在那儿），
// 只有 CI 的 Linux runner 上才 ENOENT 崩。
// 也就是说「本地绿」对这一类错误完全没有证明力。
//
// 要拦的机器相关写法（都是我在这个仓库里真实犯过的）：
//   1. 盘符绝对路径      C:\... 或 C:/...
//   2. 用户目录          C:\Users\<名>\... / /Users/<名>/...
//   3. 写死行尾 join/split("\r\n")   —— 会把 LF 文件整个换成 CRLF
//   4. 写死的 localhost 端口（工具脚本里合理，但检查脚本里通常是想探测）
//
// 范围：只扫 tools/ 与 e2e/ 下的 .js。理由：这两个目录是「在任何机器上都要能跑」的；
// 业务代码（crates/）里有大量正常的路径处理。
const fs = require("fs");
const path = require("path");

const ROOT = path.join(__dirname, "..");
const DIRS = ["tools", "e2e"];

const RULES = [
  {
    name: "盘符绝对路径",
    re: /["'][A-Za-z]:[\\/]{1,2}[^"'\n]*["']/,
    hint: "改成 path.join(__dirname, '..') 或从环境变量/参数拿",
  },
  {
    name: "用户目录",
    re: /["'](?:[A-Za-z]:)?[\\/]Users[\\/][^"'\n]*["']/,
    hint: "同上",
  },
  {
    name: "写死 CRLF",
    re: /\.(?:join|split)\("\\\\r\\\\n"\)/,
    hint: "按文件探测：s.indexOf('\\r\\n') >= 0 ? '\\r\\n' : '\\n'",
  },
];

// 4. 显式豁免：check-portable 自己的自检脚本（selftest-portable.js）必须**构造**
//    违规样本来验证检查有效，所以它必然含有盘符路径字面量。
//    豁免必须**显式列出文件**并说明理由 —— 不能靠「文件名像自检就放过」，
//    那是给自己开后门。任何新增豁免都要在这里写清为什么。
const EXEMPT = {
  "tools/selftest-portable.js":
    "本文件就是 check-portable 的自检：必须构造含盘符路径的违规样本才能证明检查有效。" +
    "它已用 __dirname 定位自己的文件（不依赖绝对路径），只有**变异数据**是字面量。",
};

let bad = 0;
const files = [];
for (const d of DIRS) {
  const base = path.join(ROOT, d);
  if (!fs.existsSync(base)) { console.log("FAIL " + d + "/ 不存在"); bad++; continue; }
  (function walk(p) {
    for (const f of fs.readdirSync(p)) {
      const fp = path.join(p, f);
      if (fs.statSync(fp).isDirectory()) { if (f !== "node_modules") walk(fp); }
      else if (f.endsWith(".js")) files.push(fp);
    }
  })(base);
}

let hits = 0;
let exempted = 0;
for (const f of files) {
  const rel = path.relative(ROOT, f).split("\\").join("/");
  if (EXEMPT[rel]) {
    exempted++;
    console.log(`豁免  ${rel}  ${EXEMPT[rel].slice(0, 70)}…`);
    continue;
  }
  const src = fs.readFileSync(f, "utf8");
  const lines = src.split(/\r?\n/);
  for (const rule of RULES) {
    lines.forEach((l, i) => {
      if (!rule.re.test(l)) return;
      // 注释里提到是允许的（文档该说清楚为什么不能这么写）
      const t = l.trim();
      if (t.startsWith("//") || t.startsWith("*") || t.startsWith("/*")) return;
      hits++;
      console.log(`FAIL ${path.relative(ROOT, f)}:${i + 1}  ${rule.name}`);
      console.log(`       ${t.slice(0, 110)}`);
      console.log(`       修法：${rule.hint}`);
    });
  }
}

if (bad === 0) {
  if (hits === 0) {
    console.log(
      `可移植性：扫描 ${files.length} 个 .js，无机器相关写法` +
        (exempted ? `（${exempted} 个显式豁免，见上面理由）` : "")
    );
  } else {
    bad++;
  }
}
process.exit(bad === 0 ? 0 : 1);
