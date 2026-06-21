//! 执行 + 判分（架构 §17.3）：准备隔离 workdir → shell out `heb run --json` → 跑校验命令。

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{anyhow, Context, Result};
use serde::Serialize;

use crate::task::{GeneralTask, R2eTask, SweTask, Task};

/// 单个任务的评测结果（进报告）。
#[derive(Debug, Clone, Serialize)]
pub struct TaskResult {
    pub id: String,
    pub pass: bool,
    /// `heb run` 报告的 outcome（done / failed / cancelled ...）。
    pub agent_outcome: String,
    /// 人话结论（判分细节 / 失败原因）。
    pub detail: String,
}

/// runner 共享配置。
pub struct RunnerConfig {
    /// `heb` 可执行文件路径。
    pub heb_bin: PathBuf,
    pub provider: Option<String>,
    pub model: Option<String>,
    /// 单任务 agent 超时（秒）。
    pub timeout_secs: u64,
    /// 评测产物根目录（每任务一个隔离子目录）。
    pub work_root: PathBuf,
}

/// `heb run --json` 末行结果（只取判分需要的字段）。
#[derive(Debug, serde::Deserialize)]
struct HebRunResult {
    outcome: String,
    #[serde(default)]
    exit_code: i32,
}

/// 跑一个任务并判分。
pub async fn run_task(cfg: &RunnerConfig, task: &Task) -> TaskResult {
    let id = task.id().to_string();
    let outcome = match task {
        Task::General(t) => run_general(cfg, t).await,
        Task::Swe(t) => run_swe(cfg, t).await,
        Task::R2e(t) => run_r2e(cfg, t).await,
    };
    match outcome {
        Ok((pass, agent_outcome, detail)) => TaskResult {
            id,
            pass,
            agent_outcome,
            detail,
        },
        Err(e) => TaskResult {
            id,
            pass: false,
            agent_outcome: "error".to_string(),
            detail: format!("runner 错误：{e}"),
        },
    }
}

// ─── 通用任务 ────────────────────────────────────────────────────────────────

async fn run_general(cfg: &RunnerConfig, task: &GeneralTask) -> Result<(bool, String, String)> {
    let workdir = prepare_workdir(cfg, &task.id, task.workdir.as_deref())?;

    if let Some(setup) = &task.setup_cmd {
        let status = sh(setup, &workdir).await?;
        if !status.success() {
            return Ok((
                false,
                "skipped".to_string(),
                format!("setup_cmd 失败（exit {:?}）", status.code()),
            ));
        }
    }

    let agent = run_heb(cfg, &task.prompt, &workdir).await?;

    // 判分：跑 verify_cmd，退出码 == expect_exit ? pass。
    let verify = sh_capture(&task.verify_cmd, &workdir).await?;
    let actual = verify.code.unwrap_or(-1);
    let pass = actual == task.expect_exit;
    let detail = if pass {
        format!("verify_cmd 退出 {actual}（期望 {}）", task.expect_exit)
    } else {
        format!(
            "verify_cmd 退出 {actual}（期望 {}）；输出：{}",
            task.expect_exit,
            truncate(&verify.merged, 300)
        )
    };
    Ok((pass, agent.outcome, detail))
}

// ─── SWE-bench 风格 ──────────────────────────────────────────────────────────

