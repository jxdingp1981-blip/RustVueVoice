// output.rs — 剪贴板写入 + 模拟键盘输出（unicode 逐字 / ctrl_v），对应 voice_mouse.py 剪贴板段
#![allow(dead_code)]

use std::thread;
use std::time::Duration;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
    VIRTUAL_KEY,
};

const CF_UNICODETEXT: u32 = 13;
const VK_CONTROL: u16 = 0x11;
const VK_V: u16 = 0x56;
const VK_RETURN: u16 = 0x0D;

/// 把文本写入剪贴板（重试 20 次等待其它程序释放，与 Python 版一致）
pub fn set_clipboard_text(text: &str) -> bool {
    unsafe {
        let mut w: Vec<u16> = text.encode_utf16().collect();
        w.push(0);
        let size_bytes = w.len() * 2;
        for _ in 0..20 {
            if OpenClipboard(None).is_ok() {
                let ok = (|| {
                    if EmptyClipboard().is_err() {
                        return false;
                    }
                    let h = match GlobalAlloc(GMEM_MOVEABLE, size_bytes) {
                        Ok(h) => h,
                        Err(_) => return false,
                    };
                    let p = GlobalLock(h);
                    if !p.is_null() {
                        std::ptr::copy_nonoverlapping(w.as_ptr(), p as *mut u16, w.len());
                        let _ = GlobalUnlock(h);
                    }
                    // 系统接管内存（成功后不得再 GlobalFree）
                    SetClipboardData(CF_UNICODETEXT, Some(HANDLE(h.0))).is_ok()
                })();
                let _ = CloseClipboard();
                return ok;
            }
            thread::sleep(Duration::from_millis(20));
        }
        false
    }
}

struct KeyEv {
    vk: u16,
    scan: u16,
    flags: u32,
}

fn send_events(evs: &[KeyEv]) {
    unsafe {
        let inputs: Vec<INPUT> = evs
            .iter()
            .map(|e| INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(e.vk),
                        wScan: e.scan,
                        dwFlags: windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS(e.flags),
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            })
            .collect();
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

/// 模拟键盘逐字输入（KEYEVENTF_UNICODE），输入到光标位置
/// （32 键一批、批间 10ms，与 Python type_text_unicode 一致）
pub fn type_text_unicode(text: &str) {
    let mut batch: Vec<KeyEv> = Vec::new();
    for ch in text.chars() {
        if ch == '\r' {
            continue;
        }
        if ch == '\n' {
            batch.push(KeyEv { vk: VK_RETURN, scan: 0, flags: 0 });
            batch.push(KeyEv { vk: VK_RETURN, scan: 0, flags: KEYEVENTF_KEYUP.0 });
        } else {
            let code = ch as u16;
            batch.push(KeyEv { vk: 0, scan: code, flags: KEYEVENTF_UNICODE.0 });
            batch.push(KeyEv { vk: 0, scan: code, flags: KEYEVENTF_UNICODE.0 | KEYEVENTF_KEYUP.0 });
        }
        if batch.len() >= 32 {
            send_events(&batch);
            batch.clear();
            thread::sleep(Duration::from_millis(10));
        }
    }
    if !batch.is_empty() {
        send_events(&batch);
    }
}

fn send_key(vk: u16, flags: u32) {
    send_events(&[KeyEv { vk, scan: 0, flags }]);
    thread::sleep(Duration::from_millis(10));
}

/// 模拟 Ctrl+V 粘贴
pub fn paste_clipboard() {
    send_key(VK_CONTROL, 0);
    send_key(VK_V, 0);
    send_key(VK_V, KEYEVENTF_KEYUP.0);
    send_key(VK_CONTROL, KEYEVENTF_KEYUP.0);
}

#[cfg(test)]
mod tests {
    // 不在 CI 里实际操作剪贴板；函数存在性由编译保证
    #[test]
    fn nothing() {}
}
