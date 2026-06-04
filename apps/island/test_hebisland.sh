#!/usr/bin/env bash
# ─── hebisland 独立功能测试脚本 ───
#
# 用法: ./test_hebisland.sh <command>
#
# 命令:
#   daemon      启动 hebisland daemon（后台，等 socket 就绪）
#   notify      用 CLI 推送 3 种类型测试通知（肉眼观察 GUI）
#   bidir       双向 socket 测试：建立持久连接 → 发送通知 → 验证 action 回传
#   stack       批量推送 5 条通知，验证右上角堆叠
#   dismiss     推送后立即 dismiss，验证窗口关闭
#   all         一键跑完 daemon → notify → bidir → stack → dismiss（30s 后自动 clean）
#   clean       停止 daemon，清理 socket 文件
#
# 前置: cargo build -p hebisland
# 依赖: Python 3（bidir 测试用）

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
SOCK="$HOME/.hebbian/island.sock"
BIN="$PROJECT_ROOT/target/debug/hebisland"
PID_FILE="$HOME/.hebbian/island.pid"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

info()  { echo -e "${CYAN}[INFO]${NC} $*"; }
ok()    { echo -e "${GREEN}[OK]${NC}   $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC} $*"; }
fail()  { echo -e "${RED}[FAIL]${NC} $*"; }

die()   { fail "$*"; exit 1; }

ensure_bin() {
    if [[ ! -x "$BIN" ]]; then
        info "hebisland 未编译，正在构建..."
        cargo build -p hebisland --manifest-path "$PROJECT_ROOT/Cargo.toml" 2>&1 | tail -3
    fi
    [[ -x "$BIN" ]] || die "构建失败: $BIN"
}

wait_sock() {
    local timeout=10
    local waited=0
    while [[ ! -S "$SOCK" ]]; do
        sleep 0.5
        waited=$((waited + 1))
        if [[ $waited -gt $((timeout * 2)) ]]; then
            die "socket 文件未在 ${timeout}s 内出现: $SOCK"
        fi
    done
    ok "socket 就绪: $SOCK"
}

# ─── daemon ───

cmd_daemon() {
    ensure_bin

    if [[ -f "$PID_FILE" ]] && kill -0 "$(cat "$PID_FILE")" 2>/dev/null; then
        warn "daemon 已在运行 (pid=$(cat "$PID_FILE"))"
        return 0
    fi

    info "启动 hebisland daemon..."
    "$BIN" daemon &
    local pid=$!
    echo "$pid" > "$PID_FILE"

    sleep 1
    if ! kill -0 "$pid" 2>/dev/null; then
        rm -f "$PID_FILE"
        die "daemon 启动后立即退出"
    fi

    wait_sock
    ok "daemon 已启动 (pid=$pid)"
}

# ─── notify（单条推送，肉眼观察） ───

cmd_notify() {
    cmd_daemon

    local id
    local msg

    echo ""
    info "=== info 通知（3s 自动消失） ==="
    id="test-info-$(date +%s)"
    msg='{"type":"show","id":"'"$id"'","card":{"id":"'"$id"'","cardType":"info","title":"测试完成","body":"cargo check --workspace 已通过"}}'
    "$BIN" notify --msg "$msg"
    ok "已推送 info 通知 → $id"
    sleep 4  # 等它自动消失

    echo ""
    info "=== approval 通知（需手动操作） ==="
    id="test-approval-$(date +%s)"
    msg='{"type":"show","id":"'"$id"'","card":{"id":"'"$id"'","cardType":"approval","title":"需要你的审批","body":"Bash 想执行 cargo test --lib","sessionId":"test-session"}}'
    "$BIN" notify --msg "$msg"
    ok "已推送 approval 通知 → $id"
    info "请在通知窗口点击「允许」或「拒绝」→ 验证窗口关闭"

    echo ""
    info "=== question 通知（点击打开） ==="
    id="test-question-$(date +%s)"
    msg='{"type":"show","id":"'"$id"'","card":{"id":"'"$id"'","cardType":"question","title":"需要你的回答","body":"是否继续下一步？","sessionId":"test-session"}}'
    "$BIN" notify --msg "$msg"
    ok "已推送 question 通知 → $id"
    info "请点击卡片 → 窗口应关闭（open action）"

    echo ""
    ok "notify 测试完成。继续观察 GUI 或运行 'clean' 停止 daemon。"
}

# ─── bidir（双向 socket，验证 action 回传） ───

