// volc_asr.rs — 火山引擎「实时语音识别」双向流式 WebSocket（付费版）
// 端点: wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_async
// 鉴权: 握手头 X-Api-Key / X-Api-Resource-Id / X-Api-Request-Id / X-Api-Connect-Id
// 二进制协议: 4 字节头(版本|头长, 消息类型|标志, JSON|GZIP, 保留) + [seq] + [size] + gzip(payload)
//   FullClientRequest seq=1 (POS_SEQUENCE) → AudioOnlyRequest seq=2.. (POS_SEQUENCE)
//   → 末包 NEG_WITH_SEQUENCE(seq 取负, 空 payload)
// 参考: 官方文档 docs/doubao-voice/bidirectional-streaming-automatic-speech-recognition-websocket
#![allow(dead_code)]

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::io::{Read, Write};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue};
use tokio_tungstenite::tungstenite::Message;

use crate::applog;
use crate::asr_engine::AsrError;
use crate::config::VolcAsrCfg;
use crate::creds::SEND_BLOCK_BYTES;

// ---- 协议常量 ----
const PROTOCOL_VERSION_V1: u8 = 0b0001;
const CLIENT_FULL_REQUEST: u8 = 0b0001;
const CLIENT_AUDIO_ONLY_REQUEST: u8 = 0b0010;
const SERVER_FULL_RESPONSE: u8 = 0b1001;
const SERVER_ERROR_RESPONSE: u8 = 0b1111;
const POS_SEQUENCE: u8 = 0b0001;
const NEG_WITH_SEQUENCE: u8 = 0b0011;
const JSON_SERIALIZATION: u8 = 0b0001;
const GZIP_COMPRESSION: u8 = 0b0001;

fn util_epoch() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

// ---------------- 二进制协议 ----------------

fn build_header(message_type: u8, flags: u8) -> [u8; 4] {
    [
        (PROTOCOL_VERSION_V1 << 4) | 1,
        (message_type << 4) | flags,
        (JSON_SERIALIZATION << 4) | GZIP_COMPRESSION,
        0,
    ]
}

fn gzip_compress(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(data)
        .map_err(|e| format!("GZIP 压缩失败: {}", e))?;
    encoder.finish().map_err(|e| format!("GZIP 结束失败: {}", e))
}

fn gzip_decompress(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut decoder = GzDecoder::new(data);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .map_err(|e| format!("GZIP 解压失败: {}", e))?;
    Ok(out)
}

/// 首包（FullClientRequest）：seq=1，payload 为 JSON（gzip）
pub fn build_full_request(payload: &Value, seq: i32) -> Result<Vec<u8>, String> {
    let payload_bytes = gzip_compress(
        serde_json::to_string(payload)
            .map_err(|e| format!("序列化首包失败: {}", e))?
            .as_bytes(),
    )?;
    let mut request = Vec::new();
    request.extend(build_header(CLIENT_FULL_REQUEST, POS_SEQUENCE));
    request.extend(seq.to_be_bytes());
    request.extend((payload_bytes.len() as u32).to_be_bytes());
    request.extend(payload_bytes);
    Ok(request)
}

/// 音频包：seq 递增；is_last=true 时 flags=NEG_WITH_SEQUENCE 且 seq 取负、payload 为空
pub fn build_audio_request(seq: i32, audio_chunk: &[u8], is_last: bool) -> Result<Vec<u8>, String> {
    let payload = gzip_compress(audio_chunk)?;
    let (flags, signed_seq) = if is_last {
        (NEG_WITH_SEQUENCE, -seq)
    } else {
        (POS_SEQUENCE, seq)
    };
    let mut request = Vec::new();
    request.extend(build_header(CLIENT_AUDIO_ONLY_REQUEST, flags));
    request.extend(signed_seq.to_be_bytes());
    request.extend((payload.len() as u32).to_be_bytes());
    request.extend(payload);
    Ok(request)
}

