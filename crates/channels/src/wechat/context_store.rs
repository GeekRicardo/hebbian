//! 微信 context_token 持久化。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct ContextStore {
    path: PathBuf,
    tokens: HashMap<String, String>,
}

impl ContextStore {
    pub fn open(data_dir: &Path, account_id: &str) -> Self {
        let path = data_dir
            .join("channels")
            .join("wechat")
            .join(account_id)
            .join("context_tokens.json");
        let tokens = std::fs::read_to_string(&path)
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default();
        Self { path, tokens }
    }

    pub fn get(&self, user_id: &str) -> Option<&str> {
        self.tokens.get(user_id).map(String::as_str)
    }

    pub fn set(&mut self, user_id: &str, token: &str) {
        self.tokens.insert(user_id.to_string(), token.to_string());
        self.flush();
    }

    fn flush(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(&self.tokens) {
            let _ = std::fs::write(&self.path, json);
        }
    }
}
