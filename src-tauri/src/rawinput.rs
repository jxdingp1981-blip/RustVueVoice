// rawinput.rs — Raw Input 按键监听（F19 录音 / F18 翻译 / F20 AI，按设备 VID 过滤）
// 对应 Python raw_input_listener.py：隐藏窗口 + RIDEV_INPUTSINK，只响应 Rulink 接收器
#![allow(dead_code)]

use std::sync::mpsc::Sender;
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::{
    GetRawInputData, GetRawInputDeviceInfoW, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER,
    RAWKEYBOARD, RIDEV_INPUTSINK, RIDI_DEVICENAME, RID_INPUT, RIM_TYPEKEYBOARD,
    RegisterRawInputDevices,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, PostMessageW, RegisterClassW,
    TranslateMessage, MSG, WNDCLASSW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_INPUT,
};

use crate::util;

pub const VK_VOICE: u16 = 130; // F19 录音
pub const VK_TRANSLATE: u16 = 129; // F18 翻译
pub const VK_AI: u16 = 131; // F20 AI
const RI_KEY_BREAK: u16 = 0x01;

pub fn key_name(vk: u16) -> String {
    match vk {
        VK_VOICE => "录音键(F19)".into(),
        VK_TRANSLATE => "翻译键(F18)".into(),
        VK_AI => "AI键(F20)".into(),
        _ => format!("VK{}", vk),
    }
}

struct RawCfg {
    tx: Sender<(u16, bool)>,
    watch: [u16; 3],
    dev_tag: String,
}

static CFG: OnceLock<RawCfg> = OnceLock::new();
static HWND_CELL: Mutex<Option<isize>> = Mutex::new(None);

pub struct RawInputKeyListener {
    thread: Option<std::thread::JoinHandle<()>>,
}

impl RawInputKeyListener {
    /// 启动监听线程；按键事件 (vk, down) 经 tx 发送
    pub fn start(tx: Sender<(u16, bool)>, dev_tag: String) -> Self {
        let _ = CFG.set(RawCfg { tx, watch: [VK_VOICE, VK_TRANSLATE, VK_AI], dev_tag });
        let thread = std::thread::Builder::new()
            .name("rawinput".into())
            .spawn(|| {
                unsafe { run_loop() };
            })
            .ok();
        Self { thread }
    }

    pub fn stop(&mut self) {
        if let Ok(g) = HWND_CELL.lock() {
            if let Some(h) = *g {
                unsafe {
                    let _ = PostMessageW(Some(HWND(h as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            }
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

unsafe fn run_loop() {
    let hinst = GetModuleHandleW(None).unwrap_or_default();
    let class_name = util::wide("RustVoiceMouseRawWnd");
    let wc = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: hinst.into(),
        lpszClassName: windows::core::PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };
    let atom = RegisterClassW(&wc);
    if atom == 0 {
        crate::applog::error("RawInput: RegisterClass 失败");
        return;
    }
    let title = util::wide("");
    let hwnd = match CreateWindowExW(
        WINDOW_EX_STYLE(0),
        wc.lpszClassName,
        windows::core::PCWSTR(title.as_ptr()),
        WINDOW_STYLE(0), // 隐藏窗口
        0, 0, 0, 0,
        None, None, Some(hinst.into()), None,
    ) {
        Ok(h) => h,
        Err(_) => {
            crate::applog::error("RawInput: 创建监听窗口失败");
            return;
        }
    };
    if let Ok(mut g) = HWND_CELL.lock() {
        *g = Some(hwnd.0 as isize);
    }
    let rid = RAWINPUTDEVICE {
        usUsagePage: 0x01,
        usUsage: 0x06, // 键盘
        dwFlags: RIDEV_INPUTSINK,
        hwndTarget: hwnd,
    };
    if RegisterRawInputDevices(&[rid], std::mem::size_of::<RAWINPUTDEVICE>() as u32).is_err() {
        crate::applog::error("RawInput: RegisterRawInputDevices 失败");
        return;
    }
    crate::applog::info("RawInput 监听已启动（usagePage=1 usage=6, 输入槽模式）");

    let mut msg = MSG::default();
    loop {
        let r = GetMessageW(&mut msg, None, 0, 0);
        if r.as_bool() == false && r.0 == 0 {
            break; // WM_QUIT
        }
        if r.0 == -1 {
            break;
        }
        let _ = TranslateMessage(&msg);
        let _ = DispatchMessageW(&msg);
    }
    crate::applog::info("RawInput 监听已退出");
    if let Ok(mut g) = HWND_CELL.lock() {
        *g = None;
    }
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_INPUT {
        handle_wm_input(lparam);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

unsafe fn handle_wm_input(lparam: LPARAM) {
    let hraw = windows::Win32::UI::Input::HRAWINPUT(lparam.0 as *mut core::ffi::c_void);
    let mut size: u32 = 0;
    GetRawInputData(
        hraw,
        RID_INPUT,
        None,
        &mut size,
        std::mem::size_of::<RAWINPUTHEADER>() as u32,
    );
    if size == 0 {
        return;
    }
    let mut buf = vec![0u8; size as usize];
    let got = GetRawInputData(
        hraw,
        RID_INPUT,
        Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
        &mut size,
        std::mem::size_of::<RAWINPUTHEADER>() as u32,
    );
    if got == 0 || got == u32::MAX {
        return;
    }
    let raw = buf.as_ptr() as *const RAWINPUT;
    if (*raw).header.dwType != RIM_TYPEKEYBOARD.0 {
        return;
    }
    let k: RAWKEYBOARD = (*raw).data.keyboard;
    let vk = k.VKey;
    let cfg = match CFG.get() {
        Some(c) => c,
        None => return,
    };
    if !cfg.watch.contains(&vk) {
        return;
    }
    let name = device_name((*raw).header.hDevice);
    if !name.to_uppercase().contains(&cfg.dev_tag) {
        return;
    }
    let down = k.Flags & RI_KEY_BREAK == 0;
    let _ = cfg.tx.send((vk, down));
}

unsafe fn device_name(hdevice: HANDLE) -> String {
    if hdevice.0.is_null() {
        return String::new();
    }
    let mut size: u32 = 0;
    let _ = GetRawInputDeviceInfoW(Some(hdevice), RIDI_DEVICENAME, None, &mut size);
    if size == 0 {
        return String::new();
    }
    let mut buf = vec![0u16; size as usize];
    let got = GetRawInputDeviceInfoW(
        Some(hdevice),
        RIDI_DEVICENAME,
        Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
        &mut size,
    );
    if got == 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf)
}
