//! 生成 `HEBBIAN_BUILD_VERSION`（§7.8.7 版本协商）：`v{pkg}-{git_short}[-dirty]-{build_id}`。
//!
//! `build_id` 来自环境变量 `HEBBIAN_BUILD_ID`——`pnpm tauri build` / `tauri dev` 的前置脚本
//! 每次生成一个新值并同时喂给 desktop + hebcore + hebweb 的编译，使**同一次 build 的多个
//! binary 注入相同版本号**。desktop 用自己编译进的版本号当基准、对比运行中 hebcore 报告的，
//! 字符串相等即同版本。未设 `HEBBIAN_BUILD_ID` 时回落 `dev`（纯 `cargo build`，dev 场景）。
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
    // build_id 变（每次 pnpm build）或 commit 移动时重算版本号。
    println!("cargo:rerun-if-env-changed=HEBBIAN_BUILD_ID");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
}