#[derive(Debug, Clone, Default)]
pub struct ParsedResponse {
    pub code: i32,
    pub is_last_package: bool,
    pub sequence: i32,
    pub payload_msg: Option<Value>,
}

/// 解析服务端二进制响应（FullServerResponse / ErrorResponse）
pub fn parse_response(message: &[u8]) -> Result<ParsedResponse, String> {
    if message.len() < 4 {
        return Err("响应长度不足".into());
    }
    let mut resp = ParsedResponse::default();
    let header_size = (message[0] & 0x0f) as usize * 4;
    if message.len() < header_size {
        return Err("响应 header 长度异常".into());
    }
    let message_type = message[1] >> 4;
    let flags = message[1] & 0x0f;
    let serialization = message[2] >> 4;
    let compression = message[2] & 0x0f;
    let mut offset = header_size;

    let read_i32 = |off: usize| -> Result<i32, String> {
        let b = message
            .get(off..off + 4)
            .ok_or_else(|| "响应 i32 字段长度不足".to_string())?;
        Ok(i32::from_be_bytes(b.try_into().map_err(|_| "响应 i32 异常")?))
    };
    let read_u32 = |off: usize| -> Result<u32, String> {
        let b = message
            .get(off..off + 4)
            .ok_or_else(|| "响应 u32 字段长度不足".to_string())?;
        Ok(u32::from_be_bytes(b.try_into().map_err(|_| "响应 u32 异常")?))
    };

    if flags & 0x01 != 0 {
        resp.sequence = read_i32(offset)?;
        offset += 4;
    }
    if flags & 0x02 != 0 {
        resp.is_last_package = true;
    }

    if message_type == SERVER_FULL_RESPONSE {
        let _size = read_u32(offset)?;
        offset += 4;
    } else if message_type == SERVER_ERROR_RESPONSE {
        resp.code = read_i32(offset)?;
        offset += 4;
        let _size = read_u32(offset)?;
        offset += 4;
    } else {
        return Err(format!("未知响应消息类型: 0x{:x}", message_type));
    }

    if offset >= message.len() {
        return Ok(resp);
    }
    let mut payload = message[offset..].to_vec();
    if compression == GZIP_COMPRESSION {
        payload = gzip_decompress(&payload)?;
    }
    if serialization == JSON_SERIALIZATION {
        resp.payload_msg = Some(
            serde_json::from_slice(&payload)
                .map_err(|e| format!("解析响应 JSON 失败: {}", e))?,
        );
    }
    Ok(resp)
}

/// 从 payload_msg 提取累计识别文本（result.text；回退到 definite 分句拼接）
pub fn extract_text(payload_msg: Option<&Value>) -> String {
    let Some(payload) = payload_msg else { return String::new() };
    let Some(result) = payload.get("result") else { return String::new() };
    if let Some(t) = result.get("text").and_then(|v| v.as_str()) {
        if !t.trim().is_empty() {
            return t.to_string();
        }
    }
    // 回退：definite 分句拼接
    let Some(utterances) = result.get("utterances").and_then(|v| v.as_array()) else {
        return String::new();
    };
    let mut text = String::new();
    for u in utterances {
        if u.get("definite").and_then(|v| v.as_bool()).unwrap_or(false) {
            if let Some(t) = u.get("text").and_then(|v| v.as_str()) {
                text.push_str(t);
            }
        }
    }
    text
}

// ---------------- 会话 ----------------

enum CtrlMsg {
    FinishSending,
    Abort,
}

#[derive(Default)]
struct VolcState {
    text: String,
    error: Option<String>,
    done: bool,
}

struct VolcShared {
    state: Mutex<VolcState>,
    done_cv: Condvar,
    last_result_at: Mutex<f64>,
}

/// 一次录音对应一个会话：feed（PCM i16 LE 字节）→ finish / abort
pub struct VolcAsrSession {
    audio_tx: UnboundedSender<Vec<u8>>,
    ctrl_tx: UnboundedSender<CtrlMsg>,
    shared: Arc<VolcShared>,
    buf: Vec<u8>,
}

