#!/usr/bin/env python3
"""从 HuggingFace datasets-server 拉 SWE-bench 任务，转成 heb-eval 的 swe.json 格式。

SWE-bench / DeepSWE 任务集接入工具（架构 §17.5 留尾巴的真实数据集接入）。
不依赖 `datasets` 库——直接用 datasets-server 的 REST API（只需 stdlib）。

用法：
  python3 fetch_swebench.py --instance pytest-dev__pytest-5262 --out suite.json
  python3 fetch_swebench.py --instance psf__requests-2317 --dataset princeton-nlp/SWE-bench_Verified

转换要点：
  repo            -> https://github.com/<repo>.git
  base_commit     -> heb-eval 的 base_commit（runner 会 clone+checkout）
  problem_statement-> heb-eval 的 problem_statement（喂给 agent）
  test_patch      -> heb-eval 的 test_patch（runner git apply）
  FAIL_TO_PASS    -> 每个 pytest node id 转成一条 `<test_cmd> '<node>'` 命令
  PASS_TO_PASS    -> 同上（防回归）；过滤掉非 node id 噪音（如 "[100%]"）
  setup_cmd       -> pip 安装项目自身（pytest/requests 等纯 python 库 `pip install -e .` 即可）
"""
import argparse
import json
import sys
import urllib.parse
import urllib.request

DS_API = "https://datasets-server.huggingface.co/rows"


def fetch_rows(dataset, split, offset, length):
    qs = urllib.parse.urlencode(
        {"dataset": dataset, "config": "default", "split": split,
         "offset": offset, "length": length}
    )
    url = f"{DS_API}?{qs}"
    with urllib.request.urlopen(url, timeout=60) as resp:
        return json.load(resp)["rows"]


def find_instance(dataset, split, instance_id, max_scan=600):
    """分页扫描找指定 instance（数据集无按 id 直查的 API）。"""
    for offset in range(0, max_scan, 100):
        rows = fetch_rows(dataset, split, offset, 100)
        if not rows:
            break
        for r in rows:
            if r["row"]["instance_id"] == instance_id:
                return r["row"]
    return None


def parse_node_list(raw):
    """FAIL_TO_PASS / PASS_TO_PASS 可能是 JSON 字符串或已是 list。过滤非 node 噪音。"""
    items = json.loads(raw) if isinstance(raw, str) else raw
    return [x for x in items if "::" in x]


def to_heb_eval(row, test_cmd):
    repo = row["repo"]
    fail = parse_node_list(row.get("FAIL_TO_PASS", "[]"))
    keep = parse_node_list(row.get("PASS_TO_PASS", "[]"))
    return {
        "type": "swe",
        "instance_id": row["instance_id"],
        "repo": f"https://github.com/{repo}.git",
        "base_commit": row["base_commit"],
        "problem_statement": row["problem_statement"],
        "test_patch": row.get("test_patch"),
        "setup_cmd": "python3 -m venv .venv && . .venv/bin/activate && "
                     "pip install -q -e . 2>&1 | tail -2 || true",
        "FAIL_TO_PASS": [f". .venv/bin/activate && {test_cmd} '{n}'" for n in fail],
        # PASS_TO_PASS 体量常很大，默认抽前 N 条防回归即可（全量太慢）。
        "PASS_TO_PASS": [f". .venv/bin/activate && {test_cmd} '{n}'" for n in keep[:5]],
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--instance", required=True)
    ap.add_argument("--dataset", default="princeton-nlp/SWE-bench_Verified")
    ap.add_argument("--split", default="test")
    ap.add_argument("--test-cmd", default="python -m pytest -q -p no:cacheprovider",
                    help="跑单个 node 的命令模板，node id 作为末尾参数追加")
    ap.add_argument("--out", default="-")
    args = ap.parse_args()

    row = find_instance(args.dataset, args.split, args.instance)
    if row is None:
        print(f"未找到 instance：{args.instance}", file=sys.stderr)
        sys.exit(1)

    suite = [to_heb_eval(row, args.test_cmd)]
    text = json.dumps(suite, ensure_ascii=False, indent=2)
    if args.out == "-":
        print(text)
    else:
        with open(args.out, "w") as f:
            f.write(text)
        print(f"已写入 {args.out}（{args.instance}）", file=sys.stderr)


if __name__ == "__main__":
    main()
