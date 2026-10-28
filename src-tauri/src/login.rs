// login.rs — 豆包登录：应用内 WebView2 窗口打开 doubao.com，登录完成后收割 cookie
// 替代旧版的 Python 登录子进程：本版自带登录能力，不再依赖外部 exe。
// cookie 读取走 Tauri 的 cookies_for_url（WebView2 CookieManager，可取 httpOnly）。
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};
use url::Url;

use crate::app::AppShared;
use crate::applog;
use crate::creds::{self, AsrParams};

const LOGIN_WINDOW_LABEL: &str = "doubao-login";
const POLL_INTERVAL: Duration = Duration::from_secs(2);
const TIMEOUT_SECS: u64 = 600;
/// SSO 登录 cookie 必须在场（任一即视为已登录）
const SSO_COOKIES: [&str; 5] = ["sid_guard", "sid_tt", "uid_tt", "SESSIONID", "sessionid"];

/// 启动登录流程（幂等：login_pending 防重入）。
/// exit_when_done=true 用于 --login 子命令：完成后打印结果并退出进程。
pub fn start_login(app: &AppHandle, shared: &Arc<AppShared>, exit_when_done: bool) {
    if shared.login_pending.swap(true, Ordering::SeqCst) {
        applog::warn("登录流程已在进行中，忽略本次请求");
        return;
    }
    let app = app.clone();
    let shared = shared.clone();
    std::thread::Builder::new()
        .name("doubao-login".into())
        .spawn(move || run_login_flow(&app, &shared, exit_when_done))
        .ok();
}

fn run_login_flow(app: &AppHandle, shared: &Arc<AppShared>, exit_when_done: bool) {
    applog::info("正在打开豆包登录窗口（应用内 WebView，用手机豆包 App 扫码或手机号登录）...");
    shared.post_status("login", "登录中…".into());

    // 建窗口须在主线程执行（run_on_main_thread 经事件循环代理，任意线程可安全调用）
    let (tx, rx) = channel::<Result<tauri::WebviewWindow, String>>();
    let app2 = app.clone();
    let url: Url = creds::LOGIN_URL
        .parse()
        .unwrap_or_else(|_| Url::parse("https://www.doubao.com/chat").unwrap());
    let dispatched = app.run_on_main_thread(move || {
        let result = WebviewWindowBuilder::new(&app2, LOGIN_WINDOW_LABEL, WebviewUrl::External(url))
            .title("豆包登录 · RustVueVoice")
            .inner_size(520.0, 680.0)
            .min_inner_size(420.0, 560.0)
            .resizable(true)
            .center()
            .build()
            .map_err(|e| e.to_string());
        let _ = tx.send(result);
    });
    if dispatched.is_err() {
        applog::error("登录窗口派发失败（事件循环不可用）");
        finish(app, shared, false, exit_when_done);
        return;
    }
    let win = match rx.recv_timeout(Duration::from_secs(15)) {
        Ok(Ok(w)) => w,
        Ok(Err(e)) => {
            applog::error(&format!("创建登录窗口失败: {}", e));
            finish(app, shared, false, exit_when_done);
            return;
        }
        Err(_) => {
            applog::error("创建登录窗口超时");
            finish(app, shared, false, exit_when_done);
            return;
        }
    };

    let deadline = Instant::now() + Duration::from_secs(TIMEOUT_SECS);
    let mut success = false;
    loop {
        std::thread::sleep(POLL_INTERVAL);
        if shared.is_stopping() {
            break;
        }
        if app.get_webview_window(LOGIN_WINDOW_LABEL).is_none() {
            applog::info("登录窗口已关闭");
            break;
        }
        match win.cookies_for_url(creds::LOGIN_URL.parse().unwrap()) {
            Ok(cookies) => {
                let mut map: BTreeMap<String, String> = BTreeMap::new();
                for c in &cookies {
                    map.insert(c.name().to_string(), c.value().to_string());
                }
                let has_sso = SSO_COOKIES.iter().any(|s| map.contains_key(*s));
                if has_sso {
                    let ttwid = map.get("ttwid").cloned().unwrap_or_default();
                    let (device_id, web_id) = crate::browser_cookies::obtain_ids(&ttwid);
                    let params = AsrParams { cookies: map, device_id, web_id };
                    if creds::save_params(&params) {
                        applog::info("登录态已从登录窗口收割并保存，凭证更新完成");
                        success = true;
                    } else {
                        applog::error("凭证保存失败（详见日志）");
                    }
                    break;
                }
            }
            Err(e) => {
                // 窗口刚建好/导航中可能暂时取不到，静默重试；持续失败低频记录
                if Instant::now() > deadline {
                    applog::warn(&format!("读取登录窗口 cookie 失败: {}", e));
                    break;
                }
            }
        }
        if Instant::now() > deadline {
            applog::warn("等待登录超时（600 秒），终止登录窗口");
            break;
        }
    }
    let _ = win.close();
    finish(app, shared, success, exit_when_done);
}

fn finish(app: &AppHandle, shared: &Arc<AppShared>, ok: bool, exit_when_done: bool) {
    shared.login_pending.store(false, Ordering::SeqCst);
    let logged = creds::load_params().is_some();
    shared.logged_in.store(logged, Ordering::SeqCst);
    shared.post_login_done(ok);
    shared.post_status(
        "login",
        if logged { "已登录".into() } else { "未登录".into() },
    );
    if exit_when_done {
        if ok {
            println!("登录成功，凭证已保存。");
        } else {
            println!("未完成登录（详见日志）。");
        }
        app.exit(if ok { 0 } else { 1 });
    }
}
