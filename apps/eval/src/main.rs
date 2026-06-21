//! heb-eval — hebbian agent 评测框架（架构 §17）。
//!
//! 通过 shell out `heb run --json` 跑任务集，自动判分出报告。完全解耦：不依赖 agent_core。
//!
//! ```bash
//! heb-eval run --suite samples/general.json --provider <id>
//! heb-eval run --suite suite.json --concurrency 4 --out report.json
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

mod report;
mod runner;
mod task;

use report::Report;
use runner::{run_task, RunnerConfig};

#[derive(Parser)]
#[command(name = "heb-eval", about = "Hebbian agent 评测框架", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 跑一个任务集并输出报告。
    Run {
        /// 任务集 JSON 文件（数组，每项 type=general|swe）。
        #[arg(long)]
        suite: PathBuf,

        /// provider id（透传给 heb run）。
        #[arg(long)]
        provider: Option<String>,

        /// model id（透传给 heb run）。
        #[arg(long, short = 'm')]
        model: Option<String>,

        /// 并发跑多少个任务（默认 1，串行）。
        #[arg(long, default_value = "1")]
        concurrency: usize,

        /// 单任务 agent 超时秒数（默认 300）。
        #[arg(long, default_value = "300")]
        timeout: u64,

        /// heb 可执行文件路径（默认从 PATH 找 `heb`）。
        #[arg(long, default_value = "heb")]
        heb_bin: PathBuf,

        /// 评测产物根目录（每任务一个隔离子目录，默认临时目录）。
        #[arg(long)]
        work_root: Option<PathBuf>,

        /// 把 JSON 报告写到此文件。
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Run {
            suite,
            provider,
            model,
            concurrency,
            timeout,
            heb_bin,
            work_root,
            out,
        } => {
            let tasks = task::load_suite(&suite)?;
            if tasks.is_empty() {
                anyhow::bail!("任务集为空：{}", suite.display());
            }

            let work_root = match work_root {
                Some(p) => p,
                None => std::env::temp_dir().join(format!("heb-eval-{}", std::process::id())),
            };
            std::fs::create_dir_all(&work_root)
                .with_context(|| format!("创建产物目录失败：{}", work_root.display()))?;

            let cfg = Arc::new(RunnerConfig {
                heb_bin,
                provider,
                model,
                timeout_secs: timeout,
                work_root,
            });

            println!(
                "跑 {} 个任务，并发 {}，单任务超时 {}s …",
                tasks.len(),
                concurrency.max(1),
                timeout
            );

            // 并发跑：Semaphore 控制同时在跑的任务数，JoinSet 收集结果。
            let sem = Arc::new(tokio::sync::Semaphore::new(concurrency.max(1)));
            let mut set = tokio::task::JoinSet::new();
            for (idx, t) in tasks.into_iter().enumerate() {
                let cfg = cfg.clone();
                let sem = sem.clone();
                set.spawn(async move {
                    let _permit = sem.acquire().await.expect("semaphore closed");
                    let id = t.id().to_string();
                    println!("  ▶ {id}");
                    let r = run_task(&cfg, &t).await;
                    let mark = if r.pass { "✓" } else { "✗" };
                    println!("  {mark} {id} — {}", r.detail);
                    (idx, r)
                });
            }

            let mut collected: Vec<(usize, runner::TaskResult)> = Vec::new();
            while let Some(joined) = set.join_next().await {
                collected.push(joined?);
            }
            // 按任务集原顺序还原（并发完成顺序乱）。
            collected.sort_by_key(|(idx, _)| *idx);
            let results: Vec<runner::TaskResult> =
                collected.into_iter().map(|(_, r)| r).collect();

            let report = Report::build(results);
            report.print_table();
            if let Some(out_path) = out {
                report.write_json(&out_path)?;
                println!("报告已写入 {}", out_path.display());
            }

            // 有任务失败时退出码非 0，便于 CI。
            if report.passed < report.total {
                std::process::exit(1);
            }
        }
    }
    Ok(())
}