async fn run_swe(cfg: &RunnerConfig, task: &SweTask) -> Result<(bool, String, String)> {
    let workdir = cfg.work_root.join(sanitize(&task.instance_id));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(&workdir)?;

    // clone + checkout base_commit
    let clone = sh_capture(
        &format!(
            "git clone --quiet {} . && git checkout --quiet {}",
            shell_quote(&task.repo),
            shell_quote(&task.base_commit)
        ),
        &workdir,
    )
    .await?;
    if !clone.code.map(|c| c == 0).unwrap_or(false) {
        return Ok((
            false,
            "skipped".to_string(),
            format!("clone/checkout 失败：{}", truncate(&clone.merged, 300)),
        ));
    }

    // 环境准备（建 venv / pip install）。SWE 任务装依赖慢，给宽松超时。
    if let Some(setup) = &task.setup_cmd {
        let setup_out = sh_capture(setup, &workdir).await?;
        if !setup_out.code.map(|c| c == 0).unwrap_or(false) {
            return Ok((
                false,
                "skipped".to_string(),
                format!(
                    "setup_cmd 失败（exit {:?}）：{}",
                    setup_out.code,
                    truncate(&setup_out.merged, 300)
                ),
            ));
        }
    }

    let agent = run_heb(cfg, &task.problem_statement, &workdir).await?;

    // 应用隐藏测试 patch
    if let Some(patch) = &task.test_patch {
        let patch_file = workdir.join(".heb-eval-test.patch");
        std::fs::write(&patch_file, patch)?;
        let apply = sh_capture("git apply .heb-eval-test.patch", &workdir).await?;
        if !apply.code.map(|c| c == 0).unwrap_or(false) {
            return Ok((
                false,
                agent.outcome,
                format!("test_patch 应用失败：{}", truncate(&apply.merged, 300)),
            ));
        }
    }

    // FAIL_TO_PASS 全转 pass + PASS_TO_PASS 全保持 pass
    for cmd in &task.fail_to_pass {
        let r = sh_capture(cmd, &workdir).await?;
        if !r.code.map(|c| c == 0).unwrap_or(false) {
            return Ok((
                false,
                agent.outcome,
                format!("FAIL_TO_PASS 未通过：`{cmd}` 退出 {:?}", r.code),
            ));
        }
    }
    for cmd in &task.pass_to_pass {
        let r = sh_capture(cmd, &workdir).await?;
        if !r.code.map(|c| c == 0).unwrap_or(false) {
            return Ok((
                false,
                agent.outcome,
                format!("PASS_TO_PASS 回归：`{cmd}` 退出 {:?}", r.code),
            ));
        }
    }

    let detail = format!(
        "FAIL_TO_PASS {} 条全过、PASS_TO_PASS {} 条无回归",
        task.fail_to_pass.len(),
        task.pass_to_pass.len()
    );
    Ok((true, agent.outcome, detail))
}

// ─── R2E-Gym / DeepSWE docker 任务 ───────────────────────────────────────────

/// 跑一个 R2E-Gym/DeepSWE docker 任务（架构 §17.2）。
///
/// 流程：`docker create` 起一个长驻容器 → cp /testbed 到宿主 workdir → `heb run` 让 agent 改 →
/// 改动 cp 回容器 /testbed → 容器内跑 run_tests.sh → 解析 pytest `-rA` 的 PASSED/FAILED，
/// 与 `expected_output` 全部吻合才 PASS。环境由镜像保证，无需本地装依赖。
async fn run_r2e(cfg: &RunnerConfig, task: &R2eTask) -> Result<(bool, String, String)> {
    let workdir = cfg.work_root.join(sanitize(&task.instance_id));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(&workdir)?;
    let cname = format!("heb-eval-{}", sanitize(&task.instance_id));

    // 起一个长驻容器（sleep 保活），失败直接返回。
    let _ = docker(&["rm", "-f", &cname]).await; // 清理同名残留
    let create = docker_capture(&[
        "run", "-d", "--name", &cname, "--platform", "linux/amd64",
        &task.docker_image, "sleep", "infinity",
    ])
    .await?;
    if !create.code.map(|c| c == 0).unwrap_or(false) {
        return Ok((
            false,
            "skipped".to_string(),
            format!("docker run 失败：{}", truncate(&create.merged, 200)),
        ));
    }
    // 用闭包保证无论判分成败都清理容器。
    let result = run_r2e_inner(cfg, task, &cname, &workdir).await;
    let _ = docker(&["rm", "-f", &cname]).await;
    result
}

