// Island window: placement on the chosen display, the two window sizes
// (full panel / invisible wake strip), click-through and the cursor poll.
//
// There is no notch on a PC, so the island is a black shape drawn at the top
// centre of the main display inside a borderless, transparent, always-on-top
// window that never takes focus.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Monitor, PhysicalPosition, PhysicalSize, WebviewWindow};

use windows::Win32::Foundation::{HWND, POINT};
use windows::core::BOOL;
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::System::Ole::RevokeDragDrop;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows::Win32::UI::WindowsAndMessaging::{EnumChildWindows, GetClassNameW};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, GWL_STYLE,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WS_CAPTION,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT, WS_MAXIMIZEBOX,
    WS_MINIMIZEBOX, WS_POPUP, WS_SYSMENU, WS_THICKFRAME,
};

/// Logical size of the full window — the largest island view, like the macOS panel.
pub const PANEL_W: f64 = 720.0;
pub const PANEL_H: f64 = 320.0;
/// Window size when docked to a side edge: a narrow, tall column.
pub const SIDE_PANEL_W: f64 = 340.0;
pub const SIDE_PANEL_H: f64 = 560.0;
/// Logical size of the invisible strip that wakes the island when it is hidden.
pub const STRIP_W: f64 = 240.0;
pub const STRIP_H: f64 = 6.0;
/// Thin-bar look: the collapsed window is the 11 px bar the island shows when idle.
pub const BAR_H: f64 = 11.0;
/// Default and widest collapsed-bar window; the front end widens the bar to fit
/// a Claude Code status line (`set_bar_width`).
pub const BAR_WIN_W: f64 = 240.0;
pub const BAR_WIN_MAX_W: f64 = 420.0;

pub const WINDOW_LABEL: &str = "island";

/// Margin around the island that still counts as "on the island", in logical px.
/// Wider than the macOS 6 pt because a click must never be swallowed.
const HIT_MARGIN: f64 = 14.0;

#[derive(Serialize, Clone)]
pub struct CursorPayload {
    pub x: f64,
    pub y: f64,
}

#[derive(Serialize, Clone)]
pub struct ScreenInfo {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub scale: f64,
}

/// The island shape in window-logical coordinates, pushed by the front end.
/// The poll thread owns the click-through decision so it lands in the same 16 ms
/// tick as the cursor read — an IPC round trip here loses clicks.
#[derive(Clone, Copy, Default)]
pub struct IslandRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Wakes / parks the cursor poll thread so a hidden island costs literally nothing.
pub struct PollGate {
    active: Mutex<bool>,
    cv: Condvar,
    pub collapsed: AtomicBool,
    pub rect: Mutex<IslandRect>,
    /// Width of the collapsed thin-bar window, logical px.
    pub bar_w: Mutex<f64>,
    /// Mirrors the window flag so we only call into Win32 when it changes.
    ignoring: AtomicBool,
}

impl PollGate {
    pub fn new() -> Self {
        Self {
            active: Mutex::new(false),
            cv: Condvar::new(),
            collapsed: AtomicBool::new(true),
            rect: Mutex::new(IslandRect::default()),
            bar_w: Mutex::new(BAR_WIN_W),
            ignoring: AtomicBool::new(false),
        }
    }

    pub fn set_rect(&self, rect: IslandRect) {
        *self.rect.lock().unwrap() = rect;
    }

    /// Forces the next poll tick to re-apply the flag (after a window resize).
    pub fn forget_ignore_state(&self) {
        self.ignoring.store(false, Ordering::Relaxed);
    }

    pub fn set_active(&self, on: bool) {
        let mut guard = self.active.lock().unwrap();
        *guard = on;
        self.cv.notify_all();
    }

    fn wait_until_active(&self) {
        let mut guard = self.active.lock().unwrap();
        while !*guard {
            guard = self.cv.wait(guard).unwrap();
        }
    }

    fn is_active(&self) -> bool {
        *self.active.lock().unwrap()
    }

