// config.rs — config.json 读取（与 Python 版同一文件、同一浅合并语义）
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceCfg {
    pub vid: String,   // "524D"
    pub pid: String,   // "4C52"
    pub mi: String,    // "00"
}
impl Default for DeviceCfg {
    fn default() -> Self {
        Self { vid: "524D".into(), pid: "4C52".into(), mi: "00".into() }
    }
}

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
#[serde(default)]
pub struct KeysCfg {
    pub voice_vk: u16,
    pub min_hold_ms: u64,
    pub drain_ms: u64,
}
impl Default for KeysCfg {
    fn default() -> Self {
        Self { voice_vk: 130, min_hold_ms: 200, drain_ms: 300 }
    }
}

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
#[serde(default)]
pub struct VolcAsrCfg {
    /// 火山引擎控制台 API Key（控制台 > API Key 管理）
    pub api_key: String,
    /// 服务 Resource-Id；本项目默认小时版（开通即送 20 小时）
    pub resource_id: String,
    /// 服务地址（双向流式）
    pub url: String,
    /// 模型名（当前仅支持 bigmodel）
    pub model_name: String,
    /// 二遍识别：句尾重识别提升最终准确率（官方推荐开启）
    pub enable_nonstream: bool,
    /// 标点
    pub enable_punc: bool,
    /// 数字/日期等书面格式规整（ITN）
    pub enable_itn: bool,
    /// 语义顺滑（去除语气词/停顿词）
    pub enable_ddc: bool,
    /// 返回分句/分词信息
    pub show_utterances: bool,
    /// VAD 判停静音阈值（ms，300~5000）
    pub end_window_size: u32,
}
impl Default for VolcAsrCfg {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            resource_id: "volc.seedasr.sauc.duration".into(),
            url: "wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_async".into(),
            model_name: "bigmodel".into(),
            enable_nonstream: true,
            enable_punc: true,
            enable_itn: true,
            enable_ddc: false,
            show_utterances: true,
            end_window_size: 800,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
#[serde(default)]
pub struct AsrCfg {
    /// "doubao"（免费网页版，默认）| "volc"（火山引擎付费版）
    pub engine: String,
    pub volc: VolcAsrCfg,
}
impl Default for AsrCfg {
    fn default() -> Self {
        Self { engine: "doubao".into(), volc: VolcAsrCfg::default() }
    }
}

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
#[serde(default)]
pub struct OutputCfg {
    pub input_mode: String,       // "unicode" | "ctrl_v"
    pub show_final_seconds: f64,
}
impl Default for OutputCfg {
    fn default() -> Self {
        Self { input_mode: "unicode".into(), show_final_seconds: 1.2 }
    }
}

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
#[serde(default)]
pub struct PathsCfg {
    pub recordings_dir: String,
    pub log_dir: String,
}
impl Default for PathsCfg {
    fn default() -> Self {
        Self { recordings_dir: "recordings".into(), log_dir: "logs".into() }
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub device: DeviceCfg,
    pub keys: KeysCfg,
    pub asr: AsrCfg,
    pub output: OutputCfg,
    pub paths: PathsCfg,
}

impl Config {
    /// 加载 config.json：默认配置 + 用户配置逐节浅合并（与 Python load_config 一致）
    pub fn load(path: &Path) -> Config {
        let defaults = serde_json::to_value(Config::default()).unwrap();
        let mut merged = defaults;
        if let Ok(text) = std::fs::read_to_string(path) {
            match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(user) => {
                    if let (Some(dm), Some(um)) = (merged.as_object_mut(), user.as_object()) {
                        for (k, v) in um {
                            dm.insert(k.clone(), v.clone());
                        }
                    }
                }
                Err(e) => crate::applog::warn(&format!("读取 config.json 失败（{}），使用默认配置", e)),
            }
        }
        serde_json::from_value(merged).unwrap_or_default()
    }

    pub fn vid_u16(&self) -> u16 { u16::from_str_radix(&self.device.vid, 16).unwrap_or(0x524D) }
    pub fn pid_u16(&self) -> u16 { u16::from_str_radix(&self.device.pid, 16).unwrap_or(0x4C52) }
    pub fn dev_tag(&self) -> String {
        format!("VID_{:04X}&PID_{:04X}", self.vid_u16(), self.pid_u16())
    }
}

/// exe 所在目录（源码运行=项目根；与 Python BASE_DIR 语义一致）
pub fn base_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
}
