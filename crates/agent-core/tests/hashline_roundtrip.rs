use agent_core::edits::hashline::format::hash3;
use agent_core::read_state::ReadStateTracker;
use agent_core::tools::edit_hashline::EditHashlineTool;
use agent_core::tools::read_hashline::ReadHashlineTool;
use agent_core::tools::Tool;
use agent_core::workspace::Workspace;
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

fn make_tools(
    tmp: &tempfile::TempDir,
) -> (ReadHashlineTool, EditHashlineTool, Arc<ReadStateTracker>) {
    let tracker = Arc::new(ReadStateTracker::new());
    let ws = Workspace::new(tmp.path(), Vec::new());
    let read_tool = ReadHashlineTool::new(None, None, Some(tracker.clone()));
    let edit_tool = EditHashlineTool::new(ws, Some(tracker.clone()));
    (read_tool, edit_tool, tracker)
}

async fn mark_read(tracker: &ReadStateTracker, path: &std::path::Path) {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mtime_ms = std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let content = std::fs::read(path).unwrap_or_default();
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    tracker.record(path, hasher.finish(), mtime_ms);
}

#[tokio::test]
async fn read_then_edit_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let (read_tool, edit_tool, tracker) = make_tools(&tmp);

    let file = tmp.path().join("src/lib.rs");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let original: String = (1..=10).map(|i| format!("line {i}\n")).collect();
    std::fs::write(&file, &original).unwrap();

    // 1) Read → 确认 hashline 格式
    let read_out = read_tool
        .execute(json!({ "file_path": file.to_string_lossy() }))
        .await
        .unwrap();

    let h1 = hash3(&original);
    assert!(
        read_out.starts_with("¶"),
        "Read 输出必须以 ¶ 开头: {read_out:.80}"
    );
    assert!(
        read_out.contains(&format!("#{h1}\n")),
        "Read 输出必须含 hash #{h1}: {read_out:.120}"
    );
    assert!(read_out.contains("\n5:line 5\n"), "行号 5 内容匹配");

    // 2) Edit：替换第 4-6 行
    mark_read(&tracker, &file).await;
    let patch = format!("¶{}#{h1}\n4 6\n+L4-new\n+L5-new\n+L6-new\n", file.to_string_lossy());
    edit_tool
        .execute(json!({ "patch": patch }))
        .await
        .unwrap();

    let after = std::fs::read_to_string(&file).unwrap();
    let expected: String = [
        "line 1", "line 2", "line 3",
        "L4-new", "L5-new", "L6-new",
        "line 7", "line 8", "line 9", "line 10",
    ]
    .iter()
    .map(|s| format!("{s}\n"))
    .collect();
    assert_eq!(after, expected, "Edit 结果不符");

    // 3) 再次 Read，确认新 hash 与旧不同
    let h2 = hash3(&after);
    assert_ne!(h1, h2, "Edit 后 hash 必须变化");
    let read_out2 = read_tool
        .execute(json!({ "file_path": file.to_string_lossy() }))
        .await
        .unwrap();
    assert!(
        read_out2.contains(&format!("#{h2}\n")),
        "第二次 Read 必须含新 hash #{h2}: {read_out2:.120}"
    );
}

#[tokio::test]
async fn stale_hash_rejected_after_external_write() {
    let tmp = tempfile::tempdir().unwrap();
    let (read_tool, edit_tool, tracker) = make_tools(&tmp);

    let file = tmp.path().join("a.txt");
    std::fs::write(&file, "original\n").unwrap();

    // Read 登记
    read_tool
        .execute(json!({ "file_path": file.to_string_lossy() }))
        .await
        .unwrap();
    mark_read(&tracker, &file).await;

    // 外部修改文件（sleep 1ms 确保 mtime 变化）
    std::thread::sleep(Duration::from_millis(10));
    std::fs::write(&file, "externally-modified\n").unwrap();

    // 用旧 hash 试图 Edit
    let old_hash = hash3("original\n");
    let patch = format!("¶{}#{old_hash}\n1 1\n+z\n", file.to_string_lossy());
    let err = edit_tool
        .execute(json!({ "patch": patch }))
        .await
        .unwrap_err();
    let s = err.to_string();
    assert!(
        s.to_lowercase().contains("stale")
            || s.to_lowercase().contains("hash")
            || s.contains("修改")
            || s.contains("modified"),
        "stale hash 或 tracker mtime 检测必须失败: {err}"
    );
}

#[tokio::test]
async fn eof_append_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let (read_tool, edit_tool, tracker) = make_tools(&tmp);

    let file = tmp.path().join("b.txt");
    std::fs::write(&file, "head\n").unwrap();

    read_tool
        .execute(json!({ "file_path": file.to_string_lossy() }))
        .await
        .unwrap();
    mark_read(&tracker, &file).await;

    let h = hash3("head\n");
    let patch = format!("¶{}#{h}\nEOF\n+tail\n", file.to_string_lossy());
    edit_tool.execute(json!({ "patch": patch })).await.unwrap();

    assert_eq!(std::fs::read_to_string(&file).unwrap(), "head\ntail\n");
}

#[tokio::test]
async fn keep_range_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let (read_tool, edit_tool, tracker) = make_tools(&tmp);

    let file = tmp.path().join("c.txt");
    let original = "A\nB\nC\nD\nE\n";
    std::fs::write(&file, original).unwrap();

    read_tool
        .execute(json!({ "file_path": file.to_string_lossy() }))
        .await
        .unwrap();
    mark_read(&tracker, &file).await;

    let h = hash3(original);
    // 替换 1..5：TOP + 保留 2..4 + BOTTOM
    let patch = format!(
        "¶{}#{h}\n1 5\n+TOP\n&2..4\n+BOTTOM\n",
        file.to_string_lossy()
    );
    edit_tool.execute(json!({ "patch": patch })).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "TOP\nB\nC\nD\nBOTTOM\n"
    );
}
