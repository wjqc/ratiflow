#!/bin/sh
# 架构依赖检查（手册 §8.2、§17.1）：
# 业务包不得依赖 api、具体 HTTP 层；模型/GitLab SDK 只能出现在 integrations 适配器内。
set -e

cd "$(dirname "$0")/.."
VIOLATIONS=0

check() {
  dir="$1"; pattern="$2"; message="$3"
  if grep -rn --include='*.go' "$pattern" "$dir" >/dev/null 2>&1; then
    echo "ARCH VIOLATION [$dir]: $message"
    grep -rn --include='*.go' "$pattern" "$dir" | head -5
    VIOLATIONS=$((VIOLATIONS+1))
  fi
}

# 业务包禁止依赖 api 层。
for pkg in auth workitem artifact workflow gate agent modelgw policy trace evidence delivery executor store events audit; do
  check "internal/$pkg" 'sixgates/internal/api' "业务包 $pkg 不得依赖 api 层"
done

# 模型 SDK 边界：Complete/ChatMessage 等供应商类型只能出现在 integrations 与 modelgw/agent 的接口参数中。
# 直接约束：integrations 之外的包不得 import integrations 的 HTTP 适配器构造函数。
for pkg in workitem artifact workflow gate policy trace evidence delivery store events audit auth; do
  check "internal/$pkg" 'NewGitLabHTTP\|NewModelHTTP\|SSHExec' "业务包 $pkg 不得引用具体适配器实现"
done

# store 不得包含六关业务判断。
check "internal/store" 'GateResult\|六关\|gate\.' "store 包不得包含门禁业务判断"

if [ "$VIOLATIONS" -gt 0 ]; then
  echo "发现 $VIOLATIONS 处架构违规。"
  exit 1
fi
echo "架构依赖检查通过。"
