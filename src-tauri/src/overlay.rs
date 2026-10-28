// overlay.rs — 屏幕居中圆角胶囊悬浮层（点击穿透 / 不抢焦点 / 呼吸+流光+波形动效）
// 视觉规格与 overlay.py 完全一致；实现改为原生 Win32 分层窗口（UpdateLayeredWindow
// 逐像素 alpha 画圆角 + SourceConstantAlpha 做呼吸）+ GDI+ 绘制。
#![allow(dead_code)]

use std::f64::consts::PI;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    HDC,
};
use windows::Win32::Graphics::GdiPlus::{
    GdipCreateFont, GdipCreateFontFamilyFromName, GdipCreateFromHDC, GdipCreatePen1,
    GdipCreateSolidFill, GdipDeleteBrush, GdipDeleteFont, GdipDeleteFontFamily,
    GdipDeleteGraphics, GdipDeletePen, GdipDeleteStringFormat, GdipDrawPolygon, GdipDrawString,
    GdipFillPolygon, GdipFillRectangleI, GdipMeasureString, GdipSetSmoothingMode,
    GdipSetStringFormatAlign, GdipSetStringFormatLineAlign, GdipSetTextRenderingHint,
    GdipStringFormatGetGenericDefault, GdiplusShutdown, GdiplusStartup, FontStyleBold,
    FontStyleRegular, GpBrush, GpFont, GpFontFamily, GpGraphics, GpPen, GpSolidFill,
    GpStringFormat, GdiplusStartupInput, PointF, RectF, SmoothingModeAntiAlias,
    StringAlignmentCenter, StringAlignmentNear, TextRenderingHintAntiAliasGridFit, UnitPixel,
    FillModeAlternate,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetSystemMetrics, GetWindowLongPtrW, GWL_EXSTYLE, KillTimer, PostMessageW, PostQuitMessage,
    RegisterClassW, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage,
    UpdateLayeredWindow,
    WINDOW_EX_STYLE, WM_CLOSE, WM_DESTROY, WM_PAINT, WM_TIMER, MSG, WNDCLASSW, HWND_TOPMOST,
    SW_HIDE, SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
    ULW_ALPHA, UPDATE_LAYERED_WINDOW_FLAGS, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::util;

// ---- 几何 / 视觉常量（照抄 overlay.py） ----
pub const OVERLAY_WIDTH: i32 = 560;
pub const OVERLAY_HEIGHT: i32 = 128;
const CORNER_PX: f64 = 18.0;
const WAVE_BARS: usize = 24;
const WAVE_BAR_W: f64 = 4.0;
const WAVE_BAR_GAP: f64 = 3.0;
const WAVE_BAR_MAX_H: f64 = 24.0; // 有文字时 18
const WAVE_CY: f64 = 30.0;
const STATUS_Y: f64 = 58.0;
const TEXT_Y: f64 = 88.0;
const TEXT_KEEP_TAIL: usize = 100;

// ARGB（0xAARRGGBB）
const CAPSULE_FILL: u32 = 0xFF12141A;
const BORDER_COLOR: u32 = 0xFF585A5E;
const WAVE_COLOR: u32 = 0xFF7FF2A6;
const TEXT_COLOR: u32 = 0xFFFFFFFF;
const STATUS_COLOR: u32 = 0xFFC0C6D4;
const STATUS_OK_COLOR: u32 = 0xFF7DD48A;
const STATUS_ERR_COLOR: u32 = 0xFFFF7A80;

// ---- 动画参数（照抄 overlay.py） ----
const TICK_MS: u32 = 60;
const BREATH_PERIOD_S: f64 = 2.4;
const OPACITY_BREATH_MIN: f64 = 0.78;
const OPACITY_BREATH_MAX: f64 = 0.98;
const GLINT_PEAK: f64 = 1.00;
const GLINT_HOLD_S: f64 = 0.22;
const GLINT_DECAY_S: f64 = 0.40;

// ---- 事件（对应 overlay.post 的元组） ----
#[derive(Debug, Clone)]
pub enum OverlayEvent {
    Show,
    Hide,
    Status(String),
    Partial(String),
    Level(f64),
    Done(String, u32),   // (text, hide_ms)
    Cancel(String, u32), // (text, hide_ms)
    Recognizing,         // 已松手，正在识别（停止计时，显示“识别中…”）
}

struct OverlayShared {
    rx: Mutex<Option<Receiver<OverlayEvent>>>,
    hwnd: Mutex<isize>,
    gdiplus_token: Mutex<usize>,
}

static OVERLAY: OnceLock<OverlayShared> = OnceLock::new();
static SCREEN_SIZE: OnceLock<Mutex<(i32, i32)>> = OnceLock::new();
static DPI_SCALE: OnceLock<f64> = OnceLock::new();

/// 悬浮层几何/字体缩放系数（96 DPI = 1.0）。悬浮层按系统 DPI（创建于主屏）等比放大，
/// 否则 2K/4K 高分屏上胶囊和文字都过小
fn scale() -> f64 {
    *DPI_SCALE.get_or_init(|| unsafe {
        (windows::Win32::UI::HiDpi::GetDpiForSystem() as f64 / 96.0).max(1.0)
    })
}
fn sf(v: f64) -> f64 {
    v * scale()
}
fn si(v: i32) -> i32 {
    (v as f64 * scale()).round() as i32
}

fn screen_size() -> (i32, i32) {
    *SCREEN_SIZE.get_or_init(|| Mutex::new((OVERLAY_WIDTH, OVERLAY_HEIGHT))).lock().unwrap()
}

/// 句柄：任意线程 post 事件
#[derive(Clone)]
pub struct OverlayHandle {
    tx: Sender<OverlayEvent>,
}

impl OverlayHandle {
    pub fn post(&self, ev: OverlayEvent) {
        let _ = self.tx.send(ev);
        unsafe {
            if let Some(o) = OVERLAY.get() {
                let h = *o.hwnd.lock().unwrap();
                if h != 0 {
                    // 唤醒 UI 线程立即处理（不等待 60ms tick）
                    let _ = PostMessageW(Some(HWND(h as *mut _)), WM_TIMER, WPARAM(1), LPARAM(0));
                }
            }
        }
    }
}

/// UI 线程创建悬浮层窗口（须在窗口消息循环所在线程调用）
pub fn create_overlay() -> OverlayHandle {
    let (tx, rx) = channel::<OverlayEvent>();
    let shared = OVERLAY.get_or_init(|| OverlayShared {
        rx: Mutex::new(Some(rx)),
        hwnd: Mutex::new(0),
        gdiplus_token: Mutex::new(0),
    });
    unsafe {
        // GDI+ 初始化
        {
            let mut token = shared.gdiplus_token.lock().unwrap();
            if *token == 0 {
                // GdiplusVersion 必须为 1（default() 是 0，会导致启动失败→整层全透明）
                let input = GdiplusStartupInput {
                    GdiplusVersion: 1,
                    ..Default::default()
                };
                let mut tok = 0usize;
                if GdiplusStartup(&mut tok, &input, std::ptr::null_mut()).0 == 0 {
                    *token = tok;
                } else {
                    crate::applog::error("GdiplusStartup 失败（悬浮层将不可见）");
                }
            }
        }
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let class_name = util::wide("RustVoiceMouseOverlay");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(overlay_wndproc),
            hInstance: hinst.into(),
            lpszClassName: windows::core::PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        RegisterClassW(&wc);
        // 全屏覆盖窗口（最顶层）：胶囊绘制在屏幕中心，其余像素全透明穿透
        let sw = GetSystemMetrics(SM_CXSCREEN);
        let sh = GetSystemMetrics(SM_CYSCREEN);
        *SCREEN_SIZE.get_or_init(|| Mutex::new((0, 0))).lock().unwrap() = (sw, sh);
        let title = util::wide("VoiceMouseOverlay");
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(
                WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0 | WS_EX_NOACTIVATE.0
                    | WS_EX_TOOLWINDOW.0 | WS_EX_TOPMOST.0,
            ),
            wc.lpszClassName,
            windows::core::PCWSTR(title.as_ptr()),
            WS_POPUP,
            0, 0, sw, sh,
            None, None, Some(hinst.into()), None,
        )
        .unwrap_or_default();
        *shared.hwnd.lock().unwrap() = hwnd.0 as isize;
        let _ = SetTimer(Some(hwnd), 1, TICK_MS, None);
        OverlayHandle { tx }
    }
}

