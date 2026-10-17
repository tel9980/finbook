# FinBook 生产部署指南（多租户）

> 本文面向「把 FinBook Web 端（finweb）正式部署到服务器」的场景。
> 桌面端（finbook）无需部署——直接分发可执行文件给会计人员即可。

---

## 1. 架构总览

```
浏览器 ──HTTPS──> 反向代理 (nginx / caddy) ──HTTP──> finweb :8080
                                                        │
                                                        ├── realm.db     平台身份库（平台账号 + 账套目录）
                                                        └── books/*.fbk  用户自建账套（每套一个 SQLite 文件，WAL 模式）
```

- **finweb**：Axum HTTP 服务，默认监听 `127.0.0.1:8080`（裸跑不对外；Docker 镜像内置 `0.0.0.0:8080`，但只应映射到回环再经反代）
- **多租户**：所有用户共用一套 `realm.db`（平台账号、账套目录）；每个账套是 `books/` 下的独立 `.fbk` 文件，账套之间彼此隔离
- **多用户**：同一服务器上多浏览器同时登录同一账套，由 WAL + `busy_timeout=5s` 保证并发安全
- **会话**：登录态存**服务进程内存**（见 §7 限制），部署时务必保持单进程

## 2. 环境变量

| 变量 | 默认值 | 说明 |
|---|---|---|
| `FINBOOK_REALM` | `./data/realm.db` | 平台身份库路径（平台账号 + 账套目录），**务必放在持久化磁盘/数据卷** |
| `FINBOOK_BOOKS_DIR` | `./data/books` | 用户自建账套存放目录 |
| `FINBOOK_LISTEN` | `127.0.0.1:8080` | 监听地址（生产建议 `127.0.0.1:8080` + 反向代理） |
| `FINWEB_STATIC_DIR` | 可执行文件同级 `static/` | 前端静态资源目录 |
| `FINBOOK_ADMIN_USER` | `admin` | 首次启动引导的平台管理员账号 |
| `FINBOOK_ADMIN_PASS` | 自动生成 | 首次启动引导的平台管理员口令；留空则自动生成强口令并打印一次 |
| `FINBOOK_ADMIN_MUST_CHANGE` | `false` | 平台管理员首次登录是否强制改密（`1`/`true` 开启） |
| `FINWEB_SECURE_COOKIE` | `false` | 会话 Cookie 加 `Secure` 标记（纯 HTTPS 部署时设 `true`） |

> **平台管理员引导**：`realm.db` 为空库时首次启动自动创建平台管理员，账号口令打印在启动日志（仅一次）。
> 生产建议直接用 `FINBOOK_ADMIN_USER` / `FINBOOK_ADMIN_PASS` 预置，不要依赖自动生成口令。

## 3. 方式一：systemd 直接部署（推荐 Linux 服务器）

1. 编译：`cargo build --release -p finweb`
2. 复制产物与静态资源：
   ```bash
   sudo mkdir -p /opt/finbook
   sudo cp target/release/finweb /opt/finbook/
   sudo cp -r crates/finweb/static /opt/finbook/
   ```
3. 创建专用用户、数据目录并安装服务（详见 `deploy/finweb.service` 注释）：
   ```bash
   sudo useradd --system --home /opt/finbook --shell /usr/sbin/nologin finbook
   sudo mkdir -p /opt/finbook/data
   # 数据目录归运行用户；二进制/静态资源保持 root 只读，防服务被攻破后篡改自身
   sudo chown -R finbook:finbook /opt/finbook/data
   sudo chown -R root:root /opt/finbook/finweb /opt/finbook/static
   sudo chmod 755 /opt/finbook/finweb
   sudo chmod -R a-w /opt/finbook/finweb /opt/finbook/static
   sudo cp deploy/finweb.service /etc/systemd/system/
   sudo systemctl daemon-reload && sudo systemctl enable --now finweb
   ```
4. 验证：`curl http://127.0.0.1:8080/api/health` 应返回 `ok`
5. 查看平台管理员口令（若未用环境变量预置）：`journalctl -u finweb` 启动横幅里有一次性凭据

## 4. 方式二：Docker 部署

```bash
docker build -t finbook:latest .
docker run -d --name finbook -p 127.0.0.1:8080:8080 \
  -v finbook_data:/data \
  -e FINBOOK_LISTEN=0.0.0.0:8080 \
  finbook:latest
```

- **数据卷**：`realm.db` 与 `books/` 都在卷 `finbook_data` 下的 `/data`（镜像内已设
  `FINBOOK_REALM=/data/realm.db`、`FINBOOK_BOOKS_DIR=/data/books`），容器销毁重建**不丢数据**
