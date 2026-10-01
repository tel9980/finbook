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
- **轮转建议**：每日全量 + 保留 30 天（cron 脚本按上例组织；同日重跑前先清理旧目录）。
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
# 健康检查
curl http://127.0.0.1:8080/api/health

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

## 9. 安全检查清单

- [ ] 反向代理已启用 HTTPS，8080 未对外暴露；纯 HTTPS 下 `FINWEB_SECURE_COOKIE=true`
- [ ] systemd 服务开了加固参数（`deploy/finweb.service` 已包含）
- [ ] `realm.db` 与 `books/` 目录权限 `finbook:finbook` 私有
- [ ] 每日备份 cron 已生效且有轮转（realm + books + attachments 三处齐全）
- [ ] 平台管理员口令已预置或用自动生成口令后立即改密
- [ ] 普通用户账号由管理员开通，默认强制首登改密
- [ ] 账套内默认管理员 `admin` 已改默认口令（新账套首登强制改密）