/// 请求关闭悬浮层（线程安全：向悬浮层线程投递 WM_CLOSE，由其消息循环完成清理）
pub fn destroy_overlay() {
    if let Some(o) = OVERLAY.get() {
        unsafe {
            let h = *o.hwnd.lock().unwrap();
            if h != 0 {
                let _ = PostMessageW(Some(HWND(h as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
    }
}

/// 在独立线程创建悬浮层并运行其消息循环（Tauri 主线程事件循环不由本程序管理，
/// 悬浮层必须自带泵）。返回事件句柄，任意线程可 post。
pub fn start_overlay_thread() -> OverlayHandle {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<OverlayHandle>();
    std::thread::Builder::new()
        .name("overlay".into())
        .spawn(move || {
            let handle = create_overlay();
            let _ = ready_tx.send(handle);
            unsafe {
                let mut msg = MSG::default();
                loop {
                    let r = GetMessageW(&mut msg, None, 0, 0);
                    if r.0 <= 0 {
                        break;
                    }
                    let _ = TranslateMessage(&msg);
                    let _ = DispatchMessageW(&msg);
                }
            }
            // 消息循环结束（WM_CLOSE → DestroyWindow → WM_DESTROY → WM_QUIT）
            if let Some(o) = OVERLAY.get() {
                unsafe {
                    let token = *o.gdiplus_token.lock().unwrap();
                    if token != 0 {
                        GdiplusShutdown(token);
                    }
                }
            }
            crate::applog::info("悬浮层线程已退出");
        })
        .ok();
    ready_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap_or_else(|_| {
            crate::applog::error("悬浮层线程未在 5 秒内就绪，悬浮层不可用（录音功能不受影响）");
            OverlayHandle {
                tx: std::sync::mpsc::channel().0,
            }
        })
}

unsafe extern "system" fn overlay_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    match msg {
        WM_TIMER => {
            overlay_tick(hwnd);
            windows::Win32::Foundation::LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            windows::Win32::Foundation::LRESULT(0)
        }
        WM_DESTROY => {
            let _ = KillTimer(Some(hwnd), 1);
            if let Some(o) = OVERLAY.get() {
                *o.hwnd.lock().unwrap() = 0;
            }
            PostQuitMessage(0);
            windows::Win32::Foundation::LRESULT(0)
        }
        WM_PAINT => {
            use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
            let mut ps = PAINTSTRUCT::default();
            let _hdc = BeginPaint(hwnd, &mut ps);
            let _ = EndPaint(hwnd, &ps);
            windows::Win32::Foundation::LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn overlay_tick(hwnd: HWND) {
    let Some(o) = OVERLAY.get() else { return };
    // 1) 消费事件（一批全清）
    loop {
        let ev = {
            let guard = o.rx.lock().unwrap();
            match guard.as_ref() {
                Some(rx) => rx.try_recv().ok(),
                None => None,
            }
        };
        match ev {
            Some(e) => drain_event(e),
            None => break,
        }
    }
    // 2) 动画 + 重绘
    let mut st = st_lock();
    if st.visible {
        let now = Instant::now();
        let epoch = *st.breath_epoch.get_or_insert(now);
        let phase = (now - epoch).as_secs_f64() / BREATH_PERIOD_S * 2.0 * PI;
        let b01 = 0.5 * (1.0 + phase.sin());
        let mut opacity = OPACITY_BREATH_MIN + (OPACITY_BREATH_MAX - OPACITY_BREATH_MIN) * b01;
        if let Some(g) = st.glint {
            let dt = (now - g).as_secs_f64();
            if dt < GLINT_HOLD_S {
                opacity = GLINT_PEAK;
            } else if dt < GLINT_HOLD_S + GLINT_DECAY_S {
                let t = (dt - GLINT_HOLD_S) / GLINT_DECAY_S;
                opacity = GLINT_PEAK + (opacity - GLINT_PEAK) * t;
            } else {
                st.glint = None;
            }
        }
        opacity = opacity.clamp(0.0, 1.0);
        if st.recording {
            let elapsed = (now - st.t0).as_secs_f64();
            st.status = format!("正在录音 {:.1}s · 松开结束", elapsed);
            st.status_color = STATUS_COLOR;
        }
        unsafe {
            redraw(hwnd, &st, opacity);
        }
    }
}

fn st_lock() -> std::sync::MutexGuard<'static, OverlayState> {
    static STATE: OnceLock<Mutex<OverlayState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(OverlayState::default())).lock().unwrap()
}

/// 一个事件的处理（照 overlay._poll 语义）
fn drain_event(ev: OverlayEvent) {
    let mut st = st_lock();
    match ev {
        OverlayEvent::Show => {
            st.status = "正在录音 · 松开结束".into();
            st.status_color = STATUS_COLOR;
            st.text.clear();
            st.levels = vec![0.0; WAVE_BARS];
            st.glint = None;
            st.visible = true;
            st.seq += 1;
            st.t0 = Instant::now();
            st.recording = true;
            show_window();
        }
        OverlayEvent::Hide => {
            st.recording = false;
            st.visible = false;
            unsafe {
                if let Some(o) = OVERLAY.get() {
                    let h = *o.hwnd.lock().unwrap();
                    if h != 0 {
                        let _ = ShowWindow(HWND(h as *mut _), SW_HIDE);
                    }
                }
            }
        }
        OverlayEvent::Status(s) => {
            st.status = s;
            st.status_color = STATUS_COLOR;
        }
        OverlayEvent::Partial(t) => {
            let grew = t.chars().count() > st.text.chars().count();
            st.text = t;
            let cnt = st.text.chars().count();
            if cnt > TEXT_KEEP_TAIL {
                let tail: String = st.text.chars().skip(cnt - TEXT_KEEP_TAIL).collect();
                st.text = format!("…{}", tail);
            }
            if grew && st.visible {
                st.glint = Some(Instant::now());
            }
        }
        OverlayEvent::Recognizing => {
            st.recording = false;
            st.status = "识别中…".into();
            st.status_color = STATUS_COLOR;
        }
        OverlayEvent::Level(v) => {
            let v = v.clamp(0.0, 1.0);
            st.levels.push(v);
            if st.levels.len() > WAVE_BARS {
                st.levels.remove(0);
            }
        }
        ev @ (OverlayEvent::Done(..) | OverlayEvent::Cancel(..)) => {
            let (text, hide_ms, is_done) = match ev {
                OverlayEvent::Done(t, ms) => (t, ms, true),
                OverlayEvent::Cancel(t, ms) => (t, ms, false),
                _ => unreachable!(),
            };
            st.recording = false;
            if !st.visible {
                st.visible = true;
                st.seq += 1;
                show_window();
            }
            st.status = if is_done { "✓ 已输出".into() } else { "✗ 已取消".into() };
            st.status_color = if is_done { STATUS_OK_COLOR } else { STATUS_ERR_COLOR };
            // 完整结果可能很长：最多保留尾部 200 字符（约 9 行），再多截断
            let cnt = text.chars().count();
            st.text = if cnt > TEXT_KEEP_TAIL * 2 {
                let tail: String = text.chars().skip(cnt - TEXT_KEEP_TAIL * 2).collect();
                format!("…{}", tail)
            } else {
                text
            };
            st.glint = None;
            let seq = st.seq;
            // 定时隐藏（新一轮 Show 会 ++seq 使本定时器失效）
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(hide_ms as u64));
                let mut st = st_lock();
                if st.seq == seq && st.visible {
                    st.visible = false;
                    st.recording = false;
                    unsafe {
                        if let Some(o) = OVERLAY.get() {
                            let h = *o.hwnd.lock().unwrap();
                            if h != 0 {
                                let _ = ShowWindow(HWND(h as *mut _), SW_HIDE);
                            }
                        }
                    }
                }
            });
        }
    }
}

fn show_window() {
    unsafe {
        let Some(o) = OVERLAY.get() else { return };
        let h = *o.hwnd.lock().unwrap();
        if h == 0 {
            return;
        }
        let hwnd = HWND(h as *mut _);
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0, 0, 0, 0,
            windows::Win32::UI::WindowsAndMessaging::SET_WINDOW_POS_FLAGS(
                SWP_NOMOVE.0 | SWP_NOSIZE.0 | SWP_NOACTIVATE.0 | SWP_SHOWWINDOW.0,
            ),
        );
        // 确保 WS_EX_ 样式存在（CreateWindowExW 已带，这里幂等）
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let want = WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0 | WS_EX_NOACTIVATE.0
            | WS_EX_TOOLWINDOW.0 | WS_EX_TOPMOST.0;
        if ex & want != want {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, (ex | want) as isize);
        }
    }
}

