//! 生成 `HEBBIAN_BUILD_VERSION`。
//! `v{pkg}-{git_short}[-dirty]-{build_id}`，`build_id` 来自 `HEBBIAN_BUILD_ID` 环境变量，
//! 用于日志和调试区分当前运行的 hebweb binary。
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    let pkg = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let short = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "nogit".into());
    let dirty = git(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
    // build_id 优先级：HEBBIAN_BUILD_ID 环境变量（app:build wrapper）> workspace 根的
    // .hebbian-build-id 文件（`pnpm tauri build` 的 beforeBuildCommand 每次写新值）> "dev"。
    let build_id = std::env::var("HEBBIAN_BUILD_ID")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::fs::read_to_string("../../.hebbian-build-id")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "dev".into());
    let version = format!(
        "v{pkg}-{short}{}-{build_id}",
        if dirty { "-dirty" } else { "" }
    );
    println!("cargo:rustc-env=HEBBIAN_BUILD_VERSION={version}");
    println!("cargo:rerun-if-env-changed=HEBBIAN_BUILD_ID");
    println!("cargo:rerun-if-changed=../../.hebbian-build-id");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
}
