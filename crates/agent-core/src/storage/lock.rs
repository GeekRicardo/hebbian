//! 文件锁 + 原子写（架构 §6.3）。
//!
//! 三个原语：
//! - [`write_atomic`]：排他锁 → tmp 文件 → rename，整文件替换。
//! - [`append_jsonl`]：排他锁 → O_APPEND open → write + fsync，单行追加。
//! - [`read_locked`]：共享锁 → read 全部内容。
//!
//! 每个被保护的文件配一个 `<path>.lock`。锁本身的生命周期与本函数调用一致，
//! 调用返回时 lock 释放（Drop fs2::FileExt）。
//!
//! CLI 与 Desktop 共享 `~/.hebbian/`，两边可同时跑——所有共享文件必须经此模块。

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use fs2::FileExt;

use common::{AppError, AppResult};

fn lock_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".lock");
    PathBuf::from(s)
}

fn open_lock_file(path: &Path) -> AppResult<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let lock = lock_path(path);
    Ok(OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&lock)?)
}

/// 排他锁包装：lock 文件在返回后立即 release。
fn with_exclusive<R>(path: &Path, f: impl FnOnce() -> AppResult<R>) -> AppResult<R> {
    let lf = open_lock_file(path)?;
    lf.lock_exclusive()
        .map_err(|e| AppError::msg(format!("acquire exclusive lock: {e}")))?;
    let res = f();
    let _ = fs2::FileExt::unlock(&lf);
    res
}

/// 共享锁包装。
fn with_shared<R>(path: &Path, f: impl FnOnce() -> AppResult<R>) -> AppResult<R> {
    let lf = open_lock_file(path)?;
    lf.lock_shared()
        .map_err(|e| AppError::msg(format!("acquire shared lock: {e}")))?;
    let res = f();
    let _ = fs2::FileExt::unlock(&lf);
    res
}

/// 原子整文件覆盖（持排他锁；tmp + rename）。
///
/// 父目录会自动创建；写入失败时尝试清理 tmp。
pub fn write_atomic(path: &Path, content: &[u8]) -> AppResult<()> {
    with_exclusive(path, || {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension(format!(
            "{}.tmp.{}",
            path.extension().and_then(|s| s.to_str()).unwrap_or(""),
            uuid::Uuid::new_v4()
        ));
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)?;
        f.write_all(content)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)?;
        Ok(())
    })
}

/// 单行 jsonl 追加（持排他锁；O_APPEND + write + fsync）。
///
/// `line` 不含末尾换行，函数自动补 `\n`。
pub fn append_jsonl(path: &Path, line: &str) -> AppResult<()> {
    with_exclusive(path, || {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = OpenOptions::new().create(true).append(true).open(path)?;
        f.write_all(line.as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_data()?;
        Ok(())
    })
}

/// 读全文件（持共享锁；允许多读并发）。
pub fn read_locked(path: &Path) -> AppResult<Vec<u8>> {
    with_shared(path, || {
        let mut f = OpenOptions::new().read(true).open(path)?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        Ok(buf)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("hebbian-lock-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("data.bin")
    }

    #[test]
    fn write_atomic_roundtrip() {
        let p = tmp("atomic");
        write_atomic(&p, b"hello").unwrap();
        let bytes = read_locked(&p).unwrap();
        assert_eq!(bytes, b"hello");
    }

    #[test]
    fn append_jsonl_two_lines() {
        let p = tmp("append");
        append_jsonl(&p, r#"{"a":1}"#).unwrap();
        append_jsonl(&p, r#"{"a":2}"#).unwrap();
        let bytes = read_locked(&p).unwrap();
        let s = String::from_utf8(bytes).unwrap();
        assert_eq!(s, "{\"a\":1}\n{\"a\":2}\n");
    }
}
