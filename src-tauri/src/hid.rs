// hid.rs — Rulink/MR08 语音鼠标厂商 HID 接口：枚举（SetupDi）+ 后台读流 + 命令写入
// 对应 Python hid_audio.py（枚举改用 SetupDi 官方 API，语义与注册表枚举一致）
#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
    SetupDiGetDeviceInterfaceDetailW, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT,
    SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
};
use windows::Win32::Devices::HumanInterfaceDevice::{
    HidD_FreePreparsedData, HidD_GetAttributes, HidD_GetPreparsedData, HidP_GetCaps,
    GUID_DEVINTERFACE_HID, HIDD_ATTRIBUTES, HIDP_CAPS, PHIDP_PREPARSED_DATA,
};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_EVENT, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_CREATION_DISPOSITION, FILE_FLAGS_AND_ATTRIBUTES,
    FILE_SHARE_MODE,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Threading::{CreateEventW, ResetEvent, WaitForSingleObject};

use crate::applog;
use crate::util;

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const FILE_FLAG_OVERLAPPED_U32: u32 = 0x4000_0000;
const OPEN_EXISTING: u32 = 3;
const ERROR_IO_PENDING: i32 = 0x8007_03E5u32 as i32; // HRESULT(997)

#[derive(Debug, Clone)]
pub struct HidDeviceInfo {
    pub path: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub usage_page: u16,
    pub usage: u16,
    pub input_report_len: u16,
    pub output_report_len: u16,
}

impl HidDeviceInfo {
    pub fn describe(&self) -> String {
        format!(
            "HID(VID_{:04X}&PID_{:04X} UsagePage=0x{:04X} Usage=0x{:04X} in={}B out={}B)",
            self.vendor_id, self.product_id, self.usage_page, self.usage,
            self.input_report_len, self.output_report_len
        )
    }
}

/// 用 SetupDi 枚举本机所有 HID 接口路径
fn enumerate_hid_paths() -> Vec<String> {
    let mut paths = Vec::new();
    unsafe {
        let Ok(devs) = SetupDiGetClassDevsW(
            Some(&GUID_DEVINTERFACE_HID),
            None,
            None,
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        ) else {
            applog::warn("SetupDiGetClassDevsW 失败");
            return paths;
        };
        let mut idx: u32 = 0;
        loop {
            let mut if_data = SP_DEVICE_INTERFACE_DATA::default();
            if_data.cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
            if SetupDiEnumDeviceInterfaces(devs, None, &GUID_DEVINTERFACE_HID, idx, &mut if_data)
                .is_err()
            {
                break;
            }
            idx += 1;
            let mut needed: u32 = 0;
            let _ = SetupDiGetDeviceInterfaceDetailW(devs, &if_data, None, 0, Some(&mut needed), None);
            if needed == 0 {
                continue;
            }
            // detail data 头部是 cbSize(4B) + DevicePath[ANYSIZE]
            let mut buf: Vec<u8> = vec![0u8; needed as usize];
            let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            (*detail).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            if SetupDiGetDeviceInterfaceDetailW(devs, &if_data, Some(detail), needed, None, None)
                .is_ok()
            {
                // DevicePath 紧跟 cbSize 之后（ANSIZ 数组首元素）
                let path_ptr = (*detail).DevicePath.as_ptr();
                let max_chars = (needed as usize - 4) / 2;
                let mut len = 0usize;
                while len < max_chars && *path_ptr.add(len) != 0 {
                    len += 1;
                }
                let ws = std::slice::from_raw_parts(path_ptr, len);
                paths.push(String::from_utf16_lossy(ws));
            }
        }
        let _ = SetupDiDestroyDeviceInfoList(devs);
    }
    paths
}

fn open_device(path: &str, access: u32, overlapped: bool) -> Option<HANDLE> {
    let wpath = util::wide(path);
    unsafe {
        let flags = if overlapped { FILE_FLAG_OVERLAPPED_U32 } else { 0 };
        CreateFileW(
            windows::core::PCWSTR(wpath.as_ptr()),
            access,
            FILE_SHARE_MODE(0x3), // READ | WRITE
            None,
            FILE_CREATION_DISPOSITION(OPEN_EXISTING),
            FILE_FLAGS_AND_ATTRIBUTES(flags),
            None,
        )
        .ok()
    }
}

/// 打开 HID 设备读取 VID/PID/Usage/报告长度（探针，读完关闭）
fn probe_hid(path: &str) -> Option<HidDeviceInfo> {
    let handle = open_device(path, GENERIC_READ | GENERIC_WRITE, false)
        .or_else(|| open_device(path, GENERIC_READ, false))?;
    let result = unsafe {
        let mut attr = HIDD_ATTRIBUTES::default();
        attr.Size = std::mem::size_of::<HIDD_ATTRIBUTES>() as u32;
        if !HidD_GetAttributes(handle, &mut attr) {
            return None;
        }
        let (mut usage_page, mut usage, mut in_len, mut out_len) = (0u16, 0u16, 0u16, 0u16);
        let mut preparsed = PHIDP_PREPARSED_DATA(0);
        if HidD_GetPreparsedData(handle, &mut preparsed) && preparsed.0 != 0 {
            let mut caps = HIDP_CAPS::default();
            let status = HidP_GetCaps(preparsed, &mut caps);
            if (status.0 as u32 & 0x8000_0000) == 0 {
                usage_page = caps.UsagePage;
                usage = caps.Usage;
                in_len = caps.InputReportByteLength;
                out_len = caps.OutputReportByteLength;
            }
            let _ = HidD_FreePreparsedData(preparsed);
        }
        Some(HidDeviceInfo {
            path: path.to_string(),
            vendor_id: attr.VendorID,
            product_id: attr.ProductID,
            usage_page,
            usage,
            input_report_len: in_len,
            output_report_len: out_len,
        })
    };
    unsafe {
        let _ = CloseHandle(handle);
    }
    result
}

