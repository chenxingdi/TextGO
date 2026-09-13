#!/usr/bin/env bash
#
# 在本机构建 TextGO 安装包（git-bash / WSL / macOS / Linux 通用）。
#
# 本机把 Rust 工具链与构建缓存放在 E 盘，避免占用空间紧张的 C 盘；
# updater 签名密钥与密码从用户目录读取，脚本本身不含任何密钥内容。
#
# 可用环境变量覆盖默认值：
#   RUSTUP_HOME / CARGO_HOME / CARGO_TARGET_DIR
#   TAURI_SIGNING_PRIVATE_KEY / TAURI_SIGNING_PRIVATE_KEY_PASSWORD
#   TAURI_SIGNING_KEY_FILE / TAURI_SIGNING_KEY_PASSWORD_FILE
#
# 用法：
#   bash scripts/build-local.sh            # 构建当前平台的安装包
#   bash scripts/build-local.sh --help     # 其余参数原样传给 tauri build
#
set -euo pipefail

# Rust 工具链位置
export RUSTUP_HOME="${RUSTUP_HOME:-E:/rustup}"
export CARGO_HOME="${CARGO_HOME:-E:/cargo}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-E:/textgo-target}"
export PATH="$CARGO_HOME/bin:$PATH"

# updater 签名密钥与密码
key_file="${TAURI_SIGNING_KEY_FILE:-${USERPROFILE:-$HOME}/.tauri/textgo.key}"
password_file="${TAURI_SIGNING_KEY_PASSWORD_FILE:-${USERPROFILE:-$HOME}/.tauri/textgo.key.password.txt}"

if [ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]; then
  if [ ! -f "$key_file" ]; then
    echo "找不到 updater 私钥文件：$key_file" >&2
    echo "可用 TAURI_SIGNING_PRIVATE_KEY 直接指定密钥内容或路径。" >&2
    exit 1
  fi
  export TAURI_SIGNING_PRIVATE_KEY="$key_file"
fi

if [ -z "${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}" ]; then
  if [ ! -f "$password_file" ]; then
    echo "找不到 updater 私钥密码文件：$password_file" >&2
    echo "可用 TAURI_SIGNING_PRIVATE_KEY_PASSWORD 直接指定密码。" >&2
    exit 1
  fi
  export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$(cat "$password_file")"
fi

# 切到项目根目录
cd "$(dirname "${BASH_SOURCE[0]}")/.."

echo "RUSTUP_HOME          = $RUSTUP_HOME"
echo "CARGO_HOME           = $CARGO_HOME"
echo "CARGO_TARGET_DIR     = $CARGO_TARGET_DIR"
echo "updater 私钥         = $TAURI_SIGNING_PRIVATE_KEY"
echo "updater 私钥密码     = 已载入（${#TAURI_SIGNING_PRIVATE_KEY_PASSWORD} 字符）"
echo

exec pnpm tauri build "$@"
