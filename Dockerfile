# =============================================================================
# Gitom — 多阶段构建
# 修复：
#   - P2-17: 运行时使用非 root 用户
#   - P2-17: 复制 Cargo.lock 确保依赖版本锁定
#   - P2-17: 使用 git2 vendored feature，避免运行时 libgit2 版本不匹配
#   - P2-17: 移除 `|| true` 掩盖构建失败的问题
# =============================================================================

FROM rust:1.78-bookworm AS builder

# vendored 模式需要 cmake 和 libssl-dev
RUN apt-get update && apt-get install -y --no-install-recommends \
    cmake pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# 先复制 Cargo 文件（含 Cargo.lock）缓存依赖层
# P2-17 修复：必须同时复制 Cargo.lock，否则每次都重新解析依赖版本
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && echo 'fn main(){}' > src/main.rs \
    && cargo build --release \
    && rm -rf src target/release/.fingerprint/gitom-*

# 复制全部源码（模板和资源也在此阶段，rust-embed 编译时嵌入）
COPY . .
RUN cargo build --release

# =============================================================================
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    git ca-certificates && rm -rf /var/lib/apt/lists/*
# P2-17: git2 vendored 模式已静态链接 libgit2，运行时无需安装 libgit2

# P2-17: 创建非 root 运行用户
RUN groupadd -r gitom && useradd -r -g gitom -d /data -s /sbin/nologin gitom

COPY --from=builder /build/target/release/gitom /usr/local/bin/gitom

# 创建数据目录并赋权
RUN mkdir -p /data && chown gitom:gitom /data

USER gitom

VOLUME /data
EXPOSE 3000

ENV GITOM_DATA_DIR=/data
ENV GITOM_LISTEN=0.0.0.0:3000

ENTRYPOINT ["/usr/local/bin/gitom"]
