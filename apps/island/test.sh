#!/bin/bash
# hebisland 独立测试脚本
#
# 覆盖：
#   1. daemon 启动 → socket 文件确认
#   2. notify --msg 单次推送（info / approval / question）
#   3. socket 长连接 → show → 等待前端 UI 触发 → 接收 action 回传
#   4. dismiss 推送
#   5. 重复 ID 视为 update
#   6. daemon 未运行时 notify 报错
#   7. 多连接并发
#
# 依赖：bash + python3 + cargo（hebisland 二进制）
#       前端渲染需手动目视确认（非 headless）

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
HEBISLAND_BIN="$PROJECT_ROOT/target/debug/hebisland"
SOCK="$HOME/.hebbian/island.sock"

PASS=0
FAIL=0
DAEMON_PID=""

# ---- 工具函数 ----

red()    { printf '\033[31m%s\033[0m' "$*"; }
green()  { printf '\033[32m%s\033[0m' "$*"; }
yellow() { printf '\033[33m%s\033[0m' "$*"; }
blue()   { printf '\033[34m%s\033[0m' "$*"; }

pass() { echo "  $(green PASS): $*"; PASS=$((PASS + 1)); }
fail() { echo "  $(red FAIL): $*"; FAIL=$((FAIL + 1)); }

cleanup() {
    echo ""
    echo "=== 清理 ==="
    if [ -n "$DAEMON_PID" ] && kill -0 "$DAEMON_PID" 2>/dev/null; then
        echo "  终止 daemon (PID $DAEMON_PID)..."
        kill "$DAEMON_PID" 2>/dev/null || true
        wait "$DAEMON_PID" 2>/dev/null || true
    fi
    rm -f "$SOCK"
}

trap cleanup EXIT

# Python socket client —— 发送一行 JSON 到 island.sock，读取一行响应。
# 用法: sock_send <json> [timeout_secs=3]
sock_send() {
    local json="$1"
    local timeout="${2:-3}"
    python3 -c "
import socket, json, sys, os, time
sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
try:
    sock.connect(os.path.expanduser('$SOCK'))
    sock.send(json.dumps($json).encode() + b'\n')
    sock.settimeout($timeout)
    try:
        resp = sock.recv(4096)
        print(resp.decode().strip())
    except socket.timeout:
        print('__TIMEOUT__')
finally:
    sock.close()
"
}

# Python socket 长连接 —— 发送 show → 等待 action 回传 → 打印
# 用法: sock_show_and_wait <id> <card_json> [timeout_secs=10]
sock_show_and_wait() {
    local id="$1"
    local card_json="$2"
    local timeout="${3:-10}"
    python3 -c "
import socket, json, sys, os, time

sock_path = os.path.expanduser('$SOCK')
msg = json.dumps({'type': 'show', 'id': '$id', 'card': json.loads('''$card_json''')})

sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
sock.connect(sock_path)
sock.send(msg.encode() + b'\n')

print('__SENT__: ' + msg)

sock.settimeout($timeout)
try:
    resp = sock.recv(4096)
    print('__RECV__: ' + resp.decode().strip())
except socket.timeout:
    print('__TIMEOUT__ (未在 ${timeout}s 内收到回传——请在 hebisland 窗口点按钮)')
finally:
    sock.close()
"
}

# ---- 编译 ----

echo "=== $(blue 编译) ==="
cd "$PROJECT_ROOT"
cargo build -p hebisland 2>&1 | tail -1
echo ""

# ---- 测试 0: daemon 未运行时报错 ----

echo "=== $(blue 测试 0): daemon 未运行时 notify 报错 ==="
rm -f "$SOCK"
if "$HEBISLAND_BIN" notify --msg '{"type":"show","id":"t0","card":{"id":"t0","cardType":"info","title":"x","body":"x"}}' 2>&1 | grep -q "无法连接"; then
    pass "daemon 未运行时 notify 正确报错"
else
    fail "daemon 未运行时 notify 应报错"
fi
echo ""

# ---- 启动 daemon ----

echo "=== $(blue 启动 daemon) ==="
rm -f "$SOCK"
"$HEBISLAND_BIN" daemon &
DAEMON_PID=$!

# 等待 socket 就绪（最多 10s）
for i in $(seq 1 20); do
    if [ -S "$SOCK" ]; then
        echo "  socket 就绪: $SOCK (${i}x0.5s)"
        break
    fi
    sleep 0.5
done

if [ ! -S "$SOCK" ]; then
    fail "daemon 启动后 socket 未出现"
    exit 1
fi
pass "daemon 启动 → socket 文件创建"
echo ""

# ---- 测试 1: info 通知 ----

echo "=== $(blue 测试 1): 单次 info 通知推送 (notify --msg) ==="
OUT=$("$HEBISLAND_BIN" notify --msg '{"type":"show","id":"t1-info","card":{"id":"t1-info","cardType":"info","title":"编译完成","body":"cargo check 已通过"}}' 2>&1)
if echo "$OUT" | grep -q "ok"; then
    pass "info 通知推送成功"
else
    fail "info 通知推送失败: $OUT"
fi
echo ""

# ---- 测试 2: approval 通知 ----