impl VolcAsrSession {
    /// on_partial：实时累计文本回调（会话线程触发）
    pub fn new(cfg: VolcAsrCfg, on_partial: Option<Box<dyn Fn(&str) + Send>>) -> Self {
        let (audio_tx, audio_rx) = unbounded_channel::<Vec<u8>>();
        let (ctrl_tx, ctrl_rx) = unbounded_channel::<CtrlMsg>();
        let shared = Arc::new(VolcShared {
            state: Mutex::new(VolcState::default()),
            done_cv: Condvar::new(),
            last_result_at: Mutex::new(0.0),
        });
        let shared2 = shared.clone();
        std::thread::Builder::new()
            .name("volc-ws".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        applog::error(&format!("创建 tokio 运行时失败: {}", e));
                        mark_done(&shared2, |s| s.error = Some(format!("runtime: {}", e)));
                        return;
                    }
                };
                rt.block_on(run(cfg, audio_rx, ctrl_rx, on_partial, shared2));
            })
            .ok();
        Self { audio_tx, ctrl_tx, shared, buf: Vec::new() }
    }

    /// 喂入 PCM i16 LE 字节（任意长度），内部按 SEND_BLOCK_BYTES 分块
    pub fn feed_bytes(&mut self, mut pcm: Vec<u8>) {
        self.buf.append(&mut pcm);
        while self.buf.len() >= SEND_BLOCK_BYTES {
            let block: Vec<u8> = self.buf.drain(..SEND_BLOCK_BYTES).collect();
            let _ = self.audio_tx.send(block);
        }
    }

    /// 冲刷尾部 → 发末包 → 等末尾结果。
    /// 服务端发 is_last_package 即返回；否则按“最后一条结果后静默”收尾判据兜底。
    pub fn finish(&mut self, timeout: f64) -> Result<String, AsrError> {
        if !self.buf.is_empty() {
            let tail = std::mem::take(&mut self.buf);
            let _ = self.audio_tx.send(tail);
        }
        let _ = self.ctrl_tx.send(CtrlMsg::FinishSending);
        applog::info("音频发送完毕，等待火山引擎末尾结果");
        let start = Instant::now();
        let quiet_return = Duration::from_millis(1200);
        let empty_return = Duration::from_millis(2500);
        let hard_timeout = Duration::from_secs_f64(timeout.clamp(0.5, 30.0));
        loop {
            {
                let st = self.shared.state.lock().unwrap();
                if st.done {
                    break;
                }
                let has_text = !st.text.is_empty();
                let last = *self.shared.last_result_at.lock().unwrap();
                let quiet = if last > 0.0 {
                    util_epoch() - last
                } else {
                    start.elapsed().as_secs_f64()
                };
                let enough = if has_text {
                    quiet >= quiet_return.as_secs_f64()
                } else {
                    quiet >= empty_return.as_secs_f64()
                };
                if enough {
                    break;
                }
            }
            if start.elapsed() >= hard_timeout {
                break;
            }
            std::thread::sleep(Duration::from_millis(80));
        }
        let st = self.shared.state.lock().unwrap();
        let _ = self.ctrl_tx.send(CtrlMsg::Abort);
        if let Some(err) = &st.error {
            return Err(AsrError::new(err.clone()));
        }
        Ok(st.text.clone())
    }

    pub fn abort(&self) {
        {
            let mut st = self.shared.state.lock().unwrap();
            st.done = true;
        }
        self.shared.done_cv.notify_all();
        let _ = self.ctrl_tx.send(CtrlMsg::Abort);
    }

    pub fn error(&self) -> Option<String> {
        self.shared.state.lock().unwrap().error.clone()
    }
}

fn mark_done(shared: &Arc<VolcShared>, mutate: impl FnOnce(&mut VolcState)) {
    {
        let mut st = shared.state.lock().unwrap();
        mutate(&mut st);
        st.done = true;
    }
    shared.done_cv.notify_all();
}