    /// Flips click-through, but only while the poll is live. Held under the same
    /// lock as `set_active`, so once the window has collapsed to the wake strip —
    /// which must always take the mouse — a late poll tick can no longer make it
    /// click-through again. That used to happen whenever the cursor was still
    /// moving at collapse time, leaving a bar that ignored hovers and clicks.
    fn apply_click_through(&self, win: &WebviewWindow, accept: bool) {
        let guard = self.active.lock().unwrap();
        if !*guard {
            return;
        }
        if self.ignoring.load(Ordering::Relaxed) == accept {
            self.ignoring.store(!accept, Ordering::Relaxed);
            set_click_through(win, !accept);
        }
        drop(guard);
    }
}

pub fn window(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window(WINDOW_LABEL)
}

fn cursor_physical() -> Option<(f64, f64)> {
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p).ok()? };
    Some((p.x as f64, p.y as f64))
}

/// Lets dropped files reach the app again.
///
/// wry installs its drop target by walking the webview's child windows **once**,
/// when the webview is created. WebView2 creates `Chrome_RenderWidgetHostHWND`
/// later and registers its own target on it; being the innermost window, that one
/// wins, and since the page has no HTML5 drop handler it refuses everything — the
/// "no drop" cursor, with nothing reaching Tauri. Revoking it makes OLE fall
/// through to the target wry registered on the parent widget, which is the one
/// that feeds Tauri's drag events.
///
/// Cheap and idempotent, so it is simply re-run whenever a drag might be starting.
pub fn unblock_webview_drops(app: &AppHandle) {
    for label in [WINDOW_LABEL, "settings"] {
        let Some(win) = app.get_webview_window(label) else { continue };
        let Some(hwnd) = hwnd_of(&win) else { continue };
        unsafe {
            let _ = EnumChildWindows(Some(hwnd), Some(revoke_render_widget), LPARAM(0));
        }
    }
}

unsafe extern "system" fn revoke_render_widget(hwnd: HWND, _: LPARAM) -> BOOL {
    let mut name = [0u16; 64];
    let len = unsafe { GetClassNameW(hwnd, &mut name) };
    if len > 0 {
        let class = String::from_utf16_lossy(&name[..len as usize]);
        if class == "Chrome_RenderWidgetHostHWND" {
            let _ = unsafe { RevokeDragDrop(hwnd) };
        }
    }
    true.into()
}

/// True while the left mouse button is held — the only signal we get that a
/// drag might be in flight before it reaches the window.
fn left_button_down() -> bool {
    unsafe { (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000) != 0 }
}

fn monitor_contains(m: &Monitor, x: f64, y: f64) -> bool {
    let p = m.position();
    let s = m.size();
    x >= p.x as f64
        && x < (p.x + s.width as i32) as f64
        && y >= p.y as f64
        && y < (p.y + s.height as i32) as f64
}

/// The display the island lives on: the primary one, or the one under the cursor.
fn target_monitor(app: &AppHandle, pref: &str) -> Option<Monitor> {
    let monitors = app.available_monitors().ok()?;
    if pref == "cursor" {
        if let Some((cx, cy)) = cursor_physical() {
            if let Some(m) = monitors.iter().find(|m| monitor_contains(m, cx, cy)) {
                return Some(m.clone());
            }
        }
    }
    app.primary_monitor()
        .ok()
        .flatten()
        .or_else(|| monitors.into_iter().next())
}

pub fn screen_info(app: &AppHandle, pref: &str) -> ScreenInfo {
    match target_monitor(app, pref) {
        Some(m) => {
            let scale = m.scale_factor();
            let p = m.position();
            let s = m.size();
            ScreenInfo {
                x: p.x as f64 / scale,
                y: p.y as f64 / scale,
                width: s.width as f64 / scale,
                height: s.height as f64 / scale,
                scale,
            }
        }
        None => ScreenInfo { x: 0.0, y: 0.0, width: 1920.0, height: 1080.0, scale: 1.0 },
    }
}

/// What `window_rect` needs to know besides the monitor.
pub struct WindowSpec<'a> {
    pub dock: &'a str,
    pub align: &'a str,
    pub offset_x: f64,
    pub offset_y: f64,
    /// Thin-bar look on the top edge.
    pub bar: bool,
    pub collapsed: bool,
}

