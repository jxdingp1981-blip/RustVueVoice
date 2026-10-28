// autostart.rs — 开机自启：读写 HKCU\Software\Microsoft\Windows\CurrentVersion\Run
// 注册表是唯一持久化位置（不经 config.json），主窗口「开机自动启动」复选框直接联动
#![allow(dead_code)]

use windows::core::PCWSTR;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
    REG_VALUE_TYPE,
};

use crate::util;

const RUN_SUBKEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const VALUE_NAME: &str = "RustVueVoice";

/// 当前是否已注册开机自启（Run 项下存在本程序值）
pub fn is_enabled() -> bool {
    unsafe {
        let sub = util::wide(RUN_SUBKEY);
        let val = util::wide(VALUE_NAME);
        let mut hk = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(sub.as_ptr()), None, KEY_QUERY_VALUE, &mut hk)
            != ERROR_SUCCESS
        {
            return false;
        }
        let mut ty = REG_VALUE_TYPE(0);
        let mut cb = 0u32;
        let ok = RegQueryValueExW(
            hk,
            PCWSTR(val.as_ptr()),
            None,
            Some(&mut ty),
            None,
            Some(&mut cb),
        ) == ERROR_SUCCESS;
        let _ = RegCloseKey(hk);
        ok
    }
}

/// 启用/停用开机自启。启用时写入当前 exe 路径（已开启但 exe 移动过也会顺带刷新）。
pub fn set_enabled(on: bool) -> bool {
    if !on {
        return disable();
    }
    let Ok(exe) = std::env::current_exe() else { return false };
    // 路径加引号，兼容带空格的目录
    let mut data: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().collect();
    data.push(0);
    unsafe {
        let sub = util::wide(RUN_SUBKEY);
        let val = util::wide(VALUE_NAME);
        let mut hk = HKEY::default();
        if RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(sub.as_ptr()),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut hk,
            None,
        ) != ERROR_SUCCESS
        {
            return false;
        }
        let bytes =
            std::slice::from_raw_parts(data.as_ptr().cast::<u8>(), data.len() * std::mem::size_of::<u16>());
        let ok = RegSetValueExW(hk, PCWSTR(val.as_ptr()), None, REG_SZ, Some(bytes)) == ERROR_SUCCESS;
        let _ = RegCloseKey(hk);
        ok
    }
}

fn disable() -> bool {
    unsafe {
        let sub = util::wide(RUN_SUBKEY);
        let val = util::wide(VALUE_NAME);
        let mut hk = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(sub.as_ptr()), None, KEY_SET_VALUE, &mut hk)
            != ERROR_SUCCESS
        {
            return true; // Run 键不存在 = 本来就没开
        }
        let r = RegDeleteValueW(hk, PCWSTR(val.as_ptr()));
        let _ = RegCloseKey(hk);
        r == ERROR_SUCCESS || r == ERROR_FILE_NOT_FOUND
    }
}
