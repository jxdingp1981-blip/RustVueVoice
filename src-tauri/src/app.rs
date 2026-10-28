// app.rs — 主状态机：按键事件 → 录音会话（HID 命令+mSBC 解码+豆包会话）→ 上屏
// 移植自 RustVoiceMouse（原生 Win32 控件版）：UI 事件改经 Tauri emit 发往 Vue 前端
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tauri::{AppHandle, Emitter};

use crate::asr_engine::AsrSession;
use crate::config::{AsrCfg, Config};
use crate::creds::{self};
use crate::doubao::DoubaoSession;
use crate::volc_asr::VolcAsrSession;
use crate::hid::{find_voice_device, HidDeviceInfo, HidStream};
use crate::msbc::MsbcDecoder;
use crate::output;
use crate::overlay::{OverlayEvent, OverlayHandle};
use crate::rawinput::{key_name, RawInputKeyListener};
use crate::util;
use crate::wav;

// ---- 接收器命令帧（来自官方驱动反汇编） ----
pub const CMD_START_RECORD: [u8; 8] = [0x00, 0xA5, 0x5A, 0x07, 0x52, 0x03, 0x01, 0xFF];
pub const CMD_STOP_RECORD: [u8; 8] = [0x00, 0xA5, 0x5A, 0x07, 0x52, 0x03, 0x00, 0xFF];
pub const CMD_INFO_QUERY: [u8; 7] = [0x00, 0xA5, 0x5A, 0x06, 0x52, 0x01, 0xFF];
pub const CMD_BATTERY_QUERY: [u8; 8] = [0x00, 0x9D, 0xA5, 0x5A, 0x05, 0x0A, 0x00, 0xFF];
pub const BATTERY_INTERVAL_SECS: u64 = 1800; // 后台 30 分钟一查（低频保底）
const BATTERY_ONDEMAND_MIN_GAP_SECS: f64 = 30.0; // 按需查询节流间隔
// 其余时机靠事件触发（request_battery_refresh）：连接成功、按下语音键（鼠标必然醒着）、
// 主控窗口被激活（用户正在看状态栏）——保证"极少查询但看到的电量始终新鲜"

/// 判断 65 字节 HID 报告的 payload 是否为 mSBC 音频帧（与驱动解码条件一致）
pub fn is_audio_frame(payload: &[u8]) -> bool {
    payload.len() >= 8
        && payload[0] == 0x9D
        && payload[1] == 0x01
        && (payload[2] & 0x0F) == 8
        && payload[3] == 0xAD
}

/// 解析电池应答帧 a5 5a 09 54 06 [电压低][电压高] ff，返回电量百分比 0~100。
/// 电压单位 10mV；官方驱动（Rulink.exe 0x42889e 处）把电压钳位到 320~420 后按
/// (V-320)*100/(420-320) 线性换算，即 3.20V→0%、4.20V→100%。非电池应答帧返回 None。
pub fn parse_battery_frame(payload: &[u8]) -> Option<u32> {
    // 兼容报告 ID 后多一个设备前缀字节的读路径
    for off in [0usize, 1usize] {
        if payload.len() >= off + 8 && payload[off..off + 2] == [0xA5, 0x5A] {
            let p = &payload[off..];
            if p[2] != 0x09 || p[3] != 0x54 || p[4] != 0x06 || p[7] != 0xFF {
                return None;
            }
            let volts = p[5] as u32 | ((p[6] as u32) << 8); // 小端，单位 10mV
            let volts = volts.clamp(320, 420);
            return Some((volts - 320) * 100 / (420 - 320));
        }
    }
    None
}

static GLOBAL_APP: OnceLock<Arc<AppShared>> = OnceLock::new();

pub fn register_global(app: Arc<AppShared>) {
    let _ = GLOBAL_APP.set(app);
}

pub fn global_app() -> Option<Arc<AppShared>> {
    GLOBAL_APP.get().cloned()
}

/// 累积一次录音：解码后的 PCM + 时间戳
pub struct RecordingSession {
    pub vk: u16,
    pub key_name: String,
    pcm: Mutex<Vec<i16>>,
    packet_count: AtomicU64,
    first_ts: Mutex<Option<f64>>,
    last_ts: Mutex<Option<f64>>,
    pub asr: Mutex<Option<Box<dyn AsrSession>>>,
}

impl RecordingSession {
    fn new(vk: u16) -> Self {
        Self {
            vk,
            key_name: key_name(vk),
            pcm: Mutex::new(Vec::new()),
            packet_count: AtomicU64::new(0),
            first_ts: Mutex::new(None),
            last_ts: Mutex::new(None),
            asr: Mutex::new(None),
        }
    }

    fn add_pcm(&self, pcm: &[i16], ts: f64) {
        let mut buf = self.pcm.lock().unwrap();
        buf.extend_from_slice(pcm);
        self.packet_count.fetch_add(1, Ordering::SeqCst);
        let mut f = self.first_ts.lock().unwrap();
        if f.is_none() {
            *f = Some(ts);
        }
        *self.last_ts.lock().unwrap() = Some(ts);
    }

