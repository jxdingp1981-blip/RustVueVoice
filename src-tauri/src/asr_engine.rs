// asr_engine.rs — 识别引擎抽象：豆包网页版（免费）与火山引擎付费版共用同一会话接口
#![allow(dead_code)]

use crate::doubao::DoubaoSession;
use crate::volc_asr::VolcAsrSession;

/// 识别失败。auth=true 表示网页版凭证失效（需要重新登录），
/// false 表示普通错误（网络 / API Key / 服务异常等）。
#[derive(Debug, Clone)]
pub struct AsrError {
    pub msg: String,
    pub auth: bool,
}

impl AsrError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self { msg: msg.into(), auth: false }
    }
    pub fn auth(msg: impl Into<String>) -> Self {
        Self { msg: msg.into(), auth: true }
    }
}

impl std::fmt::Display for AsrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.msg)
    }
}

/// 一次录音对应一个识别会话：feed（PCM i16 LE 字节）→ finish / abort
pub trait AsrSession: Send {
    /// 喂入 PCM i16 LE 字节（任意长度）
    fn feed_bytes(&mut self, pcm: Vec<u8>);
    /// 冲刷尾部 → 停止发送 → 等末尾结果。返回累计全量文本。
    fn finish(&mut self, timeout: f64) -> Result<String, AsrError>;
    /// 放弃会话（短按取消等）
    fn abort(&self);
    /// 连接层错误（区别于识别结果为空）
    fn error(&self) -> Option<String>;
}

impl AsrSession for DoubaoSession {
    fn feed_bytes(&mut self, pcm: Vec<u8>) {
        DoubaoSession::feed_bytes(self, pcm);
    }
    fn finish(&mut self, timeout: f64) -> Result<String, AsrError> {
        DoubaoSession::finish(self, timeout)
            .map_err(|e| AsrError::auth(e.0))
    }
    fn abort(&self) {
        DoubaoSession::abort(self);
    }
    fn error(&self) -> Option<String> {
        DoubaoSession::error(self)
    }
}

impl AsrSession for VolcAsrSession {
    fn feed_bytes(&mut self, pcm: Vec<u8>) {
        VolcAsrSession::feed_bytes(self, pcm);
    }
    fn finish(&mut self, timeout: f64) -> Result<String, AsrError> {
        VolcAsrSession::finish(self, timeout)
    }
    fn abort(&self) {
        VolcAsrSession::abort(self);
    }
    fn error(&self) -> Option<String> {
        VolcAsrSession::error(self)
    }
}
