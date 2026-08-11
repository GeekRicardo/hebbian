#!/usr/bin/env python3
"""最小 OpenAI 兼容 mock：第一轮返回一个 Bash tool_call（触发真实审批），
之后返回一句普通回答收尾。只为在没有真 provider 的机器上跑通完整链路。"""
import json, os, threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

TARGET = os.environ.get("MOCK_EDIT_FILE", "/root/code/chroma/revert-target.txt")
EDIT_MODE = os.environ.get("MOCK_TOOL") == "edit"
BG_MODE = os.environ.get("MOCK_TOOL") == "bg"
FIRST_TEXT = "我来改一行。" if EDIT_MODE else ("我起个后台任务。" if BG_MODE else "我来清理一下。")
DONE_TEXT = "改好了。" if EDIT_MODE else ("已经在后台跑了。" if BG_MODE else "清理完成了。")

state = {"turn": 0}
lock = threading.Lock()

def sse(chunks):
    out = []
    for c in chunks:
        out.append(f"data: {json.dumps(c)}\n\n")
    out.append("data: [DONE]\n\n")
    return "".join(out).encode()

class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_POST(self):
        n = int(self.headers.get("content-length", 0))
        body = json.loads(self.rfile.read(n) or b"{}")
        streaming = bool(body.get("stream"))
        with lock:
            state["turn"] += 1
            turn = state["turn"]
        # edit 模式要两步：Edit 工具有「先读后写」的守卫（ReadStateTracker），
        # 直接发 Edit 会被拒。所以第一轮先 Read，第二轮才 Edit。
        if EDIT_MODE and turn == 1:
            tc = {"index":0,"id":"call_mock_r","type":"function",
                  "function":{"name":"Read","arguments":json.dumps(
                      {"file_path":TARGET})}}
            if streaming:
                payload = sse([
                  {"choices":[{"index":0,"delta":{"role":"assistant","content":"先看一眼原文。"}}]},
                  {"choices":[{"index":0,"delta":{"tool_calls":[tc]}}]},
                  {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]},
                ])
            else:
                payload = json.dumps({"choices":[{"index":0,"message":{
                    "role":"assistant","content":"先看一眼原文。",
                    "tool_calls":[{"id":"call_mock_r","type":"function",
                        "function":tc["function"]}]},"finish_reason":"tool_calls"}],
                    "usage":{"prompt_tokens":900,"completion_tokens":20}}).encode()
            self.reply(payload, streaming); return

        # 发工具调用那一轮，让 dispatch 走真实的审批 / 编辑链
        if turn == (2 if EDIT_MODE else 1):
            # 默认发 Bash（触发审批链）；`MOCK_TOOL=edit` 时改发 Edit——
            # 用来在没有真模型的机器上造出**真实的** edits-worktree 快照，
            # 好验证「回退这轮」这条只有真跑过才存在的路径。
            if EDIT_MODE:
                tc = {"index":0,"id":"call_mock_1","type":"function",
                      "function":{"name":"Edit","arguments":json.dumps(
                          {"file_path":TARGET,
                           "old_string":"原始内容",
                           "new_string":"被 agent 改过的内容",
                           "description":"改一行做回退测试"})}}
            elif BG_MODE:
                # 起一个会持续吐东西的后台命令，用来验「后台任务」面板：
                # 任务编号 / 停止 / 展开看输出这三样都得有真任务才试得出来。
                tc = {"index":0,"id":"call_mock_1","type":"function",
                      "function":{"name":"Bash","arguments":json.dumps(
                          {"command":"for i in $(seq 1 300); do echo \"tick $i\"; sleep 1; done",
                           "description":"数数用的后台任务",
                           "run_in_background": True})}}
            else:
                tc = {"index":0,"id":"call_mock_1","type":"function",
                      "function":{"name":"Bash","arguments":json.dumps(
                          {"command":"rm -rf /tmp/mock-target && echo done",
                           "description":"清理临时目录"})}}
            if streaming:
                chunks = [
                  {"choices":[{"index":0,"delta":{"role":"assistant","content":FIRST_TEXT}}]},
                  {"choices":[{"index":0,"delta":{"tool_calls":[tc]}}]},
                  {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]},
                ]
                payload = sse(chunks)
            else:
                payload = json.dumps({"choices":[{"index":0,"message":{
                    "role":"assistant","content":FIRST_TEXT,
                    "tool_calls":[{"id":"call_mock_1","type":"function",
                        "function":tc["function"]}]},"finish_reason":"tool_calls"}],
                    "usage":{"prompt_tokens":1200,"completion_tokens":40}}).encode()
        else:
            if streaming:
                payload = sse([
                  {"choices":[{"index":0,"delta":{"role":"assistant","content":DONE_TEXT}}]},
                  {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]},
                ])
            else:
                payload = json.dumps({"choices":[{"index":0,"message":{
                    "role":"assistant","content":DONE_TEXT},"finish_reason":"stop"}],
                    "usage":{"prompt_tokens":1400,"completion_tokens":20}}).encode()
        self.reply(payload, streaming)

    def reply(self, payload, streaming):
        self.send_response(200)
        self.send_header("content-type", "text/event-stream" if streaming else "application/json")
        self.end_headers()
        self.wfile.write(payload)

    def do_GET(self):
        self.send_response(200); self.send_header("content-type","application/json"); self.end_headers()
        self.wfile.write(json.dumps({"data":[{"id":"mock-model"}]}).encode())

ThreadingHTTPServer(("127.0.0.1", 8799), H).serve_forever()
