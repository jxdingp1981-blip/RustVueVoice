// doubao.rs — 豆包 Web 云端语音识别（WebSocket 流式 ASR）会话
// 逐项移植 Python doubao_asr.py：URL/头/8192B PCM 块/JSON 事件/鉴权失效判定
#![allow(dead_code)]

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::applog;

fn util_epoch() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
use crate::creds::{
    AsrParams, AUTH_ERROR_CODE, AUTH_ERROR_KEYWORDS, ORIGIN, SEND_BLOCK_BYTES, WSS_BASE_URL,
};

#[derive(Debug)]
pub struct DoubaoAuthError(pub String);

/// 一次录音对应一个会话：feed（PCM i16 LE 字节）→ finish / abort
pub struct DoubaoSession {
    audio_tx: UnboundedSender<Vec<u8>>,
    ctrl_tx: UnboundedSender<CtrlMsg>,
    shared: Arc<SessionShared>,
    buf: Vec<u8>, // 攒块缓冲（feed 线程侧）
}

enum CtrlMsg {
    FinishSending,
    Abort,
}

#[derive(Default)]
struct SessionState {
    text: String,
    auth_failed: bool,
    error: Option<String>,
    done: bool,
}

struct SessionShared {
    state: Mutex<SessionState>,
    done_cv: Condvar,
    /// 最后一次收到 result 事件的时间（epoch 秒，0=未收到）
    last_result_at: Mutex<f64>,
    /// 累计文本版本号（判断是否仍在更新）
    text_version: Mutex<u64>,
}

