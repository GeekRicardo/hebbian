use std::path::PathBuf;

use anyhow::{anyhow, Result};
use serde::Serialize;

use agent_core::storage::{self, sessions};

pub struct BackfillArgs {
    pub data_dir: Option<PathBuf>,
    pub session_id: Option<String>,
    pub limit: Option<usize>,
    pub offset: usize,
    pub reset_cursor: bool,
    pub consolidate: bool,
    pub execute: bool,
    pub json: bool,
}

#[derive(Debug, Serialize)]
struct BackfillReport {
    execute: bool,
    total_candidates: usize,
    processed: usize,
    skipped: usize,
    succeeded: usize,
    failed: usize,
    sessions: Vec<SessionReport>,
}

#[derive(Debug, Serialize)]
struct SessionReport {
    session_id: String,
    title: String,
    message_count: usize,
    status: String,
    written: usize,
    model: Option<String>,
    error: Option<String>,
}

pub async fn run(args: BackfillArgs) -> Result<()> {
    let data_dir = args.data_dir.unwrap_or_else(storage::default_data_dir);
    let settings = storage::settings::load(&data_dir);
    if args.execute && !settings.memory.active() {
        return Err(anyhow!(
            "记忆系统未启用或未配置记忆模型；请先在设置里启用记忆并配置模型，或去掉 --execute 做预览"
        ));
    }

    let mut metas = sessions::list(&data_dir)?;
    if let Some(session_id) = args.session_id.as_deref() {
        metas.retain(|m| m.id == session_id);
        if metas.is_empty() {
            return Err(anyhow!("session 不存在：{session_id}"));
        }
    }
    let total_candidates = metas.len();
    let selected = metas
        .into_iter()
        .skip(args.offset)
        .take(args.limit.unwrap_or(usize::MAX))
        .collect::<Vec<_>>();

    if !args.execute {
        let report = BackfillReport {
            execute: false,
            total_candidates,
            processed: 0,
            skipped: selected.len(),
            succeeded: 0,
            failed: 0,
            sessions: selected
                .into_iter()
                .map(|m| SessionReport {
                    session_id: m.id,
                    title: m.title,
                    message_count: m.message_count,
                    status: "dry_run".to_string(),
                    written: 0,
                    model: None,
                    error: None,
                })
                .collect(),
        };
        print_report(&report, args.json)?;
        return Ok(());
    }

    let mut reports = Vec::new();
    for meta in selected {
        if args.reset_cursor {
            agent_core::storage::memory::clear_cursor(&data_dir, &meta.id)?;
        }

        match agent_core::memory_extract::extract_for_session(&data_dir, &meta.id).await {
            Ok(Some(result)) => {
                let written = result.written.len();
                let model = Some(result.model);
                if args.consolidate {
                    agent_core::memory_consolidate::consolidate_for_session(
                        &data_dir, &meta.id, 480.0, 0.0,
                    )
                    .await;
                }
                reports.push(SessionReport {
                    session_id: meta.id,
                    title: meta.title,
                    message_count: meta.message_count,
                    status: "extracted".to_string(),
                    written,
                    model,
                    error: None,
                });
            }
            Ok(None) => {
                reports.push(SessionReport {
                    session_id: meta.id,
                    title: meta.title,
                    message_count: meta.message_count,
                    status: "skipped".to_string(),
                    written: 0,
                    model: None,
                    error: None,
                });
            }
            Err(e) => {
                reports.push(SessionReport {
                    session_id: meta.id,
                    title: meta.title,
                    message_count: meta.message_count,
                    status: "failed".to_string(),
                    written: 0,
                    model: None,
                    error: Some(e.to_string()),
                });
            }
        }
    }

    let succeeded = reports.iter().filter(|r| r.status == "extracted").count();
    let skipped = reports.iter().filter(|r| r.status == "skipped").count();
    let failed = reports.iter().filter(|r| r.status == "failed").count();
    let report = BackfillReport {
        execute: true,
        total_candidates,
        processed: reports.len(),
        skipped,
        succeeded,
        failed,
        sessions: reports,
    };
    print_report(&report, args.json)?;
    Ok(())
}

fn print_report(report: &BackfillReport, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }

    if report.execute {
        println!(
            "记忆回灌完成：处理 {} 个，成功 {} 个，跳过 {} 个，失败 {} 个",
            report.processed, report.succeeded, report.skipped, report.failed
        );
    } else {
        println!(
            "记忆回灌预览：候选 {} 个，本次将处理 {} 个；加 --execute 才会调用模型并写盘",
            report.total_candidates,
            report.sessions.len()
        );
    }

    for s in &report.sessions {
        let suffix = match (&s.model, &s.error) {
            (Some(model), _) => format!(" model={model}"),
            (_, Some(error)) => format!(" error={error}"),
            _ => String::new(),
        };
        println!(
            "- {} [{}] messages={} written={}{}",
            s.session_id, s.status, s.message_count, s.written, suffix
        );
    }
    Ok(())
}
