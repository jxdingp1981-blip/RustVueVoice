// commands.rs — Tauri 命令层：Vue 前端 invoke 的入口
#![allow(dead_code)]

use serde::Serialize;
use std::sync::Arc;
use tauri::{AppHandle, State};

use crate::app::AppShared;

pub const VOLC_ACTIVATE_URL: &str = "https://console.volcengine.com/speech/new/setting/activate?ResourceID=volc.seedasr.sauc.duration&_vtm_=a106466.b106468.0_0.0_0.0.215_7701485951495521827&projectName=default";

pub const HELP_TEXT: &str = r#"【RustVueVoice 使用说明】

一、准备工作
  1. 鼠标接收器插入 USB，鼠标拨到 ON（2.4G 模式，非蓝牙）；
  2. 官方驱动 Rulink.exe 必须完全退出（任务管理器确认），否则音频会被抢走；
  3. 首次使用点击「扫码登录」，在弹出的窗口里用手机豆包 App 扫码（录音将上传豆包云端识别）。

二、语音打字
  1. 打开任意可输入的窗口（记事本 / Word / 聊天框）；
  2. 长按鼠标上的语音键（F19/F18/F20 任意一个均可）说话；
  3. 松开后文字自动输入到光标位置，并复制进剪贴板；
  4. 屏幕中央的胶囊悬浮窗实时显示识别文字与音量波形。
  手势：按住 <0.2 秒视为误触自动取消；快速短按两下隐藏悬浮层。

三、主窗口按钮
  · 扫码登录：弹出豆包登录窗口；Cookie 过期会自动提示重新登录；
  · 浏览器登录：在系统浏览器登录 doubao.com 后自动收割登录态（备选方式）；
  · 云链路测试：取最近一段录音发送云端验证整条链路；
  · 引擎设置：切换识别引擎 / 配置火山引擎 API Key（见下节）；
  · 开机自动启动：开启后写入系统注册表（HKCU Run），开机自动运行本程序；
  · 打开录音目录：每次录音的 WAV 与识别文本都保存在此；
  · 使用说明：显示本说明。
  日志同时写入 logs\rustvuevoice.log。

四、常见问题
  · 提示“未收到鼠标音频数据”：确认接收器已插、鼠标为 2.4G 模式、官方驱动已退出；
  · 提示“豆包连接失败”：检查网络，或用「云链路测试」定位；
  · 凭证与 Python/Rust 旧版共用（%USERPROFILE%\.config\doubao-input\），可互换使用。

五、火山引擎付费引擎（可选，识别更稳更准）
  本程序支持火山引擎「流式语音识别大模型 2.0（小时版）」作为付费识别引擎，
  API 契约稳定（不再依赖网页 Cookie），默认开启二遍识别 / 标点 / 数字规整。
  开通步骤：
    1. 打开火山引擎控制台开通服务（新用户开通即送 20 小时识别时长），
       网址为：
       https://console.volcengine.com/speech/new/setting/activate?ResourceID=volc.seedasr.sauc.duration&_vtm_=a106466.b106468.0_0.0_0.0.215_7701485951495521827&projectName=default
    2. 开通后在「控制台 > API Key 管理」创建一个 API Key 并复制；
    3. 回到本程序主窗口，点击「引擎设置」：
       · 识别引擎选择「火山引擎·流式语音识别 2.0」；
       · 粘贴 API Key，点击保存。
    4. 用「云链路测试」验证整条链路（引擎状态卡会显示当前引擎）。
  说明：
    · API Key 仅保存在本机 exe 旁的 config.json，不会上传给任何第三方；
    · 小时版按识别音频时长计费，具体价格以火山引擎计费页为准；
    · 切回免费引擎：在「引擎设置」中选回「豆包云端（免费）」即可。
"#;

#[derive(Serialize, Clone)]
pub struct StatusSnapshot {
    pub engine: String,
    pub logged_in: bool,
    pub device_connected: bool,
    pub listening: bool,
    pub battery: Option<u32>,
    pub recordings_dir: String,
    pub autostart: bool,
    pub version: String,
}

fn shared_of<'a>(state: &'a State<'a, Arc<AppShared>>) -> &'a Arc<AppShared> {
    state.inner()
}