cmd_bidir() {
    cmd_daemon

    echo ""
    info "=== 双向 socket 测试：验证 action 回传 ==="

    local test_id="test-bidir-$(date +%s)"

    python3 - "$SOCK" "$test_id" << 'PYEOF'
import sys, json, socket, select, time

sock_path = sys.argv[1]
test_id = sys.argv[2]

sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
sock.connect(sock_path)

# 写入 show 消息
show_msg = json.dumps({
    "type": "show",
    "id": test_id,
    "card": {
        "id": test_id,
        "cardType": "approval",
        "title": "双向测试",
        "body": "这条通知由 Python socket 直接发送。请在 GUI 点击「允许」。",
        "sessionId": "test-bidir"
    }
}) + "\n"
sock.sendall(show_msg.encode())
print(f"[SENT] {show_msg.strip()}")

# 等待 action 回传（最长 30s）
sock.settimeout(30.0)
buf = b""
try:
    while True:
        data = sock.recv(4096)
        if not data:
            break
        buf += data
        if b"\n" in buf:
            break
except socket.timeout:
    print("[TIMEOUT] 30s 内未收到回传。请在 GUI 点击按钮后重试。")
    sys.exit(1)

action_line = buf.decode().strip()
print(f"[RECV] {action_line}")

try:
    action = json.loads(action_line)
    msg_id = action.get("msg_id", "")
    act = action.get("action", "")

    if msg_id == test_id:
        print(f"[OK] msg_id 匹配: {msg_id}")
        print(f"[OK] action = {act}")
        if act == "allow":
            print("[PASS] 双向 socket 测试通过 ✓")
        else:
            print(f"[INFO] action 是 '{act}'（不是 allow，但回传链路正常）")
    else:
        print(f"[FAIL] msg_id 不匹配: 期望 {test_id}，实际 {msg_id}")
        sys.exit(1)
except json.JSONDecodeError as e:
    print(f"[FAIL] 无法解析回传 JSON: {e}")
    sys.exit(1)

sock.close()
PYEOF

    local rc=$?
    if [[ $rc -eq 0 ]]; then
        ok "bidir 测试通过"
    else
        fail "bidir 测试失败 (exit=$rc)"
    fi
}

# ─── stack（批量推送，验证堆叠） ───

cmd_stack() {
    cmd_daemon

    echo ""
    info "=== 批量推送 5 条通知 ==="

    local base="test-stack-$(date +%s)"
    for i in 1 2 3 4 5; do
        local id="${base}-${i}"
        local msg='{"type":"show","id":"'"$id"'","card":{"id":"'"$id"'","cardType":"approval","title":"堆叠测试 #'"$i"'","body":"这是第 '"$i"' 条通知，应在右上角自上而下排列"}}'
        "$BIN" notify --msg "$msg"
        ok "已推送 #$i → $id"
        sleep 0.3
    done

    info "请验证：右上角 5 条通知自上而下堆叠，间距均匀"
    info "预期顺序：最旧（#1）在上，最新（#5）在下"
}

# ─── dismiss ───

cmd_dismiss() {
    cmd_daemon

    echo ""
    info "=== dismiss 测试 ==="

    local id="test-dismiss-$(date +%s)"

    # 先推送一条 approval 通知
    local msg='{"type":"show","id":"'"$id"'","card":{"id":"'"$id"'","cardType":"approval","title":"即将消失","body":"这条通知将在 2s 后被 dismiss"}}'
    "$BIN" notify --msg "$msg"
    ok "已推送通知 → $id"

    sleep 2

    # 发送 dismiss
    local dismiss_msg='{"type":"dismiss","id":"'"$id"'"}'
    "$BIN" notify --msg "$dismiss_msg"
    ok "已发送 dismiss → $id"
    info "窗口应立即关闭"
}

# ─── all ───

cmd_all() {
    echo ""
    echo -e "${CYAN}══════════════════════════════════════════${NC}"
    echo -e "${CYAN}  hebisland 完整功能测试${NC}"
    echo -e "${CYAN}══════════════════════════════════════════${NC}"
    echo ""

    cmd_notify
    echo ""
    info "=== 继续双向测试（30s 内请在 approval 通知窗口点击「允许」）==="
    sleep 1
    cmd_bidir || true  # bidir 可能因为用户没点按钮超时，不中断后续
    cmd_stack
    cmd_dismiss

    echo ""
    echo -e "${GREEN}══════════════════════════════════════════${NC}"
    echo -e "${GREEN}  全测试完成！${NC}"
    echo -e "${GREEN}══════════════════════════════════════════${NC}"
    echo ""
    ok "daemon 仍在运行。运行 '$0 clean' 停止。"
}

# ─── clean ───

cmd_clean() {
    if [[ -f "$PID_FILE" ]]; then
        local pid
        pid=$(cat "$PID_FILE")
        if kill "$pid" 2>/dev/null; then
            ok "已停止 daemon (pid=$pid)"
        fi
        rm -f "$PID_FILE"
    fi
    rm -f "$SOCK"
    ok "已清理 socket: $SOCK"

    # 确保无残留进程
    pkill -f "hebisland daemon" 2>/dev/null || true
}

# ─── 入口 ───

case "${1:-}" in
    daemon)  cmd_daemon ;;
    notify)  cmd_notify ;;
    bidir)   cmd_bidir ;;
    stack)   cmd_stack ;;
    dismiss) cmd_dismiss ;;
    all)     cmd_all ;;
    clean)   cmd_clean ;;
    *)
        echo "用法: $0 {daemon|notify|bidir|stack|dismiss|all|clean}"
        echo ""
        echo "  daemon   启动 hebisland daemon"
        echo "  notify   推送 3 种类型测试通知（info / approval / question）"
        echo "  bidir    双向 socket 测试（需手动在 GUI 点按钮验证回传）"
        echo "  stack    批量推送 5 条通知验证堆叠"
        echo "  dismiss  推送后 dismiss 验证窗口关闭"
        echo "  all      一键跑完全部测试"
        echo "  clean    停止 daemon + 清理"
        exit 1
        ;;
esac
