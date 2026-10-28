// util.rs — 通用工具：宽字符串、时间戳、控制台输出
#![allow(dead_code)]

use std::time::{SystemTime, UNIX_EPOCH};
use windows::core::PCWSTR;

/// UTF-8 → UTF-16 空终止宽字符串（用于 Win32 W 系列 API）
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn pcw(s: &str) -> PCWSTR {
    PCWSTR::from_raw(wide(s).as_ptr())
}

/// 不含空终止的宽字符串
pub fn wide_nz(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// 本地时间字符串 "YYYY-MM-DD HH:MM:SS,mmm"（对应 Python logging 默认格式）
pub fn local_time_str() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = now.as_secs() as i64;
    let ms = now.subsec_millis();
    let (y, mo, d, h, mi, s) = civil_from_unix(secs);
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02},{:03}", y, mo, d, h, mi, s, ms)
}

/// 简短时间 "HH:MM:SS"（主控窗口日志区用）
pub fn short_time_str() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let (.., h, mi, s) = civil_from_unix(now.as_secs() as i64);
    format!("{:02}:{:02}:{:02}", h, mi, s)
}

/// unix 秒 → 本地民用时间（时区取系统当前偏移；本机时区由 tz 环境推不出，
/// 用 Windows GetTimeZoneInformation 太重——这里用运行时一次取偏移）
fn civil_from_unix(secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    let bias = tz_bias_seconds(); // 本地 = UTC - bias（分钟*60）
    let t = secs + bias;
    let days = t.div_euclid(86400);
    let rem = t.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    (y, m, d, (rem / 3600) as u32, ((rem % 3600) / 60) as u32, (rem % 60) as u32)
}

fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    ((y + if m <= 2 { 1 } else { 0 }) as i32, m as u32, d as u32)
}

/// 当前时区偏移（秒）。用 Windows GetTickCount 对比本地/系统时间不可靠，
/// 直接调用 GetTimeZoneInformation（动态 DST 正确）。
fn tz_bias_seconds() -> i64 {
    use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
    unsafe {
        let mut tzi = TIME_ZONE_INFORMATION::default();
        let rc = GetTimeZoneInformation(&mut tzi);
        let bias: i64 = tzi.Bias as i64; // UTC = local + bias(minutes)
        // rc==2（DST 生效）时 StandardBias→DaylightBias
        let extra = if rc == 2 { tzi.DaylightBias as i64 } else { tzi.StandardBias as i64 };
        -((bias + extra) * 60) // 返回 local = utc + offset
    }
}

/// 高精度单调时钟（秒）
pub fn monotonic() -> f64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static T0: OnceLock<Instant> = OnceLock::new();
    let t0 = T0.get_or_init(Instant::now);
    t0.elapsed().as_secs_f64()
}

/// epoch 秒（time.time() 等价，用于录音时间戳）
pub fn epoch_now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64()
}