echo "=== $(blue 测试 2): 审批通知推送 (notify --msg) ==="
OUT=$("$HEBISLAND_BIN" notify --msg '{"type":"show","id":"t2-approval","card":{"id":"t2-approval","cardType":"approval","title":"需要审批","body":"Bash 想执行 cargo check --workspace","sessionId":"abc"}}' 2>&1)
if echo "$OUT" | grep -q "ok"; then
    pass "审批通知推送成功"
else
    fail "审批通知推送失败: $OUT"
fi
echo ""

# ---- 测试 3: question 通知 ----

echo "=== $(blue 测试 3): 问题通知推送 (notify --msg) ==="
OUT=$("$HEBISLAND_BIN" notify --msg '{"type":"show","id":"t3-question","card":{"id":"t3-question","cardType":"question","title":"需要你的回答","body":"请确认是否继续执行后续步骤？"}}' 2>&1)
if echo "$OUT" | grep -q "ok"; then
    pass "问题通知推送成功"
else
    fail "问题通知推送失败: $OUT"
fi
echo ""

# ---- 测试 4: 重复 ID → update ----

echo "=== $(blue 测试 4): 重复 ID 视为 update ==="
# 用 info 通知测试
"$HEBISLAND_BIN" notify --msg '{"type":"show","id":"t4-dup","card":{"id":"t4-dup","cardType":"info","title":"第一次","body":"这是第一版内容"}}' >/dev/null 2>&1
sleep 0.3
"$HEBISLAND_BIN" notify --msg '{"type":"show","id":"t4-dup","card":{"id":"t4-dup","cardType":"info","title":"第二次","body":"这是更新后的内容"}}' >/dev/null 2>&1
# 这个测试主要靠目视确认；shell 层面验证两次都返回 ok
OUT2=$("$HEBISLAND_BIN" notify --msg '{"type":"show","id":"t4-dup","card":{"id":"t4-dup","cardType":"info","title":"第三次","body":"最终版本"}}' 2>&1)
if echo "$OUT2" | grep -q "ok"; then
    pass "重复 ID 推送三次均返回 ok（前端应更新而非叠加）"
else
    fail "重复 ID 推送失败: $OUT2"
fi
echo ""

# ---- 测试 5: dismiss 通知 ----

echo "=== $(blue 测试 5): dismiss 关闭通知 ==="
# 先创建一个通知
"$HEBISLAND_BIN" notify --msg '{"type":"show","id":"t5-dismiss","card":{"id":"t5-dismiss","cardType":"info","title":"即将消失","body":"这条通知会在 1s 后被 dismiss"}}' >/dev/null 2>&1
sleep 0.5
# 发送 dismiss
OUT=$("$HEBISLAND_BIN" notify --msg '{"type":"dismiss","id":"t5-dismiss"}' 2>&1)
if echo "$OUT" | grep -q "ok"; then
    pass "dismiss 推送成功"
else
    fail "dismiss 推送失败: $OUT"
fi
echo ""

# ---- 测试 6: socket 长连接 → 双向通信 ----

echo "=== $(blue 测试 6): socket 长连接双向通信 ==="
echo "  $(yellow ⚠ 此测试需要你手动操作 hebisland 窗口)"
echo "  $(yellow   等待出现 '审批测试-长连接' 卡片，点击 [允许] 按钮)"
echo ""

# 用 python 建长连接，发 show，等待 button 点击后的 action 回传
RESULT=$(sock_show_and_wait "t6-bidi" '{"id":"t6-bidi","cardType":"approval","title":"审批测试-长连接","body":"请点击允许或拒绝按钮来测试 action 回传","sessionId":"test-session"}' 25)

echo "$RESULT"

if echo "$RESULT" | grep -q '"allow"'; then
    pass "action 回传收到 allow"
elif echo "$RESULT" | grep -q '"deny"'; then
    pass "action 回传收到 deny"
elif echo "$RESULT" | grep -q '__TIMEOUT__'; then
    echo "  $(yellow → 超时未收到回传；如果你点了按钮但没收到，检查 hebisland 日志)"
else
    echo "  $(yellow → 未收到预期的 action 回传)"
fi
echo ""

# ---- 测试 7: 多连接并发 ----

echo "=== $(blue 测试 7): 多连接并发推送 ==="
for i in $(seq 1 5); do
    "$HEBISLAND_BIN" notify --msg "{\"type\":\"show\",\"id\":\"t7-concurrent-$i\",\"card\":{\"id\":\"t7-concurrent-$i\",\"cardType\":\"info\",\"title\":\"并发 #$i\",\"body\":\"并发推送测试 $i/5\"}}" >/dev/null 2>&1 &
done
wait
# 给一点时间让前端渲染
sleep 1
# 查 socket 上有没有残留问题（daemon 仍在运行即可）
if kill -0 "$DAEMON_PID" 2>/dev/null; then
    pass "5 条并发 notify → daemon 未崩溃"
else
    fail "并发推送后 daemon 崩溃"
fi
echo ""

# ---- 汇总 ----

echo "=============================="
echo "  通过: $(green "$PASS")  |  失败: $(red "$FAIL")"
echo "=============================="

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
