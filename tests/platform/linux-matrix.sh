#!/usr/bin/env bash
# 三平台故障注入矩阵 · Linux 行一键执行（ADR-030 M3 证据 / 蓝图 §13.4）。
# 在 linux/arm64 或 linux/amd64 容器内跑真实 Rust 全量单测 + 关卡回滚故障注入 e2e。
# 用法：
#   tests/platform/linux-matrix.sh [镜像]
# 参数：
#   镜像  默认 rust:1-slim-bookworm；Docker Hub 不可达时可传镜像源，
#          例：tests/platform/linux-matrix.sh docker.1ms.run/library/rust:1-slim-bookworm
# 说明：Windows 行无法本机模拟（CI windows runner 承担）；macOS 行直接跑本机套件。
set -euo pipefail

IMAGE="${1:-rust:1-slim-bookworm}"
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"

docker pull "$IMAGE"
docker run --rm \
  -v "$REPO_ROOT":/work -w /work \
  -e CARGO_TARGET_DIR=/tmp/target-linux \
  "$IMAGE" \
  bash -c '
set -e
echo "=== container: $(uname -s) $(uname -m) ==="
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq >/dev/null 2>&1 || {
  echo "deb http://mirrors.tuna.tsinghua.edu.cn/debian bookworm main" > /etc/apt/sources.list
  apt-get update -qq >/dev/null 2>&1
}
apt-get install -y -qq git nodejs >/dev/null 2>&1
echo "git $(git --version | cut -d\" \" -f3), node $(node --version)"
git config --global --add safe.directory /work
echo "=== cargo test --workspace (Linux) ==="
cargo test --workspace 2>&1 | grep -E "^test result" | awk "{p+=\$4; f+=\$6} END {print \"LINUX_RUST_PASSED:\", p, \"FAILED:\", f}"
echo "=== debug build ==="
cargo build 2>&1 | tail -1
FAILED=0
for f in rollback-e2e gate-release-race-e2e trace-e2e agent-routing-e2e e2e; do
  printf "%-24s " "$f"
  if CORE_BIN=/tmp/target-linux/debug/ratiflow-core node tests/e2e-protocol/$f.mjs >/tmp/$f.linux.log 2>&1; then
    echo PASS
  else
    echo FAIL; tail -3 /tmp/$f.linux.log; FAILED=1
  fi
done
[ "$FAILED" -eq 0 ] && echo "LINUX MATRIX: ALL GREEN" || { echo "LINUX MATRIX: FAILED"; exit 1; }
'