async fn run_r2e_inner(
    cfg: &RunnerConfig,
    task: &R2eTask,
    cname: &str,
    workdir: &Path,
) -> Result<(bool, String, String)> {
    // 1. 导出容器 /testbed 到宿主 workdir（agent 在宿主改）。
    let cp_out = docker_capture(&["cp", &format!("{cname}:/testbed/."), &workdir.to_string_lossy()])
        .await?;
    if !cp_out.code.map(|c| c == 0).unwrap_or(false) {
        return Ok((
            false,
            "skipped".to_string(),
            format!("docker cp 导出 /testbed 失败：{}", truncate(&cp_out.merged, 200)),
        ));
    }

    // 2. agent 改代码（带 R2E 上下文提示：测试在容器跑，改 /testbed 对应的本地文件即可）。
    let prompt = format!(
        "{}\n\n（这是 {} 仓库的源码。请定位并修复上述 issue，直接 Edit 工作目录里的源文件。\
         不要跑测试——测试会由评测器在隔离环境里执行。）",
        task.problem_statement, /* repo hint */ "Python"
    );
    let agent = run_heb(cfg, &prompt, workdir).await?;

    // 3. 把 agent 改动 cp 回容器 /testbed。
    let cp_in = docker_capture(&[
        "cp",
        &format!("{}/.", workdir.to_string_lossy()),
        &format!("{cname}:/testbed/"),
    ])
    .await?;
    if !cp_in.code.map(|c| c == 0).unwrap_or(false) {
        return Ok((
            false,
            agent.outcome,
            format!("docker cp 回写 /testbed 失败：{}", truncate(&cp_in.merged, 200)),
        ));
    }

    // 4. 容器内跑测试（run_tests.sh 用相对 r2e_tests，需 cwd=/testbed；测试目录在 /r2e_tests）。
    let test = docker_capture(&[
        "exec", "-w", "/testbed", cname, "bash", "-lc",
        "QT_QPA_PLATFORM=minimal PYTHONWARNINGS='ignore::UserWarning,ignore::SyntaxWarning' \
         xvfb-run --auto-servernum .venv/bin/python -W ignore -m pytest -rA /r2e_tests 2>&1 | tail -200",
    ])
    .await?;

    // 5. 解析 pytest -rA 输出的 PASSED/FAILED，与 expected 比对。
    let actual = parse_pytest_ra(&test.merged);
    let (pass, detail) = judge_r2e(&task.expected_output, &actual);
    Ok((pass, agent.outcome, detail))
}

/// 解析 pytest `-rA` 输出：每行 `PASSED <nodeid>` / `FAILED <nodeid>`，取测试方法名做 key。
fn parse_pytest_ra(output: &str) -> std::collections::BTreeMap<String, String> {
    let mut map = std::collections::BTreeMap::new();
    for line in output.lines() {
        let line = line.trim();
        let status = if line.starts_with("PASSED") {
            "PASSED"
        } else if line.starts_with("FAILED") {
            "FAILED"
        } else if line.starts_with("ERROR") {
            "FAILED"
        } else {
            continue;
        };
        // 取 `::Class::method` 尾部作 key（与 expected_output 的 `Class.method` 对齐）。
        if let Some(node) = line.split_whitespace().nth(1) {
            let key = node
                .rsplit("::")
                .take(2)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join(".");
            map.insert(key, status.to_string());
        }
    }
    map
}

/// 判分：expected 里每个测试都必须在 actual 里有同名（或后缀匹配）且状态吻合。
fn judge_r2e(
    expected: &std::collections::BTreeMap<String, String>,
    actual: &std::collections::BTreeMap<String, String>,
) -> (bool, String) {
    let mut matched = 0usize;
    let mut mismatched: Vec<String> = Vec::new();
    for (name, exp_status) in expected {
        // actual 的 key 是 Class.method；expected 也可能带或不带 Class。后缀匹配兜底。
        let got = actual
            .iter()
            .find(|(k, _)| k.ends_with(name.as_str()) || name.ends_with(k.as_str()))
            .map(|(_, v)| v.as_str());
        match got {
            Some(s) if s == exp_status => matched += 1,
            Some(s) => mismatched.push(format!("{name}: 期望{exp_status} 实际{s}")),
            None => mismatched.push(format!("{name}: 期望{exp_status} 实际(未跑到)")),
        }
    }
    let total = expected.len();
    if mismatched.is_empty() {
        (true, format!("{matched}/{total} 测试结果全部吻合 expected"))
    } else {
        (
            false,
            format!(
                "{matched}/{total} 吻合；不符 {} 项：{}",
                mismatched.len(),
                truncate(&mismatched.join("; "), 280)
            ),
        )
    }
}