- 镜像内置 `HEALTHCHECK`（探测 8080 端口）
- 预置平台管理员（可选）：
  ```bash
  docker run -d --name finbook -p 127.0.0.1:8080:8080 \
    -v finbook_data:/data \
    -e FINBOOK_ADMIN_USER=admin -e FINBOOK_ADMIN_PASS='ChangeMe123' \
    -e FINBOOK_ADMIN_MUST_CHANGE=true \
    finbook:latest
  ```
- 数据卷务必定期备份（见 §6）

### 4.1 ⚠️ 发布端口必须匹配反代实际连的地址

**这是本项目实际踩过的坑：改端口映射会让站点整体 502，而容器 healthcheck 全绿。**

反向代理连的地址**不是**「宿主哪个端口」，而是「它配置里写死的那个」。上游地址写错 /
端口没发布，用户看到的就是 502 Bad Gateway，而后端日志一切正常 —— 因为请求根本没
到后端。

**动端口前必须先确认这三件事**（不要凭印象推导）：

```bash
# 1) 反代配置里写死的上游地址是什么
nginx -T 2>/dev/null | grep -nE 'proxy_pass|listen |server_name'

# 2) 容器实际监听的端口
docker exec <容器> sh -c 'cat /proc/net/tcp | awk "NR>1 {print \$2}"'
#    或直接看镜像默认值：grep FINBOOK_LISTEN Dockerfile

# 3) 宿主机上该端口是否真的有人监听
ss -lntp | grep <端口>
```

上游可能是**回环**（`127.0.0.1`），也可能是**宿主在 docker bridge 网段上的地址**。
这两种对应的端口绑定方式不同：

| 上游写法 | 必须这样发布 | 原因 |
|---|---|---|
| `127.0.0.1:<P>` | `-p 127.0.0.1:<P>:<P>` | docker-proxy 只在回环监听 |
| `<宿主 bridge IP>:<P>` | `-p <P>:<P>`（绑 `0.0.0.0`） | 回环绑定在 bridge 地址上没有监听 |
| 宿主某个别的端口 | `-p <该端口>:<容器端口>` | 端口号不必相同，但容器侧要对得上 |

**改完端口必须从反代那侧验，而不是从容器内**：

```bash
# 1) 直接打反代配置里的上游地址，必须 200（唯一能证明反代链路通了的检查）
curl -s -o /dev/null -w '%{http_code}\n' http://<上游地址>/api/health     # 期望 200

# 2) 经反代：若外层有 basic 认证，无凭据应得 401（反代自己的挑战）；
#    **得 502 就是上游不通**
curl -sk -o /dev/null -w '%{http_code}\n' -H 'Host: <对外域名或IP>' https://127.0.0.1/

# 3) 看反代错误日志确认它连的是哪个地址
tail /var/log/nginx/<站点>.error.log
#    典型信息：connect() failed (111: Connection refused) ... upstream: "http://<上游>/"
```

> 教训：`docker inspect` 显示 healthy 只说明**容器内**探针通过。宿主→容器的
> 端口映射错了，healthcheck 发现不了。只看 healthy 就宣布部署完成，会让站点
> 挂着 502 而你以为是好的。

> `FINWEB_SECURE_COOKIE` 要和「TLS 在哪里终结」对上：若 finweb 前面没有一层
> 终结 TLS 的反代（或用户直连 http），设成 `true` 会让 Cookie 发不出去，
> 表现为**整站无法登录**。

> ⚠️ **不要把生产环境的主机名、IP、面板/反代配置路径、口令文件路径写进本仓库。**
> 本文档所在的仓库是公开的。上面这些具体值请记在仓库之外（密码管理器 / 私人笔记）；
> 本文只保留**方法**，不记录任何一台具体机器的坐标。

### 4.2 ⚠️ 若反代会改写 app.js，别改 `api()` 的 401 写法

某些部署会在反代里对下发的前端 JS 做 `sub_filter` 改写。典型场景：finweb 的 401
不带 `WWW-Authenticate`，浏览器收到后会把已缓存的 Basic 凭据一起清掉，下一轮又撞上
第一道 basic 框 —— 用户表现为「要登录两遍」。于是反代把上游 401 改写成 403，并让前端
把「403 + 未登录」也当会话失效处理：

```nginx
sub_filter 'r.status === 401'
           'r.status === 401 || (r.status === 403 && data && data.error === "未登录或会话已失效")';
header_filter_by_lua_block {
    if ngx.status == 401 and (ngx.var.upstream_addr or "") ~= "" then ngx.status = 403 end
}
```

**这个补丁靠 `app.js` 里的字面量 `r.status === 401` 生效。** 重构 `api()` 时若把
它改成 `res.status === 401`、抽成常量、或改成 switch，字面量一消失补丁就静默
失效，症状是「用户偶发要登录两遍」—— 本地和 CI 全绿，只有生产复现。

