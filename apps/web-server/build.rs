//! 生成 `HEBBIAN_BUILD_VERSION`（§7.8.7 版本协商）。与 apps/hebcore/build.rs 同逻辑：
//! `v{pkg}-{git_short}[-dirty]-{build_id}`，`build_id` 来自 `HEBBIAN_BUILD_ID` 环境变量
//! （同次 build 的多 binary 共享同一值）。hebweb 兼任 hebcore 时也要报告版本。
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
    let build_id = std::env::var("HEBBIAN_BUILD_ID").unwrap_or_else(|_| "dev".into());
    let version = format!("v{pkg}-{short}{}-{build_id}", if dirty { "-dirty" } else { "" });
    println!("cargo:rustc-env=HEBBIAN_BUILD_VERSION={version}");
    println!("cargo:rerun-if-env-changed=HEBBIAN_BUILD_ID");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
}