// ---------------- GDI+ 绘制 ----------------

fn capsule_points(x0: f64, y0: f64, x1: f64, y1: f64, r: f64, steps: usize) -> Vec<PointF> {
    let mut pts = Vec::new();
    let mut arc = |cx: f64, cy: f64, a0: f64, a1: f64| {
        for i in 0..=steps {
            let a = a0 + (a1 - a0) * i as f64 / steps as f64;
            pts.push(PointF { X: (cx + r * a.cos()) as f32, Y: (cy + r * a.sin()) as f32 });
        }
    };
    arc(x1 - r, y0 + r, -PI / 2.0, 0.0); // 右上
    arc(x1 - r, y1 - r, 0.0, PI / 2.0); // 右下
    arc(x0 + r, y1 - r, PI / 2.0, PI); // 左下
    arc(x0 + r, y0 + r, PI, 1.5 * PI); // 左上
    pts
}

struct Fonts {
    status: *mut GpFont,
    text: *mut GpFont,
    family: *mut GpFontFamily,
}

impl Drop for Fonts {
    fn drop(&mut self) {
        unsafe {
            if !self.status.is_null() {
                GdipDeleteFont(self.status);
            }
            if !self.text.is_null() {
                GdipDeleteFont(self.text);
            }
            if !self.family.is_null() {
                GdipDeleteFontFamily(self.family);
            }
        }
    }
}