    /// 最近一帧的 RMS 电平（0~1）
    fn level(&self) -> f64 {
        let buf = self.pcm.lock().unwrap();
        if buf.is_empty() {
            return 0.0;
        }
        let tail = &buf[buf.len().saturating_sub(120)..];
        if tail.is_empty() {
            return 0.0;
        }
        let sum: f64 = tail.iter().map(|&s| (s as f64) * (s as f64)).sum();
        let rms = (sum / tail.len() as f64).sqrt();
        (rms / 3000.0).min(1.0)
    }

    /// (int16 样本, 时长秒)
    fn collect(&self) -> (Vec<i16>, f64) {
        let pcm = self.pcm.lock().unwrap().clone();
        let f = *self.first_ts.lock().unwrap();
        let l = *self.last_ts.lock().unwrap();
        let duration = match (f, l) {
            (Some(a), Some(b)) if b > a => b - a,
            _ => 0.0,
        };
        (pcm, duration)
    }
}

pub struct AppShared {
    pub cfg: Config,
    pub base_dir: PathBuf,
    recordings: PathBuf,

    vid_pid_mi: (u16, u16, String),
    dev_tag: String,

    // 识别引擎运行时配置（可在「引擎设置」中切换并持久化）
    pub(crate) asr: Mutex<AsrCfg>,
    pub(crate) logged_in: AtomicBool,
    device_connected: AtomicBool,
    listening: AtomicBool,
    stopping: AtomicBool,
    pub(crate) login_pending: AtomicBool,
    device_first_fail: AtomicBool,
    // 电量：存 percent+1（0 = 未读到），避免 percent=0 与"未读"混淆
    battery_known_pct: AtomicU64,
    // 上次实际发出电量查询的时刻（epoch 秒），用于按需查询节流
    last_battery_query: Mutex<f64>,

    press_t: Mutex<f64>,
    prev_short_t: Mutex<f64>,
    short_gen: AtomicU64,
    session: Mutex<Option<Arc<RecordingSession>>>,
    hid: Mutex<Option<Arc<HidStream>>>,
    level_last: Mutex<f64>,

    key_tx: Sender<(u16, bool)>,
    key_rx: Mutex<Option<Receiver<(u16, bool)>>>,
    frame_tx: Sender<(Vec<u8>, f64)>,
    frame_rx: Mutex<Option<Receiver<(Vec<u8>, f64)>>>,
    output_lock: Mutex<()>,

    overlay: OnceLock<OverlayHandle>,
    /// Tauri 应用句柄（setup 阶段注入），用于向前端广播事件
    ui: Mutex<Option<AppHandle>>,
    /// 前端就绪前的日志积压（listen 建立后由 frontend_ready 命令一次性领取）
    log_backlog: Mutex<Vec<String>>,
    log_attached: AtomicBool,
    raw: Mutex<Option<RawInputKeyListener>>,
}

impl AppShared {
    pub fn new(cfg: Config, base_dir: PathBuf) -> Arc<Self> {
        let (key_tx, key_rx) = channel::<(u16, bool)>();
        let (frame_tx, frame_rx) = channel::<(Vec<u8>, f64)>();
        let recordings = resolve_dir(&base_dir, &cfg.paths.recordings_dir);
        let _ = std::fs::create_dir_all(&recordings);
        let vid = cfg.vid_u16();
        let pid = cfg.pid_u16();
        let mi = cfg.device.mi.clone();
        let dev_tag = cfg.dev_tag();
        let asr_cfg = cfg.asr.clone();
        Arc::new(Self {
            logged_in: AtomicBool::new(creds::load_params().is_some()),
            cfg,
            base_dir,
            recordings,
            vid_pid_mi: (vid, pid, mi),
            dev_tag,
            asr: Mutex::new(asr_cfg),
            device_connected: AtomicBool::new(false),
            listening: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            login_pending: AtomicBool::new(false),
            device_first_fail: AtomicBool::new(true),
            battery_known_pct: AtomicU64::new(0),
            last_battery_query: Mutex::new(0.0),
            press_t: Mutex::new(0.0),
            prev_short_t: Mutex::new(0.0),
            short_gen: AtomicU64::new(0),
            session: Mutex::new(None),
            hid: Mutex::new(None),
            level_last: Mutex::new(0.0),
            key_tx,
            key_rx: Mutex::new(Some(key_rx)),
            frame_tx,
            frame_rx: Mutex::new(Some(frame_rx)),
            output_lock: Mutex::new(()),
            overlay: OnceLock::new(),
            ui: Mutex::new(None),
            log_backlog: Mutex::new(Vec::new()),
            log_attached: AtomicBool::new(false),
            raw: Mutex::new(None),
        })
    }

    pub fn set_overlay(&self, h: OverlayHandle) {
        let _ = self.overlay.set(h);
    }
    pub fn set_ui(&self, h: AppHandle) {
        *self.ui.lock().unwrap() = Some(h);
    }

    /// 前端未监听时的日志缓冲（上限 1000 行）
    pub fn push_log_backlog(&self, line: String) {
        let mut g = self.log_backlog.lock().unwrap();
        if g.len() >= 1000 {
            let cut = g.len() + 1 - 1000;
            g.drain(..cut);
        }
        g.push(line);
    }