/// Window rectangle `(x, y, width, height)` in physical pixels on a monitor given
/// as `(x, y, width, height)`. Pure, so the placement can be tested: the bar must
/// stay exactly where the panel's island is when the window grows and shrinks.
pub fn window_rect(mon: (i32, i32, i32, i32), scale: f64, spec: &WindowSpec) -> (i32, i32, u32, u32) {
    let (mx, my, mw, mh) = mon;
    let side = spec.dock == "left" || spec.dock == "right";
    // The wake strip is horizontal on top, vertical on a side edge. In the thin-bar
    // look the collapsed window is the visible bar itself.
    let (lw, lh) = match (spec.collapsed, side) {
        // The panel itself: the window never changes size or place while the
        // island folds and unfolds, only its region does (`bar_outline`). Any
        // resize shows a stale frame — the web view lays out a frame late (the bar
        // sideways or gone), and growing from the bar's 11 px Windows stretches
        // the old surface for a frame, a column of it flashing as a tall line.
        (true, false) if spec.bar => (PANEL_W, PANEL_H),
        (true, false) => (STRIP_W, STRIP_H),
        (true, true) => (STRIP_H, STRIP_W),
        (false, false) => (PANEL_W, PANEL_H),
        (false, true) => (SIDE_PANEL_W, SIDE_PANEL_H),
    };
    let pw = (lw * scale).round().max(1.0) as u32;
    let ph = (lh * scale).round().max(1.0) as u32;
    let anchor_w = pw as i32;
    let free = (mw - anchor_w).max(0);
    let base = match spec.align {
        "left" => 0,
        "right" => free,
        _ => free / 2,
    };
    // Keep the window on its monitor whatever the offset, or it becomes unreachable.
    let rel = match spec.dock {
        "left" => 0,
        "right" => free,
        _ => (base + (spec.offset_x * scale).round() as i32).clamp(0, free),
    };
    let x = mx + rel;
    let y = if side {
        let free_y = (mh - ph as i32).max(0);
        my + ((spec.offset_y * scale).round() as i32).clamp(0, free_y)
    } else {
        my
    };
    (x, y, pw, ph)
}

/// Visible part `(left, right)` of the collapsed bar window, in window pixels: the
/// bar itself, centred. Everything outside it is cut off and takes no clicks.
/// Rounded down, so the cut never reaches past the bar the web view draws.
pub fn bar_region(scale: f64, bar_w: f64, window_w: u32) -> (i32, i32) {
    let bw = ((bar_w * scale).floor() as i32).clamp(1, window_w as i32);
    let left = (window_w as i32 - bw) / 2;
    (left, left + bw)
}

/// Outline of the window region for the collapsed bar, in window pixels: the bar's
/// slanted sides and chamfered lower corners (`barClipPath` in island.ts), pulled
/// a little inside the drawn shape.
///
/// A window region turns off the transparency the rest of the window relies on:
/// any pixel inside it that the web view leaves transparent — or only half covers,
/// on an anti-aliased edge — shows up white. So the region must hold only pixels
/// the bar paints solid black; losing a pixel or two of the edge is invisible.
pub fn bar_outline(scale: f64, bar_w: f64, window_w: u32) -> [(i32, i32); 6] {
    let (l, r) = bar_region(scale, bar_w, window_w);
    let h = (BAR_H * scale).floor() as i32;
    let inset = (2.0 * scale).round().max(2.0) as i32;
    let slant = (5.0 * scale).round() as i32;
    let corner = (2.5 * scale).round() as i32;
    let at = |y: i32| inset + slant * y / h.max(1);
    let low = h - corner;
    [
        (l + inset, 0),
        (r - inset, 0),
        (r - at(low), low),
        (r - at(h) - corner, h),
        (l + at(h) + corner, h),
        (l + at(low), low),
    ]
}

/// Clips the window to the bar outline (or lifts the clip).
fn set_bar_clip(hwnd: HWND, outline: Option<[(i32, i32); 6]>) {
    use windows::Win32::Graphics::Gdi::{CreatePolygonRgn, SetWindowRgn, WINDING};
    unsafe {
        // The system owns the region after SetWindowRgn, so it isn't freed here.
        let rgn = outline.map(|pts| {
            let pts = pts.map(|(x, y)| POINT { x, y });
            CreatePolygonRgn(&pts, WINDING)
        });
        let _ = SetWindowRgn(hwnd, rgn, true);
    }
}

