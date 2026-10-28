// creds.rs — 豆包凭证持久化（与 Python 版同一文件、同一格式，两版共用）
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::applog;

pub const WSS_BASE_URL: &str = "wss://ws-samantha.doubao.com/samantha/audio/asr";
pub const ORIGIN: &str = "https://www.doubao.com";
pub const LOGIN_URL: &str = "https://www.doubao.com/chat";
pub const AUDIO_SAMPLE_RATE: u32 = 16000;
pub const SEND_BLOCK_BYTES: usize = 8192; // ≈256ms，与参考项目一致
pub const AUTH_ERROR_CODE: i64 = 709599054;
pub const AUTH_ERROR_KEYWORDS: [&str; 6] =
    ["cookie", "auth", "login", "session", "unauthorized", "expired"];

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AsrParams {
    pub cookies: BTreeMap<String, String>,
    pub device_id: String,
    pub web_id: String,
}

impl AsrParams {
    pub fn cookie_header(&self) -> String {
        self.cookies.iter().map(|(k, v)| format!("{}={}", k, v)).collect::<Vec<_>>().join("; ")
    }

    pub fn is_complete(&self) -> bool {
        !self.cookies.is_empty() && !self.device_id.is_empty() && !self.web_id.is_empty()
    }
}

/// %USERPROFILE%\.config\doubao-input\asr_params.json（XDG_CONFIG_HOME 可覆盖）
pub fn params_path() -> PathBuf {
    let config_home = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("USERPROFILE").ok().map(|h| PathBuf::from(h).join(".config"))
        })
        .unwrap_or_else(|| PathBuf::from(".config"));
    config_home.join("doubao-input").join("asr_params.json")
}

pub fn load_params() -> Option<AsrParams> {
    let path = params_path();
    if !path.exists() {
        return None;
    }
    match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<AsrParams>(&text) {
            Ok(p) if p.is_complete() => Some(p),
            _ => {
                applog::warn(&format!("豆包凭证文件不完整: {}", path.display()));
                None
            }
        },
        Err(e) => {
            applog::warn(&format!("读取豆包凭证失败: {} ({})", path.display(), e));
            None
        }
    }
}

pub fn save_params(p: &AsrParams) -> bool {
    let path = params_path();
    if let Some(dir) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            applog::error(&format!("保存豆包凭证失败: {}", e));
            return false;
        }
    }
    let tmp = path.with_extension("tmp");
    let data = match serde_json::to_string_pretty(p) {
        Ok(d) => d,
        Err(e) => {
            applog::error(&format!("序列化凭证失败: {}", e));
            return false;
        }
    };
    if std::fs::write(&tmp, data).is_err() {
        return false;
    }
    match std::fs::rename(&tmp, &path) {
        Ok(_) => {
            applog::info(&format!("豆包凭证已保存: {}（{} cookies）", path.display(), p.cookies.len()));
            true
        }
        Err(e) => {
            applog::error(&format!("保存豆包凭证失败: {}", e));
            false
        }
    }
}