`node tools/check-js.js` 里有两条机器检查兜住这件事（锚点必须**恰好出现 1 次**，
且同函数内必须有 `data` 声明，否则注入的 `data &&` 会 ReferenceError 白屏）。
**改 `api()` 后必须跑它。** 即便你当前没有用 `sub_filter`，这个检查也建议保留：
它防的是「日后有人在这层加了同类补丁」时被静默打断。

### 4.3 静态检查自己也要被验证（变异测试）

`tools/check-js.js` 里除语法检查外，还有一批**行为检查**——它们编码的是只能靠
走流程才暴露的约束（期初试算平衡必须随输入刷新、侧栏「最近」必须按角色过滤、
「加载中…」不得被渲染成「加载失败 + 重试」、已保存的期初行必须可删…）。

这些检查最大的风险是**写成永远通过的样子**：判据钉的是表象而不是机制，重构一下
代码形状就悄悄失效，而 CI 全绿。本轮就有三条是这样被抓住的：

| 判据（错） | 实际机制（对） |
|---|---|
| `autoEmptyRows` 里有匹配「权限」文案的提前 return | 根因是 `renderMain()` 没等视图 Promise settle（竞态），跟文案无关 |
| 那一格里不出现 `id > 0` | 合法写法就是「已有」标记与按钮同格；要禁的只是「按钮是三元式 false 分支」 |
| 出现过 `orig_code` 这个词 | 「添加空行」里也有 `orig_code: ""`，会骗过；必须锚到 `orig_code: r.account_code` |

所以**每加一条行为检查，都要同时加一条变异**：

```bash
node tools/check-ci.js              # CI 配置 + 变异清单是否还过期 + 编码卫生
node tools/check-ci.js --selftest   # 自检自身：改坏 CI 配置/清单/编码/夹具/会话语义，必须报红（13 条）
node tools/mutate-check.js          # 改坏 app.js，验证 check-js 真的会失败（18 条）
node tools/mutate-rust.js           # 改坏 Rust 源码，验证对应测试真的会失败（17 条）
```

这几个脚本都会**先确认变异真的生效**再判成败 —— 变异串没匹配上源码（CRLF/LF、
缩进）时必须单独报「未生效」，不能混进「检查放过了」。这一点是被坑出来的：
第一版 `mutate-check.js` 有 3 条变异因为换行没匹配上而静默无效，结论完全反了；
`check-ci.js --selftest` 自己的两个探针也踩了同一个坑。
`mutate-rust.js` 还会判「测试是否真的跑了」（`0 passed` 视为没跑），
避免命令指错 crate 时拿到一个假的 PASS。

`check-ci.js` 另外盯三件逐条比对看不出来的事：

1. **CI 配置能不能解析**（加 job 最容易在这里缩进出错，肉眼看 YAML 缩进不可靠）。
2. **变异清单有没有被删/改到失效** —— 变异靠 `from` 串字面匹配源码，源码一改就可能
   对不上，那条变异从此形同虚设。外加两条数量下限，防「有人把清单清空，此后 CI 一直全绿」。
3. **全仓库编码卫生** —— 不许有 U+FFFD 替换符 / 非法 UTF-8 / BOM。
   为什么要机器判：UTF-8 字节序列被截断后，**在控制台里看起来和正常中文一模一样**；
   反过来，正常中文在 GBK 控制台里显示成乱码，我又因此误判过好几次
   「文件被我写坏了」。唯一可靠的判据是码位 —— U+FFFD（替换符）只可能来自解码失败，
   正常文本里不会出现。
   本轮这条检查上线后扫出 **7 处既有损坏**（跨 6 个文件），其中**一处是用户可见字符串**：
   `handlers.rs` 里导出的增值税申报表标题，「一般纳税」与「申报表」之间丢了三个
   字节，渲染出来是「增值税一般纳税□□□申报表（主表）」。其余是 5 处注释 + 2 个
   带 BOM 的源文件。有一处（`scm.rs`）的原字无法从上下文确定，就**重写句子保住技术
   含义**，没有猜 —— 猜错一个字比留一个替换符更难发现。

   > 讽刺的是，这条检查上线后第一个报出来的就是本节文档自己：解释 U+FFFD 时
   > 把那个字符本身写了进去。检查不认「我只是想说明它」。

> 判据要钉**机制**，不要钉**表象**。写检查时先问一句：这条检查在代码被改成
> 另一种同样正确/同样错误的写法时，还成立吗？

### 4.3.1 ⚠️ 改默认值之前，先看清有谁在继承它

本轮改了一个默认值（`require_cashier` 默认 false → true），代价是三批测试集体变红：

