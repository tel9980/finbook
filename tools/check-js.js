// JS 语法检查：按**退出码**判定，不靠肉眼看输出。
//
// 为什么要这个脚本：`node --check file.js 2>&1 | Select-Object -First 1` 在
// PowerShell 里会把「有错」和「没错」都显示成一段 stderr 文本，肉眼判断极容易
// 看错——我已经被骗过一次：`node --check` 报了语法错（app.js:1061 缺一个 `}`），
// 而我 `Select-Object -First 1` 只截到 PowerShell 的错误头，紧接着的
// `echo "js ok 若空"` 照样打印，于是我以为检查通过了，把一个**整个前端都跑不起来**
// 的文件当成好的往下做。
//
// 正确做法：看 $LASTEXITCODE，或用本脚本直接汇总「哪些文件不通过」。
const { execFileSync } = require("child_process");
const fs = require("fs");
const path = require("path");

const targets = [];
const walk = (dir) => {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) {
      if (e.name !== "node_modules" && e.name !== "test-results") walk(p);
    } else if (e.name.endsWith(".js")) {
      targets.push(p);
    }
  }
};
walk(path.join(__dirname, "..", "crates", "finweb", "static"));
walk(path.join(__dirname, "..", "e2e", "tests"));
const helpers = path.join(__dirname, "..", "e2e", "helpers.js");
if (fs.existsSync(helpers)) targets.push(helpers);

let bad = 0;
for (const f of targets) {
  try {
    execFileSync(process.execPath, ["--check", f], { stdio: "pipe" });
  } catch (e) {
    bad++;
    console.log("FAIL " + path.relative(path.join(__dirname, ".."), f));
    const out = ((e.stderr || Buffer.alloc(0)).toString() + (e.stdout || Buffer.alloc(0)).toString())
      .split("\n")
      .slice(0, 4)
      .join("\n    ");
    console.log("    " + out);
  }
}