    /// 前端就绪：翻转 attached 标记并返回积压日志（此后日志走实时广播）
    pub fn take_log_backlog(&self) -> Vec<String> {
        let mut g = self.log_backlog.lock().unwrap();
        self.log_attached.store(true, Ordering::SeqCst);
        std::mem::take(&mut *g)
    }

    pub fn log_is_attached(&self) -> bool {
        self.log_attached.load(Ordering::SeqCst)
    }

    fn overlay_post(&self, ev: OverlayEvent) {
        if let Some(o) = self.overlay.get() {
            o.post(ev);
        }
    }

    // ---- 前端事件广播 ----
    fn emit(&self, event: &str, payload: serde_json::Value) {
        if let Some(h) = self.ui.lock().unwrap().as_ref() {
            let _ = h.emit(event, payload);
        }
    }
    pub fn post_status(&self, key: &str, val: String) {
        self.emit("status", serde_json::json!({ "key": key, "value": val }));
    }
    pub fn post_login_done(&self, ok: bool) {
        self.emit("login-done", serde_json::json!({ "ok": ok }));
    }
    pub fn post_test_done(&self, summary: String) {
        self.emit("test-done", serde_json::json!({ "summary": summary }));
    }
    pub fn post_partial(&self, text: String) {
        self.emit("partial", serde_json::json!({ "text": text }));
    }
    pub fn post_session(&self, state: &str) {
        self.emit("session", serde_json::json!({ "state": state }));
    }

    // ---- UI 线程读取的状态 ----
    pub fn engine_is_doubao(&self) -> bool {
        self.asr.lock().unwrap().engine == "doubao"
    }
    pub fn engine_display(&self) -> String {
        if self.engine_is_doubao() {
            "豆包云端".into()
        } else {
            "火山引擎·流式2.0".into()
        }
    }

    // ---- 引擎配置切换（前端「引擎设置」） ----

    /// 更新引擎与 API Key：内存生效 + 持久化到 config.json（保留其它字段）
    pub fn set_asr_config(&self, engine: &str, api_key: &str) -> Result<(), String> {
        if engine != "doubao" && engine != "volc" {
            return Err(format!("未知识别引擎: {}", engine));
        }
        let key = api_key.trim().to_string();
        if engine == "volc" && key.is_empty() {
            return Err("启用火山引擎前请先填写 API Key".into());
        }
        {
            let mut a = self.asr.lock().unwrap();
            a.engine = engine.to_string();
            a.volc.api_key = key;
        }
        self.persist_asr_config(engine, api_key.trim())?;
        let display = self.engine_display();
        self.post_status("engine", display.clone());
        crate::applog::info(&format!("识别引擎已切换为 {}（config.json 已保存）", display));
        Ok(())
    }