/// Re-cuts the collapsed bar window to the current bar width. The window itself
/// keeps its size, so this is all a width change needs.
pub fn refresh_bar_clip(app: &AppHandle, bar_w: f64) {
    let Some(win) = window(app) else { return };
    let Some(hwnd) = hwnd_of(&win) else { return };
    let Ok(size) = win.inner_size() else { return };
    let scale = win.scale_factor().unwrap_or(1.0);
    set_bar_clip(hwnd, Some(bar_outline(scale, bar_w, size.width)));
}

/// Places and sizes the window. `collapsed` picks the wake strip instead of the panel.
pub fn apply_geometry(app: &AppHandle, pref: &str, collapsed: bool) {
    let Some(win) = window(app) else { return };
    let Some(m) = target_monitor(app, pref) else { return };

    let scale = m.scale_factor();
    let mp = *m.position();
    let ms = *m.size();

    let (dock, offset_y, bar, align, offset) = app
        .try_state::<crate::Shared>()
        .map(|s| {
            let s = s.settings.lock().unwrap();
            let bar = s.island_style == "bar" && s.island_dock == "top";
            (s.island_dock.clone(), s.island_offset_y, bar, s.island_align.clone(), s.island_offset_x)
        })
        .unwrap_or_else(|| ("top".into(), 160.0, false, "center".into(), 0.0));
    let bar_w = app
        .try_state::<crate::Shared>()
        .map(|s| *s.gate.bar_w.lock().unwrap())
        .unwrap_or(BAR_WIN_W);
    let (x, y, pw, ph) = window_rect(
        (mp.x, mp.y, ms.width as i32, ms.height as i32),
        scale,
        &WindowSpec { dock: &dock, align: &align, offset_x: offset, offset_y, bar, collapsed },
    );

    // Size and position in one call. Two separate ones show a frame in which the
    // window has already shrunk but not yet moved, and the bar jumps.
    match hwnd_of(&win) {
        Some(hwnd) => unsafe {
            let _ = SetWindowPos(
                hwnd,
                None,
                x,
                y,
                pw as i32,
                ph as i32,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        },
        None => {
            let _ = win.set_size(PhysicalSize::new(pw, ph));
            let _ = win.set_position(PhysicalPosition::new(x, y));
        }
    }
    if let Some(hwnd) = hwnd_of(&win) {
        let clip = (bar && collapsed).then(|| bar_outline(scale, bar_w, pw));
        set_bar_clip(hwnd, clip);
    }
    // Moving across displays can rescale the window: re-assert the physical size.
    if win.inner_size().map(|s| (s.width, s.height) != (pw, ph)).unwrap_or(false) {
        let _ = win.set_size(PhysicalSize::new(pw, ph));
    }
    let _ = win.set_always_on_top(true);
}

fn hwnd_of(win: &WebviewWindow) -> Option<HWND> {
    let raw = win.hwnd().ok()?.0 as isize;
    if raw == 0 {
        return None;
    }
    Some(HWND(raw as *mut _))
}

/// WS_EX_NOACTIVATE keeps clicks from stealing focus; WS_EX_TOOLWINDOW keeps the
/// island out of Alt-Tab.
///
/// The window also becomes a plain popup. Tao makes every window an overlapped
/// one with a caption and only hides the frame by collapsing it, and an
/// overlapped window always has a title bar in Win32, WS_CAPTION or not: whenever
/// Windows repaints the frame — a style change, a window region — it draws that
/// bar over the screen, a light strip titled "Coucou". A popup has none.
pub fn make_non_activating(win: &WebviewWindow) {
    let Some(hwnd) = hwnd_of(win) else { return };
    let frame = WS_CAPTION.0 | WS_SYSMENU.0 | WS_THICKFRAME.0 | WS_MINIMIZEBOX.0 | WS_MAXIMIZEBOX.0;
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        SetWindowLongPtrW(hwnd, GWL_STYLE, (style & !(frame as isize)) | WS_POPUP.0 as isize);
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let want = ex | WS_EX_NOACTIVATE.0 as isize | WS_EX_TOOLWINDOW.0 as isize;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, want);
        let _ = SetWindowSubclass(hwnd, Some(no_frame_paint), 1, 0);
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
    clear_surface(hwnd);
}