| 夹具 | 表现 |
|---|---|
| `finweb/tests/api.rs` 的 `test_state()` | 28 条集成测试变红 |
| `findb/src/lib.rs` 的 `tests::test_opts()` | 57 条单测变红 |
| `findb/tests/{core_loop,ext_loop}.rs` 的夹具 | 21 条变红 |

三批的成因完全一样：夹具写了 `BookOptions { enable_audit: false, ..Default::default() }`，
**显式关掉一道闸门、继承另一道**。默认值一翻，继承的那道就跟着变。

难看的不是数量，是**报错指向**。三批的报错分别是「记账应成功 left: 400 right: 200」、
「批量记账：涉及现金/银行科目的凭证需出纳先签字再记账」，全都指向**凭证记账逻辑**。
任何人顺着报错去查记账逻辑都查不到 —— 真实原因在几百行之外的测试夹具里。

两条必须做的事：

1. **夹具里两道闸门都显式写出来**，并写一句「为什么测试要关、生产默认要开」。
   半年后没人敢动它，也得让人知道它护着什么。
2. **检查要按「每一个字面量」扫，不要按名字列举**。我一开始只查了 `test_state()` 和
   `test_opts()` 两个，第三处立刻又炸了一遍。按名字列举 = 按记忆列举 = 迟早漏。
   `check-ci.js` 现在扫测试代码里每一个 `BookOptions` 字面量
   （排除函数返回类型 `pub fn … -> BookOptions {`，否则有 2 条永久性误报）。

> 顺带一个更隐蔽的检查缺陷：`check-ci.js` 里的 `fnBodyOf()` 原先只按大括号计数收尾，
> 而 `async fn post_login(` 的签名是**跨行**的 —— 在还没见到第一个 `{` 时 depth 就已经是 0，
> 于是抽出来只有签名两行，签名里当然找不到任何方法调用，所有用它的检查都报
> 「一个都没用」。修法是**必须先见过一个 `{`** 才可能在 depth 回到 0 时收尾。
> 检查工具的失败模式比被检查的代码更隐蔽：它不报错，只是让别的检查变成永真。

### 4.4 「只有会计 + 出纳」的配置必须真的走得通

这条单独立一节，因为它已经绊倒我五次，每次绊法还不同。内控闸门与登录设备策略各自都能把
小公司堵死，而**每一条单看都合理**：

| 闸门 / 策略 | 谁被卡住 | 为什么单看合理 | 怎么解 |
|---|---|---|---|
| `PeriodClose` 只挂主管 | 会计结不了账，账做到 12 月断掉 | 结账像是监督动作 | 会计也给 —— 结账没有三方分离的价值 |
| 收付款单审核只认 `VoucherAudit` | 出纳录的单没人能审成凭证，**且与 `enable_audit` 无关** | 三方分离 | 认 `VoucherAudit` 或 `VoucherPost`；审核与撤审必须同一判据 |
| `Role::Supervisor` 权限组含 `CashierSign` | **主管这个岗位根本建不出来** | —— | 主管不兼出纳签字 |
| `require_cashier` 默认**关** | 会计绕过出纳直接把现金/银行凭证记进总账，而**谁都不必做任何决定** | 「资金域节点不是全行业默认」 | 默认**开**，要关得在账套参数里显式关；建账向导必须同时问「有没有出纳」 |
| 一个账号只绑一台设备 | 一个人两台机器（公司台式 + 家里本）就得找管理员重置，**天天要人工介入** | 一人一机最安全 | 上限 2 台（`MAX_DEVICES_PER_USER`）；登录只顶掉**本设备**的旧会话 |

第二条尤其阴险：它在建账向导的承诺**之外**。向导会问「有无独立的审核人」并据此设
`enable_audit`，但收付款的审核判据压根不看这个开关，所以选「不启用审核环节」也
救不了。

判这类问题的办法只有一个：**用真实配置把每个岗位的日常动作跑一遍**。权限表是静态的，
看不出「这个角色组合能不能把一个月走完」。

> 上面第三条之所以活下来：**没有任何测试真的创建过主管账号** —— 既有用例都
> `User::new()` 直接造对象、不走 `validate_duty_separation`。
> 「测过 `Role::perms()` 的内容」和「这个角色用得起来」是两件事。
>
> 第四、五条是同一类错误的两个变种：**默认值本身就是一项决定**。默认值选「安全但费事」
> 还是「省事但绕过控制」，不会有人去评审，它直接决定了几百个新账套的形态。
> 第五条还有个更隐蔽的连锁：允许绑 2 台之后，登录仍然 `remove_by_username` 清掉该账号
> **全部**会话，于是第 2 台一登录就把第 1 台踢下线 —— 上限只是个数字，
> 用户体感仍然是只能登一台。改「能绑几台」时必须同时改「会话怎么存」。