    /// 把引擎选择与 API Key 写回 exe 旁的 config.json（读-改-写，保留其它字段）
    fn persist_asr_config(&self, engine: &str, api_key: &str) -> Result<(), String> {
        let path = self.base_dir.join("config.json");
        let mut root: serde_json::Value = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        if !root.is_object() {
            root = serde_json::json!({});
        }
        let obj = root.as_object_mut().unwrap();
        let asr_node = obj.entry("asr").or_insert_with(|| serde_json::json!({}));
        let Some(am) = asr_node.as_object_mut() else {
            return Err("config.json 的 asr 节格式错误".into());
        };
        am.insert("engine".into(), serde_json::json!(engine));
        let volc_node = am.entry("volc").or_insert_with(|| serde_json::json!({}));
        let Some(vm) = volc_node.as_object_mut() else {
            return Err("config.json 的 asr.volc 节格式错误".into());
        };
        vm.insert("api_key".into(), serde_json::json!(api_key));
        let data = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, data).map_err(|e| format!("写入 config.json 失败: {}", e))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("保存 config.json 失败: {}", e))?;
        Ok(())
    }
    pub fn logged_in(&self) -> bool {
        self.logged_in.load(Ordering::SeqCst)
    }
    pub fn device_connected(&self) -> bool {
        self.device_connected.load(Ordering::SeqCst)
    }
    pub fn listening(&self) -> bool {
        self.listening.load(Ordering::SeqCst)
    }
    /// 最近一次读到的鼠标电量（未读到返回 None）
    pub fn battery_percent(&self) -> Option<u32> {
        let v = self.battery_known_pct.load(Ordering::SeqCst);
        if v == 0 {
            None
        } else {
            Some((v - 1) as u32)
        }
    }
    pub fn recordings_dir(&self) -> String {
        self.recordings.display().to_string()
    }
    pub fn shutdown(&self) {
        self.stopping.store(true, Ordering::SeqCst);
    }
    pub fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    /// 启动所有后台线程（幂等）
    pub fn start(self: &Arc<Self>) {
        {
            let mut raw = self.raw.lock().unwrap();
            if raw.is_none() {
                let tx = self.key_tx.clone();
                let tag = self.dev_tag.clone();
                *raw = Some(RawInputKeyListener::start(tx, tag));
                self.listening.store(true, Ordering::SeqCst);
            }
        }
        let app = self.clone();
        std::thread::Builder::new().name("keys".into()).spawn(move || app.key_worker()).ok();
        let app = self.clone();
        std::thread::Builder::new().name("decode".into()).spawn(move || app.decode_worker()).ok();
        let app = self.clone();
        std::thread::Builder::new().name("device".into()).spawn(move || app.device_worker()).ok();

        // 初始状态（前端状态卡）
        self.post_status("engine", self.engine_display());
        self.post_status(
            "login",
            if self.logged_in() { "已登录".into() } else { "未登录".into() },
        );
        self.post_status(
            "listen",
            if self.listening() { "运行中".into() } else { "-".into() },
        );
        if self.logged_in() {
            applog_info("豆包凭证已加载（与 Python/Rust 版共用）");
        } else {
            applog_info("豆包未登录：点击「扫码登录」或在「引擎设置」切换火山引擎后即可使用语音输入");
        }
        applog_info(&format!("就绪（{}）：长按鼠标“录音键”说话，松开自动上屏", self.engine_display()));
    }

    pub fn find_device(&self) -> Result<HidDeviceInfo, String> {
        let (vid, pid, mi) = self.vid_pid_mi.clone();
        match find_voice_device(vid, pid, &mi) {
            Some(d) => {
                crate::applog::info(&format!("找到语音接口: {}", d.describe()));
                Ok(d)
            }
            None => Err(format!(
                "未找到 Rulink 接收器的语音接口（{} MI_{}）。\n请确认：1) 接收器已插入 USB；2) 鼠标已拨到 ON 并处于 2.4G 模式；\n        3) 官方驱动未运行（任务管理器结束 Rulink 相关进程）。",
                self.dev_tag, self.vid_pid_mi.2
            )),
        }
    }

    fn device_worker(&self) {
        while !self.stopping.load(Ordering::SeqCst) {
            if self.hid.lock().unwrap().is_some() {
                return;
            }
            match self.find_device() {
                Err(msg) => {
                    if self.device_first_fail.swap(false, Ordering::SeqCst) {
                        crate::applog::warn("接收器未连接，将自动重试（插入后 5 秒内生效）");
                        crate::applog::info(&msg);
                    }
                    self.post_status("device", "未连接（自动重试中）".into());
                    std::thread::sleep(Duration::from_secs(5));
                }
                Ok(dev) => {
                    let (tx, rx) = channel::<(Vec<u8>, f64)>();
                    let stream = Arc::new(HidStream::start(dev, tx));
                    let app = global_app().unwrap_or_else(|| unreachable_app());
                    std::thread::Builder::new().name("hid-packets".into()).spawn(move || {
                        for (data, ts) in rx {
                            app.on_hid_packet(&data, ts);
                        }
                    }).ok();
                    stream.write(&CMD_INFO_QUERY); // 与驱动一致：连接后发信息查询唤醒语音通道
                    *self.hid.lock().unwrap() = Some(stream);
                    self.device_connected.store(true, Ordering::SeqCst);
                    let app2 = global_app().unwrap_or_else(|| unreachable_app());
                    std::thread::Builder::new().name("battery".into()).spawn(move || {
                        app2.battery_worker();
                    }).ok();
                    self.post_status("device", "已连接".into());
                    self.post_session("idle");
                    crate::applog::info("✓ 接收器已连接");
                    return;
                }
            }
        }
    }

    /// 电量轮询（连接后立即先读一次；此后低频 30 分钟保底，新鲜度靠事件触发；
    /// 鼠标休眠时写入成功但设备沉默属正常，唤醒后 0.2 秒内即有应答）
    fn battery_worker(&self) {
        while !self.stopping.load(Ordering::SeqCst) {
            let ok = self
                .hid
                .lock()
                .unwrap()
                .as_ref()
                .map(|h| h.write(&CMD_BATTERY_QUERY))
                .unwrap_or(false);
            *self.last_battery_query.lock().unwrap() = util::epoch_now();
            if ok {
                crate::applog::info("已发送电量查询命令");
            } else {
                crate::applog::warn("电量查询命令发送失败");
            }
            for _ in 0..BATTERY_INTERVAL_SECS {
                if self.stopping.load(Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }

    /// 按需刷新电量（事件驱动：连接成功 / 按下语音键 / 主窗口激活）。
    /// 30 秒节流，避免重复打扰设备；鼠标刚被使用时必然醒着，应答 0.2 秒内即到。
    pub fn request_battery_refresh(&self) {
        {
            let now = util::epoch_now();
            let mut last = self.last_battery_query.lock().unwrap();
            if now - *last < BATTERY_ONDEMAND_MIN_GAP_SECS {
                return;
            }
            *last = now;
        }
        let ok = self
            .hid
            .lock()
            .unwrap()
            .as_ref()
            .map(|h| h.write(&CMD_BATTERY_QUERY))
            .unwrap_or(false);
        if ok {
            crate::applog::info("已按需查询鼠标电量");
        }
    }

    fn on_hid_packet(&self, data: &[u8], ts: f64) {
        if data.len() < 2 || data[0] != 0 {
            return;
        }
        let payload = &data[1..];
        if let Some(pct) = parse_battery_frame(payload) {
            // 仅在数值变化时写日志/刷新 UI（同值静默）
            let newv = pct as u64 + 1;
            let prev = self.battery_known_pct.swap(newv, Ordering::SeqCst);
            if prev != newv {
                crate::applog::info(&format!("鼠标电量: {}%", pct));
                self.post_status("battery", format!("{}%", pct));
            }
            return;
        }
        if !is_audio_frame(payload) {
            return;
        }
        if self.session.lock().unwrap().is_none() {
            return;
        }
        let end = std::cmp::min(60, payload.len());
        let frame = payload[3..end].to_vec(); // 57 字节 mSBC 帧
        let _ = self.frame_tx.send((frame, ts));
    }

    fn decode_worker(&self) {
        let rx = self.frame_rx.lock().unwrap().take();
        let Some(rx) = rx else { return };
        let mut dec = MsbcDecoder::new();
        for (frame, ts) in rx {
            let Some(pcm) = dec.decode_frame(&frame) else { continue };
            let sess = self.session.lock().unwrap().clone();
            let Some(sess) = sess else { continue };
            sess.add_pcm(&pcm, ts);
            // 喂豆包（i16 LE 原始字节，协议即 16k/s16le PCM）
            if let Ok(mut guard) = sess.asr.lock() {
                if let Some(asr) = guard.as_mut() {
                    let mut bytes = Vec::with_capacity(pcm.len() * 2);
                    for s in &pcm {
                        bytes.extend_from_slice(&s.to_le_bytes());
                    }
                    asr.feed_bytes(bytes);
                }
            }
            // 电平（30ms 节流）
            let now = util::epoch_now();
            let mut last = self.level_last.lock().unwrap();
            if now - *last >= 0.03 {
                *last = now;
                self.overlay_post(OverlayEvent::Level(sess.level()));
            }
        }
    }

    fn key_worker(&self) {
        let rx = self.key_rx.lock().unwrap().take();
        let Some(rx) = rx else { return };
        for (vk, down) in rx {
            if down {
                self.on_key_down(vk);
            } else {
                self.on_key_up(vk);
            }
        }
    }

    fn on_key_down(&self, vk: u16) {
        self.short_gen.fetch_add(1, Ordering::SeqCst); // 撤销上一短按提示
        if self.hid.lock().unwrap().is_none() {
            self.overlay_post(OverlayEvent::Cancel(
                "接收器未连接\n请插入 Rulink 接收器（2.4G 模式）".into(),
                2500,
            ));
            return;
        }
        {
            let mut g = self.session.lock().unwrap();
            if g.is_some() {
                return; // 已在录音中（含长按自动重复），忽略
            }
            let (engine, volc_cfg) = {
                let a = self.asr.lock().unwrap();
                (a.engine.clone(), a.volc.clone())
            };
            let sess = Arc::new(RecordingSession::new(vk));
            let app_for_partial = global_app();
            let on_partial: Box<dyn Fn(&str) + Send> = Box::new(move |t: &str| {
                if let Some(app) = app_for_partial.as_ref() {
                    app.overlay_post(OverlayEvent::Partial(t.to_string()));
                    app.post_partial(t.to_string());
                }
            });
            // 双引擎：volc=火山引擎付费版（API Key）；doubao=免费网页版（扫码凭证）
            let asr: Box<dyn AsrSession> = if engine == "volc" {
                if volc_cfg.api_key.trim().is_empty() {
                    self.overlay_post(OverlayEvent::Cancel(
                        "未配置火山引擎 API Key\n请在主窗口「引擎设置」中填写".into(),
                        3000,
                    ));
                    return;
                }
                Box::new(VolcAsrSession::new(volc_cfg, Some(on_partial)))
            } else {
                if !self.logged_in() {
                    self.overlay_post(OverlayEvent::Cancel(
                        "豆包未登录：请在主窗口点击「扫码登录」".into(),
                        3000,
                    ));
                    return;
                }
                let params = match creds::load_params() {
                    Some(p) => p,
                    None => {
                        self.overlay_post(OverlayEvent::Cancel(
                            "豆包凭证读取失败\n请重新扫码登录".into(),
                            2500,
                        ));
                        return;
                    }
                };
                Box::new(DoubaoSession::new(params, Some(on_partial)))
            };
            *sess.asr.lock().unwrap() = Some(asr);
            *g = Some(sess);
        }
        *self.press_t.lock().unwrap() = util::epoch_now();
        self.request_battery_refresh(); // 用户正在用鼠标（必醒着），顺带把电量刷成最新
        let ok = self
            .hid
            .lock()
            .unwrap()
            .as_ref()
            .map(|h| h.write(&CMD_START_RECORD))
            .unwrap_or(false);
        let name = self.session.lock().unwrap().as_ref().map(|s| s.key_name.clone()).unwrap_or_default();
        crate::applog::info(&format!("{}按下，录音开始（命令发送={}）", name, ok));
        self.overlay_post(OverlayEvent::Show);
        self.post_session("recording");
    }

    fn on_key_up(&self, vk: u16) {
        let sess = {
            let mut g = self.session.lock().unwrap();
            match g.as_ref() {
                Some(s) if s.vk == vk => g.take().unwrap(),
                _ => return, // 只有开始录音的那个键松开才结束本次录音
            }
        };
        if let Some(h) = self.hid.lock().unwrap().as_ref() {
            h.write(&CMD_STOP_RECORD);
        }
        let held_ms = (util::epoch_now() - *self.press_t.lock().unwrap()) * 1000.0;
        let now = util::epoch_now();
        if held_ms < self.cfg.keys.min_hold_ms as f64 {
            if let Ok(mut g) = sess.asr.lock() {
                if let Some(asr) = g.take() {
                    asr.abort();
                }
            }
            let prev = *self.prev_short_t.lock().unwrap();
            if prev > 0.0 && (now - prev) < 0.6 {
                // 快速短按两下录音键：立即隐藏悬浮层
                *self.prev_short_t.lock().unwrap() = 0.0;
                self.short_gen.fetch_add(1, Ordering::SeqCst);
                crate::applog::info("双击录音键：隐藏悬浮层");
                self.overlay_post(OverlayEvent::Hide);
                self.post_session("idle");
                return;
            }
            // 单次短按：延迟 0.45 秒确认不是双击，再提示“太短”
            *self.prev_short_t.lock().unwrap() = now;
            let gen = self.short_gen.load(Ordering::SeqCst);
            std::thread::Builder::new().name("short-timer".into()).spawn(move || {
                std::thread::sleep(Duration::from_millis(450));
                if let Some(app) = global_app() {
                    if app.short_gen.load(Ordering::SeqCst) == gen {
                        app.overlay_post(OverlayEvent::Cancel("时长太短，已取消".into(), 900));
                        app.post_session("idle");
                        crate::applog::info("短按（无双击）：已取消");
                    }
                }
            }).ok();
            return;
        }
        *self.prev_short_t.lock().unwrap() = 0.0;
        // 松手立即反馈：停止“正在录音”计时，显示“识别中…”
        self.overlay_post(OverlayEvent::Recognizing);
        self.post_session("recognizing");
        std::thread::Builder::new().name("finalize".into()).spawn(move || {
            if let Some(app) = global_app() {
                app.finalize(&sess, held_ms);
            }
        }).ok();
    }

    fn finalize(&self, sess: &Arc<RecordingSession>, held_ms: f64) {
        std::thread::sleep(Duration::from_millis(self.cfg.keys.drain_ms));
        let (pcm, duration) = sess.collect();
        if sess.packet_count.load(Ordering::SeqCst) == 0 {
            crate::applog::warn("未收到鼠标音频数据！请检查接收器/2.4G 模式/官方驱动是否退出");
            self.overlay_post(OverlayEvent::Cancel(
                "未收到鼠标音频数据\n请检查接收器连接与官方驱动".into(),
                2500,
            ));
            self.post_session("idle");
            return;
        }
        if held_ms < self.cfg.keys.min_hold_ms as f64 || duration < 0.25 {
            crate::applog::info(&format!("按住 {:.0}ms，太短，已取消", held_ms));
            self.overlay_post(OverlayEvent::Cancel("时长太短，已取消".into(), 900));
            self.post_session("idle");
            return;
        }

        // 最终文本：豆包=云端累计结果（finish 内含冲刷+等待末尾结果）
        let asr = sess.asr.lock().unwrap().take();
        let (text, asr_error) = match asr {
            Some(mut a) => {
                let r = a.finish(6.0);
                let err = a.error();
                match r {
                    Ok(t) => (t, err),
                    Err(e) => {
                        if e.auth {
                            // 网页版凭证失效：保存录音后触发重新登录
                            crate::applog::warn(&format!(
                                "豆包凭证失效（录音已保存为 {}），触发重新登录: {}",
                                save_wav(&self.recordings, &pcm)
                                    .map(|p| p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default())
                                    .unwrap_or_default(),
                                e.msg
                            ));
                            if let Some(h) = self.ui.lock().unwrap().clone() {
                                self.trigger_login(&h);
                            }
                            self.overlay_post(OverlayEvent::Cancel(
                                "豆包登录已过期\n已自动弹出重新登录窗口".into(),
                                3500,
                            ));
                        } else {
                            // 火山引擎/网络等普通错误：提示原因（多为 API Key/开通状态）
                            crate::applog::warn(&format!("识别引擎错误: {}", e.msg));
                            self.overlay_post(OverlayEvent::Cancel(
                                format!("识别失败\n{}", e.msg),
                                3500,
                            ));
                        }
                        self.post_session("idle");
                        return;
                    }
                }
            }
            None => (String::new(), None),
        };
        let text = text.trim().to_string();

        // 录音落盘（16kHz 16bit 单声道 WAV）
        let wav_path = save_wav(&self.recordings, &pcm);

        if text.is_empty() {
            match asr_error {
                Some(err) => {
                    crate::applog::warn(&format!("豆包 ASR 连接失败: {}", err));
                    self.overlay_post(OverlayEvent::Cancel("豆包连接失败，请检查网络".into(), 2500));
                }
                None => {
                    crate::applog::warn(&format!("识别结果为空（时长 {:.1}s）", duration));
                    self.overlay_post(OverlayEvent::Cancel("未识别到内容".into(), 900));
                }
            }
            self.post_session("idle");
            return;
        }

        let ok = output::set_clipboard_text(&text);
        if !ok {
            crate::applog::warn("写入剪贴板失败（可能被其它程序占用）");
        }
        {
            let _g = self.output_lock.lock().unwrap();
            if self.cfg.output.input_mode == "ctrl_v" {
                output::paste_clipboard();
            } else {
                output::type_text_unicode(&text);
            }
        }
        if let Some(p) = wav_path.as_ref() {
            let txt = p.with_extension("txt");
            let _ = std::fs::write(&txt, format!("{}\n", text));
        }
        crate::applog::info(&format!(
            "输出文字 ({}, 剪贴板={}): {}",
            self.cfg.output.input_mode, ok, text
        ));
        self.overlay_post(OverlayEvent::Done(
            text,
            (self.cfg.output.show_final_seconds * 1000.0) as u32,
        ));
        self.post_session("idle");
    }

    // ---- 登录 ----

    /// 应用内登录窗口（WebView2 打开 doubao.com，登录态自动收割）
    pub fn trigger_login(&self, app: &AppHandle) {
        crate::login::start_login(app, &global_app().unwrap_or_else(|| unreachable_app()), false);
    }

    /// 浏览器登录：打开系统浏览器让用户登录豆包，然后收割 cookies
    pub fn trigger_browser_login(&self) {
        if self.login_pending.swap(true, Ordering::SeqCst) {
            return;
        }
        std::thread::Builder::new().name("browser-login".into()).spawn(move || {
            if let Some(app) = global_app() {
                // 第一步：先尝试直接收割（已有登录会话）
                if let Ok(params) = crate::browser_cookies::harvest_params() {
                    if creds::save_params(&params) {
                        crate::applog::info("已从本机现有会话收割登录态，凭证更新完成（无需重新登录）");
                        app.login_pending.store(false, Ordering::SeqCst);
                        app.logged_in.store(true, Ordering::SeqCst);
                        app.post_login_done(true);
                        app.post_status("login", "已登录".into());
                        return;
                    }
                }
                crate::applog::info("本机现有会话无登录态：正在打开系统浏览器（请在浏览器里登录 doubao.com，完成后回到本窗口）...");
                // 打开系统浏览器
                let opened = unsafe {
                    use windows::Win32::UI::Shell::ShellExecuteW;
                    let verb = util::wide("open");
                    let url = util::wide(creds::LOGIN_URL);
                    !ShellExecuteW(
                        None,
                        windows::core::PCWSTR(verb.as_ptr()),
                        windows::core::PCWSTR(url.as_ptr()),
                        windows::core::PCWSTR::null(),
                        windows::core::PCWSTR::null(),
                        windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
                    ).0.is_null()
                };
                if !opened {
                    crate::applog::error("打开系统浏览器失败");
                    app.login_pending.store(false, Ordering::SeqCst);
                    app.post_login_done(false);
                    return;
                }
                // 等用户在浏览器完成登录（最长 5 分钟，每 5 秒探测一次 SSO cookie）
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
                crate::applog::info("等待浏览器登录中（登录完成后自动识别，最长等 5 分钟）...");
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(5));
                    if app.stopping.load(Ordering::SeqCst) {
                        return;
                    }
                    match crate::browser_cookies::harvest_params() {
                        Ok(params) => {
                            if creds::save_params(&params) {
                                crate::applog::info("浏览器登录态已收割并保存，凭证更新完成");
                                app.login_pending.store(false, Ordering::SeqCst);
                                app.logged_in.store(true, Ordering::SeqCst);
                                app.post_login_done(true);
                                app.post_status("login", "已登录".into());
                            } else {
                                crate::applog::error("✗ 凭证保存失败（详见日志）");
                                app.login_pending.store(false, Ordering::SeqCst);
                                app.post_login_done(false);
                            }
                            return;
                        }
                        Err(e) => {
                            if std::time::Instant::now() > deadline {
                                crate::applog::warn(&format!("等待浏览器登录超时: {}", e));
                                app.login_pending.store(false, Ordering::SeqCst);
                                app.post_login_done(false);
                                return;
                            }
                            // 未登录态：继续等（不打日志刷屏）
                        }
                    }
                }
            }
        }).ok();
    }

    pub fn run_link_test_async(&self) {
        std::thread::Builder::new().name("link-test".into()).spawn(move || {
            if let Some(app) = global_app() {
                let summary = app.run_link_test();
                app.post_test_done(summary);
            }
        }).ok();
    }

    fn run_link_test(&self) -> String {
        let (engine, volc_cfg) = {
            let a = self.asr.lock().unwrap();
            (a.engine.clone(), a.volc.clone())
        };
        if engine == "volc" && volc_cfg.api_key.trim().is_empty() {
            return "未配置火山引擎 API Key：请在「引擎设置」中填写".into();
        }
        if engine != "volc" && creds::load_params().is_none() {
            return "未登录：请先点击「扫码登录」".into();
        }
        let mut wavs: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&self.recordings) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().map(|x| x == "wav").unwrap_or(false) {
                    if let Ok(m) = e.metadata() {
                        if let Ok(t) = m.modified() {
                            wavs.push((p, t));
                        }
                    }
                }
            }
        }
        wavs.sort_by_key(|(_, t)| *t);
        let Some((wav_path, _)) = wavs.pop() else {
            return "recordings 目录还没有录音：先长按鼠标键录一段再测".into();
        };
        let (_info, raw) = match wav::read_wav(&wav_path) {
            Ok(v) => v,
            Err(e) => {
                crate::applog::error(&format!("读取录音失败: {} ({})", wav_path.display(), e));
                return "读取录音失败（详见日志）".into();
            }
        };
        let duration = raw.len() as f64 / 2.0 / creds::AUDIO_SAMPLE_RATE as f64;
        let engine_name = self.engine_display();
        crate::applog::info(&format!(
            "链路测试：发送 {}（{:.1}s）到 {} ...",
            wav_path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
            duration,
            engine_name
        ));
        let on_partial = std::sync::Arc::new(|t: &str| crate::applog::info(&format!("[测试·实时] {}", t)));
        if engine == "volc" {
            match crate::volc_asr::probe(volc_cfg, &raw, 2.0, on_partial) {
                Ok(text) if !text.is_empty() => format!("链路正常 ✓ 识别结果：{}", text),
                Ok(_) => "未收到识别结果（详见日志）".into(),
                Err(e) => e.msg,
            }
        } else {
            match crate::doubao::probe(&raw, 2.0, on_partial) {
                Ok(text) if !text.is_empty() => format!("链路正常 ✓ 识别结果：{}", text),
                _ => "未收到识别结果（网络不通或接口已变化，详见日志）".into(),
            }
        }
    }
}