/// Wipes the window's own surface to transparent. The web view draws on top of
/// it, and wherever the page is transparent this surface shows through — so the
/// title bar tao paints when it creates the window stayed there for good, a light
/// "Coucou" strip above the island. GDI black has zero alpha, which this
/// transparent (blur-behind) window shows as see-through.
fn clear_surface(hwnd: HWND) {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::{FillRect, GetStockObject, GetWindowDC, ReleaseDC, BLACK_BRUSH, HBRUSH};
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
    unsafe {
        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).is_err() {
            return;
        }
        let rc = RECT { left: 0, top: 0, right: r.right - r.left, bottom: r.bottom - r.top };
        let hdc = GetWindowDC(Some(hwnd));
        if hdc.is_invalid() {
            return;
        }
        FillRect(hdc, &rc, HBRUSH(GetStockObject(BLACK_BRUSH).0));
        ReleaseDC(Some(hwnd), hdc);
    }
}

/// Keeps Windows from ever painting a frame or caption on the island again, and
/// clears whatever needs repainting to transparent instead of leaving it as is.
unsafe extern "system" fn no_frame_paint(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::{WM_ERASEBKGND, WM_NCACTIVATE, WM_NCPAINT};
    match msg {
        WM_NCPAINT => LRESULT(0),
        // TRUE lets the activation change go through without drawing the frame.
        WM_NCACTIVATE => LRESULT(1),
        WM_ERASEBKGND => {
            clear_surface(hwnd);
            LRESULT(1)
        }
        _ => unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) },
    }
}

/// Click-through on or off: the two extended-style bits and nothing else.
///
/// Not `set_ignore_cursor_events`: tao rewrites the whole window style for it —
/// caption included, and dropping NOACTIVATE / TOOLWINDOW — then forces a frame
/// recalculation (SWP_FRAMECHANGED). The click-through flips every time the
/// cursor reaches or leaves the island, and each flip could flash a title bar.
fn set_click_through(win: &WebviewWindow, on: bool) {
    let Some(hwnd) = hwnd_of(win) else { return };
    let bits = (WS_EX_TRANSPARENT.0 | WS_EX_LAYERED.0) as isize;
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let want = if on { ex | bits } else { ex & !bits };
        if want != ex {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, want);
        }
    }
}

/// Temporarily allow activation so a text field inside the island can be typed in.
pub fn set_activating(win: &WebviewWindow, activating: bool) {
    let Some(hwnd) = hwnd_of(win) else { return };
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let want = if activating {
            ex & !(WS_EX_NOACTIVATE.0 as isize)
        } else {
            ex | WS_EX_NOACTIVATE.0 as isize
        };
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, want);
    }
}

/// Position, size and scale of the monitor the island lives on. Any change here
/// means the island has to be placed again.
fn current_screen_key(app: &AppHandle) -> Option<(i32, i32, u32, u32, u64)> {
    let pref = app
        .try_state::<crate::Shared>()
        .map(|s| s.settings.lock().unwrap().screen.clone())
        .unwrap_or_else(|| "primary".into());
    let m = target_monitor(app, &pref)?;
    let p = m.position();
    let size = m.size();
    Some((p.x, p.y, size.width, size.height, m.scale_factor().to_bits()))
}

