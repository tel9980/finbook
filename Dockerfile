# syntax=docker/dockerfile:1
# FinBook Web 端容器镜像（多阶段构建，多租户模式）
# 构建: docker build -t finbook:latest .
# 运行: docker run -d --name finbook -p 127.0.0.1:8080:8080 \
#         -v finbook_data:/data finbook:latest
# 说明: 平台身份库 realm.db 与用户自建账套 books/ 都放在 /data（数据卷），
#       容器销毁重建不会丢数据；只监听回环，正式对外必须配 HTTPS 反向代理。
# 平台管理员首次启动自动引导，账号口令见启动日志（可用 FINBOOK_ADMIN_USER/PASS 预置）。

# ── 构建阶段 ──────────────────────────────────────────────
FROM rust:1-bookworm AS builder
WORKDIR /build
# 先拷贝全部 workspace 清单缓存依赖层（cargo 解析 workspace 需要所有成员清单；
# 缺一个成员目录会直接报错，因此这里 5 个 crate 的清单都要有）
COPY Cargo.toml Cargo.lock ./
COPY crates/fincore/Cargo.toml crates/fincore/
COPY crates/findb/Cargo.toml crates/findb/
COPY crates/finui/Cargo.toml crates/finui/
COPY crates/finbook/Cargo.toml crates/finbook/
COPY crates/finweb/Cargo.toml crates/finweb/
# 预取依赖（等价源码占位，让 cargo 缓存依赖编译产物）
RUN mkdir -p crates/fincore/src crates/findb/src crates/finui/src crates/finbook/src crates/finweb/src \
 && echo '' > crates/fincore/src/lib.rs \
 && echo '' > crates/findb/src/lib.rs \
 && echo '' > crates/finui/src/lib.rs \
 && echo 'fn main() {}' > crates/finbook/src/main.rs \
 && echo 'fn main() {}' > crates/finbook/build.rs \
 && echo 'fn main() {}' > crates/finweb/src/main.rs \
 && cargo build --release --locked -p finweb
# 拷贝真实源码并构建。
# 注意：预取层留下的空占位库产物与指纹仍在 target 里，而 COPY 保留源文件
# 的 mtime（可能比产物更旧），cargo 会误判"无需重编"而复用空库/空 main 的
# 编译产物，打出一个假 finweb。因此：
#   1) touch 全部源文件，保证 mtime 新于任何已有产物；
#   2) cargo clean --release 清掉三个 workspace crate 的产物与指纹
#      （不带 --release 只清 dev profile，对 release 产物等于没清）；
# 外部依赖的编译缓存保留，预取仍然有效。
COPY crates/ crates/
RUN find crates -type f \( -name '*.rs' -o -name 'Cargo.toml' -o -name 'build.rs' \) -exec touch {} + \
 && cargo clean --release -p fincore -p findb -p finweb \
 && cargo build --release --locked -p finweb

# ── 运行阶段 ──────────────────────────────────────────────
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --uid 10001 --home-dir /app --shell /usr/sbin/nologin --create-home finbook \
 && mkdir -p /data \
 && chown -R finbook:finbook /data
WORKDIR /app
COPY --from=builder /build/target/release/finweb /usr/local/bin/finweb
# 静态资源（SPA）
COPY --chown=finbook:finbook crates/finweb/static ./static
# 平台身份库 + 账套目录统一挂到 /data 数据卷（重建容器不丢数据）
VOLUME ["/data"]
ENV FINBOOK_REALM=/data/realm.db \
    FINBOOK_BOOKS_DIR=/data/books \
    FINBOOK_LISTEN=0.0.0.0:8080 \
    FINWEB_STATIC_DIR=/app/static \
    FINWEB_SECURE_COOKIE=true \
    FINBOOK_REQUIRE_BOOKS_DIR=true
# 非 root 运行：数据库文件属主为 finbook（bind mount 时请确保宿主机目录 uid=10001 可写）
USER finbook
EXPOSE 8080
# ⚠ 这条 healthcheck 里的 `&&` 链是**必需的**，不要"简化"成
#   `exec 3<>/dev/tcp/127.0.0.1:8080 || exit 1`。
#
# bash 陷阱：当 `exec` 只带重定向、且是 `-c` 字符串里最后一条命令时，bash 会走
# exec 优化（原地 re-exec 自己），此时重定向被当成**普通文件路径**打开，于是报
# `No such file or directory`（ENOENT）—— 而 /dev/tcp 并不是真实文件。
# 后面接 `&& <cmd>` 时不走该优化，netredir 才生效。
#
# 已在生产实测确认两种写法的差别（同一镜像内）：
#   `... && printf ... >&3 && grep -q ok <&3`  → 退出码 0，拿到 HTTP/1.0 200 OK
#   `... || exit 1`                          → 退出码 1，ENOENT，healthcheck 恒失败
#
# 症状极具迷惑性：应用完全正常（/api/health 返回 ok、容器内 TCP 也通、
# readiness 全绿），但 `docker ps` 长期显示 unhealthy 且 FailingStreak 一直涨。
# 恒失败的探针比没有探针更糟 —— 它训练所有人忽略健康信号，真出事时也不会有人看。
#
# 这个镜像刻意不含 curl / wget（见上方 debian-slim 基础镜像），所以只能借
# bash 的 /dev/tcp。更稳的做法是给 finweb 加 `--health` 自检子命令，让
# HEALTHCHECK 直接跑二进制、不依赖 shell 技巧。
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD bash -c 'exec 3<>/dev/tcp/127.0.0.1/8080 && printf "GET /api/health HTTP/1.0\r\n\r\n" >&3 && grep -q ok <&3'
CMD ["finweb"]
