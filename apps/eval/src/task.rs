//! 任务集格式（架构 §17.2）：通用任务 + SWE-bench 风格，按 `type` 标签区分。

use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

/// 一个 suite 文件 = 一个任务数组（两种格式可混装）。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Task {
    /// 通用任务：prompt 跑 agent，verify_cmd 判分。
    General(GeneralTask),
    /// SWE-bench 风格：repo + base_commit + 隐藏测试。
    Swe(SweTask),
    /// R2E-Gym / DeepSWE 风格：docker 镜像内 /testbed 改代码，run_tests.sh 判分。
    R2e(R2eTask),
}

impl Task {
    /// 人类可读的任务 id（报告 / 日志用）。
    pub fn id(&self) -> &str {
        match self {
            Task::General(t) => &t.id,
            Task::Swe(t) => &t.instance_id,
            Task::R2e(t) => &t.instance_id,
        }
    }
}

/// R2E-Gym / DeepSWE 任务（架构 §17.2）：每个任务有预构建 docker 镜像，环境已装好。
///
/// 流程：从镜像导出 `/testbed`（base commit 状态）到宿主 workdir → `heb run` 让 agent 改 →
/// 改动 patch 回容器 → 跑 `/testbed/run_tests.sh`（pytest /r2e_tests）→ 比对
/// `expected_output_json`（一组测试名→PASSED）。这是 DeepSWE 官方用 Docker 解决环境矩阵的方式。
#[derive(Debug, Clone, Deserialize)]
pub struct R2eTask {
    pub instance_id: String,
    /// 预构建的 docker 镜像（含 /testbed 仓库 + 装好的环境 + /r2e_tests 测试）。
    pub docker_image: String,
    /// 喂给 `heb run` 的问题描述（GitHub issue）。
    pub problem_statement: String,
    /// 期望的测试结果：测试名 → "PASSED"/"FAILED"。判分时实际结果须与之全部吻合。
    pub expected_output: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GeneralTask {
    pub id: String,
    /// 喂给 `heb run` 的任务文本。
    pub prompt: String,
    /// 工作目录模板；不填则用临时目录。
    #[serde(default)]
    pub workdir: Option<String>,
    /// 跑 agent 前的准备命令（建文件 / 装依赖）。
    #[serde(default)]
    pub setup_cmd: Option<String>,
    /// 跑完后的校验命令——退出码 == expect_exit 则 pass。
    pub verify_cmd: String,
    /// verify_cmd 的期望退出码（默认 0）。
    #[serde(default)]
    pub expect_exit: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SweTask {
    pub instance_id: String,
    /// 仓库地址（git clone）或本地路径。
    pub repo: String,
    pub base_commit: String,
    /// 喂给 `heb run` 的问题描述。
    pub problem_statement: String,
    /// clone + checkout 后、跑 agent 前的环境准备命令（建 venv / pip install 等）。
    #[serde(default)]
    pub setup_cmd: Option<String>,
    /// 隐藏测试 patch（git apply），跑 agent 后应用。
    #[serde(default)]
    pub test_patch: Option<String>,
    /// agent 改完后必须由 fail 转 pass 的测试命令（全部退出 0 才 pass）。
    #[serde(rename = "FAIL_TO_PASS", default)]
    pub fail_to_pass: Vec<String>,
    /// 必须保持 pass 的测试命令（防回归）。
    #[serde(rename = "PASS_TO_PASS", default)]
    pub pass_to_pass: Vec<String>,
}

/// 从 JSON 文件加载任务集。
pub fn load_suite(path: &Path) -> Result<Vec<Task>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("读取 suite 文件失败：{}", path.display()))?;
    let tasks: Vec<Task> =
        serde_json::from_str(&text).with_context(|| format!("解析 suite 失败：{}", path.display()))?;
    Ok(tasks)
}
