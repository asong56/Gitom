# Gitom

> **Self Deploy · No Social · Pure Git · Single Binary Executable · Single User**

Gitom is an ultra-lightweight, minimalist, **single-user self-hosted Git server**. Stripped of bloated social features, it focuses on providing a clean, secure, and out-of-the-box private Git remote repository with an extremely low resource footprint.

---

## ✨ Features

* 📦 **All-in-One Binary**: Compiles down to a single, standalone executable with zero external dynamic library dependencies (the Linux version uses fully static `musl` linking).
* 👤 **Exclusive Single-User**: Purpose-built for individual developers. No complex organization hierarchies, no social noise—just pure productivity.
* 🗄️ **Embedded SQLite**: Utilizes a lightweight, high-performance SQLite database via SQLx to manage metadata and logs, eliminating the need to spin up separate MySQL or PostgreSQL instances.
* 🔒 **Modern Security**: Uses `Argon2` for secure password hashing and `JWT` (JSON Web Tokens) for session management.
* 🪝 **Event Hooks**: Built-in webhook delivery queue allows automatic notifications to external services on actions like Git push, catering to your self-deployment needs.
* 🚀 **Highly Optimized**: Fine-tuned via `LTO (Link-Time Optimization)`, binary stripping, and panic-abort behavior to ensure lightning-fast speeds and microscopic memory usage.

---

## 🛠️ Tech Stack

* **Core**: Rust 2021 (Axum + Tokio async runtime)
* **Database**: SQLx + SQLite
* **Template Engine**: MiniJinja (a lightweight Jinja2 implementation in Rust)
* **Security**: Argon2 + jsonwebtoken
* **Git Core**: git2-rs
* **Assets**: rust-embed (assets are baked directly into the binary)

---

## 🚀 Quick Start

### 1. Build from Source

Cross-platform compilation is supported out of the box. Ensure you have the Rust toolchain installed locally:

```bash
# Build the optimized release version for your local platform
cargo build --release

```

The compiled binary will be located at `target/release/gitom`.

### 2. Running & Configuration

Gitom is configured via environment variables. Before launching, you need to generate a secure JWT secret:

```bash
# Generate and export the JWT secret
export GITOM_JWT_SECRET=$(openssl rand -hex 32)

# Set your initial Gitom password
export GITOM_PASSWORD="your-secure-password"

# (Optional) Enable JSON-formatted logging
export GITOM_LOG_JSON=false

# Spin up the server
./gitom

```

The service will default to listening on `http://0.0.0.0:3000`.

---

## 📦 Cross-Platform Distribution

The project uses GitHub Actions for automated CI/CD. When you push a version tag (e.g., `v1.0.0`), binaries for the following platforms are automatically built and attached to the GitHub Release:

* **Linux (x86_64)**: `gitom-linux-musl-amd64.tar.gz` (statically linked; runs on any Linux distribution or scratch Docker images)
* **Windows (x86_64)**: `gitom-windows-amd64.zip`
* **macOS (Apple Silicon)**: `gitom-macos-aarch64.tar.gz`

---

## 📄 License

This project is licensed under the **BlueOak-1.0.0** License.