## 5. HTTPS 反向代理（必须）

finweb 本身只提供 HTTP。**生产环境禁止把 8080 裸暴露到公网**（口令明文传输）。
推荐用 nginx 或 caddy 终结 TLS，把请求转发到 `127.0.0.1:8080`。

### nginx 配置示例

```nginx
server {
    listen 443 ssl;
    server_name finance.example.com;
    ssl_certificate     /etc/letsencrypt/live/finance.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/finance.example.com/privkey.pem;

    # 导入 Excel 走 base64 JSON，按需放宽（后端上限约 2MB 请求体 + 10MB 附件）
    client_max_body_size 12m;

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        # 报表/导入/备份可能较慢，默认 60s 易被截断
        proxy_read_timeout 300s;
        proxy_send_timeout 300s;
        # 常见安全响应头（应用侧未内置）
        add_header Strict-Transport-Security "max-age=31536000" always;
        add_header X-Content-Type-Options nosniff always;
        add_header Referrer-Policy same-origin always;
        add_header X-Frame-Options DENY always;
    }
}

server {
    listen 80;
    server_name finance.example.com;
    return 301 https://$host$request_uri;
}
```

### caddy 配置示例（自动 HTTPS）

```
finance.example.com {
    reverse_proxy 127.0.0.1:8080
}
```

> 证书建议用 Let's Encrypt（certbot 或 caddy 自动签发）。纯 HTTPS 部署时把
> `FINWEB_SECURE_COOKIE` 设为 `true`，会话 Cookie 会带 `Secure` 标记；
> 同时确保浏览器访问地址始终是 `https://`。内网/WireGuard 明文访问时保持默认关闭。

## 6. 备份策略（必须）

多租户下数据分三处，**三处都要备份**：

| 内容 | 位置 | 说明 |
|---|---|---|
| 平台身份库 | `realm.db` | 平台账号、账套目录（不含账套业务数据） |
| 账套库 | `books/*.fbk` | 每个账套一个文件（业务数据全部在此） |
| 应用内备份 | `books/backups/*.fbk` | Web 端「备份」功能产出的副本（若使用过） |
| 外置附件 | 各账套同目录 `.attachments/` | 大于 256KB 的附件落盘，备份时必须一起打包（桌面端） |

- **热备**（finweb 运行时可以直接执行，需主机安装 sqlite3）：
  ```bash
  set -euo pipefail
  BK=/backup/$(date +%F_%H%M)
  mkdir -p "$BK/books"
  # 账套：VACUUM INTO 产出紧凑副本，不影响运行
  for f in /opt/finbook/data/books/*.fbk; do
    [ -e "$f" ] || continue
    key=$(basename "$f" .fbk)
    sub="$BK/books/$key"
    mkdir -p "$sub"
    sqlite3 "$f" "VACUUM INTO '$sub/$key.fbk'"
    # 附件随账套分目录保存，避免不同账套同名文件互相覆盖
    if [ -d "$(dirname "$f")/.attachments" ]; then
      cp -a "$(dirname "$f")/.attachments" "$sub/.attachments"
    fi
  done
  # 平台身份库：同样用 VACUUM INTO（realm.db 是 SQLite）
  sqlite3 /opt/finbook/data/realm.db "VACUUM INTO '$BK/realm.db'"
  ```
- **冷备**：`systemctl stop finweb` 后直接 `cp -a /opt/finbook/data /backup/cold-$(date +%F)` 整个目录。
### 6.1 ⚠️ 手动备份时踩过的三个坑（`integrity_check` 通过 ≠ 备份可用）

2026-09-30 部署前重写备份脚本时把这三个坑各踩了一遍。它们共同的形状是：
**校验通过，但备份其实没用**。

| 坑 | 现象 | 为什么校验骗了过去 |
|---|---|---|
| 只扫 `/data` 根目录 | 账套一个都没备份到，只拿到一个 0 字节旧文件 | — |
| 0 字节文件 | `sqlite3` 对空文件返回 **`ok`** | 空文件是「合法的空 SQLite 库」，integrity_check 没有任何意见 |
| 只拷 `realm.db` | 丢掉尚未 checkpoint 的已提交事务 | WAL 模式下 `.db` 单文件不是完整快照 |

账套**不在** `/data` 根下，而在 `/data/books/*.fbk`（`/data/finbook.fbk` 是 9 月 13 日
留下的 0 字节旧文件）。所以：

