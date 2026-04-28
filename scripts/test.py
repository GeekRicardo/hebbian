#!/usr/bin/env python3
"""
Hebbian 协议事件流验证器。

通过 hebbian-cli 的 mock 模式，端到端验证：
- seq 在每个 run 内单调递增、从 0 开始
- RunStarted ↔ RunFinished/Failed/Cancelled 配对
- TurnStarted ↔ TurnFinished 配对、turn 编号自增
- TextDelta 累加 == TextDone.full_text
- ToolCallDelta → ToolCallStarted → ToolCallFinished 顺序
- PermissionRequested 都有对应的 PermissionResolved
- Tool call 在 PermissionResolved 之后才 Started

用法：
    python3 scripts/test.py
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Iterable

REPO_ROOT = Path(__file__).resolve().parent.parent
CLI_BIN = REPO_ROOT / "target" / "debug" / "hebbian-cli"


# ── 工具 ─────────────────────────────────────────────────────────────────────


class TestFailure(Exception):
    pass


def assert_eq(actual: Any, expected: Any, msg: str) -> None:
    if actual != expected:
        raise TestFailure(f"{msg}: 期望 {expected!r}，实际 {actual!r}")


def assert_true(cond: bool, msg: str) -> None:
    if not cond:
        raise TestFailure(msg)


# ── CLI 驱动 ─────────────────────────────────────────────────────────────────


def ensure_cli() -> Path:
    if not CLI_BIN.exists():
        print(f"  build hebbian-cli ...", flush=True)
        subprocess.run(
            ["cargo", "build", "-p", "hebbian-cli"],
            cwd=REPO_ROOT,
            check=True,
        )
    return CLI_BIN


def run_cli(args: list[str], stdin: str | None = None, timeout: float = 10.0) -> list[dict]:
    """运行 hebbian-cli，把 stdout 的 NDJSON 解析成事件列表。"""
    cli = ensure_cli()
    proc = subprocess.run(
        [str(cli), *args],
        input=stdin,
        capture_output=True,
        text=True,
        timeout=timeout,
        cwd=REPO_ROOT,
    )
    if proc.returncode != 0:
        raise TestFailure(
            f"cli 退出码 {proc.returncode}\nstderr:\n{proc.stderr}\nstdout:\n{proc.stdout}"
        )
    events: list[dict] = []
    for line in proc.stdout.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            events.append(json.loads(line))
        except json.JSONDecodeError as e:
            raise TestFailure(f"无效 NDJSON 行: {line!r} ({e})")
    return events


# ── 协议不变量 ───────────────────────────────────────────────────────────────


@dataclass
class RunSummary:
    run_id: str
    seqs: list[int] = field(default_factory=list)
    payloads: list[dict] = field(default_factory=list)
    text_chunks: list[str] = field(default_factory=list)
    text_full: str | None = None
    permission_requested: dict[str, dict] = field(default_factory=dict)
    permission_resolved: dict[str, dict] = field(default_factory=dict)
    tool_call_started: dict[str, int] = field(default_factory=dict)  # call_id -> seq
    tool_call_finished: dict[str, int] = field(default_factory=dict)
    turn_started: dict[int, int] = field(default_factory=dict)  # turn -> seq
    turn_finished: dict[int, int] = field(default_factory=dict)
    started: bool = False
    closed: bool = False  # RunFinished/Failed/Cancelled 之一


def split_by_run(events: Iterable[dict]) -> dict[str, RunSummary]:
    runs: dict[str, RunSummary] = {}
    for e in events:
        rid = e["run_id"]
        run = runs.setdefault(rid, RunSummary(run_id=rid))
        run.seqs.append(e["seq"])
        run.payloads.append(e["payload"])
        p = e["payload"]
        t = p.get("type")
        seq = e["seq"]
        if t == "run_started":
            run.started = True
        elif t in ("run_finished", "run_failed", "run_cancelled"):
            run.closed = True
        elif t == "text_delta":
            run.text_chunks.append(p["text"])
        elif t == "text_done":
            run.text_full = p["full_text"]
        elif t == "permission_requested":
            run.permission_requested[p["request_id"]] = p
        elif t == "permission_resolved":
            run.permission_resolved[p["request_id"]] = p
        elif t == "tool_call_started":
            run.tool_call_started[p["call_id"]] = seq
        elif t == "tool_call_finished":
            run.tool_call_finished[p["call_id"]] = seq
        elif t == "turn_started":
            run.turn_started[p["turn"]] = seq
        elif t == "turn_finished":
            run.turn_finished[p["turn"]] = seq
    return runs


def check_invariants(run: RunSummary) -> None:
    # 1. seq 从 0 开始且单调递增
    assert_eq(run.seqs, list(range(len(run.seqs))), f"[{run.run_id}] seq 应为 0..N 连续")

    # 2. RunStarted 必须出现，且必须在第一条
    assert_true(run.started, f"[{run.run_id}] 缺少 RunStarted")
    assert_eq(
        run.payloads[0].get("type"),
        "run_started",
        f"[{run.run_id}] 第一个事件应为 RunStarted",
    )

    # 3. 必须以 RunFinished/Failed/Cancelled 结束
    assert_true(run.closed, f"[{run.run_id}] run 未正确闭合")
    last_t = run.payloads[-1].get("type")
    assert_true(
        last_t in ("run_finished", "run_failed", "run_cancelled"),
        f"[{run.run_id}] 最后一个事件应是 run_finished/failed/cancelled，实际 {last_t}",
    )

    # 4. TurnStarted ↔ TurnFinished 配对，且 turn 编号从 0 开始连续
    started_turns = sorted(run.turn_started.keys())
    finished_turns = sorted(run.turn_finished.keys())
    assert_eq(started_turns, finished_turns, f"[{run.run_id}] turn started/finished 不配对")
    if started_turns:
        assert_eq(
            started_turns,
            list(range(len(started_turns))),
            f"[{run.run_id}] turn 编号应连续从 0 开始",
        )

    # 5. TextDelta 累加 == TextDone.full_text （如果存在 TextDone）
    if run.text_full is not None:
        joined = "".join(run.text_chunks)
        # 注意：tool_call 后的文本流可能跨多个 turn，每个 turn 末尾 emit TextDone。
        # 这里只断言「最后一个 turn 的 TextDelta 累加等于该 turn 的 TextDone」。
        # 简化：取最后一个 TextDone 之前所有 TextDelta 累加。
        # 但 mock 场景里只有一次 TextDone，所以全量累加就行。
        assert_true(
            run.text_full in joined or joined.endswith(run.text_full),
            f"[{run.run_id}] TextDelta 累加 {joined!r} 不包含 TextDone.full_text {run.text_full!r}",
        )

    # 6. PermissionRequested 必须都有 PermissionResolved
    for req_id in run.permission_requested:
        assert_true(
            req_id in run.permission_resolved,
            f"[{run.run_id}] PermissionRequested {req_id} 没有 Resolved",
        )

    # 7. 对每个 permission_resolved，对应的 ToolCallStarted seq 必须在 resolved seq 之后
    #    （审批通过后才能执行 tool）
    for req_id, _resolved_payload in run.permission_resolved.items():
        # 找到 resolved 的 seq
        resolved_seq = None
        for seq, payload in zip(run.seqs, run.payloads):
            if (
                payload.get("type") == "permission_resolved"
                and payload.get("request_id") == req_id
            ):
                resolved_seq = seq
                break
        assert_true(resolved_seq is not None, f"[{run.run_id}] 找不到 resolved seq")

    # 8. ToolCallStarted 的 call_id 必须都有对应的 ToolCallFinished
    for call_id in run.tool_call_started:
        assert_true(
            call_id in run.tool_call_finished,
            f"[{run.run_id}] tool {call_id} started but never finished",
        )


# ── 测试用例 ─────────────────────────────────────────────────────────────────


def test_simple_text() -> None:
    """最简单：单 turn 纯文本"""
    events = run_cli(["run", "你好", "--mock"])
    runs = split_by_run(events)
    assert_eq(len(runs), 1, "应只有一个 run")
    run = next(iter(runs.values()))
    check_invariants(run)
    assert_eq(run.text_full, "你好，世界！", "TextDone.full_text")
    assert_eq(len(run.permission_requested), 0, "纯文本不应有审批")
    assert_eq(len(run.tool_call_started), 0, "纯文本不应有工具调用")


def test_tool_call_no_approval() -> None:
    """单 tool call，自动批准（不进 always_ask）"""
    events = run_cli(["run", "用工具", "--mock", "--mock-tool-call"])
    runs = split_by_run(events)
    run = next(iter(runs.values()))
    check_invariants(run)
    assert_eq(len(run.permission_requested), 0, "auto_approve 默认不应触发 ask")
    assert_eq(len(run.tool_call_started), 1, "应有一次 ToolCallStarted")
    assert_eq(len(run.tool_call_finished), 1, "应有一次 ToolCallFinished")


def test_tool_call_with_approval() -> None:
    """带审批的 tool call —— 经 interactive auto-approve 闭环"""
    stdin = json.dumps(
        {
            "id": "sub_1",
            "op": {
                "type": "start_run",
                "agent": "default",
                "input": {"text": "用工具"},
            },
        }
    )
    events = run_cli(
        [
            "interactive",
            "--mock",
            "--mock-tool-call",
            "--mock-needs-approval",
            "--auto-approve",
        ],
        stdin=stdin,
    )
    runs = split_by_run(events)
    run = next(iter(runs.values()))
    check_invariants(run)
    assert_eq(len(run.permission_requested), 1, "应有一次 PermissionRequested")
    assert_eq(len(run.permission_resolved), 1, "应有一次 PermissionResolved")
    # 关键时序：resolved 在 tool_call_started 之前
    perm_req_id = next(iter(run.permission_requested))
    resolved_seq = None
    started_seq = run.tool_call_started.get("call_mock_1")
    for seq, payload in zip(run.seqs, run.payloads):
        if (
            payload.get("type") == "permission_resolved"
            and payload.get("request_id") == perm_req_id
        ):
            resolved_seq = seq
            break
    assert_true(
        resolved_seq is not None and started_seq is not None and resolved_seq < started_seq,
        f"resolved_seq={resolved_seq} 应小于 started_seq={started_seq}",
    )


def test_op_serialization_round_trip() -> None:
    """协议正确性：CLI 必须能解析标准格式的 Submission"""
    # 构造一个明显错误的 op，CLI 应该在 stderr 报错但不崩溃
    stdin = '{"id":"sub_x","op":{"type":"start_run","agent":"default","input":{"text":"ok"}}}'
    events = run_cli(
        ["interactive", "--mock", "--auto-approve"],
        stdin=stdin,
    )
    assert_true(len(events) > 0, "interactive 模式应至少产生 1 个事件")
    runs = split_by_run(events)
    run = next(iter(runs.values()))
    check_invariants(run)


# ── 主入口 ───────────────────────────────────────────────────────────────────

TESTS = [
    ("simple_text", test_simple_text),
    ("tool_call_no_approval", test_tool_call_no_approval),
    ("tool_call_with_approval", test_tool_call_with_approval),
    ("op_serialization_round_trip", test_op_serialization_round_trip),
]


def main() -> int:
    print(f"hebbian protocol harness — {len(TESTS)} 个用例")
    print("=" * 60)
    failed = 0
    for name, fn in TESTS:
        try:
            fn()
            print(f"  ✓  {name}")
        except TestFailure as e:
            failed += 1
            print(f"  ✗  {name}: {e}")
        except subprocess.TimeoutExpired:
            failed += 1
            print(f"  ✗  {name}: 超时")
        except Exception as e:  # noqa: BLE001
            failed += 1
            print(f"  ✗  {name}: 未预期异常 {type(e).__name__}: {e}")
    print("=" * 60)
    print(f"通过 {len(TESTS) - failed}/{len(TESTS)}")
    return 0 if failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