/// 找到接收器的所有 HID 接口（不区分 MI）
pub fn find_rulink_devices(vid: u16, pid: u16) -> Vec<HidDeviceInfo> {
    let want = format!("VID_{:04X}&PID_{:04X}", vid, pid).to_uppercase();
    enumerate_hid_paths()
        .into_iter()
        .filter(|p| p.to_uppercase().contains(&want))
        .filter_map(|p| probe_hid(&p))
        .collect()
}

/// 找到语音数据所在的厂商自定义 HID 接口（复合设备 MI_00，UsagePage 0xFF02）
pub fn find_voice_device(vid: u16, pid: u16, mi: &str) -> Option<HidDeviceInfo> {
    let mi_up = mi.to_uppercase();
    for info in find_rulink_devices(vid, pid) {
        let up = info.path.to_uppercase();
        if up.contains("&COL") {
            continue;
        }
        if !up.contains(&format!("&MI_{}#", mi_up)) {
            continue;
        }
        if info.usage_page != 0xFF02 && info.input_report_len == 0 {
            continue;
        }
        return Some(info);
    }
    None
}

/// 后台 HID 流：常驻一个挂起的 ReadFile，每包回调 Sender；支持 write() 发送命令。
/// （HANDLE 不跨线程存放，内部统一转 isize）
pub struct HidStream {
    info: HidDeviceInfo,
    stop: Arc<AtomicBool>,
    handle: Arc<Mutex<isize>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl HidStream {
    pub fn start(info: HidDeviceInfo, packet_tx: Sender<(Vec<u8>, f64)>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let handle = Arc::new(Mutex::new(0isize));
        let stop2 = stop.clone();
        let handle2 = handle.clone();
        let info2 = info.clone();
        let thread = std::thread::Builder::new()
            .name("hid-reader".into())
            .spawn(move || HidStream::run(&info2, &stop2, &handle2, &packet_tx))
            .ok();
        Self { info, stop, handle, thread }
    }

    fn current_handle(&self) -> Option<HANDLE> {
        let h = *self.handle.lock().ok()?;
        if h == 0 {
            None
        } else {
            Some(HANDLE(h as *mut _))
        }
    }

    /// 向设备写一个输出报告（含 report id 字节，补齐到报告长度）
    pub fn write(&self, data: &[u8]) -> bool {
        let Some(handle) = self.current_handle() else { return false };
        unsafe {
            let size = std::cmp::max(self.info.output_report_len.max(65) as usize, data.len());
            let mut buf = vec![0u8; size];
            buf[..data.len()].copy_from_slice(data);
            let Ok(event) = CreateEventW(None, true, false, None) else { return false };
            let mut ov = OVERLAPPED::default();
            ov.hEvent = event;
            let mut n: u32 = 0;
            let mut ok = WriteFile(handle, Some(&buf), Some(&mut n), Some(&mut ov)).is_ok();
            if !ok {
                // overlapped 写返回 pending：等待完成
                let w = WaitForSingleObject(event, 1000);
                if w == WAIT_OBJECT_0 {
                    ok = GetOverlappedResult(handle, &mut ov, &mut n, false).is_ok();
                }
            }
            let _ = ResetEvent(event);
            let _ = CloseHandle(event);
            ok
        }
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.current_handle() {
            unsafe {
                let _ = CancelIoEx(h, None);
            }
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    fn run(
        info: &HidDeviceInfo,
        stop: &AtomicBool,
        shared: &Mutex<isize>,
        packet_tx: &Sender<(Vec<u8>, f64)>,
    ) {
        unsafe {
            let buf_len = std::cmp::max(64, info.input_report_len.max(65) as usize);
            while !stop.load(Ordering::SeqCst) {
                let handle = open_device(&info.path, GENERIC_READ | GENERIC_WRITE, true)
                    .or_else(|| open_device(&info.path, GENERIC_READ, true));
                let Some(handle) = handle else {
                    applog::warn("HID 打开失败，2 秒后重试");
                    std::thread::sleep(Duration::from_secs(2));
                    continue;
                };
                *shared.lock().unwrap() = handle.0 as isize;
                let Ok(event) = CreateEventW(None, true, false, None) else {
                    *shared.lock().unwrap() = 0;
                    let _ = CloseHandle(handle);
                    return;
                };
                let mut buf = vec![0u8; buf_len];
                let mut ov = OVERLAPPED::default();
                ov.hEvent = event;
                loop {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let r = ReadFile(handle, Some(&mut buf), None, Some(&mut ov));
                    if let Err(e) = r {
                        if e.code().0 != ERROR_IO_PENDING {
                            break; // 设备拔出等
                        }
                    }
                    let w: WAIT_EVENT = WaitForSingleObject(event, 500);
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    if w == WAIT_TIMEOUT {
                        continue;
                    }
                    if w != WAIT_OBJECT_0 {
                        break;
                    }
                    let mut got: u32 = 0;
                    if GetOverlappedResult(handle, &mut ov, &mut got, false).is_err() {
                        break;
                    }
                    if got > 0 {
                        let _ = packet_tx.send((buf[..got as usize].to_vec(), util::epoch_now()));
                    }
                    let _ = ResetEvent(event);
                }
                let _ = CancelIoEx(handle, None);
                let _ = CloseHandle(event);
                *shared.lock().unwrap() = 0;
                let _ = CloseHandle(handle);
                if !stop.load(Ordering::SeqCst) {
                    applog::warn("HID 读取中断（设备可能已拔出），2 秒后重连");
                    std::thread::sleep(Duration::from_secs(2));
                }
            }
        }
    }
}
