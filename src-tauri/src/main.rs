// main.rs — RustVueVoice 入口（Tauri 2 + Vue 3 版 VoiceMouse）
// 用法对齐旧版：
//   RustVueVoice.exe                 打开主控窗口并进入监听
//   RustVueVoice.exe --check         自检（接收器/凭证），不进入监听
//   RustVueVoice.exe --probe x.wav   云端链路冒烟测试
//   RustVueVoice.exe --login         应用内登录窗口完成扫码后退出
//   RustVueVoice.exe --import-browser  从系统浏览器收割豆包登录 cookie
//   RustVueVoice.exe --overlay-demo  悬浮层动效演示

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod applog;
mod asr_engine;
mod autostart;
mod browser_cookies;
mod commands;
mod config;
mod creds;
mod doubao;
mod hid;
mod login;
mod msbc;
mod output;
mod overlay;
mod rawinput;
mod util;
mod volc_asr;
mod wav;

use std::path::PathBuf;

use tauri::{Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

const VERSION: &str = concat!("RustVueVoice v", env!("CARGO_PKG_VERSION"));

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let base = config::base_dir();

    // 控制台场景：附加父进程控制台（子命令输出）
    let want_console = args.iter().any(|a| {
        a == "--check" || a == "--probe" || a == "--login" || a == "--import-browser" || a == "--help"
    });
    if want_console {
        use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
        unsafe {
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }

    // 日志（GUI 模式照常写文件）
    let cfg = config::Config::load(&base.join("config.json"));
    let log_dir = resolve(&base, &cfg.paths.log_dir);
    let logfile = log_dir.join("rustvuevoice.log");
    applog::init(Some(&logfile), want_console);
    applog::info(&format!("=== RustVueVoice {} 启动（日志文件: {}）===", VERSION, logfile.display()));

    if args.iter().any(|a| a == "--help") {
        print_help();
        return;
    }
    if args.iter().any(|a| a == "--check") {
        cmd_check(&cfg);
        return;
    }
    if let Some(pos) = args.iter().position(|a| a == "--probe") {
        match args.get(pos + 1) {
            Some(p) => cmd_probe(p),
            None => println!("用法: RustVueVoice.exe --probe <wav 文件>"),
        }
        return;
    }
    if args.iter().any(|a| a == "--import-browser") {
        cmd_import_browser();
        return;
    }
    if args.iter().any(|a| a == "--overlay-demo") {
        cmd_overlay_demo();
        return;
    }
    let login_only = args.iter().any(|a| a == "--login");
    if login_only {
        println!("正在打开豆包扫码登录窗口（用手机豆包 App 扫码）...");
    }

    run_gui(cfg, base, login_only);
}

fn resolve(base: &std::path::Path, dir: &str) -> PathBuf {
    let p = std::path::PathBuf::from(dir);
    if p.is_absolute() {
        p
    } else {
        base.join(p)
    }
}

fn print_help() {
    println!("RustVueVoice — Rulink/MR08 语音鼠标 · 豆包云端语音打字（Tauri 2 + Vue 3 版）");
    println!();
    println!("用法:");
    println!("  RustVueVoice.exe                打开主控窗口并进入监听");
    println!("  RustVueVoice.exe --check        自检（接收器/凭证）");
    println!("  RustVueVoice.exe --probe <wav>  云端链路冒烟测试");
    println!("  RustVueVoice.exe --login        应用内登录窗口完成扫码后退出");
    println!("  RustVueVoice.exe --import-browser  从系统浏览器收割豆包登录 cookie");
    println!("  RustVueVoice.exe --overlay-demo 悬浮层动效演示");
}

/// --check：自检（接收器/引擎凭证）
fn cmd_check(cfg: &config::Config) {
    println!("RustVueVoice {}", VERSION);
    match creds::load_params() {
        Some(p) => {
            println!(
                "识别引擎: 豆包云端（凭证 OK：{} cookies，device_id={}…，web_id={}…）",
                p.cookies.len(),
                &p.device_id[..p.device_id.len().min(8)],
                &p.web_id[..p.web_id.len().min(8)]
            );
            println!("凭证文件: {}", creds::params_path().display());
        }
        None => {
            println!("识别引擎: 豆包云端（未登录！运行 --login 或使用主窗口「扫码登录」）");
            println!("凭证文件（待创建）: {}", creds::params_path().display());
        }
    }
    let vid = cfg.vid_u16();
    let pid = cfg.pid_u16();
    let mi = cfg.device.mi.clone();
    match hid::find_voice_device(vid, pid, &mi) {
        Some(d) => {
            println!("接收器语音接口: OK -> {}", d.describe());
            println!("悬浮层: OK（胶囊悬浮窗·独立线程）");
            println!("自检通过。");
        }
        None => {
            println!("接收器语音接口: 未找到（{} MI_{}）", cfg.dev_tag(), mi);
            println!("请确认：1) 接收器已插入 USB；2) 鼠标已拨到 ON 并处于 2.4G 模式；");
            println!("        3) 官方驱动未运行。");
            println!("自检未通过（其余项正常）。");
            std::process::exit(2);
        }
    }
}

/// --probe：把一段本地 WAV 发给豆包验证链路
fn cmd_probe(wav_path: &str) {
    applog::set_quiet(true);
    let path = std::path::PathBuf::from(wav_path);
    if !path.is_file() {
        println!("文件不存在: {}", wav_path);
        std::process::exit(1);
    }
    let cfg = config::Config::load(&config::base_dir().join("config.json"));
    // 火山引擎付费版不依赖豆包凭证；网页版才需要
    let params = if cfg.asr.engine == "volc" {
        None
    } else {
        match creds::load_params() {
            Some(p) => Some(p),
            None => {
                println!("未找到豆包凭证（{}）。", creds::params_path().display());
                println!("请先扫码登录（主窗口「扫码登录」或 --login），或在「引擎设置」切换火山引擎。");
                std::process::exit(1);
            }
        }
    };
    let (info, raw) = match wav::read_wav(&path) {
        Ok(v) => v,
        Err(e) => {
            println!("读取 WAV 失败: {}", e);
            std::process::exit(1);
        }
    };
    if info.sample_rate != creds::AUDIO_SAMPLE_RATE || info.channels != 1 || info.bits != 16 {
        println!(
            "警告：WAV 不是 16kHz/单声道/16bit（{}Hz/{}ch/{}bit），结果可能异常。",
            info.sample_rate, info.channels, info.bits
        );
    }
    let duration = raw.len() as f64 / 2.0 / creds::AUDIO_SAMPLE_RATE as f64;
    let engine = cfg.asr.engine.clone();
    if engine == "volc" {
        println!(
            "发送 {} ({:.1}s) 到火山引擎 ASR（{}）...",
            wav_path, duration, cfg.asr.volc.resource_id
        );
    } else {
        println!(
            "凭证 OK（{} cookies），发送 {} ({:.1}s) 到豆包云端 ASR ...",
            params.as_ref().map(|p| p.cookies.len()).unwrap_or(0),
            wav_path,
            duration
        );
    }
    let on_partial = std::sync::Arc::new(|t: &str| {
        use std::io::Write;
        print!("\r[实时] {}", t);
        let _ = std::io::stdout().flush();
    });
    let result = if engine == "volc" {
        if cfg.asr.volc.api_key.trim().is_empty() {
            println!("未配置火山引擎 API Key（config.json asr.volc.api_key 或主窗口「引擎设置」）。");
            std::process::exit(1);
        }
        doubao_unwrap_result(volc_asr::probe(cfg.asr.volc.clone(), &raw, 2.0, on_partial))
    } else {
        match doubao::probe(&raw, 2.0, on_partial) {
            Ok(t) => Ok(t),
            Err(e) => Err(e.0),
        }
    };
    match result {
        Ok(text) if !text.is_empty() => {
            println!("\n[最终] {}", text);
            println!("链路正常。");
        }
        Ok(_) => {
            println!("\n未收到识别结果。");
            std::process::exit(3);
        }
        Err(e) => {
            println!("\n{}", e);
            std::process::exit(2);
        }
    }
}

/// 统一 volc probe 的错误类型（AsrError → String）
fn doubao_unwrap_result(r: Result<String, asr_engine::AsrError>) -> Result<String, String> {
    r.map_err(|e| e.msg)
}

/// --import-browser：从系统浏览器收割豆包登录 cookie（先在浏览器里登录 doubao.com）
fn cmd_import_browser() {
    println!("从系统浏览器（Edge/Chrome）收割 doubao.com 登录 cookie ...");
    match browser_cookies::harvest_params() {
        Ok(params) => {
            if creds::save_params(&params) {
                println!(
                    "✓ 登录态已保存（{} cookies，device_id={}…）—— 重新启动主程序即可使用",
                    params.cookies.len(),
                    &params.device_id[..params.device_id.len().min(8)]
                );
            } else {
                println!("✗ 凭证保存失败（详见日志）");
                std::process::exit(1);
            }
        }
        Err(e) => {
            println!("✗ {}", e);
            println!();
            println!("操作指引：");
            println!("  1. 用系统默认浏览器（Edge/Chrome）打开 https://www.doubao.com/chat");
            println!("  2. 用手机号+验证码完成登录；");
            println!("  3. 保持浏览器开着（无需关闭），重新运行本命令。");
            std::process::exit(1);
        }
    }
}

/// --overlay-demo：胶囊悬浮层动效演示（呼吸/流光/波形/完成提示）
fn cmd_overlay_demo() {
    use std::time::Duration;
    let handle = overlay::start_overlay_thread();
    println!("胶囊悬浮窗演示：模拟一轮“录音→出字→完成”流程，约 11 秒后退出。");
    let h = handle.clone();
    std::thread::spawn(move || {
        h.post(overlay::OverlayEvent::Show);
        let sentence = "长按鼠标录音键说话，松开后文字自动输入到光标位置，这是悬浮窗演示。";
        for i in 0..70 {
            let v = 0.35 + 0.55 * ((i as f64) / 6.0).sin().abs();
            std::thread::sleep(Duration::from_millis(90));
            h.post(overlay::OverlayEvent::Level(v));
        }
        for i in 1..=sentence.chars().count() {
            std::thread::sleep(Duration::from_millis(120));
            let part: String = sentence.chars().take(i).collect();
            h.post(overlay::OverlayEvent::Partial(part));
        }
        std::thread::sleep(Duration::from_millis(600));
        h.post(overlay::OverlayEvent::Done(sentence.to_string(), 2500));
    });
    std::thread::sleep(Duration::from_secs(11));
    overlay::destroy_overlay();
    std::thread::sleep(Duration::from_millis(300));
    std::process::exit(0);
}

/// 正常 GUI 启动（Tauri 事件循环；login_only=true 时仅开登录窗口）
fn run_gui(cfg: config::Config, base: PathBuf, login_only: bool) {
    let shared = app::AppShared::new(cfg, base);
    app::register_global(shared.clone());

    let builder = tauri::Builder::default();
    let builder = if !login_only {
        // 单实例：重复启动直接把已有主窗口带到前台
        builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            applog::info("已有实例在运行：激活其主窗口后退出本次启动");
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
    } else {
        builder
    };

    builder
        .setup(move |app| {
            app.manage(shared.clone());
            shared.set_ui(app.handle().clone());
            {
                // 所有 applog 日志行：前端已监听则实时广播，否则进积压缓冲
                let h = app.handle().clone();
                let shared_for_log = shared.clone();
                applog::attach_gui(Box::new(move |line: &str| {
                    use tauri::Emitter;
                    if shared_for_log.log_is_attached() {
                        let _ = h.emit("log", serde_json::json!({ "line": line }));
                    } else {
                        shared_for_log.push_log_backlog(line.to_string());
                    }
                }));
            }
            if !login_only {
                // 悬浮层（独立线程 + 自带消息泵），随后启动全部后台工作线程
                let ov = overlay::start_overlay_thread();
                shared.set_overlay(ov);
                let _win = WebviewWindowBuilder::new(
                    app,
                    "main",
                    WebviewUrl::App("index.html".into()),
                )
                .title("RustVueVoice 语音鼠标 · 主控")
                .inner_size(1060.0, 760.0)
                .min_inner_size(880.0, 620.0)
                .center()
                .resizable(true)
                .build()?;
                // 开机自启已注册但 exe 路径变化时，刷新注册表里的路径
                if autostart::is_enabled() {
                    autostart::set_enabled(true);
                }
                shared.start();
            } else {
                // --login 子命令：仅登录窗口，完成后退出进程
                let shared2 = shared.clone();
                let h2 = app.handle().clone();
                std::thread::spawn(move || {
                    login::start_login(&h2, &shared2, true);
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_status,
            commands::start_login,
            commands::start_browser_login,
            commands::run_link_test,
            commands::open_recordings_dir,
            commands::get_autostart,
            commands::set_autostart,
            commands::exit_app,
            commands::get_help,
            commands::frontend_ready,
            commands::get_asr_config,
            commands::set_asr_config,
            commands::open_url
        ])
        .on_window_event(|window, event| match event {
            WindowEvent::Focused(true) => {
                // 用户打开/点击主窗口查看状态：即时刷新电量（内部有 30 秒节流）
                if window.label() == "main" {
                    if let Some(s) = window
                        .app_handle()
                        .try_state::<std::sync::Arc<app::AppShared>>()
                    {
                        s.request_battery_refresh();
                    }
                }
            }
            WindowEvent::CloseRequested { api, .. } => {
                // 关闭主窗口 = 退出程序（与旧版语义一致）
                if window.label() == "main" {
                    api.prevent_close();
                    if let Some(s) = window
                        .app_handle()
                        .try_state::<std::sync::Arc<app::AppShared>>()
                    {
                        s.shutdown();
                    }
                    overlay::destroy_overlay();
                    window.app_handle().exit(0);
                }
            }
            _ => {}
        })
        .run(tauri::generate_context!())
        .expect("RustVueVoice 运行失败");
    applog::info("退出");
}
