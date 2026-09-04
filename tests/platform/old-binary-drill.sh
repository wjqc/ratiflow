#!/usr/bin/env bash
# M5 演练（§16.3 旧包兼容）：用上一发布提交构建的旧 core 打开 v23 库。
# 旧包必须 no-op 跳过已应用的 v23 迁移并正常启动（core.version.schemaVersion=23）。
# 用法：tests/platform/old-binary-drill.sh [旧提交，默认 HEAD]
set -euo pipefail
cd "$(dirname "$0")/../.."

OLD_COMMIT=${1:-HEAD}
NEW_CORE="target/release/sixgates-core"
if [[ ! -x "$NEW_CORE" ]]; then
  echo "先构建新 core：cargo build --release -p sixgates-core" >&2
  exit 1
fi

WORK=$(mktemp -d /tmp/sg-drill-XXXXXX)
trap 'git worktree remove --force "$WORK/src" 2>/dev/null || true; rm -rf "$WORK"' EXIT
echo "[1/4] worktree @ $OLD_COMMIT"
git worktree add --detach "$WORK/src" "$OLD_COMMIT" >/dev/null

echo "[2/4] 构建旧 core（独立 target，约数分钟）..."
(cd "$WORK/src" && CARGO_TARGET_DIR="$WORK/target" cargo build --release -p sixgates-core)
OLD_CORE="$WORK/target/release/sixgates-core"

echo "[3/4] 新 core 建 v23 库并写入记忆数据"
DATA=$(mktemp -d /tmp/sg-drill-data-XXXXXX)
python3 - "$NEW_CORE" "$DATA" << 'PY'
import json, subprocess, sys, time
core, data = sys.argv[1], sys.argv[2]
p = subprocess.Popen([core, 'app-server', '--data-dir', data],
                     stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
nid = [0]
def call(method, params):
    nid[0] += 1
    p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": str(nid[0]), "method": method, "params": params}) + '\n')
    p.stdin.flush()
    while True:
        m = json.loads(p.stdout.readline())
        if m.get('id') == str(nid[0]):
            return m
    time.sleep(0)
call('project.create', {"gitlabInstance": "x", "namespace": "n", "project": "drill", "name": "Drill"})
call('memory.create', {"projectId": call('project.list', {})['result']['items'][0]['id'],
                       "title": "旧包兼容演练", "kind": "fact", "body": "v23 数据由旧包读取。",
                       "idempotencyKey": "drill-1"})
p.kill()
PY

echo "[4/4] 旧 core 打开 v23 库"
python3 - "$OLD_CORE" "$DATA" << 'PY'
import json, subprocess, sys, time
core, data = sys.argv[1], sys.argv[2]
p = subprocess.Popen([core, 'app-server', '--data-dir', data],
                     stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
nid = [0]
def call(method, params):
    nid[0] += 1
    p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": str(nid[0]), "method": method, "params": params}) + '\n')
    p.stdin.flush()
    while True:
        m = json.loads(p.stdout.readline())
        if m.get('id') == str(nid[0]):
            return m
    time.sleep(0)
deadline = time.time() + 15
hello = None
while time.time() < deadline:
    line = p.stdout.readline()
    if not line:
        break
    m = json.loads(line)
    if 'protocolVersion' in m:
        hello = m
        break
assert hello, "旧包未完成 hello 握手"
v = call('core.version', {})
schema = v['result']['schemaVersion']
assert schema == 23, f"schemaVersion 应为 23，实际 {schema}"
# 旧包能读项目（共享域不受 v23 影响）；memory.* 在旧包为未知方法属预期。
projects = call('project.list', {})
assert projects['result']['items'], "旧包应能读共享域数据"
print(f"PASS：旧包 schemaVersion={schema}，共享域可读，启动无迁移冲突")
p.kill()
PY
echo "旧包兼容演练通过（§16.3）"