/// 构建握手请求（URL + X-Api-* 鉴权头）
fn build_request(cfg: &VolcAsrCfg) -> Result<tokio_tungstenite::tungstenite::http::Request<()>, String> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let mut request = cfg
        .url
        .as_str()
        .into_client_request()
        .map_err(|e| format!("创建火山 ASR 请求失败: {}", e))?;
    let headers = [
        ("X-Api-Key", cfg.api_key.trim().to_string()),
        ("X-Api-Resource-Id", cfg.resource_id.clone()),
        ("X-Api-Request-Id", request_id.clone()),
        ("X-Api-Connect-Id", request_id),
    ];
    for (name, value) in headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|e| format!("header 名称无效: {}", e))?;
        let value = HeaderValue::from_str(&value)
            .map_err(|e| format!("header 值无效: {}", e))?;
        request.headers_mut().insert(name, value);
    }
    Ok(request)
}

fn build_full_payload(cfg: &VolcAsrCfg) -> Value {
    json!({
        "user": { "uid": "rustvuevoice" },
        "audio": {
            "format": "pcm",
            "codec": "raw",
            "rate": 16000,
            "bits": 16,
            "channel": 1,
        },
        "request": {
            "model_name": cfg.model_name,
            "enable_nonstream": cfg.enable_nonstream,
            "enable_punc": cfg.enable_punc,
            "enable_itn": cfg.enable_itn,
            "enable_ddc": cfg.enable_ddc,
            "show_utterances": cfg.show_utterances,
            "result_type": "full",
            "end_window_size": cfg.end_window_size,
        },
    })
}

/// 握手/服务错误 → 面向用户的可读信息
fn friendly_error(detail: &str) -> String {
    let d = detail.to_lowercase();
    if d.contains("401") || d.contains("403") || d.contains("forbidden") || d.contains("unauthorized") {
        return "火山引擎鉴权失败：API Key 无效或未开通流式语音识别 2.0（小时版）\
                \n请在「引擎设置」检查 API Key，并确认已在火山引擎控制台开通服务。".into();
    }
    if d.contains("timeout") || d.contains("timed out") {
        return "火山引擎连接超时，请检查网络/代理。".into();
    }
    format!("火山引擎 ASR 错误: {}", detail)
}

