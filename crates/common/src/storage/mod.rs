pub mod sessions;

use crate::AppResult;
use serde::{de::DeserializeOwned, Serialize};
use std::path::{Path, PathBuf};

pub fn providers_path(data_dir: &Path) -> PathBuf {
    data_dir.join("providers.json")
}

pub fn sessions_dir(data_dir: &Path) -> PathBuf {
    let dir = data_dir.join("sessions");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn read_json<T: DeserializeOwned + Default>(path: &Path) -> AppResult<T> {
    if !path.exists() {
        return Ok(T::default());
    }
    let bytes = std::fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub fn read_json_required<T: DeserializeOwned>(path: &Path) -> AppResult<T> {
    let bytes = std::fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    let bytes = serde_json::to_vec_pretty(value)?;
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}
