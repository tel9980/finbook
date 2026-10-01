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
    FINWEB_STATIC_DIR=/app/static
# 非 root 运行：数据库文件属主为 finbook（bind mount 时请确保宿主机目录 uid=10001 可写）
USER finbook
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD bash -c 'exec 3<>/dev/tcp/127.0.0.1/8080 || exit 1'
CMD ["finweb"]