// ─── heb run 调用 ────────────────────────────────────────────────────────────

struct AgentRun {
    outcome: String,
}

/// shell out `heb run "<prompt>" --workdir <iso> --yolo --json --timeout T`，解析末行结果。
async fn run_heb(cfg: &RunnerConfig, prompt: &str, workdir: &Path) -> Result<AgentRun> {
    let mut cmd = tokio::process::Command::new(&cfg.heb_bin);
    cmd.arg("run")
        .arg(prompt)
        .arg("--workdir")
        .arg(workdir)
        .arg("--yolo")
        .arg("--json")
        .arg("--timeout")
        .arg(cfg.timeout_secs.to_string());
    if let Some(p) = &cfg.provider {
        cmd.arg("--provider").arg(p);
    }
    if let Some(m) = &cfg.model {
        cmd.arg("--model").arg(m);
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::null());

    let out = cmd
        .output()
        .await
        .with_context(|| format!("启动 heb run 失败（heb_bin={}）", cfg.heb_bin.display()))?;
    let stdout = String::from_utf8_lossy(&out.stdout);

    // 末行是结果 JSON（前面是 NDJSON 事件流，§17.1）。
    let last = stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .ok_or_else(|| anyhow!("heb run 无输出"))?;
    let result: HebRunResult =
        serde_json::from_str(last).with_context(|| format!("解析 heb run 结果失败：{last}"))?;
    let _ = result.exit_code;
    Ok(AgentRun {
        outcome: result.outcome,
    })
}

// ─── 辅助 ────────────────────────────────────────────────────────────────────

/// 准备隔离 workdir：有模板就拷贝，否则建空目录。
fn prepare_workdir(cfg: &RunnerConfig, id: &str, template: Option<&str>) -> Result<PathBuf> {
    let dest = cfg.work_root.join(sanitize(id));
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::create_dir_all(&dest)?;
    if let Some(tpl) = template {
        copy_dir(Path::new(tpl), &dest)
            .with_context(|| format!("拷贝 workdir 模板失败：{tpl}"))?;
    }
    Ok(dest)
}

fn copy_dir(src: &Path, dest: &Path) -> Result<()> {
    if !src.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if from.is_dir() {
            std::fs::create_dir_all(&to)?;
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

struct ShOut {
    code: Option<i32>,
    merged: String,
}

/// 跑一条 shell 命令，只取退出状态。
async fn sh(cmd: &str, cwd: &Path) -> Result<std::process::ExitStatus> {
    Ok(tokio::process::Command::new("bash")
        .arg("-lc")
        .arg(cmd)
        .current_dir(cwd)
        .status()
        .await?)
}

/// 跑一条 shell 命令，捕获 stdout+stderr 与退出码。
async fn sh_capture(cmd: &str, cwd: &Path) -> Result<ShOut> {
    let out = tokio::process::Command::new("bash")
        .arg("-lc")
        .arg(cmd)
        .current_dir(cwd)
        .output()
        .await?;
    let mut merged = String::from_utf8_lossy(&out.stdout).into_owned();
    merged.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok(ShOut {
        code: out.status.code(),
        merged,
    })
}

/// 跑一条 docker 命令（只取退出状态，丢输出）。
async fn docker(args: &[&str]) -> Result<std::process::ExitStatus> {
    Ok(tokio::process::Command::new("docker")
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await?)
}

/// 跑一条 docker 命令，捕获 stdout+stderr 与退出码。
async fn docker_capture(args: &[&str]) -> Result<ShOut> {
    let out = tokio::process::Command::new("docker").args(args).output().await?;
    let mut merged = String::from_utf8_lossy(&out.stdout).into_owned();
    merged.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok(ShOut {
        code: out.status.code(),
        merged,
    })
}

/// 把 id 转成安全的目录名。
fn sanitize(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{t}…")
    }
}
