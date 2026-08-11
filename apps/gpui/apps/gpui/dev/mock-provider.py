#!/usr/bin/env python3
"""最小 OpenAI 兼容 mock：第一轮返回一个 Bash tool_call（触发真实审批），
之后返回一句普通回答收尾。只为在没有真 provider 的机器上跑通完整链路。"""
import json, threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

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
        # 第一轮：发一个 Bash 工具调用，让 dispatch 走审批链
        if turn == 1:
            tc = {"index":0,"id":"call_mock_1","type":"function",
                  "function":{"name":"Bash","arguments":json.dumps(
                      {"command":"rm -rf /tmp/mock-target && echo done",
                       "description":"清理临时目录"})}}
            if streaming:
                chunks = [
                  {"choices":[{"index":0,"delta":{"role":"assistant","content":"我来清理一下。"}}]},
                  {"choices":[{"index":0,"delta":{"tool_calls":[tc]}}]},
                  {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]},
                ]
                payload = sse(chunks)
            else:
                payload = json.dumps({"choices":[{"index":0,"message":{
                    "role":"assistant","content":"我来清理一下。",
                    "tool_calls":[{"id":"call_mock_1","type":"function",
                        "function":tc["function"]}]},"finish_reason":"tool_calls"}],
                    "usage":{"prompt_tokens":1200,"completion_tokens":40}}).encode()
        else:
            if streaming:
                payload = sse([
                  {"choices":[{"index":0,"delta":{"role":"assistant","content":"清理完成了。"}}]},
                  {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]},
                ])
            else:
                payload = json.dumps({"choices":[{"index":0,"message":{
                    "role":"assistant","content":"清理完成了。"},"finish_reason":"stop"}],
                    "usage":{"prompt_tokens":1400,"completion_tokens":20}}).encode()
        self.send_response(200)
        self.send_header("content-type", "text/event-stream" if streaming else "application/json")
        self.end_headers()
        self.wfile.write(payload)
    def do_GET(self):
        self.send_response(200); self.send_header("content-type","application/json"); self.end_headers()
        self.wfile.write(json.dumps({"data":[{"id":"mock-model"}]}).encode())

ThreadingHTTPServer(("127.0.0.1", 8799), H).serve_forever()