fn applog_info(msg: &str) {
    crate::applog::info(msg);
}

fn unreachable_app() -> Arc<AppShared> {
    // 全局注册在启动最早期完成，正常流程不会走到
    panic!("AppShared 未注册全局");
}

/// 保存 WAV，返回路径（失败返回 None）
fn save_wav(recordings: &Path, pcm: &[i16]) -> Option<PathBuf> {
    let stamp = local_stamp();
    let wav_path = recordings.join(format!("{}.wav", stamp));
    match wav::write_wav(&wav_path, pcm) {
        Ok(_) => Some(wav_path),
        Err(e) => {
            crate::applog::error(&format!("保存录音失败: {}", e));
            None
        }
    }
}

/// 本地时间戳 "YYYYMMDD_HHMMSS"（录音文件名）
fn local_stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let bias = -tz_offset_secs();
    let t = secs + bias;
    let days = t.div_euclid(86400);
    let rem = t.rem_euclid(86400);
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let yy = (y + if m <= 2 { 1 } else { 0 }) as i32;
    format!(
        "{:04}{:02}{:02}_{:02}{:02}{:02}",
        yy, m as u32, d as u32, (rem / 3600) as u32, ((rem % 3600) / 60) as u32, (rem % 60) as u32
    )
}

fn tz_offset_secs() -> i64 {
    use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
    unsafe {
        let mut tzi = TIME_ZONE_INFORMATION::default();
        let rc = GetTimeZoneInformation(&mut tzi);
        let bias = tzi.Bias as i64;
        let extra = if rc == 2 { tzi.DaylightBias as i64 } else { tzi.StandardBias as i64 };
        (bias + extra) * 60 // UTC = local + bias
    }
}

