use std::ffi::OsString;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

const SHELL_PATH_TIMEOUT_SECS: u64 = 10;

/// 用指定 shell（或系统默认 SHELL）的 login+interactive 模式捕获 PATH。
/// 失败时返回 None，调用方应回退到当前进程的 PATH。
pub async fn resolve_shell_path(shell: Option<&str>) -> Option<OsString> {
 let shell = shell
        .map(str::trim)
 .filter(|v| !v.is_empty())
    .map(str::to_string)
        .or_else(|| std::env::var("SHELL").ok())?;
 let marker = format!("HEBBIAN_PATH_{}", uuid::Uuid::new_v4().simple());
let script = format!(
        "printf '{marker}'; command printenv PATH; printf '{marker}'",
        marker = marker
    );
    let output = tokio::time::timeout(
     Duration::from_secs(SHELL_PATH_TIMEOUT_SECS),
     Command::new(&shell)
      .arg("-lic")
   .arg(&script)
         .stdout(Stdio::piped())
  .stderr(Stdio::null())
  .stdin(Stdio::null())
     .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !output.status.success() {
  return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let (_, rest) = stdout.split_once(&marker)?;
    let (path, _) = rest.split_once(&marker)?;
    let path = path.trim();
    if !path.is_empty() {
        Some(OsString::from(path))
    } else {
        None
    }
}
