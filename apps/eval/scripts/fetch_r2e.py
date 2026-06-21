#!/usr/bin/env python3
"""从 HuggingFace 拉 R2E-Gym 任务（DeepSWE 训练/评测用的数据集），转 heb-eval r2e 格式。

R2E-Gym（https://huggingface.co/datasets/R2E-Gym/R2E-Gym-Subset）是 DeepSWE 用的数据集：
每个任务有预构建 docker 镜像（环境装好、/testbed 是 base commit 状态、/r2e_tests 是测试），
expected_output_json 是判分标准（测试名→PASSED/FAILED）。只用 python stdlib。

用法：
  python3 fetch_r2e.py --offset 0 --count 3 --out r2e-suite.json
  python3 fetch_r2e.py --offset 0 --count 1 --max-files 1   # 只挑改动最小的任务
"""
import argparse
import json
import urllib.parse
import urllib.request

DS_API = "https://datasets-server.huggingface.co/rows"


def fetch_rows(dataset, split, offset, length):
    qs = urllib.parse.urlencode(
        {"dataset": dataset, "config": "default", "split": split,
         "offset": offset, "length": length}
    )
    with urllib.request.urlopen(f"{DS_API}?{qs}", timeout=60) as resp:
        return json.load(resp)["rows"]


def to_heb_eval(row):
    # expected_output_json 是 JSON 字符串：{ "Class.method": "PASSED", ... }
    exp_raw = row.get("expected_output_json", "{}")
    expected = json.loads(exp_raw) if isinstance(exp_raw, str) else exp_raw
    return {
        "type": "r2e",
        "instance_id": f"{row['repo_name']}-{row['commit_hash'][:8]}",
        "docker_image": row["docker_image"],
        "problem_statement": row["problem_statement"],
        "expected_output": expected,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dataset", default="R2E-Gym/R2E-Gym-Subset")
    ap.add_argument("--split", default="train")
    ap.add_argument("--offset", type=int, default=0)
    ap.add_argument("--count", type=int, default=1)
    ap.add_argument("--max-files", type=int, default=0,
                    help=">0 时只保留 num_non_test_files <= 该值的任务（挑改动小的）")
    ap.add_argument("--out", default="-")
    args = ap.parse_args()

    rows = fetch_rows(args.dataset, args.split, args.offset, args.count)
    suite = []
    for r in rows:
        row = r["row"]
        if args.max_files and row.get("num_non_test_files", 999) > args.max_files:
            continue
        suite.append(to_heb_eval(row))

    text = json.dumps(suite, ensure_ascii=False, indent=2)
    if args.out == "-":
        print(text)
    else:
        with open(args.out, "w") as f:
            f.write(text)
        print(f"已写入 {args.out}（{len(suite)} 条 R2E-Gym 任务）", file=__import__("sys").stderr)


if __name__ == "__main__":
    main()