fn resolve_dir(base: &Path, dir: &str) -> PathBuf {
    let p = PathBuf::from(dir);
    if p.is_absolute() {
        p
    } else {
        base.join(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_battery_frame() {
        // Rulink Log.txt 里的真实应答帧（report_id 00 之后的 payload），4.09V → 89%
        let mut resp = vec![0xA5u8, 0x5A, 0x09, 0x54, 0x06, 0x99, 0x01, 0xFF];
        resp.extend_from_slice(&[0u8; 56]);
        assert_eq!(parse_battery_frame(&resp), Some(89));
        // 兼容多一个前导前缀字节
        let mut prefixed = vec![0u8];
        prefixed.extend_from_slice(&resp);
        assert_eq!(parse_battery_frame(&prefixed), Some(89));
        // 3.4V（0x0154）→ 20%
        assert_eq!(
            parse_battery_frame(&[0xA5, 0x5A, 0x09, 0x54, 0x06, 0x54, 0x01, 0xFF]),
            Some(20)
        );
        // 钳位：3.0V 以下 → 0%，4.2V 以上 → 100%
        assert_eq!(
            parse_battery_frame(&[0xA5, 0x5A, 0x09, 0x54, 0x06, 0x20, 0x01, 0xFF]),
            Some(0)
        );
        assert_eq!(
            parse_battery_frame(&[0xA5, 0x5A, 0x09, 0x54, 0x06, 0xAE, 0x01, 0xFF]),
            Some(100)
        );
        // 音频帧 / 空数据 / 错误命令字 → None
        assert_eq!(parse_battery_frame(&[0x9D, 0x01, 0x08, 0xAD, 0, 0, 0, 0]), None);
        assert_eq!(parse_battery_frame(&[]), None);
        assert_eq!(
            parse_battery_frame(&[0xA5, 0x5A, 0x09, 0x54, 0x02, 0x99, 0x01, 0xFF]),
            None
        );
    }
}