/// Emits `cursor` (window-logical coordinates) at ~60 Hz while the island is
/// visible. Parked on a condvar the rest of the time.
pub fn spawn_cursor_poll(app: AppHandle, gate: Arc<PollGate>) {
    std::thread::spawn(move || {
        let mut was_down = false;
        // Remembered across wakes so a display change while hidden is noticed the
        // moment the island comes back.
        let mut last_screen: Option<(i32, i32, u32, u32, u64)> = None;
        loop {
            gate.wait_until_active();
            let mut last = (f64::MIN, f64::MIN);
            let mut ticks: u32 = 0;
            while gate.is_active() {
                std::thread::sleep(Duration::from_millis(16));
                // Parked while we slept: this tick belongs to a window that is gone.
                if !gate.is_active() {
                    break;
                }

                // Monitors get plugged in, unplugged, rearranged and rescaled, and
                // an island pinned to coordinates that no longer exist is an island
                // nobody can reach. Checked about twice a second — the cursor poll
                // is already running, so this costs one monitor query.
                ticks = ticks.wrapping_add(1);
                if ticks % 30 == 0 {
                    let now = current_screen_key(&app);
                    if now.is_some() && now != last_screen {
                        let first = last_screen.is_none();
                        last_screen = now;
                        if !first {
                            crate::log::line("display layout changed — repositioning".to_string());
                            let _ = app.emit_to(WINDOW_LABEL, "screen-changed", ());
                        }
                    }
                }

                let Some(win) = window(&app) else { continue };
                let Ok(origin) = win.outer_position() else { continue };
                let scale = win.scale_factor().unwrap_or(1.0);
                let Some((cx, cy)) = cursor_physical() else { continue };
                let x = (cx - origin.x as f64) / scale;
                let y = (cy - origin.y as f64) / scale;
                let size = match win.inner_size() {
                    Ok(s) => (s.width as f64 / scale, s.height as f64 / scale),
                    Err(_) => (PANEL_W, PANEL_H),
                };
                if (x - last.0).abs() < 1.0 && (y - last.1).abs() < 1.0 {
                    continue;
                }
                last = (x, y);

                // Click-through: the window only takes the mouse over the island
                // shape. A small entry margin means the flag is already off by the
                // time a moving cursor reaches a button.
                let r = *gate.rect.lock().unwrap();
                let on_island = r.w > 0.0
                    && x >= r.x - HIT_MARGIN
                    && x <= r.x + r.w + HIT_MARGIN
                    && y >= r.y - HIT_MARGIN
                    && y <= r.y + r.h + HIT_MARGIN;

                // A file being dragged has to be able to find us. WS_EX_TRANSPARENT
                // — what click-through is on Windows — hides the window from
                // WindowFromPoint, so OLE finds no drop target and shows the "no
                // drop" cursor. macOS has no such problem: AppKit delivers drags to
                // registered destinations whatever ignoresMouseEvents says. So while
                // a button is held anywhere over the panel, the whole panel takes
                // the mouse, which also makes the drop zone as forgiving as the Mac's.
                // A press may be the start of a drag: make sure the drop target is
                // ours before the file arrives.
                let down = left_button_down();
                if down && !was_down {
                    let handle = app.clone();
                    let _ = app.run_on_main_thread(move || unblock_webview_drops(&handle));
                }
                was_down = down;

                let dragging = down
                    && x >= 0.0
                    && x <= size.0
                    && y >= 0.0
                    && y <= size.1;

                gate.apply_click_through(&win, on_island || dragging);

                let _ = win.emit("cursor", CursorPayload { x, y });
            }
        }
    });
}

/// True while a full-screen app (game, video, presentation) owns the display.
fn fullscreen_app_running() -> bool {
    use windows::Win32::UI::Shell::{
        SHQueryUserNotificationState, QUNS_BUSY, QUNS_PRESENTATION_MODE,
        QUNS_RUNNING_D3D_FULL_SCREEN,
    };
    match unsafe { SHQueryUserNotificationState() } {
        Ok(s) => s == QUNS_BUSY || s == QUNS_RUNNING_D3D_FULL_SCREEN || s == QUNS_PRESENTATION_MODE,
        Err(_) => false,
    }
}