```bash
# ① 递归，且只取非空的
for src in $(docker run --rm -v finbook_data:/data --entrypoint sh finbook:latest \
               -c 'find /data -name "*.fbk" -size +0'); do
  rel=${src#/data/}
  docker run --rm -v finbook_data:/data --entrypoint cat finbook:latest "$src" > /tmp/bk.fbk

  # ② 非空 —— 必须单独判，integrity_check 不会替你判
  [ -s /tmp/bk.fbk ] || { echo "BAD $rel 是 0 字节"; exit 1; }

  # ③ 结构完好
  ic=$(sqlite3 /tmp/bk.fbk 'pragma integrity_check')
  [ "$ic" = ok ] || { echo "BAD $rel : $ic"; exit 1; }

  # ④ **业务语义**：证明它真的是账套库，不是「合法但空」的库
  n=$(sqlite3 /tmp/bk.fbk "select count(*) from sqlite_master where type='table';")
  [ "${n:-0}" -gt 0 ] || { echo "BAD $rel 里一张表都没有"; exit 1; }
  echo "OK   $rel  $(stat -c %s /tmp/bk.fbk) bytes  表 $n 张  凭证 $(sqlite3 /tmp/bk.fbk 'select count(*) from voucher;') 张"

  mkdir -p "$(dirname "$DEST/$rel")" && cp /tmp/bk.fbk "$DEST/$rel"
done

# ⑤ 一个非空的都没找到 = 备份等于没做
[ "$found" -gt 0 ] || { echo '!! 没找到任何非空 .fbk'; exit 1; }

# ⑥ WAL 模式的身份库：-wal / -shm 必须一起拷
for n in realm.db realm.db-wal realm.db-shm; do ... ; done
```

> 顺带：`sqlite3` 只在**宿主机**上（`/usr/bin/sqlite3`），finbook 镜像里没有。
> 在容器里 `sqlite3 …` 会得到 `sh: sqlite3: not found` —— 而那不是「校验通过」，
> 是校验根本没跑。

> 备份做完还要**按业务语义回读一次**：`select username from realm_user`、
> `select company from realm_book`、`select count(*) from account`。
> 这一步能同时抓住「备份了但不是这份数据」和「备份了但少了关键表」。

- **轮转建议**：每日全量 + 保留 30 天（cron 脚本按上例组织；同日重跑前先清理旧目录）。

### 6.2 每日备份 cron（已落地：阿里云）

`tools/finbook-backup.sh` 是上面热备的**可执行版本**，已装在服务器
`/usr/local/bin/finbook-backup.sh`，crontab `30 4 * * *` 每天 04:30 跑：

| 环节 | 做法 | 对应 §6.1 的坑 |
|---|---|---|
| 一致快照 | 宿主机 `sqlite3 … .backup`（`realm.db` + `books/*.fbk`） | ③ WAL 下裸拷丢已提交事务 |
| 非空 | 源文件与快照都 `test -s`，0 字节直接 FAILED | ① 0 字节照样过校验 |
| 业务语义回读 | `realm_book` / `realm_user` 行数 > 0，账套表数 > 0，凭证数入日志 | ② 「合法但空」的库 |
| 附件 | `.attachments/` 两个可能位置存在即打包 | 三处齐全 |
| 归档自检 | `tar tzf` 可读，且必须含 `realm.db` 与 `books/*.fbk` | 打包漏文件 |
| 轮转 | 保留 14 天（`FINBOOK_BACKUP_KEEP`） | — |
| 日志 | `/var/log/finbook-backup.log`，每档一行 `OK size= 账套= realm_book=` | — |

路径全部可用环境变量覆盖（`FINBOOK_DATA_VOL` / `FINBOOK_BACKUP_OUT` /
`FINBOOK_BACKUP_KEEP` / `FINBOOK_BACKUP_LOG`）。自检脚本正是靠这四个变量
把**变异夹具**喂进去的，而不是只跑一遍绿灯：

```bash
bash tools/selftest-finbook-backup.sh            # 服务器上：4 条该失败的探针 + 1 条正例 + 真实卷
bash tools/selftest-finbook-backup.sh ./tools/finbook-backup.sh   # 指定被检脚本
```