async fn run(
    cfg: VolcAsrCfg,
    mut audio_rx: UnboundedReceiver<Vec<u8>>,
    mut ctrl_rx: UnboundedReceiver<CtrlMsg>,
    on_partial: Option<Box<dyn Fn(&str) + Send>>,
    shared: Arc<VolcShared>,
) {
    if cfg.api_key.trim().is_empty() {
        mark_done(&shared, |s| {
            s.error = Some("未配置火山引擎 API Key（主窗口「引擎设置」中填写）".into())
        });
        return;
    }
    let request = match build_request(&cfg) {
        Ok(r) => r,
        Err(e) => {
            applog::error(&e);
            mark_done(&shared, |s| s.error = Some(e));
            return;
        }
    };

    applog::info("连接火山引擎实时语音识别 WebSocket ...");
    let ws_stream = tokio::time::timeout(Duration::from_secs(8), tokio_tungstenite::connect_async(request)).await;
    let (mut ws, _resp) = match ws_stream {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            let msg = friendly_error(&e.to_string());
            applog::error(&format!("火山引擎 WebSocket 异常: {}（{}）", e, msg));
            mark_done(&shared, |s| s.error = Some(msg));
            return;
        }
        Err(_) => {
            let msg = "火山引擎连接超时（8s）".to_string();
            applog::error(&msg);
            mark_done(&shared, |s| s.error = Some(msg));
            return;
        }
    };
    applog::info("火山引擎 ASR 已连接");

    // 首包（FullClientRequest，seq=1）
    let full = match build_full_request(&build_full_payload(&cfg), 1) {
        Ok(f) => f,
        Err(e) => {
            applog::error(&e);
            mark_done(&shared, |s| s.error = Some(e));
            return;
        }
    };
    if let Err(e) = ws.send(Message::Binary(full.into())).await {
        let msg = format!("火山引擎首包发送失败: {}", e);
        applog::error(&msg);
        mark_done(&shared, |s| s.error = Some(msg));
        return;
    }

    let mut seq = 1usize; // 音频包从 2 开始
    let mut finishing = false;
    let mut closing = false;
    loop {
        tokio::select! {
            ctrl = ctrl_rx.recv() => {
                match ctrl {
                    Some(CtrlMsg::FinishSending) => {
                        finishing = true;
                        // 冲刷残余音频；最后发一个空 payload 的负 seq 末包
                        while let Ok(block) = audio_rx.try_recv() {
                            seq += 1;
                            match build_audio_request(seq as i32, &block, false) {
                                Ok(f) => {
                                    if ws.send(Message::Binary(f.into())).await.is_err() {
                                        break;
                                    }
                                }
                                Err(e) => { applog::error(&e); break; }
                            }
                        }
                        seq += 1;
                        match build_audio_request(seq as i32, &[], true) {
                            Ok(last) => {
                                if ws.send(Message::Binary(last.into())).await.is_err() {
                                    let msg = "火山引擎末包发送失败（连接断开）".to_string();
                                    applog::error(&msg);
                                    mark_done(&shared, |s| { if s.error.is_none() { s.error = Some(msg); } });
                                }
                            }
                            Err(e) => applog::error(&e),
                        }
                    }
                    Some(CtrlMsg::Abort) | None => {
                        closing = true;
                        let _ = ws.close(None).await;
                        break;
                    }
                }
            }
            audio = audio_rx.recv(), if !finishing => {
                if let Some(block) = audio {
                    seq += 1;
                    match build_audio_request(seq as i32, &block, false) {
                        Ok(frame) => {
                            if ws.send(Message::Binary(frame.into())).await.is_err() {
                                let msg = "火山引擎发送失败（连接断开）".to_string();
                                applog::error(&msg);
                                mark_done(&shared, |s| { if s.error.is_none() { s.error = Some(msg); } });
                                break;
                            }
                        }
                        Err(e) => { applog::error(&e); break; }
                    }
                }
            }
            msg = ws.next() => {
                match msg {
                    Some(Ok(Message::Binary(data))) => {
                        match parse_response(&data) {
                            Ok(parsed) => {
                                if parsed.code != 0 {
                                    let msg = format!("火山引擎返回错误 code={}（请检查 API Key 与服务开通状态）", parsed.code);
                                    applog::warn(&msg);
                                    mark_done(&shared, |s| s.error = Some(msg));
                                    break;
                                }
                                if let Some(text) = extract_text(parsed.payload_msg.as_ref()).empty_to_none() {
                                    {
                                        let mut st = shared.state.lock().unwrap();
                                        st.text = text.clone();
                                    }
                                    *shared.last_result_at.lock().unwrap() = util_epoch();
                                    if let Some(cb) = on_partial.as_ref() {
                                        cb(&text);
                                    }
                                }
                                if parsed.is_last_package {
                                    applog::info("火山引擎 ASR 会话结束（is_last_package）");
                                    break;
                                }
                            }
                            Err(e) => {
                                let msg = format!("火山引擎响应解析失败: {}", e);
                                applog::error(&msg);
                                mark_done(&shared, |s| { if s.error.is_none() { s.error = Some(msg); } });
                                break;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) => {
                        // 服务端正常关闭：已有文本视为收尾
                        break;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        let msg = format!("火山引擎 WebSocket 异常: {}", e);
                        applog::error(&msg);
                        mark_done(&shared, |s| { if s.error.is_none() { s.error = Some(msg); } });
                        break;
                    }
                    None => break,
                }
            }
        }
    }
    if !closing {
        let _ = ws.close(None).await;
    }
    mark_done(&shared, |_| {});
    applog::info("火山引擎 ASR 已断开");
}

trait EmptyToNone {
    fn empty_to_none(self) -> Option<String>;
}
impl EmptyToNone for String {
    fn empty_to_none(self) -> Option<String> {
        if self.trim().is_empty() {
            None
        } else {
            Some(self)
        }
    }
}

/// 云链路测试 / --probe：把一段 WAV（16k mono s16le 原始字节）发给火山引擎并返回识别文本
pub fn probe(
    cfg: VolcAsrCfg,
    raw: &[u8],
    pace: f64,
    on_partial: std::sync::Arc<dyn Fn(&str) + Send + Sync>,
) -> Result<String, AsrError> {
    if cfg.api_key.trim().is_empty() {
        return Err(AsrError::new("未配置火山引擎 API Key，请在「引擎设置」中填写"));
    }
    let mut sess = VolcAsrSession::new(cfg, Some(Box::new(move |t: &str| on_partial(t))));
    let chunk_sec = SEND_BLOCK_BYTES as f64 / 2.0 / crate::creds::AUDIO_SAMPLE_RATE as f64;
    for i in (0..raw.len()).step_by(SEND_BLOCK_BYTES) {
        let end = std::cmp::min(i + SEND_BLOCK_BYTES, raw.len());
        let _ = sess.audio_tx.send(raw[i..end].to_vec());
        std::thread::sleep(Duration::from_secs_f64(chunk_sec / pace));
    }
    sess.finish(10.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_request_has_expected_header_and_sequence() {
        let packet = build_full_request(&json!({"audio": {"format": "pcm"}}), 1).unwrap();
        assert_eq!(packet[0], 0x11);
        assert_eq!(packet[1], 0x11); // FULL_CLIENT_REQUEST(1) | POS_SEQUENCE(1)
        assert_eq!(packet[2], 0x11); // JSON(1) | GZIP(1)
        assert_eq!(i32::from_be_bytes(packet[4..8].try_into().unwrap()), 1);
    }

    #[test]
    fn last_audio_request_uses_negative_sequence() {
        let packet = build_audio_request(9, b"pcm", true).unwrap();
        assert_eq!(packet[1], 0x23); // AUDIO_ONLY(2) | NEG_WITH_SEQUENCE(3)
        assert_eq!(i32::from_be_bytes(packet[4..8].try_into().unwrap()), -9);
    }

    #[test]
    fn audio_request_positive_sequence() {
        let packet = build_audio_request(2, b"pcm", false).unwrap();
        assert_eq!(packet[1], 0x21); // AUDIO_ONLY(2) | POS_SEQUENCE(1)
        assert_eq!(i32::from_be_bytes(packet[4..8].try_into().unwrap()), 2);
    }

    #[test]
    fn parses_server_full_response() {
        let payload =
            gzip_compress("{\"result\":{\"text\":\"你好世界\"}}".as_bytes()).unwrap();
        let mut packet = Vec::new();
        packet.extend([0x11, 0x91, 0x11, 0x00]); // SERVER_FULL_RESPONSE | POS_SEQUENCE
        packet.extend(3_i32.to_be_bytes());
        packet.extend((payload.len() as u32).to_be_bytes());
        packet.extend(payload);
        let parsed = parse_response(&packet).unwrap();
        assert_eq!(parsed.sequence, 3);
        assert!(parsed.code == 0);
        assert_eq!(
            extract_text(parsed.payload_msg.as_ref()),
            "你好世界"
        );
    }

    #[test]
    fn parses_error_response() {
        let mut packet = Vec::new();
        packet.extend([0x11, 0xf0, 0x01, 0x00]); // ERROR_RESPONSE | NO_SEQUENCE
        packet.extend(3001_i32.to_be_bytes());
        packet.extend(0_u32.to_be_bytes());
        let parsed = parse_response(&packet).unwrap();
        assert_eq!(parsed.code, 3001);
    }

    #[test]
    fn parse_response_rejects_garbage() {
        assert!(parse_response(&[0x11]).is_err());
        assert!(parse_response(&[]).is_err());
    }
}