impl DoubaoSession {
    /// on_partial：实时累计文本回调（在会话线程触发），传 None 则忽略
    pub fn new(params: AsrParams, on_partial: Option<Box<dyn Fn(&str) + Send>>) -> Self {
        let (audio_tx, audio_rx) = unbounded_channel::<Vec<u8>>();
        let (ctrl_tx, ctrl_rx) = unbounded_channel::<CtrlMsg>();
        let shared = Arc::new(SessionShared {
            state: Mutex::new(SessionState::default()),
            done_cv: Condvar::new(),
            last_result_at: Mutex::new(0.0),
            text_version: Mutex::new(0),
        });
        let shared2 = shared.clone();
        std::thread::Builder::new()
            .name("doubao-ws".into())
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
                rt.block_on(run(params, audio_rx, ctrl_rx, on_partial, shared2));
            })
            .ok();
        Self { audio_tx, ctrl_tx, shared, buf: Vec::new() }
    }

    /// 喂入 PCM i16 LE 字节（任意长度），内部按 8192 字节分块发送
    pub fn feed_bytes(&mut self, mut pcm: Vec<u8>) {
        self.buf.append(&mut pcm);
        while self.buf.len() >= SEND_BLOCK_BYTES {
            let block: Vec<u8> = self.buf.drain(..SEND_BLOCK_BYTES).collect();
            let _ = self.audio_tx.send(block);
        }
    }

    /// 冲刷尾部 → 停止发送 → 等末尾结果 → 断开。
    /// 服务端通常不发 finish 事件：以“最后一条 result 后静默 1.2s”作为收尾判据，
    /// 有文本时约 1.2~1.8s 返回；无文本时最多等 2.5s；硬超时兜底。
    /// 返回累计全量文本；凭证失效返回 Err(DoubaoAuthError)。
    pub fn finish(&mut self, timeout: f64) -> Result<String, DoubaoAuthError> {
        if !self.buf.is_empty() {
            let tail = std::mem::take(&mut self.buf);
            let _ = self.audio_tx.send(tail);
        }
        let _ = self.ctrl_tx.send(CtrlMsg::FinishSending);
        applog::info("音频发送完毕，等待服务端末尾结果");
        let start = Instant::now();
        let quiet_return = Duration::from_millis(1200); // 有文本：静默即返回
        let empty_return = Duration::from_millis(2500); // 无文本：再等一会儿
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
                    (util_epoch() - last) as f64
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
        let _ = self.ctrl_tx.send(CtrlMsg::Abort); // 确保 ws 关闭
        if st.auth_failed {
            return Err(DoubaoAuthError("豆包凭证失效（Cookie 过期或未登录）".into()));
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

    pub fn text(&self) -> String {
        self.shared.state.lock().unwrap().text.clone()
    }
}

fn mark_done(shared: &Arc<SessionShared>, mutate: impl FnOnce(&mut SessionState)) {
    {
        let mut st = shared.state.lock().unwrap();
        mutate(&mut st);
        st.done = true;
    }
    shared.done_cv.notify_all();
}

/// 构建 WSS URL（固定参数 + device_id/web_id/tea_uuid/web_tab_id）
fn build_url(p: &AsrParams) -> String {
    let mut q = url::form_urlencoded::Serializer::new(String::new());
    let pairs: [(&str, &str); 11] = [
        ("version_code", "20800"),
        ("language", "zh"),
        ("device_platform", "web"),
        ("aid", "497858"),
        ("real_aid", "497858"),
        ("pkg_type", "release_version"),
        ("pc_version", "3.12.3"),
        ("region", ""),
        ("sys_region", ""),
        ("samantha_web", "1"),
        ("use-olympus-account", "1"),
    ];
    for (k, v) in pairs {
        q.append_pair(k, v);
    }
    q.append_pair("format", "pcm");
    q.append_pair("device_id", &p.device_id);
    q.append_pair("web_id", &p.web_id);
    q.append_pair("tea_uuid", &p.web_id);
    q.append_pair("web_tab_id", &uuid::Uuid::new_v4().to_string());
    format!("{}?{}", WSS_BASE_URL, q.finish())
}

async fn run(
    params: AsrParams,
    mut audio_rx: UnboundedReceiver<Vec<u8>>,
    mut ctrl_rx: UnboundedReceiver<CtrlMsg>,
    on_partial: Option<Box<dyn Fn(&str) + Send>>,
    shared: Arc<SessionShared>,
) {
    let url = build_url(&params);
    // 手工构建完整握手请求（tungstenite 0.28 对 http::Request 不自动生成握手头）
    use base64::Engine as _;
    let mut key_raw = [0u8; 16];
    {
        // 随机 Sec-WebSocket-Key（16 字节）
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hasher};
        let mut seed = RandomState::new().build_hasher().finish();
        for b in key_raw.iter_mut() {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            *b = (seed >> 33) as u8;
        }
        // 再混入进程级熵
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        key_raw[0] ^= t as u8;
        key_raw[15] ^= (t >> 8) as u8;
    }
    let ws_key = base64::engine::general_purpose::STANDARD.encode(key_raw);
    let host = url::Url::parse(&url)
        .ok()
        .map(|u| match u.port() {
            Some(p) => format!("{}:{}", u.host_str().unwrap_or(""), p),
            None => u.host_str().unwrap_or("").to_string(),
        })
        .unwrap_or_default();
    let request = match http::Request::builder()
        .uri(&url)
        .header("Host", host)
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", ws_key)
        .header("Cookie", params.cookie_header())
        .header("Origin", ORIGIN)
        .body(())
    {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("构建请求失败: {}", e);
            applog::error(&msg);
            mark_done(&shared, |s| s.error = Some(msg));
            return;
        }
    };

    applog::info("连接豆包 ASR WebSocket ...");
    let ws_stream = tokio::time::timeout(
        Duration::from_secs(5),
        tokio_tungstenite::connect_async(request),
    )
    .await;
    let (mut ws, _resp) = match ws_stream {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            let msg = format!("豆包 ASR WebSocket 异常: {}", e);
            applog::error(&msg);
            mark_done(&shared, |s| s.error = Some(msg));
            return;
        }
        Err(_) => {
            let msg = "豆包 ASR WebSocket 连接超时（5s）".to_string();
            applog::error(&msg);
            mark_done(&shared, |s| s.error = Some(msg));
            return;
        }
    };
    applog::info("豆包 ASR 已连接");
    {
        // 连接前缓冲的块（feed 早于连接完成时积压在 channel）已在 audio_rx 中，
        // 自然按序冲刷——与 Python pending 缓冲等效。
        let mut first = true;
        while let Ok(block) = audio_rx.try_recv() {
            if first {
                applog::info("冲刷连接前缓冲的音频");
                first = false;
            }
            let payload: tokio_tungstenite::tungstenite::Bytes = block.into();
                    if ws.send(tokio_tungstenite::tungstenite::Message::Binary(payload)).await.is_err() {
                break;
            }
        }
    }

    let mut finishing = false;
    let mut closing = false;
    loop {
        tokio::select! {
            // 控制消息
            ctrl = ctrl_rx.recv() => {
                match ctrl {
                    Some(CtrlMsg::FinishSending) => {
                        finishing = true;
                        // 停止接收前先冲刷：把 channel 里残余音频全部发出（尾块不能丢）
                        while let Ok(block) = audio_rx.try_recv() {
                            let payload: tokio_tungstenite::tungstenite::Bytes = block.into();
                            if ws.send(tokio_tungstenite::tungstenite::Message::Binary(payload)).await.is_err() {
                                break;
                            }
                        }
                    }
                    Some(CtrlMsg::Abort) | None => {
                        closing = true;
                        let _ = ws.close(None).await;
                        break;
                    }
                }
            }
            // 上行音频
            audio = audio_rx.recv(), if !finishing => {
                if let Some(block) = audio {
                    let payload: tokio_tungstenite::tungstenite::Bytes = block.into();
                    if ws.send(tokio_tungstenite::tungstenite::Message::Binary(payload)).await.is_err() {
                        let msg: String = "豆包 ASR 发送失败（连接断开）".to_string();
                        applog::error(&msg);
                        mark_done(&shared, |s| {
                            if s.error.is_none() { s.error = Some(msg.clone()); }
                        });
                        break;
                    }
                }
            }
            // 下行消息
            msg = ws.next() => {
                match msg {
                    Some(Ok(m)) => {
                        if handle_message(m, &on_partial, &shared) {
                            // finish 事件：服务端确认会话结束
                            break;
                        }
                    }
                    Some(Err(e)) => {
                        let msg = format!("豆包 ASR WebSocket 异常: {}", e);
                        applog::error(&msg);
                        mark_done(&shared, |s| {
                            if s.error.is_none() { s.error = Some(msg.clone()); }
                        });
                        break;
                    }
                    None => {
                        // 连接关闭：已有文本时视为正常收尾（与 Python on_error 语义一致）
                        mark_done(&shared, |_| {});
                        break;
                    }
                }
            }
        }
    }
    if !closing {
        let _ = ws.close(None).await;
    }
    mark_done(&shared, |_| {});
    applog::info("豆包 ASR 已断开");
}