> 4 条失败探针：数据卷不存在、账套 0 字节、账套 0 张表（「合法但空」）、
> `realm_book` 0 行（备份到了别的数据）。任一条**没被拦下**即自检失败。
- **自动化脚本（推荐）**：`deploy/` 下有两个现成脚本，省掉手写 cron：
  | 脚本 | 作用 | 建议频率 |
  |---|---|---|
  | `deploy/backup.sh` | 走产品自身 `POST /api/backups` 逐个账套备份（内部是 WAL checkpoint + 一致快照），并清理超期备份 | 每天一次（凌晨） |
  | `deploy/healthcheck.sh` | 探 `/api/health` + 账套目录可写性 + 磁盘水位，连续失败才告警（避免抖动误报） | 每 5 分钟 |

  ```bash
  # 口令别写进脚本：放环境变量或同目录 .env（chmod 600）
  echo 'FINBOOK_ADMIN_PASS=你的口令' > /opt/finbook/backup.env && chmod 600 /opt/finbook/backup.env
  # 宝塔计划任务填：FINBOOK_ADMIN_PASS 由任务环境变量提供，然后
  bash /opt/finbook/deploy/backup.sh
  bash /opt/finbook/deploy/healthcheck.sh
  ```
  > 用管理员账号跑备份脚本是安全的：管理员登录不绑定设备、也不踢掉其他会话，
  > 不会把正在用系统的同事挤下线。但该账号若处于「必须修改口令」状态，除改密接口外
  > 全部 401，脚本会报「获取账套列表失败」——先用浏览器登录改完口令即可。
  > 脚本只覆盖应用内账套备份；`realm.db` 与外置 `.attachments/` 仍需按上面的热备/冷备单独处理。
- **恢复（务必按顺序）**：
  1. `systemctl stop finweb`（或停容器）
  2. 用备份覆盖 `realm.db`
  3. 把 `<key>.fbk` 按原文件名放回 `books/<key>.fbk`（应用内备份目录 `books/backups/` 同理）
  4. 把各账套附件放回对应账套目录 `.attachments/`
  5. 启动服务，抽查登录与账套数据（列表/报表）

> ⚠️ 不要把 `realm.db` / `books/` 放在 NFS/SMB 网络盘被多台电脑并发直开（WAL 在网络文件系统上不稳定）。
> 多用户请统一走「服务器本机运行 + 浏览器访问」模式。

## 7. 会话与多实例限制（重要）

- 登录会话存 **finweb 进程内存**：进程重启后所有用户需重新登录（可接受，账套数据不受影响）；
  登录失败限流同样为内存态，重启清零。
- **不要**在同一数据目录上水平扩容为多个 finweb 实例：会话不共享且无分布式锁，
  SQLite 并发写虽安全但多实例会放大锁竞争。单服务器单进程即可支撑中小团队。
- 若确需高可用，方案是「主库 + 定期 VACUUM INTO 副本」用备份恢复，而不是多活。

## 8. 运维命令

```bash
# 存活探针：只回答「进程还在收请求吗」，**不碰任何依赖**（恒返回 ok）
curl http://127.0.0.1:8080/api/health

# 深度探针：真去查账号库 / 账套库 / 数据目录可否读写
# 依赖不可用时返回 **503**，并列出具体哪一项坏了
curl -i http://127.0.0.1:8080/api/health/ready

# 查看日志（含访问日志与启动横幅）
journalctl -u finweb -f

# 升级流程（替换二进制即可，账套 schema 自动迁移）
sudo systemctl stop finweb
sudo cp target/release/finweb /opt/finbook/
sudo systemctl start finweb

# 单账套忘密（对某个 .fbk 一键重置所有账号口令为 admin123，交接/紧急用）
cargo run -p findb --release --example reset_pwd -- /opt/finbook/data/books/company.fbk
# 常规做法：平台管理员在 Web 端「平台账号/用户管理」重置口令，用户首次登录强制改密
```

### 8.1 为什么有两个探针

| 端点 | 含义 | 碰依赖 | 失败时该做什么 |
|---|---|---|---|
| `/api/health` | liveness | 否 | **重启**应用 |
| `/api/health/ready` | readiness | 是（账号库 / 账套库 / 数据目录） | **摘流量**，别重启应用 |

这个区分是硬要求：liveness 一旦碰 DB，数据库变慢就会把容器判成 unhealthy，
编排器随即重启应用 —— 于是「数据库一慢就重启应用，重启又让数据库更慢」，
把一次本可自愈的抖动放大成宕机。

反过来，只查 liveness 也不够：它返回常量 `ok`，磁盘写满、账套文件损坏、
账号库被锁这些「系统已经不能用了」的状态它一律报健康。**监控要盯 readiness。**

`healthcheck.sh` 默认查 readiness；`deploy/finweb.service` 与 `Dockerfile`
已把 `FINBOOK_REQUIRE_BOOKS_DIR=true` 打开，数据目录不存在时服务会**启动失败**
而不是凭空建一个目录开始记账（详见 §10）。

### 8.2 证书到期看门狗（已落地：阿里云）

`tools/cert-expiry-check.sh` 装在 `/usr/local/bin/`，crontab `15 */6 * * *`：

- 从 `nginx -T` 抓**实际在用**的每一张 `ssl_certificate` —— 不手写清单，
  新增站点自动纳入；路径不存在也算 ERROR（配置改了但文件没落地）
- 算 `notAfter - now`，剩余天数 < 阈值（**4 天**）→ 往 `/var/log/cert-watch.log`
  写一行 `ERROR cert-expires-in-Nd` 并 `exit 1`