unsafe fn create_fonts() -> Fonts {
    let mut family: *mut GpFontFamily = std::ptr::null_mut();
    let name = util::wide("Microsoft YaHei UI");
    GdipCreateFontFamilyFromName(windows::core::PCWSTR(name.as_ptr()), std::ptr::null_mut(), &mut family);
    let mut f = Fonts { status: std::ptr::null_mut(), text: std::ptr::null_mut(), family };
    if !f.family.is_null() {
        let mut s: *mut GpFont = std::ptr::null_mut();
        GdipCreateFont(f.family, sf(15.0) as f32, FontStyleRegular.0, UnitPixel, &mut s);
        f.status = s;
        let mut t: *mut GpFont = std::ptr::null_mut();
        GdipCreateFont(f.family, sf(22.0) as f32, FontStyleBold.0, UnitPixel, &mut t);
        f.text = t;
    }
    f
}

/// 全量重绘悬浮层（呼吸系数 opacity 0~1）。
/// 高度动态：正文换行超过一行时胶囊底部自动向下扩行，顶部位置保持不变。
unsafe fn redraw(hwnd: HWND, st: &OverlayState, opacity: f64) {
    let w = si(OVERLAY_WIDTH);
    let base_h = si(OVERLAY_HEIGHT);
    let hdc_screen = GetDC(None);
    let fonts = create_fonts();
    // 先用屏幕 DC 测量正文换行后的高度，决定胶囊是否向下扩高
    let text_max_w = si(OVERLAY_WIDTH - 60) as f32;
    let (_tw, th) = measure_text(hdc_screen, fonts.text, &st.text, text_max_w);
    let multiline = !st.text.is_empty() && th > sf(34.0) as f32;
    let h = if multiline {
        // 状态行下方 sf(76) 起排文字，底部留 sf(16) 内边距
        ((sf(76.0) as f32 + th + sf(16.0) as f32).ceil() as i32).max(base_h)
    } else {
        base_h
    };
    let mem = CreateCompatibleDC(Some(hdc_screen));

    // 32bpp ARGB DIB（top-down）
    let bmi = windows::Win32::Graphics::Gdi::BITMAPINFO {
        bmiHeader: windows::Win32::Graphics::Gdi::BITMAPINFOHEADER {
            biSize: std::mem::size_of::<windows::Win32::Graphics::Gdi::BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: 0u32, // BI_RGB
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
    let dib = CreateDIBSection(Some(mem), &bmi, windows::Win32::Graphics::Gdi::DIB_USAGE(0), &mut bits, None, 0)
        .unwrap_or_default();
    let old = SelectObject(mem, dib.into());

    // 清零（全透明）
    std::ptr::write_bytes(bits as *mut u8, 0, (w * h * 4) as usize);

    // GDI+ 绘制
    let mut graphics: *mut GpGraphics = std::ptr::null_mut();
    if GdipCreateFromHDC(mem, &mut graphics).0 == 0 && !graphics.is_null() {
        GdipSetSmoothingMode(graphics, SmoothingModeAntiAlias);
        GdipSetTextRenderingHint(graphics, TextRenderingHintAntiAliasGridFit);

        // 胶囊底 + 白边
        let pts = capsule_points(1.0, 1.0, (w - 1) as f64, (h - 1) as f64, sf(CORNER_PX), 8);
        let mut fill: *mut GpSolidFill = std::ptr::null_mut();
        GdipCreateSolidFill(CAPSULE_FILL, &mut fill);
        if !fill.is_null() {
            GdipFillPolygon(graphics, fill as *mut GpBrush, pts.as_ptr(), pts.len() as i32, FillModeAlternate);
            GdipDeleteBrush(fill as *mut GpBrush);
        }
        let mut pen: *mut GpPen = std::ptr::null_mut();
        GdipCreatePen1(BORDER_COLOR, sf(1.5) as f32, UnitPixel, &mut pen);
        if !pen.is_null() {
            GdipDrawPolygon(graphics, pen, pts.as_ptr(), pts.len() as i32);
            GdipDeletePen(pen);
        }

        // 波形条
        let bar_w = sf(WAVE_BAR_W);
        let bar_gap = sf(WAVE_BAR_GAP);
        let wave_total = WAVE_BARS as f64 * (bar_w + bar_gap) - bar_gap;
        let x0 = (w as f64 - wave_total) / 2.0;
        let max_h = if st.text.is_empty() { sf(WAVE_BAR_MAX_H) } else { sf(18.0) };
        let mut wave_fill: *mut GpSolidFill = std::ptr::null_mut();
        GdipCreateSolidFill(WAVE_COLOR, &mut wave_fill);
        if !wave_fill.is_null() {
            for (i, v) in st.levels.iter().enumerate() {
                let bh = (v * max_h).max(sf(2.0));
                let x = x0 + i as f64 * (bar_w + bar_gap);
                GdipFillRectangleI(
                    graphics,
                    wave_fill as *mut GpBrush,
                    x as i32,
                    (sf(WAVE_CY) - bh / 2.0) as i32,
                    bar_w as i32,
                    bh as i32,
                );
            }
            GdipDeleteBrush(wave_fill as *mut GpBrush);
        }

        // 文字：状态行固定单行居中；正文单行时垂直居中，多行时顶对齐（胶囊已扩高）
        if !fonts.family.is_null() {
            draw_centered(graphics, fonts.status, sf(STATUS_Y), &st.status, st.status_color,
                          si(OVERLAY_WIDTH - 40) as f32, h as f32);
            if multiline {
                let layout = RectF {
                    X: (w as f32 - text_max_w) / 2.0,
                    Y: sf(76.0) as f32,
                    Width: text_max_w,
                    Height: th,
                };
                draw_string_fmt(graphics, fonts.text, &st.text, TEXT_COLOR, layout,
                                StringAlignmentCenter, StringAlignmentNear);
            } else {
                draw_centered(graphics, fonts.text, sf(TEXT_Y), &st.text, TEXT_COLOR,
                              text_max_w, h as f32);
            }
        }
        GdipDeleteGraphics(graphics);
    }
    drop(fonts);

    // UpdateLayeredWindow（per-pixel alpha + 呼吸整体 alpha）
    // 全屏窗口 + pptDst 局部更新：仅把胶囊区域贴到屏幕中心
    let (sw, sh) = screen_size();
    let dst_pt = POINT {
        x: std::cmp::max(0, (sw - w) / 2),
        y: std::cmp::max(0, (sh - base_h) / 2), // 顶部固定在基准胶囊位置，扩行向下生长
    };
    let dst = RECT { left: dst_pt.x, top: dst_pt.y, right: dst_pt.x + w, bottom: dst_pt.y + h };
    let _ = dst;
    let size = SIZE { cx: w, cy: h };
    let src = POINT { x: 0, y: 0 };
    let blend = windows::Win32::Graphics::Gdi::BLENDFUNCTION {
        BlendOp: 0,     // AC_SRC_OVER
        BlendFlags: 0,
        SourceConstantAlpha: (opacity * 255.0) as u8,
        AlphaFormat: 1, // AC_SRC_ALPHA
    };
    let _ = UpdateLayeredWindow(
        hwnd,
        Some(hdc_screen),
        Some(&dst_pt),
        Some(&size),
        Some(mem),
        Some(&src),
        windows::Win32::Foundation::COLORREF(0),
        Some(&blend),
        UPDATE_LAYERED_WINDOW_FLAGS(ULW_ALPHA.0),
    );

    let _ = SelectObject(mem, old);
    let _ = DeleteObject(dib.into());
    let _ = DeleteDC(mem);
    let _ = ReleaseDC(None, hdc_screen);
}

/// 居中单行绘制：水平居中、垂直中心在 cy（布局矩形高 = 当前胶囊高 cap_h）
unsafe fn draw_centered(
    g: *mut GpGraphics,
    font: *mut GpFont,
    cy: f64,
    text: &str,
    color: u32,
    max_w: f32,
    cap_h: f32,
) {
    // 垂直居中到 cy：布局矩形 [cy*2 - cap_h, cy*2]，中心即 cy
    let layout = RectF {
        X: (si(OVERLAY_WIDTH) as f32 - max_w) / 2.0,
        Y: (((cy * 2.0) - cap_h as f64).max(0.0)) as f32,
        Width: max_w,
        Height: cap_h,
    };
    draw_string_fmt(g, font, text, color, layout, StringAlignmentCenter, StringAlignmentCenter);
}

/// 通用文字绘制（调用方给定布局矩形与水平/垂直对齐）
unsafe fn draw_string_fmt(
    g: *mut GpGraphics,
    font: *mut GpFont,
    text: &str,
    color: u32,
    layout: RectF,
    halign: windows::Win32::Graphics::GdiPlus::StringAlignment,
    valign: windows::Win32::Graphics::GdiPlus::StringAlignment,
) {
    if font.is_null() || text.is_empty() {
        return;
    }
    let mut brush: *mut GpSolidFill = std::ptr::null_mut();
    GdipCreateSolidFill(color, &mut brush);
    if brush.is_null() {
        return;
    }
    let wide = util::wide_nz(text);
    let mut format: *mut GpStringFormat = std::ptr::null_mut();
    if GdipStringFormatGetGenericDefault(&mut format).0 == 0 && !format.is_null() {
        GdipSetStringFormatAlign(format, halign);
        GdipSetStringFormatLineAlign(format, valign);
        GdipDrawString(
            g,
            windows::core::PCWSTR(wide.as_ptr()),
            wide.len() as i32,
            font,
            &layout,
            format,
            brush as *const GpBrush,
        );
        GdipDeleteStringFormat(format);
    }
    GdipDeleteBrush(brush as *mut GpBrush);
}

/// 测量文本在 max_w 宽度内自动换行后的包围尺寸（用屏幕 DC，不依赖目标位图）
unsafe fn measure_text(hdc_screen: HDC, font: *mut GpFont, text: &str, max_w: f32) -> (f32, f32) {
    if font.is_null() || text.is_empty() {
        return (0.0, 0.0);
    }
    let mut g: *mut GpGraphics = std::ptr::null_mut();
    if GdipCreateFromHDC(hdc_screen, &mut g).0 != 0 || g.is_null() {
        return (0.0, 0.0);
    }
    let mut bounds = RectF { X: 0.0, Y: 0.0, Width: 0.0, Height: 0.0 };
    let mut format: *mut GpStringFormat = std::ptr::null_mut();
    if GdipStringFormatGetGenericDefault(&mut format).0 == 0 && !format.is_null() {
        let layout = RectF { X: 0.0, Y: 0.0, Width: max_w, Height: 1.0e6 };
        let wide = util::wide_nz(text);
        GdipMeasureString(
            g,
            windows::core::PCWSTR(wide.as_ptr()),
            wide.len() as i32,
            font,
            &layout,
            format,
            &mut bounds,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        GdipDeleteStringFormat(format);
    }
    GdipDeleteGraphics(g);
    (bounds.Width, bounds.Height)
}

// OverlayState 保持私有（同文件使用）
struct OverlayState {
    visible: bool,
    recording: bool,
    t0: Instant,
    text: String,
    status: String,
    status_color: u32,
    levels: Vec<f64>,
    glint: Option<Instant>,
    breath_epoch: Option<Instant>,
    seq: u64,
}

impl Default for OverlayState {
    fn default() -> Self {
        Self {
            visible: false,
            recording: false,
            t0: Instant::now(),
            text: String::new(),
            status: String::new(),
            status_color: STATUS_COLOR,
            levels: vec![0.0; WAVE_BARS],
            glint: None,
            breath_epoch: None,
            seq: 0,
        }
    }
}