/// 返回 true 表示收到 finish（会话正常结束）
fn handle_message(
    m: tokio_tungstenite::tungstenite::Message,
    on_partial: &Option<Box<dyn Fn(&str) + Send>>,
    shared: &Arc<SessionShared>,
) -> bool {
    let text = match m {
        tokio_tungstenite::tungstenite::Message::Text(t) => t.to_string(),
        tokio_tungstenite::tungstenite::Message::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
        tokio_tungstenite::tungstenite::Message::Close(_) => return true,
        _ => return false,
    };
    let data: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let code = data.get("code").and_then(|v| v.as_i64()).unwrap_or(0);
    let event = data.get("event").and_then(|v| v.as_str()).unwrap_or("");
    let message = data.get("message").and_then(|v| v.as_str()).unwrap_or("");

    if code != 0 {
        let lower = message.to_lowercase();
        if code == AUTH_ERROR_CODE || AUTH_ERROR_KEYWORDS.iter().any(|kw| lower.contains(kw)) {
            applog::warn(&format!("豆包凭证失效: code={}, message={}", code, message));
            mark_done(shared, |s| s.auth_failed = true);
            return true;
        }
        applog::warn(&format!("豆包 ASR 返回异常码: code={}, message={}", code, message));
    }

    match event {
        "result" => {
            let t = data
                .pointer("/result/Text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if !t.is_empty() {
                {
                    let mut st = shared.state.lock().unwrap();
                    st.text = t.clone();
                }
                *shared.last_result_at.lock().unwrap() = util_epoch();
                if let Some(cb) = on_partial {
                    cb(&t);
                }
            }
            false
        }
        "finish" => {
            applog::info("豆包 ASR 会话结束（finish）");
            true
        }
        _ => false,
    }
}

/// 云链路测试 / --probe：把一段本地 WAV（16k mono s16le 原始字节）发给豆包并返回识别文本
pub fn probe(
    raw: &[u8],
    pace: f64,
    on_partial: std::sync::Arc<dyn Fn(&str) + Send + Sync>,
) -> Result<String, DoubaoAuthError> {
    let params = crate::creds::load_params()
        .ok_or_else(|| DoubaoAuthError("未找到豆包凭证，请先扫码登录".into()))?;
    let mut sess = DoubaoSession::new(params, Some(Box::new(move |t: &str| on_partial(t))));
    let chunk_sec = SEND_BLOCK_BYTES as f64 / 2.0 / crate::creds::AUDIO_SAMPLE_RATE as f64;
    for i in (0..raw.len()).step_by(SEND_BLOCK_BYTES) {
        let end = std::cmp::min(i + SEND_BLOCK_BYTES, raw.len());
        let _ = sess.audio_tx.send(raw[i..end].to_vec());
        std::thread::sleep(Duration::from_secs_f64(chunk_sec / pace));
    }
    sess.finish(10.0)
}
