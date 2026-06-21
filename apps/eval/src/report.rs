//! 评测报告（架构 §17.3）：逐任务结果 + 汇总，输出终端表格 + 可选 JSON 文件。

use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use crate::runner::TaskResult;

#[derive(Debug, Serialize)]
pub struct Report {
    pub total: usize,
    pub passed: usize,
    pub pass_rate: f64,
    pub by_instance: Vec<TaskResult>,
}

impl Report {
    pub fn build(results: Vec<TaskResult>) -> Self {
        let total = results.len();
        let passed = results.iter().filter(|r| r.pass).count();
        let pass_rate = if total == 0 {
            0.0
        } else {
            passed as f64 / total as f64
        };
        Self {
            total,
            passed,
            pass_rate,
            by_instance: results,
        }
    }

    /// 打印终端表格。
    pub fn print_table(&self) {
        println!("\n{:─<72}", "");
        println!("{:<40} {:<8} {}", "任务", "结果", "说明");
        println!("{:─<72}", "");
        for r in &self.by_instance {
            let mark = if r.pass { "✓ PASS" } else { "✗ FAIL" };
            println!(
                "{:<40} {:<8} {}",
                truncate(&r.id, 38),
                mark,
                truncate(&r.detail, 50)
            );
        }
        println!("{:─<72}", "");
        println!(
            "合计 {}/{} 通过，通过率 {:.1}%\n",
            self.passed,
            self.total,
            self.pass_rate * 100.0
        );
    }

    /// 写 JSON 报告文件。
    pub fn write_json(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{t}…")
    }
}
