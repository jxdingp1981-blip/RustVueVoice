// applog.rs — 全局日志：文件 + 控制台（WriteConsoleW 直写，免代码页问题）+ GUI 广播
#![allow(dead_code)]

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::util;

enum Sink {
    File(File),
}

struct Logger {
    sink: Option<Sink>,
    gui: Option<Box<dyn Fn(&str) + Send>>,
    console: bool,
}

static LOGGER: OnceLock<Mutex<Logger>> = OnceLock::new();
static QUIET: AtomicBool = AtomicBool::new(false);

/// 初始化日志。console=true 时向标准输出写（若已附加控制台）。
pub fn init(log_file: Option<&std::path::Path>, console: bool) {
    let file = log_file.and_then(|p| {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        OpenOptions::new().create(true).append(true).open(p).ok()
    });
    let _ = LOGGER.set(Mutex::new(Logger { sink: file.map(Sink::File), gui: None, console }));
}

/// GUI 日志区接入：之后所有日志行都会回调给该函数（任意线程）
pub fn attach_gui(f: Box<dyn Fn(&str) + Send>) {
    if let Some(l) = LOGGER.get() {
        if let Ok(mut l) = l.lock() {
            l.gui = Some(f);
        }
    }
}

pub fn set_quiet(q: bool) {
    QUIET.store(q, Ordering::Relaxed);
}

pub fn log(level: &str, msg: &str) {
    let line = format!("{} {} {}", util::local_time_str(), level, msg);
    // 控制台
    if !QUIET.load(Ordering::Relaxed) {
        console_write(&line);
    }
    // 文件 + GUI
    if let Some(l) = LOGGER.get() {
        if let Ok(mut l) = l.lock() {
            if let Some(Sink::File(f)) = l.sink.as_mut() {
                let _ = writeln!(f, "{}", line);
            }
            if let Some(f) = &l.gui {
                f(&line);
            }
        }
    }
}

pub fn info(msg: impl AsRef<str>) {
    log("INFO", msg.as_ref());
}
pub fn warn(msg: impl AsRef<str>) {
    log("WARNING", msg.as_ref());
}
pub fn error(msg: impl AsRef<str>) {
    log("ERROR", msg.as_ref());
}

/// 控制台输出：优先 WriteConsoleW（Unicode 无乱码），回退 stdout
fn console_write(line: &str) {
    use windows::Win32::System::Console::{
        AttachConsole, GetStdHandle, WriteConsoleW, ATTACH_PARENT_PROCESS, STD_OUTPUT_HANDLE,
    };
    unsafe {
        static ATTACHED: std::sync::OnceLock<bool> = OnceLock::new();
        let attached = *ATTACHED.get_or_init(|| AttachConsole(ATTACH_PARENT_PROCESS).is_ok());
        if attached {
            if let Ok(h) = GetStdHandle(STD_OUTPUT_HANDLE) {
                let s = line.encode_utf16().collect::<Vec<_>>();
                let mut written = 0u32;
                let _ = WriteConsoleW(h, &s, Some(&mut written), None);
                let nl = [0x000Du16, 0x000A];
                let _ = WriteConsoleW(h, &nl, Some(&mut written), None);
                return;
            }
        }
    }
    // 无控制台（双击运行）：丢弃；但 --check 等场景依赖前述路径
    println!("{}", line);
}
