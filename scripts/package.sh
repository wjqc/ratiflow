#!/bin/sh
# 发布打包（手册 §17.4/§19.1）：构建产物 + SHA-256 校验 + 模块清单（SBOM 基础）。
set -e

cd "$(dirname "$0")/.."
VERSION="${VERSION:-${1:-dev}}"
OUT="dist/release-$VERSION"
mkdir -p "$OUT"

echo "==> 构建 Go 二进制（当前平台）"
go build -trimpath -ldflags "-X main.version=$VERSION" -o "$OUT/sixgates" ./cmd/sixgates

echo "==> 构建 Web 静态资源"
npm run build:web >/dev/null

echo "==> 构建 VS Code 扩展"
npm run build:extension >/dev/null

echo "==> 生成 SHA-256 校验文件"
(
  cd "$OUT"
  shasum -a 256 sixgates > SHA256SUMS
)

echo "==> 生成来源清单（go version -m 基础 SBOM）"
go version -m "$OUT/sixgates" > "$OUT/sbom.txt"

echo "==> 输出清单"
cat "$OUT/SHA256SUMS"
ls -la "$OUT"
echo "打包完成：$OUT"