// ---------------------------------------------------------------------------
// 重复 id 检查（只在**同一个函数内**比对）
//
// 为什么要单独查：**重复 id 是完全合法的 JavaScript，语法检查抓不到**，但后果
// 和语法错一样致命——`$("#ak-list")` 只操作第一个，第二个永远停在「加载中…」，
// 页面上看起来「有个列表但没数据」，E2E 报 strict mode violation，排查方向
// 还会被带偏到「接口没返回」。
//
// 我为此白跑了一轮 9.7 分钟的 E2E：把 app.js 回退到 HEAD 后重放自己的编辑，
// 而 HEAD 里本来就有密钥管理块，我又加了一份，页面上出现两个 `#ak-list`。
//
// **只在同一函数内比**，因为全文件比对会误报 23 处：同一个 id 在不同视图各写
// 一份是完全正常的（同一时刻只渲染一个视图）。一个会误报 23 次的检查等于没有
// 检查——人会开始无视它，那真 bug 就一起漏了。函数边界用「顶格的 function /
// async function」近似，本文件的视图函数都是这个写法。
const STATIC = path.join(__dirname, "..", "crates", "finweb", "static");
const idTargets = [path.join(STATIC, "app.js"), path.join(STATIC, "index.html")].filter((f) =>
  fs.existsSync(f)
);
const TOP_FN = /^(?:async\s+)?function\s+\w+|^(?:const|let|var)\s+\w+\s*=\s*(?:async\s*)?\(/;
for (const f of idTargets) {
  const src = fs.readFileSync(f, "utf8");
  const lines = src.split(/\r?\n/);
  // 顶层函数名 -> Map(id -> 行号)
  const scopes = new Map();
  let scope = "<module>";
  lines.forEach((line, i) => {
    if (TOP_FN.test(line)) {
      const m = /(\w+)\s*[=(]/.exec(line);
      scope = m ? m[1] : "<anon>";
    }
    if (!scopes.has(scope)) scopes.set(scope, new Map());
    const inScope = scopes.get(scope);
    const re = /\bid="([A-Za-z][\w:.-]*)"/g;
    let m;
    while ((m = re.exec(line)) !== null) {
      const id = m[1];
      if (!inScope.has(id)) inScope.set(id, []);
      inScope.get(id).push(i + 1);
    }
  });
  for (const [fn, ids] of scopes) {
    for (const [id, at] of ids) {
      // 同一行内的重复是**故意的互斥分支**，不是 bug。
      // 典型写法：`${list.length ? `<select id="x">…` : `<input id="x" …>`}`
      // 两者只渲染一个，而读值都走 `$("#x").value`，对 select 和 input 都成立。
      // 把它判成错就等于逼着人把 id 拆成两个，只会让代码变丑而不会变对。
      if (at.length > 1 && new Set(at).size === 1) continue;
      if (at.length > 1) {
        bad++;
        console.log(
          `FAIL 重复 id #${id} 在 ${path.relative(path.join(__dirname, ".."), f)} 的 ` +
            `${fn}() 内出现 ${at.length} 次（第 ${at.join("、")} 行）——` +
            `$("#${id}") 只会操作第一个，另一个永远不会被更新`
        );
      }
    }
  }
}

// ---------------------------------------------------------------------------
// nginx sub_filter 锚点检查
//
// 为什么要查这个：某些部署的反代并不只是"转发"，它还在**下发时改写 app.js**
// （在 vhost 里配 sub_filter，具体配置路径因面板而异，不写进本仓库）：
//
//     sub_filter 'r.status === 401'
//                'r.status === 401 || (r.status === 403 && data && data.error === "未登录或会话已失效")';
//     header_filter_by_lua_block { if ngx.status == 401 and upstream_addr ~= "" then ngx.status = 403 end }
//
// 起因：finweb 的 401 不带 WWW-Authenticate，浏览器收到后会把已缓存的 Basic
// 凭据一起清掉，下一轮又撞上第一道 basic 框 —— 用户表现为"登录两遍"。
// 于是反代把上游 401 改写成 403，并让前端把"403 + 未登录"也当会话失效处理。
//
// **这个补丁靠 app.js 里的字面量 `r.status === 401` 生效。** 一旦重构
// api()（换变量名、改成 switch、抽成常量）字面量消失，sub_filter 就静默变成
// 空操作，症状是"用户偶发要登录两遍"——本地和 CI 全绿，只有生产复现。
// 语法检查抓不到这种"部署配置依赖源码字面量"的耦合，所以在这里机器化。
//
// 判定（两条都要满足）：
//   1. 该字面量在 app.js 里**恰好出现 1 次**——0 次 = 补丁失效；
//      >1 次 = 补丁会在多处注入，可能改到不该改的分支。
//   2. 它前面（同一函数内）必须有 `let data = null` 之类的 data 声明，
//      否则注入的 `data &&` 引用未定义变量 → 直接 ReferenceError，整个前端白屏。
// ---------------------------------------------------------------------------
const ANCHOR = "r.status === 401";
const appFile = path.join(STATIC, "app.js");
if (fs.existsSync(appFile)) {
  const src = fs.readFileSync(appFile, "utf8");
  const hits = src.split(ANCHOR).length - 1;
  if (hits !== 1) {
    bad++;
    console.log(
      `FAIL nginx sub_filter 锚点 \`${ANCHOR}\` 在 app.js 里出现 ${hits} 次（应为 1 次）——\n` +
        `    生产 nginx 靠这个字面量把上游 401 改写成 403 并让前端识别会话失效；\n` +
        `    0 次 = 补丁静默失效，症状是「用户要登录两遍」；>1 次 = 补丁会改到多处分支。\n` +
        `    修法：保证 api() 里恰有一处 \`if (r.status === 401) {\`，不要抽成常量或换写法。`
    );
  } else {
    // 注入的表达式里用了 data，必须在同一函数作用域内已声明
    const at = src.indexOf(ANCHOR);
    // 往上找最近的函数起点
    const before = src.slice(0, at);
    const fnStart = Math.max(
      before.lastIndexOf("\nasync function "),
      before.lastIndexOf("\nfunction ")
    );
    const scope = src.slice(fnStart < 0 ? 0 : fnStart, at);
    if (!/\b(let|const|var)\s+data\b/.test(scope)) {
      bad++;
      console.log(
        `FAIL nginx sub_filter 锚点 \`${ANCHOR}\` 所在函数里没有 data 声明——\n` +
          `    nginx 注入的 \`data && data.error === "未登录或会话已失效"\` 会引用未定义的 data，\n` +
          `    结果是整页 ReferenceError 白屏。修法：在 401 判断之前声明 data。`
      );
    }
  }
}


// ---------------------------------------------------------------------------
// 三条前端行为检查（2026-09-29 全岗位流程走查发现的 ①②⑥）
//
// 这三条的共同形态：**代码看起来完全正常，界面上也不报错，只有走一遍流程才发现
// 结果是错的**。所以不能靠"改的时候记得"，得让机器盯着。
//
// 判据一律取**函数体**（顶格 function 到下一个顶格 `}`），不在全文件里 grep：
// 一个会误报的检查等于没有检查 —— 人会开始无视它，真 bug 就一起漏了。
// ---------------------------------------------------------------------------
const appSrc = fs.existsSync(appFile) ? fs.readFileSync(appFile, "utf8") : "";
const appLines = appSrc.split(/\r?\n/);

/** 取某个顶层函数的完整函数体；找不到返回 "" */
function fnBody(name) {
  let start = -1;
  for (let i = 0; i < appLines.length; i++) {
    if (new RegExp("^(?:async\\s+)?function\\s+" + name + "\\s*\\(").test(appLines[i])) {
      start = i;
      break;
    }
  }
  if (start < 0) return "";
  for (let i = start + 1; i < appLines.length; i++) {
    if (/^\}/.test(appLines[i])) return appLines.slice(start, i + 1).join("\n");
  }
  return "";
}

/**
 * 剥掉行注释再比对。
 *
 * 为什么必须剥：这几条检查要判「代码里有没有做某个判断」，而函数体里的注释
 * 恰恰会把关键词原样念一遍（例：autoEmptyRows 的注释里有「没有权限」「403」）。
 * 不剥注释的话，把守卫整行删掉，检查照样通过 —— 我第一版就栽在这：
 * 变异测试报「FAIL 放过了 ⑥ 去掉 autoEmptyRows 的权限提前 return」。
 * 靠注释过关的检查比没有检查更糟，它给人虚假的安全感。
 */
const stripComments = (src) => src.replace(/\/\/[^\n]*/g, "");

const rule = (title) => {
  console.log(
    "FAIL " + title + "\n" +
      "    （对应 2026-09-29 走查发现：缺陷只在走流程时暴露，语法检查与后端测试都抓不到）"
  );
};

// --- ① 侧栏「收藏 / 最近」必须按当前角色可见性过滤 + 按账号分区 -----------------
//
// 实测：出纳在「最近」里看到并点进了「期初建账 / 固定资产 / 期末处理」——这三个
// 都要求他没有的权限（opening / account_edit / period_close），点进去只得到
// 「权限不足」。主导航一直是正确过滤的，只有 localStorage 这条路径漏了。
//
// 附带修的隐私问题：两个 key 原来所有账号共用，同一台电脑上会计访问过的模块，
// 出纳登录后一眼看到 —— 既是入口泄漏也是隐私泄漏。
if (appSrc) {
  for (const fn of ["getFavs", "getRecent"]) {
    const body = stripComments(fnBody(fn));
    if (!body) {
      bad++;
      rule(`${fn}() 不见了 —— 侧栏「收藏/最近」的权限过滤是它的职责，改名或删除请同步改本检查`);
    } else if (!/isNavVisible\s*\(/.test(body)) {
      bad++;
      rule(
        `${fn}() 没有调 isNavVisible() —— 收藏/最近会列出当前角色看不到的页面，` +
          `\n    出纳能点进会计专属页并撞上「权限不足」。修法：在 filter 里加 isNavVisible(NAV_INDEX[id])。`
      );
    }
  }
  // 账号分区：要判的是 acctKey **函数体**真的在拼用户名，不是「文件里出现过 acctKey
  // 这个词」（第一版就是这么写的，把 acctKey 改成 return base 照样通过）
  const ak = stripComments(fnBody("acctKey"));
  if (!ak) {
    bad++;
    rule("acctKey() 不见了 —— 收藏/最近必须按登录账号分区");
  } else if (!/session\.user/.test(ak) || !/base\s*\+\s*"/.test(ak)) {
    bad++;
    rule(
      `acctKey() 里没有把当前登录用户名拼进 key —— localStorage 的 nav_fav / nav_recent ` +
        `\n    会变成所有账号共用，同一台电脑上同事访问过哪些模块一眼可见。修法：return base + "@" + <username>;`
    );
  }
}

// --- ② 期初的试算平衡必须随输入实时重算 ---------------------------------------
//
// 实测：填入 154 万真实数据，卡片仍显示 0.00 / ✓平衡；故意造成 165,999 的
// 不平衡，也照样显示「✓ 平衡」。根因：合计只在 render() 里算，而 render() 只在
// 「添加/移除科目行」时触发，oninput 只写 draft 不重绘。
//
// 判据：三个金额输入框的 oninput 都必须调 paintTotals()。刻意不检查 render()
// 本身 —— 走 render() 会重建 <input>，正在输入的框失焦、光标跳到开头。
if (appSrc) {
  const body = stripComments(fnBody("renderBegin"));
  if (!body) {
    bad++;
    rule("renderBegin() 不见了 —— 期初试算平衡的重算入口是它，本检查需要跟着改");
  } else {
    // ④ 已保存的行也必须能删。
    //
    // 原先那一格是 `${r.id > 0 ? "已有" : 移除按钮}` —— **已经保存过的行压根没有
    // 移除按钮**，编码框还是 readonly。也就是会计录错一个科目之后，界面上既删不掉
    // 也改不了，只能去数据库里手改。后端补的 delete_ids 因此是死代码。
    // 判据：data-rm 按钮不能是某个三元式的 **false 分支**（`… : \`<button … data-rm`）。
    //
    // 不要写成「那一格里不能出现 id > 0」—— 合法写法就是把「已有」标记和按钮放在
    // 同一格里（`${r.id > 0 ? '<span>已有</span>' : ''}<button … data-rm>`），
    // 那种写法会被误判（我第一版就误判了，检查在正确代码上报红）。
    // 真正要禁的只有"按钮被 : 包住"，而 false 分支必然以字符串字面量开头，
    // 所以 `: \`<button` 这个形状是唯一需要禁的。
    const rmCell = (() => {
      const rmAt = body.indexOf('data-rm="${i}"');
      if (rmAt < 0) return null;
      const s = body.lastIndexOf("<td", rmAt);
      const e = body.indexOf("</td>", rmAt);
      return { found: true, text: body.slice(s < 0 ? 0 : s, e < 0 ? rmAt + 200 : e) };
    })();
    if (!rmCell) {
      bad++;
      rule(
        `renderBegin() 里找不到渲染 data-rm 的按钮 —— 已保存的行可能又变回"不可移除"了\n` +
          `    （原来就是 r.id > 0 ? "已有" : 移除按钮，会计录错科目后界面上删不掉也改不了）。\n` +
          `    修法：移除按钮无条件渲染，"已有"只作为附加标记。`
      );
    } else if (/:\s*[`'"]\s*<button[^>]*data-rm/.test(rmCell.text)) {
      bad++;
      rule(
        `data-rm 按钮被写成了三元式的 false 分支 —— 已保存的行将没有移除按钮，后端补的\n` +
          `    delete_ids 永远走不到。修法：按钮无条件渲染，"已有"另起一个标记。`
      );
    }
    // 改编码的已有行必须删旧行，否则 upsert 会在新编码下再插一条、旧编码那条留着。
    // 两处都要在：写入侧（orig_code: … 初始化）与读取侧（r.orig_code 比较）。
    // 只查 "orig_code" 这个词会被读取侧的 r.orig_code 骗过 —— 变异测试报过
    // 「FAIL 放过了 ④ 不再记录 orig_code」。
    // 注意必须锚到 `orig_code: r.account_code`：只查 "orig_code:" 会被
    // 「添加空行」时 push 的 `orig_code: ""` 骗过（变异测试报过「放过了」）。
    if (!/orig_code\s*:\s*r\.account_code/.test(body)) {
      bad++;
      rule(
        `renderBegin() 读回期初行时没有记录 orig_code —— 会计把已保存行的科目编码改掉后，\n` +
          `    upsert 会按新编码插一条、旧编码那条留在库里，同一个科目出现两笔期初。\n` +
          `    修法：初始化时加 orig_code: r.account_code。`
      );
    }
    if (!/r\.orig_code/.test(body)) {
      bad++;
      rule(
        `保存时没有比较 r.orig_code —— 改了编码的已有行不会被删旧行，新旧两条并存。\n` +
          `    修法：if (r.id > 0 && (r.orig_code || "") !== code) deleteIds.push(r.id);`
      );
    }
    for (const cls of ["bg-yb", "bg-ad", "bg-ac"]) {
      // 抓 `... $all(".bg-yb", main).forEach((inp) => inp.oninput = ...)` 这一行
      const handlers = body
        .split("\n")
        .filter((l) => l.includes(cls) && /\.oninput\s*=/.test(l));
      if (handlers.length === 0) {
        bad++;
        rule(
          `renderBegin() 里找不到 .${cls} 的 oninput —— 年初/累计金额输了不重算试算平衡，` +
            `\n    会计会照着一个恒显示「✓平衡」的假守卫录完整个期初。`
        );
      } else if (!handlers.some((h) => /paintTotals\s*\(\s*\)/.test(h))) {
        bad++;
        rule(
          `.${cls} 的 oninput 没有调 paintTotals() —— 输入金额时试算平衡不刷新，` +
            `\n    界面继续显示上一次的数字（实测填 154 万仍显示 0.00 / ✓平衡）。`
        );
      }
    }
  }
}

// --- ⑥ 「加载中…」不能被渲染成「加载失败 + 重试」 ------------------------------
//
// 实测：出纳打开「固定资产」，同时看到「没有「科目维护」权限」和
// 「⚠ 加载失败 / 数据没能取回来，可能是网络问题或会话过期 / 重试」，而重试
// 永远不会成功（是无权限，不是网络问题）。用户会去排查不存在的网络故障。
//
// 根因有两条，缺一不可，所以两条都钉：
//   1. `EMPTY_PLACEHOLDERS` 里**不能**含「加载中」。它是待定状态，不是空、不是失败。
//   2. `render()` 判空态前必须**等视图的 Promise settle**。多数视图先填
//      「加载中…」再 await api()，不等就是在请求还在飞时判它失败。
//
// 第一版我把判据写成「autoEmptyRows 里有匹配权限文案的提前 return」——那是猜的
// 机制，真实机制是竞态，猜错的方向检查就永远通过（变异测试当时也没能揭穿它，
// 因为我把注释里的「权限」二字算成了守卫）。教训记在这里：判据要钉**机制**，
// 不要钉**表象**。
if (appSrc) {
  const stripAll = (s) => s.replace(/\/\/[^\n]*/g, "");
  const constRe = (name) => {
    const m = new RegExp("const\\s+" + name + "\\s*=\\s*([^;]+);").exec(stripAll(appSrc));
    return m ? m[1] : "";
  };
  const emptyRe = constRe("EMPTY_PLACEHOLDERS");
  if (!emptyRe) {
    bad++;
    rule("找不到 EMPTY_PLACEHOLDERS —— 空态占位词表改名或删除了，本检查需要跟着改");
  } else if (/加载/.test(emptyRe)) {
    bad++;
    rule(
      `EMPTY_PLACEHOLDERS 又把「加载中」算成空态了 —— 会渲染出「⚠ 加载失败 + 重试」，` +
        `\n    而那一刻请求还在飞、什么都没失败；加载真失败时这个假错误态还会永久留着。\n` +
        `    修法：「加载中」归 LOADING_PLACEHOLDERS，autoEmptyRows 里遇到它直接 return。`
    );
  }
  const loadRe = constRe("LOADING_PLACEHOLDERS");
  if (!loadRe || !/加载/.test(loadRe)) {
    bad++;
    rule(
      `缺少 LOADING_PLACEHOLDERS（或它不含「加载中」）—— 「待定」必须有自己的一类，` +
        `\n    混进 EMPTY_PLACEHOLDERS 就是把待定当失败。`
    );
  }
  const body = stripComments(fnBody("autoEmptyRows"));
  if (!body) {
    bad++;
    rule("autoEmptyRows() 不见了 —— 占位词分类的判定在它里面");
  } else if (!/LOADING_PLACEHOLDERS\.test\(txt\)\s*\)\s*return;/.test(body)) {
    bad++;
    rule(
      `autoEmptyRows() 遇到「加载中」没有直接 return —— 待定状态仍会被改写成空态` +
        `\n    （空态或"加载失败 + 重试"）。修法：if (LOADING_PLACEHOLDERS.test(txt)) return;`
    );
  }
  // renderMain() 里判空态必须等 Promise settle
  const renderBody = stripComments(fnBody("renderMain"));
  if (!renderBody) {
    bad++;
    rule("renderMain() 不见了 —— 空态判定的时机在它里面");
  } else if (!/Promise\.resolve\(r\)\s*\.\s*then\([\s\S]{0,400}autoEmptyRows\(\)/.test(renderBody)) {
    bad++;
    rule(
      `renderMain() 没有等视图的 Promise settle 就判空态 —— fn(main) 返回的 Promise 在第一个\n` +
        `    await 上还没 resolve，此时 <tbody> 里全是「加载中…」，被判成"加载失败"渲染出\n` +
        `    「⚠ 加载失败 + 重试」。修法：把 autoEmptyRows() 放进 Promise.resolve(r).then(...) 里。`
    );
  }
}

console.log(
  bad === 0
    ? `OK: ${targets.length} 个 JS 文件语法通过 + ${idTargets.length} 个文件无重复 id + nginx sub_filter 锚点在位`
    : `FAIL: ${bad} 个问题`
);
// --- ⑰ 建账向导必须两道闸门都问、都写进账套参数、提示语都覆盖 ----------------
//
// 实测：require_cashier 的出厂默认从 false 改成 true 之后，向导只问审核、不问出纳。
// 于是「一个人记账」的小微企业建完账套才发现：现金/银行存款的凭证永远卡在
// 「需出纳签字」，而他根本没有出纳可签 —— **向导里没有任何一个字提到出纳**，
// 也没有任何报错，只是第一次录现金凭证时被拒。
//
// 判据取 showSetupWizard 的函数体：app.js 别处也有 enable_audit（凭证弹窗的
// 「记账」按钮、账套参数页的 checkbox），全文件 grep 会把「向导没问」判成「问过」。
if (appSrc) {
  const wiz = fnBody("showSetupWizard");
  if (!wiz) {
    bad++;
    rule("showSetupWizard() 不见了 —— 建账向导改名时本检查要跟着改");
  } else {
    // 判据分三层，缺一层就会被一种绕过方式骗过去：
    //   ① 函数体里出现字段名            —— 挡住「整个问题删掉」
    //   ② 出现 `id="set-xxx-on"` 输入元素 —— 挡住「字段名还在但没有那个控件」
    //   ③ 出现 `$("#set-xxx-on")` 读取    —— 挡住「控件在但值写死成 true」
    // 第 ② 层不能用 `set-cash-on` 这个词代替：读取那行
    // `const require_cashier = ($("#set-cash-on") && …)` 里也有同一个词，
    // 我第一版就这么写，结果把单选删掉检查照样通过。
    // 第 ③ 层也不能省：只判 ①② 的话，把值改成 `const require_cashier = true`
    // 依然全绿 —— 而那正是「向导不问用户」的实现方式。
    for (const g of [
      { f: "require_cashier", id: "set-cash-on", why: "建账向导必须问「有没有出纳」" },
      { f: "enable_audit", id: "set-audit-on", why: "建账向导必须问「有没有独立审核人」" },
    ]) {
      const hasField = new RegExp("\\b" + g.f + "\\b").test(wiz);
      if (!hasField) {
        bad++;
        rule(
          `${g.why}：showSetupWizard() 里没有出现 ${g.f}。` +
            `\n    出处：require_cashier 出厂默认改成 true 之后，向导只问审核不问出纳，` +
            `\n    「一个人记账」的小微企业建完账套才发现现金凭证永远记不了账，` +
            `\n    而向导里没有半个字提到出纳。修法：加一对 set-cash 单选并写进账套参数。`
        );
        continue;
      }
      if (!wiz.includes(`id="${g.id}"`)) {
        bad++;
        rule(
          `showSetupWizard() 读了 ${g.f}，但没有 \`id="${g.id}"\` 这个输入控件` +
            `\n    （只找 ${g.id} 这个词是不够的：读取它的那行里也有同一个词）`
        );
        continue;
      }
      if (!wiz.includes(`$("#${g.id}")`)) {
        bad++;
        rule(
          `showSetupWizard() 里没有读取 \`$("#${g.id}")\` —— ${g.f} 的值多半是写死的。` +
            `\n    控件摆在那里但值恒为 true/false，用户的选择不起作用，等于没问。`
        );
      }
    }
    // 提示语：判据落在「构成提示的那几行」上，而不是「附近 600 字里出现过」。
    // 后者被注释一长就失效 —— 我第一版就是这么写的，删掉出纳那句它照样通过。
    const pushes = wiz.split(/\n/).filter((l) => /notes\.push\(|notes\s*=\s*\[/.test(l));
    const says = pushes.join("\n");
    if (!/enable_audit/.test(says) || !/require_cashier/.test(says)) {
      bad++;
      rule(
        `建账成功的提示语没同时说出两道闸门的实际状态` +
          `\n    判据看的是构成提示的那几行（notes.push / notes = [），` +
          `\n    不是「附近若干字里出现过」—— 后者被注释一长就失效。` +
          `\n    原来只说审核：用户以为万事大吉，第一次录现金凭证就被出纳签字拦下，说不出为什么`
      );
    }
    // 正向输出：**没有这行我无法区分「检查通过」和「检查根本没执行」**。
    // 一检查默默没跑，报的还是「OK」，那比没有检查更危险。
    console.log("建账向导：审核与出纳签字两问都在，值都写进账套参数，提示语都覆盖");
  }
}

// --- ⑱ 弹窗文案不许出现 markdown 强调号 ------------------------------------
//
// `confirmDialog` 用 `esc(message)` 渲染 —— 那是 **HTML 转义**，不是 markdown。
// 所以文案里写了 `**全部**`，用户看到的就是字面的 `**全部**`（带星号）。
// toast 的文案同样按纯文本显示。
//
// 这类错语法检查、变异测试、集成测试全都抓不到，只能靠机器扫 ——
// 我写「设备上限变 2 台」的文案时就犯了这个，写完顺手 grep 才发现。
if (appSrc) {
  const dlgHits = [];
  appLines.forEach((l, i) => {
    if (!/confirmDialog\(|toast\(/.test(l)) return;
    if (/\*\*/.test(l)) dlgHits.push(i + 1);
  });
  if (dlgHits.length) {
    bad++;
    for (const h of dlgHits) {
      console.log(`FAIL app.js:${h} 的 confirmDialog/toast 文案里有 markdown 强调号 **`);
    }
    console.log(
      "    confirmDialog 走 esc(message)，那是 HTML 转义不是 markdown；" +
        "toast 文案同样按纯文本显示。修法：强调用「」书名号，" +
        "或把 <b> 写进 innerHTML 而不是文案里。"
    );
  } else {
    console.log("弹窗文案：confirmDialog/toast 里没有 markdown 强调号（按纯文本渲染）");
  }
}

// --- ⑲ 已签字的凭证不该再显示「出纳签字」按钮 -------------------------------
//
// 出纳签字从「默认关」变成「默认开」之后，这个按钮对**所有**用户常驻出现。
// 已签字时同时摆着「出纳签字」和「取消签字」两个按钮：
// 点前者是幂等空操作（保留首位签字人），但用户看不出哪个才是该点的。
// 正确形态是二选一：没签字给「出纳签字」，签过了给「取消签字」。
//
// 判据钉在按钮的**显示条件**上（`!v.cashier`），不是「有没有这个按钮」。
if (appSrc) {
  const vw = fnBody("openVoucherEditor");
  if (!vw) {
    bad++;
    rule("openVoucherEditor() 不见了 —— 凭证弹窗改名时本检查要跟着改");
  } else {
    const signLine = vw.split("\n").find((l) => l.includes('id="v-sign"')) || "";
    if (signLine && !/!v\.cashier/.test(signLine)) {
      bad++;
      rule(
        `凭证弹窗的「出纳签字」按钮没按「!v.cashier」隐藏` +
          `\n    已签字时它和「取消签字」并排出现：点前者是幂等空操作，` +
          `\n    但用户看不出该点哪个。修法：显示条件加上 && !v.cashier。`
      );
    } else {
      console.log("凭证弹窗：已签字时只给「取消签字」，不再并排摆「出纳签字」");
    }
  }
}

// --- ⑳ 侧栏「最近」是追加，不是搬家 -------------------------------------------
//
// 2026-10-06 用户反馈：点过一个导航项，它就从原来的分组里消失、只剩「最近」里一份。
// 「最近」本该是**多给一个入口**，不是把菜单项搬走 —— 搬走之后分组少一项、组内计数
// 也跟着少，用起来像菜单项被删了。
//
// 判据钉机制（三条缺一条都是真故障）：
//   1. 铺开分组用的 `rest` 只能排「收藏」：排了 recents 就是搬家；
//   2. 「最近」那一份必须是 nav-jump —— 同一个 data-view 出现两份 nav-item，
//      `.nav-item[data-view=x]`（E2E 里到处是这个写法）会一次匹配两个，Playwright
//      严格模式直接失败、toHaveCount(1) 会变成 2；
//   3. 侧栏点击委托必须认 nav-jump —— 不认就是「点了没反应」：不报错、不跳转，
//      是最难查的一类。
if (appSrc) {
  const b20 = bad;
  const rs = stripComments(fnBody("renderSidebar"));
  if (!rs) {
    bad++;
    rule("renderSidebar() 不见了 —— 侧栏分组与「最近」的渲染都在它里面，改名请同步改本检查");
  } else {
    // 判据是**语义**「rest 的过滤条件里只有 favs，没有 recents」，不是字面
    // `all.filter(…)`。原来写死了字面量，于是给 rest 加任何一层包装
    // （比如精简模式下改成 listed.filter(…)）都会被当成缺陷 —— 而那种改动
    // 本来是完全正确的。字面匹配会把「正确的重构」判成「行为变了」，
    // 逼着改动者要么放弃改进、要么去改这个检查，越改越僵。
    //
    // 反过来也不能太松：只要 rest 的谓词里出现了 recents 就必须报红。
    const restLine = rs.split("\n").find((l) => l.indexOf("const rest =") >= 0) || "";
    if (!restLine) {
      bad++;
      rule("renderSidebar() 里找不到 `const rest =` —— 铺开分组的过滤行没了");
    } else if (restLine.indexOf("recents") >= 0) {
      bad++;
      rule(
        "renderSidebar() 铺开分组的 `rest` 把 recents 也排掉了 —— 最近访问过的页面被从" +
          "\n    原分组里搬走了（点一下菜单项，菜单项就没了）。修法：rest 只排 favs。"
      );
    } else if (restLine.indexOf("favs") < 0) {
      bad++;
      rule(
        "renderSidebar() 铺开分组的 `rest` 不排 favs —— 「常用」会在原分组里出现第二份。"
      );
    }
    const recLine = rs.split("\n").find((l) => l.indexOf('secHtml("recent"') >= 0) || "";
    if (recLine.indexOf("nav-jump") < 0) {
      bad++;
      rule(
        '「最近」分区不是用 nav-jump 渲染的 —— 要么改回了 nav-item（同一 data-view 两份，' +
          '\n    E2E 严格模式会炸），要么这一行被拆了。修法：itemHtml(n, "", "nav-jump")。'
      );
    }
    const deleg = appSrc.split('closest(".nav-item, .nav-jump")').length - 1;
    if (deleg !== 1) {
      bad++;
      rule(
        "侧栏点击委托没认 nav-jump（匹配 " + deleg + " 次，应为 1）—— 「最近」里那一份" +
          "\n    点了不跳转也不报错。修法：e.target.closest(\".nav-item, .nav-jump\")。"
      );
    }
  }
  if (bad === b20) console.log("侧栏「最近」：原分组那一份保留，最近只是多一个入口（nav-jump）");
}

// --- ㉑ 视图快照不许存「入口跳转哨兵」选项 -------------------------------------
//
// 2026-10-06 用户反馈：数据导入页选了「银行对账单 →（转到银行对账）」跳走之后，就再也
// 回不到数据导入 —— 每次点进去 300ms 后又被拽去银行对账。
//
// 链路：选中 __bank → bc.click() 同步走 goView → saveViewState 读 #imp-kind 的值，
// 把 `__bank` 写进 viewState.imports → 回到该页 restoreViewState 还原它、并在 300ms 后
// 给它派发 change → syncKind 见到 __bank 又跳一次 → 离开时再存回 __bank：死循环。
// （select 的哨兵项是**入口跳转**，从来不是用户选的值，本来就不该进快照。）
//
// 判据：saveViewState 的**存值语句之前**必须有哨兵拦截。判在代码上，不是「注释里
// 提过 __bank」—— 注释里提过照样能把人拽走。
if (appSrc) {
  const sv = stripComments(fnBody("saveViewState"));
  if (!sv) {
    bad++;
    rule("saveViewState() 不见了 —— 视图快照的哨兵拦截在它里面，改名请同步改本检查");
  } else if (!/el\.tagName === "SELECT" && el\.value\.indexOf\("__"\) === 0/.test(sv)) {
    bad++;
    rule(
      'saveViewState() 没有拦掉 `__` 开头的哨兵选项 —— __bank 这种「入口跳转」项被存进' +
        '\n    快照后，回到该页会在 300ms 后派发 change 把用户再拽去银行对账，出不来。' +
        '\n    修法：存值前 if (el.tagName === "SELECT" && el.value.indexOf("__") === 0) return;'
    );
  } else {
    console.log("视图快照：`__` 开头的入口跳转哨兵不进快照（__bank 不会让数据导入页回不去）");
  }
}

// 视图函数「引用了作用域外变量」→ 浏览器里白屏，`node --check` 只验语法验不出来。
// 这一项把函数原文抽出来给桩**真调用一次**，ReferenceError 当场就现形。
// （v1.8.21 档案页的「显示停用」勾选框就是这么白屏的，靠 CI 的 Playwright 抓到。）
{
  const r = require("child_process").spawnSync(
    process.execPath,
    [path.join(__dirname, "check-app-render.js")],
    { encoding: "utf8" }
  );
  const out = ((r.stdout || "") + (r.stderr || "")).trim();
  if (r.status !== 0) {
    bad++;
    console.log("FAIL 视图函数渲染检查未通过：");
    console.log(out.split("\n").filter((l) => l.startsWith("FAIL")).map((l) => "     " + l).join("\n") || "     " + out);
  } else {
    console.log(`视图函数渲染：${out.split("\n").filter((l) => l.startsWith("OK")).length} 个通过（引用作用域外变量会当场抛 ReferenceError）`);
  }
}

process.exit(bad === 0 ? 0 : 1);