- 每次跑都记 `OK <剩余天数>`，日志本身就是一张证书台账

为什么阈值是 4：IP 站点用 Let's Encrypt **shortlived（6 天）**证书，acme.sh 每天
21:53 跑 `--cron`，ARI 把下次续期定在到期前约 3.3 天。若那次续期失败，剩余就是
3.3 天 —— 阈值 4 会在**续期失败的当次**立刻报，而不是等到剩 2 天。

```bash
tail -5 /var/log/cert-watch.log          # 看台账
/usr/local/bin/cert-expiry-check.sh 99   # 自证：阈值放大到 99 天，必须 ERROR + exit 1
/usr/local/bin/cert-expiry-check.sh 3    # 还原，必须 exit 0
```

> 告警目前**只落日志**（没有短信/邮件通道）。没人看日志 = 没有告警，
> 这一条要靠值班巡检或后续接通知渠道补上。

## 9. 安全检查清单

- [ ] 反向代理已启用 HTTPS，8080 未对外暴露；纯 HTTPS 下 `FINWEB_SECURE_COOKIE=true`
- [ ] systemd 服务开了加固参数（`deploy/finweb.service` 已包含）
- [ ] `realm.db` 与 `books/` 目录权限 `finbook:finbook` 私有
- [x] 每日备份 cron 已生效且有轮转（realm + books + attachments 三处齐全）—— 阿里云已装，见 §6.2
- [ ] 平台管理员口令已预置或用自动生成口令后立即改密
- [ ] 普通用户账号由管理员开通，默认强制首登改密
- [ ] 账套内默认管理员 `admin` 已改默认口令（新账套首登强制改密）

## 10. 数据目录必须事先存在（防静默数据丢失）

`FINBOOK_REQUIRE_BOOKS_DIR=true` 时，`FINBOOK_BOOKS_DIR` 指向的目录不存在，
服务会**直接启动失败**并说明原因，而不是自动创建一个。

为什么值得这么严格：容器部署时 `--volume` 路径写错、或 bind mount 因宿主机
目录不存在而没挂上，服务会在**容器可写层**里凭空造一个 `books` 目录。
然后一切照常：能登录、能录凭证、备份脚本也照常把 `.fbk` 写进同一层。
问题在于没人收到任何信号 —— 直到容器重建，账套和备份一起消失。

> 那个场景下 `df` 看磁盘是满的、`/api/health` 报 ok、监控一片绿色。
> 唯一的征兆是数据在重建后没了，而那时已经无法追溯。

```bash
# 部署前自检：先建好宿主机目录再起容器
mkdir -p /srv/finbook/books && chown -R 10001:10001 /srv/finbook
docker run -d -v /srv/finbook/books:/data/books finbook

# 确认数据确实落在宿主机上（而不是可写层）
docker inspect <容器> --format '{{range .Mounts}}{{.Source}} -> {{.Destination}}{{"\n"}}{{end}}'
```

### 10.1 ⚠️ 用**命名卷**时，`/data/books` 同样要事先建

上面那条只覆盖 **bind mount**。而 README 与 `Dockerfile` 的示例都是**命名卷**：
`docker run -v finbook_data:/data finbook`。这条路径上「宿主机目录不存在」这个
说法**对不上** —— 卷是 Docker 凭空建的**空卷**，镜像里预建的只有空 `/data`，
没有 `books/`。于是服务照样启动失败、照样报那串错，而那串错说的原因
（bind mount 失败）在这个场景下是**错的**，会让人往错的方向排查。

实测两处都踩过：

- CI 的 `Docker image smoke test` 就是这么挂的 —— `docker run` 没挂卷也没建目录，
  容器 `Exited(1)`、健康检查永远不通。
- 第一次看这个报错的人会以为是自己 `--volume` 路径写错了，其实卷是对的、
  只是里面少一个目录。

```bash
# 命名卷：先在卷里建好 books/，属主给成 finbook(uid 10001)
docker volume create finbook_data
docker run --rm -v finbook_data:/data --entrypoint mkdir    finbook:latest -p /data/books
docker run --rm -v finbook_data:/data --entrypoint chown    finbook:latest -R finbook:finbook /data
docker run -d -v finbook_data:/data finbook:latest
```

> 「起容器前先 mkdir」在 bind mount 上是**为了宿主机目录**，在命名卷上是为了
> **卷内部** —— 两件事，但结果一样：不做就起不来。而这个差异只写在报错里、
> 不写在文档里，就会有人被报错带偏。

确认部署不需要持久化（例如临时演示环境）时，显式关掉：
`FINBOOK_REQUIRE_BOOKS_DIR=0`。
