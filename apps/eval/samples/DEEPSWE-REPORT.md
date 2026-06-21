# DeepSWE / R2E-Gym 真实评测报告（2026-06-21）

用 `heb-eval` 跑 **DeepSWE 实际使用的数据集 R2E-Gym**（官方 docker 镜像 + 官方判分），
得出有证据的「是 bug 还是效果不好」结论。

## 背景：DeepSWE 用的是 R2E-Gym，不是 SWE-bench

[DeepSWE](https://www.together.ai/blog/deepswe)（Agentica/Together AI）在
[R2E-Gym](https://huggingface.co/datasets/R2E-Gym/R2E-Gym-Subset) 数据集上训练 + 评测。
R2E-Gym 的每个任务自带**预构建 docker 镜像**（环境装好、`/testbed` 是 base commit 状态、
`/r2e_tests` 是隐藏测试），`expected_output_json` 是判分标准（测试名→PASSED/FAILED）——
这正是 R2E-Gym 用 Docker 解决「环境矩阵」难点的方式。

heb-eval 为此新增 `r2e` 任务类型（docker 模式）：
导出容器 `/testbed` → `heb run --yolo` 让 agent 改 → 改动 cp 回容器 → 跑官方 `run_tests.sh`
（`xvfb-run pytest -rA /r2e_tests`）→ 解析 PASSED/FAILED 与 expected 比对。

## 真实运行结果

任务 `orange3-2d9617bd`（R2E-Gym-Subset，orange3 仓库，issue: context migration 不移除
不兼容 context），agent 模型 deepseek-v4-pro：

| 阶段 | 结果 | 证据 |
|---|---|---|
| **base 状态**（未修） | 9 passed / **1 FAILED** | 目标测试 `test_migrates_settings_removes_incompatible` 抛 `IncompatibleContext` |
| **agent 修复后** | 9 passed / **1 FAILED**（不变）| agent 只 Grep×4 + Read×4 探索，**0 次 Edit**，没改代码 |
| **官方 gold patch** | **10 passed**（全过）| 证明正确修复可达、判分链路正确 |

判分输出：`9/10 吻合；不符 1 项：test_migrates_settings_removes_incompatible: 期望PASSED 实际FAILED`

## 结论：是 bug 还是效果不好？——是「agent 效果不好」，不是框架 bug

**三条独立证据锁定结论：**

### 证据 1：判分链路正确（排除框架 bug）

应用官方 gold patch（覆盖 `Orange/widgets/settings.py` 等 3 个文件）后，10 个测试**全部
PASSED**。说明 heb-eval 的 docker 判分链路——导出/改/回写/跑测试/解析比对——完全正确，
**PASS 是可达的，FAIL 不是判分 bug**。

### 证据 2：agent 探索后没产出修复（定位到效果问题）

逐条看 model_io.jsonl（工具调用在 `response.type=="ToolCalls"` 的 `calls[]`）：
- `#0~#3`：agent Grep×4 + Read×4 探索代码，正常
- `#4`：**模型返回空 Done（finish=Stop, text=""）**——探索后过早收尾，没进入 Edit 阶段
- `#5`：agent loop 又试一次（Read）
- `#6~#8`：**连续 3 次空 Done**——模型持续不产出修复，loop 重试耗尽后结束

agent_outcome=done（正常结束，非崩溃/超时），src 零改动。**hebbian 的 agent loop + 重试机制
工作正常（确实重试了），是模型本身在该任务上探索后过早 Stop、不写 Edit。**

### 证据 3：跨数据集一致的失败模式（确认是模型行为）

同一「探索后不收尾」模式在 SWE-bench Verified 的 6202/7982 任务上也出现（见
EVAL-REPORT.md）。而 deepseek-v4-pro 在另一些任务上能改对（SWE-bench 5262/5809/7205 串行跑
PASS，修复与官方 gold patch 逻辑一致）——说明这是**任务相关 + 有方差的模型能力问题**，
不是确定性的框架缺陷。

## 一句话结论

> heb-eval 评测框架本身正确（gold patch 可达 PASS、判分精确到单个测试）；当前 FAIL 的根因是
> **agent 模型（deepseek-v4-pro）在部分任务上探索后过早返回空响应、不进入代码修复阶段**——
> 是「效果不好」，不是「框架有 bug」。改善方向：换更强模型 / prompt 引导坚持到修复 / pass@k 多采样。

## 复现

```bash
python3 scripts/fetch_r2e.py --offset 0 --count 1 --max-files 2 --out r2e.json
heb-eval run --suite r2e.json --provider <id> --heb-bin ./target/debug/heb \
  --timeout 900 --work-root /tmp/r2e-run --out report.json
```

需要本机 Docker（拉镜像 ~2.4GB/任务）。原始报告见 `RESULTS-r2e-deepswe.json`、
任务样例见 `r2e-gym-orange3-sample.json`。
