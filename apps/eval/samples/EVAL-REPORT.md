# SWE-bench Verified 真实评测报告（2026-06-21）

用 `heb-eval` 跑 SWE-bench Verified 数据集的真实任务（DeepSWE 评测用的同一任务集），
agent 模型 deepseek-v4-pro。这是真实拉取数据集 + 真实跑 + 结果分析的记录。

## 任务集

5 条 pytest 任务（`<15 min fix` 难度，覆盖 pytest v4.5~v6.2），用
`scripts/fetch_swebench.py` 从 HuggingFace `princeton-nlp/SWE-bench_Verified` 拉取：

| instance | pytest 版本 | FAIL_TO_PASS 数 |
|---|---|---|
| pytest-dev__pytest-5262 | v4.5 | 1 |
| pytest-dev__pytest-5809 | v4.6 | 1 |
| pytest-dev__pytest-6202 | v5.2 | 1 |
| pytest-dev__pytest-7205 | v5.4 | 10 |
| pytest-dev__pytest-7982 | v6.2 | 1 |

## 结果数据

| instance | 并发跑(c=2) | 串行跑(c=1) | agent 是否调 Edit |
|---|---|---|---|
| 5262 | ✗ FAIL | ✓ PASS | 串行有(Read×2 Grep×3 Edit×1)，并发无 |
| 5809 | ✓ PASS | ✓ PASS | 有(Read×1 Edit×1) |
| 6202 | ✗ FAIL | ✗ FAIL | 无（探索后没改） |
| 7205 | ✗ FAIL | ✓ PASS | 串行有(Read×2 Grep×3 Edit×2)，并发无 |
| 7982 | ✗ FAIL | ✗ FAIL | 无（只跑 Bash 研究 git 历史） |
| **通过率** | **1/5 = 20%** | **3/5 = 60%** | |

判分口径：clone repo@base_commit → 建 venv 装依赖 → `heb run --yolo` 让 agent 修 →
git apply 隐藏测试 → 跑 FAIL_TO_PASS 全转 pass + PASS_TO_PASS 无回归才算 PASS。

## 结果分析：是 bug 还是效果不好？

**结论：既不是 hebbian 框架的 bug，也不是 agent 改错代码——3 个失败里 2 个是「agent 探索后
没进行到修复阶段就停了」（模型行为），1.x 个是「单次运行方差」。**

### 1. 三个 PASS 的修复质量：与官方 gold patch 逻辑完全一致

- **5262**（capture.py）：agent 给 `EncodedFile` 加 `mode` property 去掉 `b` 二进制标志
  → 与官方 gold patch 一字不差。
- **5809**（pastebin.py）：agent 把 bpaste 的 `lexer` 从 `"python3"` 改成 `"text"`
  → 与官方修复一致（pytest #5806：bpaste 不再支持 python3 lexer 致 HTTP 400）。
- **7205**（setuponly.py）：agent 引入 `saferepr` 安全表示 fixture 参数
  → 与官方修复思路一致，10 个 FAIL_TO_PASS 测试全过。

agent 真实读代码、定位、改对了——证明 agent + yolo + heb run 的执行链路完全跑通。

### 2. 两个 FAIL（6202 / 7982）：模型探索后过早 Stop，没改代码

逐条看 model_io.jsonl（工具调用在 `response.calls`，类型 `ToolCalls`）：

- **7982**：agent 用 Bash 跑 `git log`/`git show`/`git diff` 研究引入 bug 的 commit
  （#0~#2），研究完后**模型返回空 Done**（finish=Stop, text=""），没进入 Edit 阶段。
  agent loop **正确重试了 3 次**（#4~#6），但模型每次都空 Done，最终放弃。
- **6202**：agent 用 Skill/codegraph/Read/Bash 探索，同样在探索后没产出 Edit。

这是 **agent 效果问题**（deepseek-v4-pro 在这两条任务上探索后过早收尾），不是框架缺陷。
hebbian 的重试机制工作正常，只是模型持续不产出修复。

### 3. 单次运行方差：并发 20% vs 串行 60%

同一套任务、同一 provider、同一 agent，仅运行批次不同：**5262 和 7205 在并发批次没改代码
(FAIL)，串行批次改对了(PASS)**。这不是限流（两批的工具调用都正常返回、无 HTTP Error），
而是 **LLM agent 的本质方差**——同一任务多跑几次，模型有时探索后停、有时坚持到修复。
所以严肃评测需要 pass@k 多次采样，单次结果不可靠。

> 排查教训：分析时一度把 `response.calls` 误当 `tool_calls` 找，得出"全是空响应限流"的
> 错误结论。后用正确字段（`type:"ToolCalls"` + `calls[]`）复核才看清真相。usage 的
> `output_tokens=0` 是该 provider 中转层不回传 usage，**不代表空响应**。

## 复现

```bash
python3 scripts/fetch_swebench.py --instance pytest-dev__pytest-5262 --out suite.json
# 编辑 suite.json 的 setup_cmd 为兼容老 pytest 的版本（见 README）
heb-eval run --suite suite.json --provider <id> --heb-bin ./target/debug/heb \
  --timeout 900 --concurrency 1 --work-root /tmp/swebench-run --out report.json
```

原始报告数据见 `RESULTS-concurrent.json` / `RESULTS-serial.json`。
