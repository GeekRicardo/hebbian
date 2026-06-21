# heb-eval sample 任务集

## 本地最小 sample（开箱即跑，验证判分链路）

- `general.json`：通用任务集 2 条（创建文件 + 修 add 函数 bug），verify_cmd 判分。
- `swe.json`：SWE 风格 1 条（修 multiply bug），依赖本地最小 repo。
  跑前先 `bash make-swe-repo.sh` 生成 `/tmp/heb-eval-swe-repo`。

```bash
heb-eval run --suite samples/general.json --provider <id> --heb-bin ./target/debug/heb
bash samples/make-swe-repo.sh
heb-eval run --suite samples/swe.json --provider <id> --heb-bin ./target/debug/heb
```

## 真实 SWE-bench / DeepSWE 任务（拉公开数据集）

`scripts/fetch_swebench.py` 从 HuggingFace datasets-server 拉任意 instance，转成 heb-eval 格式
（只用 python stdlib，不依赖 `datasets` 库）：

```bash
python3 scripts/fetch_swebench.py --instance pytest-dev__pytest-5262 --out suite.json
heb-eval run --suite suite.json --provider <强模型 id> --heb-bin ./target/debug/heb \
  --timeout 900 --work-root /tmp/swebench-run
```

- `swebench-verified-pytest-5262.json` 是一条已拉取并跑通的真实任务存档
  （SWE-bench Verified，难度「<15 min fix」），可直接当 suite 跑。
- **环境坑（真实经验）**：2019 年的 pytest 4.5 跑在新 Python 上需要特殊 setup——
  升级 pip + 预装 `setuptools_scm`，再 `pip install "setuptools<81"`（新 setuptools 移除了
  pytest 4.5 依赖的 `pkg_resources`）。该 suite 的 `setup_cmd` 已固化这套修复。真实 SWE-bench
  的环境矩阵（每个 repo 特定 commit 的依赖版本）是评测的主要难点，官方用 Docker 镜像解决；
  本地裸跑需按库逐个调 setup_cmd。

## 验收记录（2026-06-21）

`pytest-dev__pytest-5262` 真实跑通：agent（deepseek-v4-pro）自主定位 `_pytest/capture.py`
的 `EncodedFile` 类，加 `mode` property 去掉 `b` 二进制标志——与官方 gold patch 逻辑完全一致，
FAIL_TO_PASS 转 pass、PASS_TO_PASS 无回归，判 **PASS**。

**provider 影响很大**：同一任务换某个 opus 中转 provider 时模型返回空响应
（input_tokens=0，大 context 下被服务端拒），agent 空转判 FAIL；deepseek-v4-pro 正常完成。
评测结果同时反映「agent 能力 × provider 稳定性」两个维度。