/// Tucks the island away while a full-screen app is up and brings it back when it
/// ends. The front end slides the island off the edge (`fullscreen` event) and the
/// window is hidden once that has played, so it can't sit on top of the game; on
/// the way back the window is shown first, then the island slides in.
pub fn spawn_fullscreen_watch(app: AppHandle) {
    std::thread::spawn(move || {
        let mut away = false;
        loop {
            std::thread::sleep(Duration::from_millis(800));
            let full = fullscreen_app_running();
            if full == away {
                continue;
            }
            let Some(win) = window(&app) else { continue };
            away = full;
            if full {
                let _ = win.emit("fullscreen", true);
                std::thread::sleep(Duration::from_millis(450));
                // Re-check: a quick alt-tab must not leave the island hidden.
                if fullscreen_app_running() {
                    let _ = win.hide();
                } else {
                    away = false;
                    let _ = win.emit("fullscreen", false);
                }
            } else {
                let _ = win.show();
                // Showing goes through tao, which puts back the styles it likes.
                make_non_activating(&win);
                let _ = win.set_always_on_top(true);
                std::thread::sleep(Duration::from_millis(120));
                let _ = win.emit("fullscreen", false);
            }
        }
    });
}

pub fn set_ignore_cursor(app: &AppHandle, ignore: bool) {
    if let Some(win) = window(app) {
        set_click_through(&win, ignore);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MON: (i32, i32, i32, i32) = (0, 0, 1920, 1080);

    fn spec<'a>(collapsed: bool, _bar_w: f64, align: &'a str, offset_x: f64) -> WindowSpec<'a> {
        WindowSpec { dock: "top", align, offset_x, offset_y: 0.0, bar: true, collapsed }
    }

    #[test]
    fn window_never_moves_or_resizes_when_it_collapses() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            for align in ["center", "left", "right"] {
                for offset in [0.0, 120.0, -300.0] {
                    let panel = window_rect(MON, scale, &spec(false, 240.0, align, offset));
                    for bar_w in [150.0, 200.0, 240.0, 333.0, 420.0] {
                        let bar = window_rect(MON, scale, &spec(true, bar_w, align, offset));
                        assert_eq!(bar, panel, "scale {scale} {align} {offset} bar {bar_w}");
                    }
                }
            }
        }
    }

    #[test]
    fn bar_is_centred_in_its_window() {
        for scale in [1.0, 1.25, 2.0] {
            for bar_w in [150.0, 240.0, 333.0, 420.0] {
                let pw = (PANEL_W * scale).round() as u32;
                let (l, r) = bar_region(scale, bar_w, pw);
                assert!((l - (pw as i32 - r)).abs() <= 1, "scale {scale} bar {bar_w}: {l}..{r} of {pw}");
                assert_eq!(r - l, (bar_w * scale).floor() as i32);
            }
        }
    }

    #[test]
    fn bar_region_never_exceeds_the_window() {
        let (l, r) = bar_region(1.0, 5000.0, 720);
        assert_eq!((l, r), (0, 720));
    }

    #[test]
    fn bar_outline_stays_inside_the_drawn_bar() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let pw = (PANEL_W * scale).round() as u32;
            let h = BAR_H * scale;
            for bar_w in [150.0, 240.0, 333.5, 420.0] {
                // Left edge of the bar the web view draws: centred, in physical px.
                let drawn_l = (pw as f64 - bar_w * scale) / 2.0;
                let pts = bar_outline(scale, bar_w, pw);
                for (x, y) in pts {
                    assert!(y as f64 <= h, "scale {scale}: {y} below the bar");
                    let slant = 5.0 * scale * y as f64 / h;
                    let edge = drawn_l + slant;
                    let mirrored = pw as f64 - x as f64;
                    assert!(x as f64 >= edge + 1.0, "scale {scale} bar {bar_w}: {x},{y} vs {edge}");
                    assert!(mirrored >= edge + 1.0, "scale {scale} bar {bar_w}: {x},{y} vs {edge}");
                }
            }
        }
    }

    #[test]
    fn collapsed_bar_window_keeps_the_panel_size() {
        let r = window_rect(MON, 1.0, &spec(true, 240.0, "center", 0.0));
        assert_eq!((r.2, r.3), (PANEL_W as u32, PANEL_H as u32));
    }

    #[test]
    fn bar_stays_on_its_own_monitor() {
        let mon = (1920, 0, 2560, 1440);
        let r = window_rect(mon, 1.0, &spec(true, 240.0, "center", 0.0));
        assert!(r.0 >= 1920 && r.0 + r.2 as i32 <= 1920 + 2560);
    }
}
