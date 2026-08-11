# 本地调试辅助

## `mock-provider.py` —— 不用真 API Key 也能跑通整条链路

一个最小的 OpenAI 兼容 mock。**它的价值不是"省钱"，而是让「发消息 → 工具调用 →
审批弹窗 → 工具执行 → 收尾」这条链路可以在没有任何 provider 凭证的机器上跑通**——
审批卡片、流式渲染、上下文占用这些只有真实 run 才会出现的东西，否则根本验不了。

```bash
python3 apps/gpui/dev/mock-provider.py     # 监听 127.0.0.1:8799
```

然后在 `~/.hebbian/providers.json` 里加一个指过去的 provider：

```json
{ "id": "mock", "name": "Mock", "kind": "openai", "enabled": true,
  "base_url": "http://127.0.0.1:8799/v1", "api_key": "sk-mock",
  "models": ["mock-model"], "default_model": "mock-model" }
```

行为：对话里**还没有工具结果**时返回一个 `Bash` 工具调用（命令刻意包含 `rm`，
用来触发「不可记忆」段），已经有工具结果就返回一句收尾文本。

**用「对话里有没有工具结果」判定而不是数轮次**：标题生成等旁路调用也会打到同一个
端点，用轮次计数会被它们打乱（踩过这个坑，表现为第一轮直接返回收尾文本、工具调用
永远不出现）。

### 与 `apps/cli/src/mock_provider.rs` 的区别

仓库里已经有一个 `MockClient`（heb CLI 的 `--mock`），但它**只吐固定文本、不调用任何
工具**，所以触发不了审批 / 编辑这些链路。这个 HTTP mock 是补它够不着的那一段：

- 它是**真的 HTTP provider**，通过 `providers.json` 生效，三个 surface 都能用，不用改代码；
- 能发工具调用：默认 `Bash`（触发审批），`MOCK_TOOL=edit` 时发 `Edit`（造真实的
  edits-worktree 快照，用来验「回退这轮」）。

```bash
MOCK_TOOL=edit python3 apps/gpui/dev/mock-provider.py
```
