//! 本 surface 自己的 UI 偏好（`~/.hebbian/gpui-ui.json`）。
//!
//! 架构 §7.3 明确：`desktop-settings.json` 这类纯 UI 偏好不走 `CoreRequest`——
//! 它不是 core 业务。原 Web 前端把这些存在 localStorage 里，原生端没有 localStorage，
//! 所以落一个同级别的小文件，只本机有效、丢了不影响任何对话数据。
//!
//! 刻意只放「丢了也无所谓」的东西：项目排序、折叠状态。窗口尺寸、列宽等一次性
//! 状态仍然不持久化，与原前端保持一致。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const FILE_NAME: &str = "gpui-ui.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UiPrefs {
    /// 项目在侧栏里的显示顺序（存 project id）。不在表里的新项目排到末尾。
    #[serde(default)]
    pub project_order: Vec<String>,
    /// 折叠着的项目。
    #[serde(default)]
    pub collapsed: HashSet<String>,
}

fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE_NAME)
}

/// 读偏好。文件不存在或解析失败都退回默认——UI 偏好坏了不该挡住启动。
pub fn load(data_dir: &Path) -> UiPrefs {
    std::fs::read(path(data_dir))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// 写偏好。失败只记日志不打扰用户——偏好没存上不影响任何实际功能。
pub fn save(data_dir: &Path, prefs: &UiPrefs) {
    let Ok(bytes) = serde_json::to_vec_pretty(prefs) else {
        return;
    };
    if let Err(err) = std::fs::write(path(data_dir), bytes) {
        tracing::warn!(error = %err, "界面偏好没存上");
    }
}

/// 把项目按保存的顺序排；没在表里的（新建的项目）保持原相对顺序排到末尾。
pub fn apply_order<T>(order: &[String], items: &mut [T], id_of: impl Fn(&T) -> String) {
    let rank: std::collections::HashMap<&str, usize> = order
        .iter()
        .enumerate()
        .map(|(i, id)| (id.as_str(), i))
        .collect();
    items.sort_by_key(|item| {
        rank.get(id_of(item).as_str())
            .copied()
            .unwrap_or(usize::MAX)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_order_wins_and_new_projects_go_last() {
        let order = vec!["c".to_string(), "a".to_string()];
        let mut items = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        apply_order(&order, &mut items, |s| s.clone());
        // c、a 按保存的顺序在前；没排过的 b 落到末尾。
        assert_eq!(items, vec!["c", "a", "b"]);
    }

    #[test]
    fn empty_order_keeps_original_sequence() {
        let mut items = vec!["a".to_string(), "b".to_string()];
        apply_order(&[], &mut items, |s| s.clone());
        assert_eq!(items, vec!["a", "b"]);
    }
}