#[tauri::command]
pub fn get_status(state: State<'_, Arc<AppShared>>) -> StatusSnapshot {
    let s = shared_of(&state);
    StatusSnapshot {
        engine: s.engine_display(),
        logged_in: s.logged_in(),
        device_connected: s.device_connected(),
        listening: s.listening(),
        battery: s.battery_percent(),
        recordings_dir: s.recordings_dir(),
        autostart: crate::autostart::is_enabled(),
        version: concat!("v", env!("CARGO_PKG_VERSION")).into(),
    }
}

// ---- 识别引擎配置（豆包网页版 / 火山引擎付费版） ----

#[derive(Serialize, Clone)]
pub struct AsrConfigDto {
    pub engine: String,
    pub api_key: String,
    pub resource_id: String,
}

#[tauri::command]
pub fn get_asr_config(state: State<'_, Arc<AppShared>>) -> AsrConfigDto {
    let s = shared_of(&state);
    let a = s.asr.lock().unwrap();
    AsrConfigDto {
        engine: a.engine.clone(),
        api_key: a.volc.api_key.clone(),
        resource_id: a.volc.resource_id.clone(),
    }
}

#[tauri::command]
pub fn set_asr_config(
    state: State<'_, Arc<AppShared>>,
    engine: String,
    api_key: String,
) -> Result<(), String> {
    shared_of(&state).set_asr_config(&engine, &api_key)
}

/// 用系统默认浏览器打开 URL（火山引擎开通页等）
#[tauri::command]
pub fn open_url(url: String) -> Result<(), String> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("仅允许打开 http(s) 链接".into());
    }
    let verb = crate::util::wide("open");
    let u = crate::util::wide(&url);
    let ok = unsafe {
        !ShellExecuteW(
            None,
            windows::core::PCWSTR(verb.as_ptr()),
            windows::core::PCWSTR(u.as_ptr()),
            windows::core::PCWSTR::null(),
            windows::core::PCWSTR::null(),
            SW_SHOWNORMAL,
        )
        .0
        .is_null()
    };
    if ok {
        Ok(())
    } else {
        Err("打开浏览器失败".into())
    }
}

#[tauri::command]
pub fn start_login(app: AppHandle, state: State<'_, Arc<AppShared>>) -> Result<(), String> {
    crate::login::start_login(&app, shared_of(&state), false);
    Ok(())
}

#[tauri::command]
pub fn start_browser_login(state: State<'_, Arc<AppShared>>) -> Result<(), String> {
    shared_of(&state).trigger_browser_login();
    Ok(())
}

#[tauri::command]
pub fn run_link_test(state: State<'_, Arc<AppShared>>) -> Result<(), String> {
    shared_of(&state).run_link_test_async();
    Ok(())
}

#[tauri::command]
pub fn open_recordings_dir(state: State<'_, Arc<AppShared>>) -> Result<(), String> {
    let dir = shared_of(&state).recordings_dir();
    open_in_explorer(&dir)
}

#[tauri::command]
pub fn get_autostart() -> bool {
    crate::autostart::is_enabled()
}

#[tauri::command]
pub fn set_autostart(on: bool) -> Result<bool, String> {
    Ok(crate::autostart::set_enabled(on))
}

#[tauri::command]
pub fn exit_app(app: AppHandle, state: State<'_, Arc<AppShared>>) {
    shared_of(&state).shutdown();
    crate::overlay::destroy_overlay();
    app.exit(0);
}

#[tauri::command]
pub fn get_help() -> String {
    HELP_TEXT.into()
}

/// 前端就绪：领取启动阶段的积压日志（此后日志走实时广播）
#[tauri::command]
pub fn frontend_ready(state: State<'_, Arc<AppShared>>) -> Vec<String> {
    shared_of(&state).take_log_backlog()
}

/// 用资源管理器打开目录（ShellExecuteW）
fn open_in_explorer(dir: &str) -> Result<(), String> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    if !std::path::Path::new(dir).is_dir() {
        let _ = std::fs::create_dir_all(dir);
    }
    let verb = crate::util::wide("open");
    let d = crate::util::wide(dir);
    let ok = unsafe {
        !ShellExecuteW(
            None,
            windows::core::PCWSTR(verb.as_ptr()),
            windows::core::PCWSTR(d.as_ptr()),
            windows::core::PCWSTR::null(),
            windows::core::PCWSTR::null(),
            SW_SHOWNORMAL,
        )
        .0
        .is_null()
    };
    if ok {
        Ok(())
    } else {
        Err(format!("打开目录失败: {}", dir))
    }
}
