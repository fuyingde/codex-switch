//! 原生 Win32 窗口，使用 Direct2D / DirectWrite 绘制界面。
//! 切号逻辑独立于绘制层，不依赖 WebView 或额外运行时。

#[path = "ui_render.rs"]
mod rendering;
#[path = "ui_settings.rs"]
mod settings_ui;
#[path = "ui_update.rs"]
mod updates_ui;
#[path = "ui_usage.rs"]
mod usage_workers;
#[path = "ui_visual.rs"]
mod visual;
#[path = "ui_shadow.rs"]
mod window_shadow;
use rendering::{Canvas, Font, Graphics};

use crate::identity;
use crate::store::Store;
use crate::usage;
use crate::win;
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, UpdateWindow, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT,
    DT_SINGLELINE, DT_VCENTER, PAINTSTRUCT,
};
use windows_sys::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    EnumWindows, GetClassNameW, GetClientRect, GetCursorPos, GetMessageW, GetWindowLongPtrW,
    GetWindowRect, GetWindowThreadProcessId, KillTimer, LoadCursorW, LoadIconW, PostMessageW,
    PostQuitMessage, RegisterClassExW, SetCursor, SetForegroundWindow, SetTimer, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, GWL_EXSTYLE,
    HICON, HTCAPTION, HTCLIENT, IDC_ARROW, IDC_HAND, IDI_APPLICATION, MSG, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_HIDE, SW_MINIMIZE, SW_SHOWNOACTIVATE,
    WM_APP, WM_CREATE, WM_DESTROY, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEWHEEL,
    WM_MOVE, WM_MOVING, WM_NCACTIVATE, WM_NCCALCSIZE, WM_NCCREATE, WM_NCHITTEST, WM_NCPAINT,
    WM_PAINT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETCURSOR, WM_TIMER, WNDCLASSEXW, WS_CAPTION,
    WS_EX_APPWINDOW, WS_EX_TOOLWINDOW, WS_MINIMIZEBOX, WS_SYSMENU, WS_THICKFRAME,
};

const WIDTH: i32 = 1200;
const HEIGHT: i32 = 760;
const MIN_HEIGHT: i32 = 350;
const COMPACT_WIDTH: i32 = 350;
const COMPACT_CARD: i32 = 92;
const COMPACT_HEADER: i32 = 216;
const COMPACT_TITLE: i32 = 54;
const COMPACT_FOOTER: i32 = 54;
const COMPACT_MAX_HEIGHT: i32 = 800;
const TITLE_BAR: i32 = 56;
const TOOLBAR_TOP: i32 = 72;
const TOOLBAR_HEIGHT: i32 = 42;
const TABLE_TOP: i32 = 132;
const HEADER_ROW: i32 = 44;
const DATA_ROW: i32 = 72;
const FOOTER: i32 = 52;
const TIMER_ID: usize = 1;
const TIMER_INTERVAL: u32 = 80;
const ANIM_MS: u64 = 180;
const WM_DATA: u32 = WM_APP + 1;
const WM_TRAY: u32 = WM_APP + 2;
const WM_SHOW_INSTANCE: u32 = WM_APP + 3;
const TRAY_ID: u32 = 1;
const QUEUED_SWITCH_NOTICE: &str = "正在切号，等待当前对话结束后自动切换";

const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

const CARD: u32 = rgb(250, 250, 247);
const LINE: u32 = rgb(228, 226, 220);
const TEXT: u32 = rgb(36, 40, 36);
const MUTED: u32 = rgb(116, 122, 117);
const ACCENT: u32 = rgb(49, 119, 97);
const TRACK: u32 = rgb(230, 231, 227);
const GREEN: u32 = rgb(54, 152, 107);
const QUOTA_ORANGE: u32 = rgb(213, 139, 39);
const QUOTA_RED: u32 = rgb(212, 77, 71);
const WINDOW_BORDER: u32 = rgb(207, 212, 206);
const SAGE: u32 = rgb(232, 243, 236);
const HOVER: u32 = rgb(241, 244, 239);
const AMBER: u32 = rgb(195, 135, 39);

struct RowView {
    id: String,
    label: String,
    plan: Option<String>,
    active_until: Option<String>,
    is_active: bool,
    five_hour: Option<u8>,
    five_hour_reset: Option<i64>,
    weekly: Option<u8>,
    weekly_reset: Option<i64>,
    reset_credits: Option<u32>,
    error: Option<String>,
    expired: bool,
    loading: bool,
}

struct Request {
    id: String,
    home: PathBuf,
}

enum Message {
    Usage {
        id: String,
        result: std::result::Result<usage::Usage, String>,
    },
}

struct Fonts {
    caption: Font,
    compact_number: Font,
    title: Font,
    body: Font,
    small: Font,
    header: Font,
}

struct App {
    hwnd: HWND,
    icon: HICON,
    rows: Vec<RowView>,
    homes: HashMap<String, PathBuf>,
    notice: Option<String>,
    notice_at: Instant,
    last_notice: Option<String>,
    compact_mode: bool,
    full_panel: PanelPlacement,
    compact_panel: PanelPlacement,
    docked: bool,
    dock_return: Option<(i32, i32)>,
    dock_work: Option<RECT>,
    visible: bool,
    tray_hidden: bool,
    dock_wait_for_pointer: bool,
    anim: Option<Anim>,
    confirm: Option<(Confirm, String)>,
    busy: Option<String>,
    connecting: bool,
    connection_established: bool,
    connection_check: Option<Instant>,
    add_menu: bool,
    context_row: Option<usize>,
    context_at: (i32, i32),
    hover_item: Option<HoverItem>,
    compact_hover: Option<CompactHit>,
    compact_left: i32,
    masked: bool,
    scroll: i32,
    full_height: i32,
    scroll_drag: Option<(i32, i32)>,
    compact_scroll: i32,
    compact_height_limit: i32,
    hover_row: Option<usize>,
    refresh_active_minutes: u32,
    refresh_all_minutes: u32,
    usage_concurrency: u32,
    usage_control: Option<usage_workers::Controller>,
    close_action: CloseAction,
    settings: Option<settings_ui::SettingsDialog>,
    updates: updates_ui::State,
    usage_pending: HashSet<String>,
    refresh_pending: HashSet<String>,
    refresh_failed: bool,
    last_all_refresh: Instant,
    last_active_refresh: Instant,
    fonts: Option<Fonts>,
    graphics: Option<Graphics>,
    shadow: Option<window_shadow::WindowShadow>,
    render_failure: Option<String>,
    scale: f32,
    tx: Sender<Request>,
    rx: Receiver<Message>,
    switch_tx: Sender<std::result::Result<SwitchNotice, String>>,
    switch_rx: Receiver<std::result::Result<SwitchNotice, String>>,
    login_pending: bool,
    login_cancel: Arc<AtomicBool>,
    login_tx: Sender<std::result::Result<String, String>>,
    login_rx: Receiver<std::result::Result<String, String>>,
}

struct SwitchNotice {
    message: String,
    finished: bool,
}

struct Anim {
    from: i32,
    to: i32,
    start: Instant,
    duration: Duration,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum CloseAction {
    Exit,
    #[default]
    #[serde(other)]
    Tray,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Confirm {
    Switch,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompactHit {
    Close,
    Refresh,
    Expand,
    Dock,
    Add,
    Privacy,
    Card(usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HoverItem {
    AddRegister,
    AddLogin,
    ContextRefresh,
    ContextSwitch,
    ContextDelete,
}

struct Layout {
    refresh_all: RECT,
    shrink: RECT,
    privacy: RECT,
    settings: RECT,
    add: RECT,
    pin: RECT,
    min: RECT,
    close: RECT,
    header: RECT,
    rows: Vec<RECT>,
    switch: Vec<RECT>,
    locate: RECT,
    context_menu: Option<RECT>,
    context_refresh: RECT,
    context_switch: RECT,
    context_delete: RECT,
    menu: Option<RECT>,
    menu_register: RECT,
    menu_login: RECT,
    dialog: RECT,
    dialog_cancel: RECT,
    dialog_ok: RECT,
}

pub fn run() -> Result<()> {
    unsafe { run_inner() }
}

fn find_existing_window() -> HWND {
    struct Ctx {
        found: HWND,
    }
    unsafe extern "system" fn callback(hwnd: HWND, lparam: isize) -> i32 {
        let ctx = &mut *(lparam as *mut Ctx);
        let mut buffer = [0u16; 256];
        let length = GetClassNameW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32);
        if length > 0 {
            let name = String::from_utf16_lossy(&buffer[..length as usize]);
            if name == "CodexSwitchWindow" {
                ctx.found = hwnd;
                return 0;
            }
        }
        1
    }

    let mut ctx = Ctx {
        found: std::ptr::null_mut(),
    };
    unsafe {
        EnumWindows(Some(callback), &mut ctx as *mut Ctx as isize);
    }
    ctx.found
}

unsafe fn run_inner() -> Result<()> {
    let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    let class_name = to_wide("CodexSwitchWindow");

    // 单实例：已经有一个在跑，就把前台让给它、让它自己弹出来，然后本进程直接退出。
    // 设置 CODEX_SWITCH_ALLOW_MULTI=1 可跳过此检查（调试用）。
    let allow_multi = std::env::var_os("CODEX_SWITCH_ALLOW_MULTI").is_some();
    let existing = if allow_multi {
        std::ptr::null_mut()
    } else {
        find_existing_window()
    };
    if !existing.is_null() {
        let mut pid = 0u32;
        GetWindowThreadProcessId(existing, &mut pid);
        if pid != 0 {
            AllowSetForegroundWindow(pid);
            PostMessageW(existing, WM_SHOW_INSTANCE, 0, 0);
        }
        return Ok(());
    }

    let (rows, homes, notice) = load_rows();
    let ui_settings = load_ui_settings();
    let (tx, rx, usage_control) = spawn_worker(ui_settings.usage_concurrency);
    let (switch_tx, switch_rx) = mpsc::channel();
    let (login_tx, login_rx) = mpsc::channel();

    let mut app = Box::new(App {
        hwnd: std::ptr::null_mut(),
        icon: std::ptr::null_mut(),
        rows,
        homes,
        notice,
        notice_at: Instant::now(),
        last_notice: None,
        compact_mode: false,
        full_panel: ui_settings.full_panel,
        compact_panel: ui_settings.compact_panel,
        docked: false,
        dock_return: None,
        dock_work: None,
        visible: true,
        tray_hidden: false,
        dock_wait_for_pointer: false,
        anim: None,
        confirm: None,
        busy: None,
        connecting: false,
        connection_established: false,
        connection_check: Some(Instant::now()),
        add_menu: false,
        context_row: None,
        context_at: (0, 0),
        hover_item: None,
        compact_hover: None,
        compact_left: ui_settings.compact_left,
        masked: ui_settings.masked,
        scroll: 0,
        full_height: HEIGHT,
        scroll_drag: None,
        compact_scroll: 0,
        compact_height_limit: COMPACT_MAX_HEIGHT,
        hover_row: None,
        refresh_active_minutes: ui_settings.active_minutes,
        refresh_all_minutes: ui_settings.all_minutes,
        usage_concurrency: ui_settings.usage_concurrency,
        usage_control: Some(usage_control),
        close_action: ui_settings.close_action,
        settings: None,
        updates: updates_ui::State::load(),
        usage_pending: HashSet::new(),
        refresh_pending: HashSet::new(),
        refresh_failed: false,
        last_all_refresh: Instant::now(),
        last_active_refresh: Instant::now(),
        fonts: Some(create_fonts(std::ptr::null_mut())),
        graphics: None,
        shadow: None,
        render_failure: None,
        scale: 1.0,
        tx,
        rx,
        switch_tx,
        switch_rx,
        login_pending: false,
        login_cancel: Arc::new(AtomicBool::new(false)),
        login_tx,
        login_rx,
    });

    for index in 0..app.rows.len() {
        queue_usage(&mut app, index);
    }

    app.updates.checker.begin(false);
    let app_ptr = Box::into_raw(app);
    let instance = windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null());

    let mut class: WNDCLASSEXW = std::mem::zeroed();
    class.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
    class.style = CS_HREDRAW | CS_VREDRAW | windows_sys::Win32::UI::WindowsAndMessaging::CS_DBLCLKS;
    class.lpfnWndProc = Some(window_proc);
    class.hInstance = instance;
    class.hCursor = LoadCursorW(std::ptr::null_mut(), IDC_ARROW);
    let mut icon: HICON = LoadIconW(instance, 1 as *const u16);
    if icon.is_null() {
        icon = LoadIconW(std::ptr::null_mut(), IDI_APPLICATION);
    }
    class.hIcon = icon;
    class.hIconSm = icon;
    (*app_ptr).icon = icon;
    class.lpszClassName = class_name.as_ptr();
    if RegisterClassExW(&class) == 0 {
        return Err(anyhow::anyhow!(
            "注册窗口类失败：{}",
            std::io::Error::last_os_error()
        ));
    }

    let screen_width = win::primary_screen_size().0;
    let x = ((screen_width - WIDTH) / 2).max(0);

    let hwnd = CreateWindowExW(
        WS_EX_APPWINDOW,
        class_name.as_ptr(),
        to_wide("Codex切号器").as_ptr(),
        WS_CAPTION
            | WS_SYSMENU
            | WS_MINIMIZEBOX
            | WS_THICKFRAME
            | windows_sys::Win32::UI::WindowsAndMessaging::WS_CLIPCHILDREN,
        x,
        0,
        WIDTH,
        HEIGHT,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        instance,
        app_ptr as *mut core::ffi::c_void,
    );
    if hwnd.is_null() {
        return Err(anyhow::anyhow!(
            "创建窗口失败：{}",
            std::io::Error::last_os_error()
        ));
    }

    restore_panel_placement(&mut *app_ptr, ui_settings.full_panel, None);
    match window_shadow::WindowShadow::new() {
        Ok(shadow) => (*app_ptr).shadow = Some(shadow),
        Err(error) => (*app_ptr).notice = Some(format!("窗口阴影初始化失败：{error:#}")),
    }
    apply_window_frame(hwnd);

    SetTimer(hwnd, TIMER_ID, TIMER_INTERVAL, None);
    add_tray(hwnd, icon);
    ShowWindow(hwnd, 1);
    UpdateWindow(hwnd);

    let mut message: MSG = std::mem::zeroed();
    while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
        if message.message == windows_sys::Win32::UI::WindowsAndMessaging::WM_KEYDOWN
            && (updates_ui::keyboard(&mut *app_ptr, message.wParam as u32)
                || settings_ui::keyboard(&mut *app_ptr, message.wParam as u32))
        {
            continue;
        }
        TranslateMessage(&message);
        DispatchMessageW(&message);
    }

    let _ = KillTimer(hwnd, TIMER_ID);
    let _ = Box::from_raw(app_ptr);
    Ok(())
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_NCCREATE => {
            // 必须先交给 DefWindowProc：窗口标题（任务栏显示的名字）就在这一步被系统记录，
            // 直接返回 TRUE 会得到空标题。
            let result = DefWindowProcW(hwnd, message, wparam, lparam);
            let create =
                &*(lparam as *const windows_sys::Win32::UI::WindowsAndMessaging::CREATESTRUCTW);
            let app = create.lpCreateParams as *mut App;
            if !app.is_null() {
                (*app).hwnd = hwnd;
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, app as isize);
            }
            result
        }
        WM_CREATE => 0,
        windows_sys::Win32::UI::WindowsAndMessaging::WM_CLOSE => {
            let ptr = app_from(hwnd);
            if !ptr.is_null() {
                request_close(&mut *ptr);
            }
            0
        }
        windows_sys::Win32::UI::WindowsAndMessaging::WM_KEYDOWN if wparam == 27 => {
            let ptr = app_from(hwnd);
            if !ptr.is_null() {
                dismiss_menus(&mut *ptr);
                invalidate_rect(hwnd, std::ptr::null(), 0);
            }
            0
        }
        windows_sys::Win32::UI::WindowsAndMessaging::WM_ACTIVATEAPP if wparam == 0 => {
            let ptr = app_from(hwnd);
            if !ptr.is_null() {
                dismiss_menus(&mut *ptr);
                invalidate_rect(hwnd, std::ptr::null(), 0);
            }
            0
        }
        windows_sys::Win32::UI::WindowsAndMessaging::WM_CTLCOLOREDIT => {
            let dc = wparam as windows_sys::Win32::Graphics::Gdi::HDC;
            windows_sys::Win32::Graphics::Gdi::SetTextColor(dc, TEXT);
            windows_sys::Win32::Graphics::Gdi::SetBkColor(dc, 0xffffff);
            windows_sys::Win32::Graphics::Gdi::GetStockObject(
                windows_sys::Win32::Graphics::Gdi::WHITE_BRUSH,
            ) as LRESULT
        }
        windows_sys::Win32::UI::WindowsAndMessaging::WM_ERASEBKGND => 1,
        windows_sys::Win32::UI::WindowsAndMessaging::WM_DPICHANGED => {
            let app = app_from(hwnd);
            if !app.is_null() {
                let suggested = *(lparam as *const RECT);
                apply_window_scale(&mut *app, Some((suggested.left, suggested.top)));
            }
            0
        }
        windows_sys::Win32::UI::WindowsAndMessaging::WM_SIZE => {
            let app = app_from(hwnd);
            if !app.is_null() {
                let app = &mut *app;
                if wparam == 1 {
                    if let Some(graphics) = app.graphics.as_mut() {
                        graphics.release_window();
                    }
                } else if !app.compact_mode {
                    let mut rect: RECT = std::mem::zeroed();
                    GetClientRect(hwnd, &mut rect);
                    app.full_height = dip(app, rect.bottom).clamp(MIN_HEIGHT, HEIGHT);
                    app.scroll = app.scroll.clamp(0, max_scroll(app));
                    settings_ui::reposition(app);
                }
            }
            invalidate_rect(hwnd, std::ptr::null(), 0);
            0
        }
        // Preserve the proposed window rectangle. rgrc[1] is the OLD window,
        // not the new client bounds; copying it leaves stale right/bottom edges.
        WM_NCCALCSIZE => 0,
        windows_sys::Win32::UI::WindowsAndMessaging::WM_DWMCOMPOSITIONCHANGED => {
            apply_window_frame(hwnd);
            sync_window_shadow(hwnd);
            0
        }
        windows_sys::Win32::UI::WindowsAndMessaging::WM_WINDOWPOSCHANGED
        | windows_sys::Win32::UI::WindowsAndMessaging::WM_ACTIVATE => {
            let result = DefWindowProcW(hwnd, message, wparam, lparam);
            sync_window_shadow(hwnd);
            result
        }
        windows_sys::Win32::UI::WindowsAndMessaging::WM_GETMINMAXINFO => {
            let ptr = app_from(hwnd);
            if !ptr.is_null() {
                let app = &*ptr;
                let bounds =
                    &mut *(lparam as *mut windows_sys::Win32::UI::WindowsAndMessaging::MINMAXINFO);
                let (width, min, max) = window_size_limits(app);
                bounds.ptMinTrackSize.x = width;
                bounds.ptMaxTrackSize.x = width;
                bounds.ptMinTrackSize.y = min;
                bounds.ptMaxTrackSize.y = max;
                bounds.ptMaxSize.x = width;
                bounds.ptMaxSize.y = max;
            }
            0
        }
        windows_sys::Win32::UI::WindowsAndMessaging::WM_SIZING => {
            let ptr = app_from(hwnd);
            if !ptr.is_null() {
                constrain_resize(&*ptr, &mut *(lparam as *mut RECT), wparam as u32);
            }
            1
        }
        windows_sys::Win32::UI::WindowsAndMessaging::WM_SYSCOMMAND
            if wparam & 0xfff0
                == windows_sys::Win32::UI::WindowsAndMessaging::SC_MAXIMIZE as usize =>
        {
            0
        }
        WM_NCHITTEST => {
            let app = app_from(hwnd);
            if app.is_null() {
                return DefWindowProcW(hwnd, message, wparam, lparam);
            }
            let app = &*app;
            if !app.visible || app.anim.is_some() {
                return HTCLIENT as LRESULT;
            }
            let screen_x = (lparam & 0xFFFF) as u16 as i16 as i32;
            let screen_y = ((lparam >> 16) & 0xFFFF) as u16 as i16 as i32;
            let mut rect: RECT = std::mem::zeroed();
            GetWindowRect(hwnd, &mut rect);
            let x = dip(app, screen_x - rect.left);
            let y = dip(app, screen_y - rect.top);
            let layout = layout(app);
            let over_button = point_in(&layout.shrink, x, y)
                || point_in(&layout.add, x, y)
                || point_in(&layout.refresh_all, x, y)
                || point_in(&layout.privacy, x, y)
                || point_in(&layout.settings, x, y)
                || point_in(&layout.pin, x, y)
                || point_in(&layout.min, x, y)
                || point_in(&layout.close, x, y);
            if app.compact_mode {
                // 紧凑模式：标题条可拖动，关闭按钮和其余区域正常响应点击
                let compact = compact_layout(app);
                if y >= 0
                    && y < COMPACT_TITLE
                    && !point_in(&compact.close, x, y)
                    && !point_in(&compact.pin, x, y)
                    && !point_in(&compact.expand, x, y)
                {
                    return HTCAPTION as LRESULT;
                }
                return HTCLIENT as LRESULT;
            }
            if let Some(edge) = vertical_resize_hit(app, y) {
                return edge as LRESULT;
            }
            // 标题栏空白处交给系统拖动，按钮还是要自己处理点击。
            if y >= 0
                && y < TITLE_BAR
                && !over_button
                && !app.updates.blocked()
                && app.confirm.is_none()
                && layout.menu.is_none()
            {
                return HTCAPTION as LRESULT;
            }
            HTCLIENT as LRESULT
        }
        WM_NCACTIVATE => 1,
        WM_NCPAINT => 0,
        WM_PAINT => {
            let app = app_from(hwnd);
            let mut paint: PAINTSTRUCT = std::mem::zeroed();
            let _ = BeginPaint(hwnd, &mut paint);
            if !app.is_null() {
                paint_window(&mut *app);
            }
            EndPaint(hwnd, &paint);
            0
        }
        WM_TIMER => {
            if wparam == TIMER_ID {
                let app = app_from(hwnd);
                if !app.is_null() {
                    tick(&mut *app);
                }
            }
            0
        }
        WM_DATA => {
            let app = app_from(hwnd);
            if !app.is_null() {
                drain(&mut *app);
                invalidate_rect(hwnd, std::ptr::null(), 0);
            }
            0
        }
        WM_MOUSEWHEEL => {
            let app = app_from(hwnd);
            if !app.is_null() {
                let delta = ((wparam >> 16) & 0xFFFF) as u16 as i16 as i32;
                let app = &mut *app;
                if app.updates.blocked()
                    || app.confirm.is_some()
                    || app.login_pending
                    || app.settings.is_some()
                {
                    return 0;
                }
                app.context_row = None;
                app.add_menu = false;
                if app.compact_mode {
                    let limit = compact_max_scroll(app);
                    app.compact_scroll = (app.compact_scroll - (delta / 120) * 60).clamp(0, limit);
                } else {
                    scroll_by(app, -(delta / 120) * 72);
                }
                invalidate_rect(hwnd, std::ptr::null(), 0);
            }
            0
        }
        WM_RBUTTONDOWN => {
            let app = app_from(hwnd);
            if !app.is_null() {
                let x = (lparam & 0xFFFF) as u16 as i16 as i32;
                let y = ((lparam >> 16) & 0xFFFF) as u16 as i16 as i32;
                right_clicked(&mut *app, dip(&*app, x), dip(&*app, y));
                invalidate_rect(hwnd, std::ptr::null(), 0);
            }
            0
        }
        WM_MOVE => {
            let app = app_from(hwnd);
            if !app.is_null() {
                let app = &mut *app;
                if app.docked && app.anim.is_none() {
                    let mut rect: RECT = std::mem::zeroed();
                    GetWindowRect(hwnd, &mut rect);
                    let monitor = monitor_rect(hwnd);
                    app.compact_left = rect.left.clamp(
                        monitor.left,
                        (monitor.right - window_dimensions(app).0).max(monitor.left),
                    );
                }
            }
            0
        }
        WM_LBUTTONDBLCLK => {
            let app = app_from(hwnd);
            if !app.is_null() {
                let x = (lparam & 0xFFFF) as u16 as i16 as i32;
                let y = ((lparam >> 16) & 0xFFFF) as u16 as i16 as i32;
                double_clicked(&mut *app, dip(&*app, x), dip(&*app, y));
            }
            0
        }
        windows_sys::Win32::UI::WindowsAndMessaging::WM_MOUSEMOVE => {
            let ptr = app_from(hwnd);
            if !ptr.is_null() {
                let app = &mut *ptr;
                let x = dip(app, (lparam & 0xffff) as u16 as i16 as i32);
                let y = dip(app, ((lparam >> 16) & 0xffff) as u16 as i16 as i32);
                if app.scroll_drag.is_some() {
                    drag_scrollbar(app, y);
                }
                update_pointer_hover(app, x, y);
            }
            0
        }
        WM_LBUTTONUP => {
            let ptr = app_from(hwnd);
            if !ptr.is_null() && (*ptr).scroll_drag.take().is_some() {
                windows_sys::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture();
                invalidate_rect(hwnd, std::ptr::null(), 0);
            }
            0
        }
        windows_sys::Win32::UI::WindowsAndMessaging::WM_CAPTURECHANGED => {
            let ptr = app_from(hwnd);
            if !ptr.is_null() {
                (*ptr).scroll_drag = None;
            }
            0
        }
        WM_LBUTTONDOWN => {
            let app = app_from(hwnd);
            if !app.is_null() {
                let x = (lparam & 0xFFFF) as u16 as i16 as i32;
                let y = ((lparam >> 16) & 0xFFFF) as u16 as i16 as i32;
                clicked(&mut *app, dip(&*app, x), dip(&*app, y));
            }
            0
        }
        WM_MOVING => {
            // 拖动时把窗口限制在屏幕工作区内：左、右、下都不能越界。
            let rect = &mut *(lparam as *mut RECT);
            let work = monitor_work_rect(hwnd);
            let width = rect.right - rect.left;
            let height = rect.bottom - rect.top;
            if rect.left < work.left {
                rect.left = work.left;
                rect.right = rect.left + width;
            }
            if rect.right > work.right {
                rect.right = work.right;
                rect.left = rect.right - width;
            }
            if rect.bottom > work.bottom {
                rect.bottom = work.bottom;
                rect.top = rect.bottom - height;
            }
            1
        }
        WM_SETCURSOR => {
            let app = app_from(hwnd);
            if !app.is_null() && (lparam & 0xFFFF) as u32 == HTCLIENT as u32 {
                let mut point: windows_sys::Win32::Foundation::POINT = std::mem::zeroed();
                GetCursorPos(&mut point);
                let mut rect: RECT = std::mem::zeroed();
                GetWindowRect(hwnd, &mut rect);
                let hand = clickable(
                    &*app,
                    dip(&*app, point.x - rect.left),
                    dip(&*app, point.y - rect.top),
                );
                let cursor = LoadCursorW(
                    std::ptr::null_mut(),
                    if hand { IDC_HAND } else { IDC_ARROW },
                );
                SetCursor(cursor);
                return 1;
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_SHOW_INSTANCE => {
            let app = app_from(hwnd);
            if !app.is_null() {
                let app = &mut *app;
                // Reopening the EXE restores its current mode without toggling size/docking.
                restore_from_tray(app);
                SetForegroundWindow(hwnd);
                invalidate_rect(hwnd, std::ptr::null(), 0);
            }
            0
        }
        WM_TRAY => {
            let app = app_from(hwnd);
            if !app.is_null() {
                let mouse_message = (lparam & 0xFFFF) as u32;
                if mouse_message == WM_LBUTTONUP {
                    toggle_visibility(&mut *app);
                    invalidate_rect(hwnd, std::ptr::null(), 0);
                } else if mouse_message == WM_RBUTTONUP {
                    tray_menu(&mut *app);
                }
            }
            0
        }
        WM_DESTROY => {
            let app = app_from(hwnd);
            if !app.is_null() {
                (*app).shadow = None;
                settings_ui::close(&mut *app);
            }
            if !app.is_null() {
                let app = &*app;
                let _ = save_ui_settings(app_settings(app));
            }
            remove_tray(hwnd);
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

unsafe fn app_from(hwnd: HWND) -> *mut App {
    let value = windows_sys::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(hwnd, GWLP_USERDATA);
    value as *mut App
}

unsafe fn invalidate_rect(hwnd: HWND, rect: *const RECT, erase: i32) -> i32 {
    if hwnd.is_null() {
        return 0;
    }
    windows_sys::Win32::Graphics::Gdi::InvalidateRect(hwnd, rect, erase)
}

/// The client owns its border/corners and a separate transparent Win32 surface
/// paints the outer shadow. Disable the system's optional NC shadow to avoid doubles.
unsafe fn apply_window_frame(hwnd: HWND) {
    use windows_sys::Win32::Graphics::Dwm::{DwmExtendFrameIntoClientArea, DwmSetWindowAttribute};
    let margins = windows_sys::Win32::UI::Controls::MARGINS {
        cxLeftWidth: 0,
        cxRightWidth: 0,
        cyTopHeight: 0,
        cyBottomHeight: 0,
    };
    let disabled: u32 = 1; // DWMNCRP_DISABLED
    DwmSetWindowAttribute(hwnd, 2, (&disabled as *const u32).cast(), 4);
    DwmExtendFrameIntoClientArea(hwnd, &margins);
}

unsafe fn sync_window_shadow(hwnd: HWND) {
    let ptr = app_from(hwnd);
    if ptr.is_null() {
        return;
    }
    // SetWindowRgn can send nested size/position notifications. Take the surface
    // out until synchronization finishes so the nested handler cannot reenter it.
    let Some(mut shadow) = (*ptr).shadow.take() else {
        return;
    };
    let result = shadow.sync(hwnd, (*ptr).scale, !(*ptr).tray_hidden);
    (*ptr).shadow = Some(shadow);
    if let Err(error) = result {
        (*ptr).notice = Some(format!("窗口阴影绘制失败：{error:#}"));
    }
}

fn window_size_limits(app: &App) -> (i32, i32, i32) {
    if app.compact_mode {
        let height = px(app, compact_height(app));
        (px(app, COMPACT_WIDTH), height, height)
    } else {
        (px(app, WIDTH), px(app, MIN_HEIGHT), px(app, HEIGHT))
    }
}

fn vertical_resize_hit(app: &App, y: i32) -> Option<u32> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{HTBOTTOM, HTTOP};
    if app.compact_mode {
        return None;
    }
    if (0..5).contains(&y) {
        Some(HTTOP)
    } else if (app.full_height - 5..app.full_height).contains(&y) {
        Some(HTBOTTOM)
    } else {
        None
    }
}

fn constrain_resize(app: &App, rect: &mut RECT, edge: u32) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{WMSZ_TOP, WMSZ_TOPLEFT, WMSZ_TOPRIGHT};
    let (width, min, max) = window_size_limits(app);
    let height = (rect.bottom - rect.top).clamp(min, max);
    rect.right = rect.left + width;
    if [WMSZ_TOP, WMSZ_TOPLEFT, WMSZ_TOPRIGHT].contains(&edge) {
        rect.top = rect.bottom - height;
    } else {
        rect.bottom = rect.top + height;
    }
}

struct Scrollbar {
    track: RECT,
    thumb: RECT,
}

fn list_scroll_metrics(app: &App) -> (i32, i32, i32, i32, i32) {
    if app.compact_mode {
        let count = app.rows.iter().filter(|r| !r.is_active).count() as i32;
        (
            COMPACT_WIDTH,
            COMPACT_HEADER,
            compact_height(app) - COMPACT_FOOTER,
            count * COMPACT_CARD,
            app.compact_scroll,
        )
    } else {
        (
            WIDTH,
            TABLE_TOP + HEADER_ROW,
            app.full_height - FOOTER,
            app.rows.len() as i32 * DATA_ROW,
            app.scroll,
        )
    }
}

fn list_max_scroll(app: &App) -> i32 {
    if app.compact_mode {
        compact_max_scroll(app)
    } else {
        max_scroll(app)
    }
}

fn set_list_scroll(app: &mut App, value: i32) {
    let value = value.clamp(0, list_max_scroll(app));
    if app.compact_mode {
        app.compact_scroll = value;
    } else {
        app.scroll = value;
    }
}

fn scrollbar(app: &App) -> Option<Scrollbar> {
    let max = list_max_scroll(app);
    if max == 0 {
        return None;
    }
    let (width, top, bottom, content, scroll) = list_scroll_metrics(app);
    let viewport = bottom - top;
    let track_height = viewport - 12;
    let track = box_rect(
        width - if app.compact_mode { 10 } else { 20 },
        top + 6,
        if app.compact_mode { 4 } else { 6 },
        track_height,
    );
    let thumb_height = (track_height * viewport / content)
        .max(28)
        .min(track_height);
    let offset = (track_height - thumb_height) * scroll.clamp(0, max) / max;
    Some(Scrollbar {
        track,
        thumb: box_rect(
            track.left,
            track.top + offset,
            track.right - track.left,
            thumb_height,
        ),
    })
}

fn scrollbar_pressed(app: &mut App, x: i32, y: i32) -> bool {
    let Some(bar) = scrollbar(app) else {
        return false;
    };
    let hit = box_rect(
        bar.track.left - 3,
        bar.track.top,
        bar.track.right - bar.track.left + 6,
        bar.track.bottom - bar.track.top,
    );
    if !point_in(&hit, x, y) {
        return false;
    }
    if y >= bar.thumb.top && y < bar.thumb.bottom {
        let (_, _, _, _, scroll) = list_scroll_metrics(app);
        app.scroll_drag = Some((y, scroll));
        if !app.hwnd.is_null() {
            unsafe {
                windows_sys::Win32::UI::Input::KeyboardAndMouse::SetCapture(app.hwnd);
            }
        }
    } else {
        let (_, top, bottom, _, scroll) = list_scroll_metrics(app);
        set_list_scroll(
            app,
            scroll
                + if y < bar.thumb.top {
                    top - bottom
                } else {
                    bottom - top
                },
        );
    }
    unsafe {
        invalidate_rect(app.hwnd, std::ptr::null(), 0);
    }
    true
}

fn drag_scrollbar(app: &mut App, y: i32) {
    let Some((start_y, start_scroll)) = app.scroll_drag else {
        return;
    };
    if let Some(bar) = scrollbar(app) {
        let travel = (bar.track.bottom - bar.track.top) - (bar.thumb.bottom - bar.thumb.top);
        if travel > 0 {
            set_list_scroll(
                app,
                start_scroll + (y - start_y) * list_max_scroll(app) / travel,
            );
            unsafe {
                invalidate_rect(app.hwnd, std::ptr::null(), 0);
            }
        }
    }
}

fn locate_current(app: &mut App) {
    if let Some(index) = app.rows.iter().position(|row| row.is_active) {
        let visible = app.full_height - FOOTER - TABLE_TOP - HEADER_ROW;
        app.scroll = (index as i32 * DATA_ROW - (visible - DATA_ROW) / 2).clamp(0, max_scroll(app));
    }
}

fn account_at(app: &App, x: i32, y: i32) -> Option<usize> {
    if app.compact_mode {
        if point_in(&box_rect(14, 62, COMPACT_WIDTH - 28, 144), x, y) {
            return app.rows.iter().position(|r| r.is_active);
        }
        compact_row_at(app, x, y)
    } else {
        let l = layout(app);
        if y < l.header.bottom || y >= app.full_height - FOOTER {
            return None;
        }
        l.rows.iter().position(|row| point_in(row, x, y))
    }
}

fn update_pointer_hover(app: &mut App, x: i32, y: i32) {
    let blocked = app.updates.blocked()
        || app.confirm.is_some()
        || app.login_pending
        || app.settings.is_some()
        || app.add_menu
        || app.context_row.is_some();
    let item = hover_item_at(app, x, y);
    let compact = if app.compact_mode {
        if let Some(index) = app.context_row {
            Some(CompactHit::Card(index))
        } else if !blocked {
            compact_hit_at(app, x, y)
        } else {
            None
        }
    } else {
        None
    };
    let row = if !app.compact_mode {
        app.context_row.or_else(|| {
            if !blocked {
                account_at(app, x, y)
            } else {
                None
            }
        })
    } else {
        None
    };
    if item != app.hover_item || compact != app.compact_hover || row != app.hover_row {
        app.hover_item = item;
        app.compact_hover = compact;
        app.hover_row = row;
        unsafe {
            invalidate_rect(app.hwnd, std::ptr::null(), 0);
        }
    }
}

fn px(app: &App, value: i32) -> i32 {
    (value as f32 * app.scale).round() as i32
}
fn dip(app: &App, value: i32) -> i32 {
    (value as f32 / app.scale).floor() as i32
}

fn fitted_scale(dpi: u32, work_width: i32, work_height: i32, compact: bool) -> f32 {
    let (width, height) = if compact {
        (COMPACT_WIDTH, COMPACT_HEADER + COMPACT_FOOTER + 52)
    } else {
        (WIDTH, HEIGHT)
    };
    (dpi.max(96) as f32 / 96.0)
        .min((work_width - 16).max(1) as f32 / width as f32)
        .min((work_height - 16).max(1) as f32 / height as f32)
        .max(0.25)
}

fn window_scale(hwnd: HWND, compact: bool) -> f32 {
    let dpi = unsafe { windows_sys::Win32::UI::HiDpi::GetDpiForWindow(hwnd) };
    let work = monitor_work_rect(hwnd);
    fitted_scale(dpi, work.right - work.left, work.bottom - work.top, compact)
}

fn apply_window_scale(app: &mut App, origin: Option<(i32, i32)>) {
    app.scale = window_scale(app.hwnd, app.compact_mode);
    let work = monitor_work_rect(app.hwnd);
    if app.compact_mode {
        update_compact_height_limit(app, &work);
    }
    let (width, height) = if app.compact_mode {
        (px(app, COMPACT_WIDTH), px(app, compact_height(app)))
    } else {
        (px(app, WIDTH), px(app, app.full_height))
    };
    let (x, y) = origin.unwrap_or((work.left + ((work.right - work.left) - width) / 2, work.top));
    let x = x.clamp(work.left, (work.right - width).max(work.left));
    let y = y.clamp(work.top, (work.bottom - height).max(work.top));
    let (x, y) = if app.docked {
        let (left, top) = dock_origin_in(app, &work);
        (left, if app.visible { top } else { top - height })
    } else {
        (x, y)
    };
    if app.compact_mode {
        app.compact_left = x;
    }
    if let Some(graphics) = app.graphics.as_mut() {
        graphics.release_window();
    }
    unsafe {
        SetWindowPos(
            app.hwnd,
            std::ptr::null_mut(),
            x,
            y,
            width,
            height,
            SWP_NOACTIVATE | SWP_NOZORDER,
        );
        invalidate_rect(app.hwnd, std::ptr::null(), 0);
    }
    settings_ui::reposition(app);
}

fn tick(app: &mut App) {
    if updates_ui::tick(app) {
        unsafe {
            invalidate_rect(app.hwnd, std::ptr::null(), 0);
        }
    }
    if app.updates.exit_ready {
        return;
    }
    if drain(app) {
        unsafe { invalidate_rect(app.hwnd, std::ptr::null(), 0) };
    }
    if app.busy.is_none()
        && !app.login_pending
        && !app.updates.blocked()
        && app.confirm.is_none()
        && app.settings.is_none()
        && app.connection_check.is_some_and(|at| Instant::now() >= at)
    {
        start_connection(app, app.connection_established);
    }

    // 自动刷新：整表每小时一次，当前账号每 5 分钟一次。
    if app.last_all_refresh.elapsed() >= Duration::from_secs(app.refresh_all_minutes as u64 * 60) {
        app.last_all_refresh = Instant::now();
        app.last_active_refresh = Instant::now();
        refresh_all_rows(app);
        unsafe { invalidate_rect(app.hwnd, std::ptr::null(), 0) };
    } else if app.last_active_refresh.elapsed()
        >= Duration::from_secs(app.refresh_active_minutes as u64 * 60)
    {
        app.last_active_refresh = Instant::now();
        refresh_active_row(app);
        unsafe { invalidate_rect(app.hwnd, std::ptr::null(), 0) };
    }

    // 提示文字只在出现后停留几秒，之后自动消失。
    if app.notice != app.last_notice {
        app.last_notice = app.notice.clone();
        app.notice_at = Instant::now();
    } else if app.busy.is_none()
        && app.refresh_pending.is_empty()
        && app.notice.is_some()
        && app.notice_at.elapsed() > Duration::from_secs(4)
    {
        app.notice = None;
        app.last_notice = None;
        unsafe { invalidate_rect(app.hwnd, std::ptr::null(), 0) };
    }

    if app.tray_hidden
        || (!app.hwnd.is_null()
            && unsafe { windows_sys::Win32::UI::WindowsAndMessaging::IsIconic(app.hwnd) != 0 })
    {
        return;
    }
    if app.anim.is_some() {
        step_anim(app);
        return;
    }

    if let Some((cursor_x, cursor_y)) = win::cursor_pos() {
        let mut rect: RECT = unsafe { std::mem::zeroed() };
        unsafe {
            GetWindowRect(app.hwnd, &mut rect);
        }
        update_pointer_hover(
            app,
            dip(app, cursor_x - rect.left),
            dip(app, cursor_y - rect.top),
        );
    }
    if app.docked {
        if app.updates.blocked()
            || app.confirm.is_some()
            || app.add_menu
            || app.login_pending
            || app.context_row.is_some()
            || app.scroll_drag.is_some()
            || app.settings.is_some()
        {
            return;
        }
        // 调试：强制常驻显示紧凑列表（CODEX_SWITCH_PREVIEW=1），便于截图核对。
        if std::env::var_os("CODEX_SWITCH_PREVIEW").is_some() {
            if !app.visible {
                slide_down(app);
            }
            return;
        }
        let Some((cursor_x, cursor_y)) = win::cursor_pos() else {
            return;
        };
        let mut bounds: RECT = unsafe { std::mem::zeroed() };
        unsafe {
            GetWindowRect(app.hwnd, &mut bounds);
        }
        let work = app.dock_work.unwrap_or_else(|| monitor_work_rect(app.hwnd));
        match dock_pointer_action(app, cursor_x, cursor_y, &bounds, &work) {
            DockAction::Show => slide_down(app),
            DockAction::Hide => slide_up(app),
            DockAction::None => {}
        }
        return;
    }
}

fn dismiss_menus(app: &mut App) {
    app.context_row = None;
    app.add_menu = false;
    app.hover_item = None;
}

fn hover_item_at(app: &App, x: i32, y: i32) -> Option<HoverItem> {
    let layout = layout(app);
    let hit = |rect: &RECT| x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom;

    if app.context_menu_visible() {
        if hit(&layout.context_refresh) {
            return Some(HoverItem::ContextRefresh);
        }
        if hit(&layout.context_switch) {
            return Some(HoverItem::ContextSwitch);
        }
        if hit(&layout.context_delete) {
            return Some(HoverItem::ContextDelete);
        }
    }
    if app.add_menu {
        if hit(&layout.menu_register) {
            return Some(HoverItem::AddRegister);
        }
        if hit(&layout.menu_login) {
            return Some(HoverItem::AddLogin);
        }
    }
    None
}

impl App {
    fn context_menu_visible(&self) -> bool {
        self.context_row.is_some()
    }
}

fn monitor_rect(hwnd: HWND) -> RECT {
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) != 0 {
            info.rcMonitor
        } else {
            RECT {
                left: 0,
                top: 0,
                right: WIDTH,
                bottom: HEIGHT,
            }
        }
    }
}

fn monitor_work_rect(hwnd: HWND) -> RECT {
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) != 0 {
            info.rcWork
        } else {
            monitor_rect(hwnd)
        }
    }
}

fn set_y(app: &App, y: i32) {
    if app.hwnd.is_null() {
        return;
    }
    unsafe {
        let mut rect: RECT = std::mem::zeroed();
        GetWindowRect(app.hwnd, &mut rect);
        SetWindowPos(
            app.hwnd,
            std::ptr::null_mut(),
            rect.left,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

fn start_anim(app: &mut App, to: i32) {
    let mut rect: RECT = unsafe { std::mem::zeroed() };
    unsafe { GetWindowRect(app.hwnd, &mut rect) };
    if rect.top == to {
        app.anim = Some(Anim {
            from: to,
            to,
            start: Instant::now() - Duration::from_millis(ANIM_MS),
            duration: Duration::from_millis(ANIM_MS),
        });
        step_anim(app);
        return;
    }
    app.anim = Some(Anim {
        from: rect.top,
        to,
        start: Instant::now(),
        duration: Duration::from_millis(ANIM_MS),
    });
    unsafe { SetTimer(app.hwnd, TIMER_ID, 16, None) };
}

fn step_anim(app: &mut App) {
    let Some((from, to, start, duration)) = app
        .anim
        .as_ref()
        .map(|anim| (anim.from, anim.to, anim.start, anim.duration))
    else {
        return;
    };
    let total = duration.as_secs_f32().max(0.001);
    let progress = (start.elapsed().as_secs_f32() / total).min(1.0);
    let eased = 1.0 - (1.0 - progress).powi(3);
    let y = from as f32 + (to - from) as f32 * eased;
    set_y(app, y as i32);
    if progress >= 1.0 {
        app.anim = None;
        unsafe { SetTimer(app.hwnd, TIMER_ID, TIMER_INTERVAL, None) };
        if app.docked {
            let (_, top) = dock_origin(app);
            if to < top {
                unsafe { ShowWindow(app.hwnd, SW_HIDE) };
                app.visible = false;
                app.compact_hover = None;
                if let Some(graphics) = app.graphics.as_mut() {
                    graphics.release_window();
                }
                unsafe {
                    SetTimer(app.hwnd, TIMER_ID, 160, None);
                }
            }
        }
    }
}

/// One request per profile, shared by startup, reload, manual and timed refresh.
/// Keeping this outside RowView preserves deduplication when the list is reloaded.
fn queue_usage(app: &mut App, index: usize) -> bool {
    let Some(row) = app.rows.get_mut(index) else {
        return false;
    };
    let Some(home) = app.homes.get(&row.id).cloned() else {
        return false;
    };
    row.loading = true;
    row.error = None;
    if !app.usage_pending.insert(row.id.clone()) {
        return true;
    }
    if app
        .tx
        .send(Request {
            id: row.id.clone(),
            home,
        })
        .is_err()
    {
        app.usage_pending.remove(&row.id);
        row.loading = false;
        row.error = Some("刷新服务不可用，请重新打开切号器".into());
        return false;
    }
    true
}

fn begin_refresh(app: &mut App, indices: Vec<usize>, notice: String) {
    if app.refresh_pending.is_empty() {
        app.refresh_failed = false;
    }
    let mut any = false;
    for index in indices {
        let Some(row) = app.rows.get(index) else {
            continue;
        };
        let id = row.id.clone();
        if !app.homes.contains_key(&id) {
            continue;
        }
        any = true;
        app.refresh_pending.insert(id.clone());
        if !queue_usage(app, index) {
            app.refresh_pending.remove(&id);
            app.refresh_failed = true;
        }
    }
    if any {
        app.notice = Some(if app.refresh_pending.is_empty() && app.refresh_failed {
            "刷新失败，刷新服务不可用".into()
        } else {
            notice
        });
        app.notice_at = Instant::now();
    }
}

fn refresh_one(app: &mut App, index: usize) {
    let label = app
        .rows
        .get(index)
        .map(|r| visual::shown(app, r))
        .unwrap_or_default();
    if app.rows.get(index).is_some_and(|r| r.is_active) {
        app.last_active_refresh = Instant::now();
    }
    begin_refresh(app, vec![index], format!("正在刷新{label}"));
}
fn refresh_all_rows(app: &mut App) {
    app.last_all_refresh = Instant::now();
    app.last_active_refresh = Instant::now();
    begin_refresh(
        app,
        (0..app.rows.len()).collect(),
        "正在刷新全部账号".into(),
    );
}
fn refresh_active_row(app: &mut App) {
    if let Some(index) = app.rows.iter().position(|r| r.is_active) {
        refresh_one(app, index);
    }
}

fn max_scroll(app: &App) -> i32 {
    let visible = (app.full_height - FOOTER) - (TABLE_TOP + HEADER_ROW);
    (app.rows.len() as i32 * DATA_ROW - visible).max(0)
}

fn scroll_by(app: &mut App, delta: i32) {
    app.scroll = (app.scroll + delta).clamp(0, max_scroll(app));
}

fn compact_height(app: &App) -> i32 {
    let count = app.rows.iter().filter(|row| !row.is_active).count() as i32;
    let list = if count == 0 { 52 } else { count * COMPACT_CARD };
    (COMPACT_HEADER + list + COMPACT_FOOTER)
        .min(COMPACT_MAX_HEIGHT)
        .min(app.compact_height_limit)
}

fn compact_max_scroll(app: &App) -> i32 {
    let count = app.rows.iter().filter(|row| !row.is_active).count() as i32;
    let visible = compact_height(app) - COMPACT_HEADER - COMPACT_FOOTER;
    (count * COMPACT_CARD - visible).max(0)
}

fn ui_settings_path() -> Option<PathBuf> {
    if let Some(custom) = std::env::var_os("CODEX_SWITCH_UI_FILE") {
        return Some(PathBuf::from(custom));
    }
    let base = std::env::var_os("LOCALAPPDATA")?;
    let mut path = PathBuf::from(base);
    path.push("CodexSwitch");
    path.push("ui.json");
    Some(path)
}

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize, Debug, Default, PartialEq, Eq)]
#[serde(default)]
struct PanelPlacement {
    origin: Option<(i32, i32)>,
    dock_anchor: Option<(i32, i32)>,
    docked: bool,
}

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct UiSettings {
    compact_left: i32,
    full_panel: PanelPlacement,
    compact_panel: PanelPlacement,
    masked: bool,
    active_minutes: u32,
    all_minutes: u32,
    usage_concurrency: u32,
    close_action: CloseAction,
}
impl Default for UiSettings {
    fn default() -> Self {
        Self {
            compact_left: 0,
            full_panel: PanelPlacement::default(),
            compact_panel: PanelPlacement::default(),
            masked: false,
            active_minutes: 5,
            all_minutes: 60,
            usage_concurrency: usage_workers::DEFAULT_CONCURRENCY,
            close_action: CloseAction::Tray,
        }
    }
}
fn app_settings(app: &App) -> UiSettings {
    UiSettings {
        compact_left: app.compact_left,
        full_panel: if app.compact_mode {
            app.full_panel
        } else {
            current_panel_placement(app)
        },
        compact_panel: if app.compact_mode {
            current_panel_placement(app)
        } else {
            app.compact_panel
        },
        masked: app.masked,
        active_minutes: app.refresh_active_minutes,
        all_minutes: app.refresh_all_minutes,
        usage_concurrency: app.usage_concurrency,
        close_action: app.close_action,
    }
}
fn parse_ui_settings(text: &str) -> UiSettings {
    let mut value: UiSettings =
        serde_json::from_str(text.trim_start_matches('\u{feff}')).unwrap_or_default();
    if !(2..=120).contains(&value.active_minutes) {
        value.active_minutes = 5;
    }
    if !(20..=300).contains(&value.all_minutes) {
        value.all_minutes = 60;
    }
    if !(1..=usage_workers::MAX_CONCURRENCY).contains(&value.usage_concurrency) {
        value.usage_concurrency = usage_workers::DEFAULT_CONCURRENCY;
    }
    value
}
fn load_ui_settings() -> UiSettings {
    ui_settings_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| parse_ui_settings(&s))
        .unwrap_or_default()
}
fn save_ui_settings(settings: UiSettings) -> Result<()> {
    let path = ui_settings_path().ok_or_else(|| anyhow::anyhow!("找不到界面设置目录"))?;
    crate::win::write_atomic(&path, &serde_json::to_vec_pretty(&settings)?)
}

/// `someone@icloud.com` → `some****@**ud.com`
fn mask_account(label: &str) -> String {
    let Some((local, domain)) = label.split_once('@') else {
        return label.to_string();
    };
    let head: String = local.chars().take(4).collect();

    let mut parts = domain.rsplitn(2, '.');
    let tld = parts.next().unwrap_or("");
    let body = parts.next();
    let masked_domain = match body {
        Some(body) => {
            let count = body.chars().count();
            let tail: String = body.chars().skip(count.saturating_sub(2)).collect();
            if count > 2 {
                format!("**{tail}.{tld}")
            } else {
                format!("{body}.{tld}")
            }
        }
        None => domain.to_string(),
    };

    format!("{head}****@{masked_domain}")
}

fn window_dimensions(app: &App) -> (i32, i32) {
    if app.compact_mode {
        (px(app, COMPACT_WIDTH), px(app, compact_height(app)))
    } else {
        (px(app, WIDTH), px(app, app.full_height))
    }
}

fn dock_origin_in(app: &App, work: &RECT) -> (i32, i32) {
    let max_left = (work.right - window_dimensions(app).0).max(work.left);
    (app.compact_left.clamp(work.left, max_left), work.top)
}
fn dock_origin(app: &App) -> (i32, i32) {
    dock_origin_in(
        app,
        &app.dock_work.unwrap_or_else(|| monitor_work_rect(app.hwnd)),
    )
}

#[derive(Debug, PartialEq, Eq)]
enum DockAction {
    None,
    Show,
    Hide,
}

fn dock_pointer_action(app: &mut App, x: i32, y: i32, bounds: &RECT, work: &RECT) -> DockAction {
    if !app.docked
        || app.tray_hidden
        || app.anim.is_some()
        || app.updates.blocked()
        || app.confirm.is_some()
        || app.settings.is_some()
        || app.add_menu
        || app.context_row.is_some()
        || app.login_pending
        || app.scroll_drag.is_some()
    {
        return DockAction::None;
    }
    if app.visible {
        if point_in(bounds, x, y) {
            app.dock_wait_for_pointer = false;
            DockAction::None
        } else if app.dock_wait_for_pointer {
            DockAction::None
        } else {
            DockAction::Hide
        }
    } else {
        let (left, top) = dock_origin_in(app, work);
        if y >= top && y < top + 2 && x >= left && x < left + window_dimensions(app).0 {
            DockAction::Show
        } else {
            DockAction::None
        }
    }
}

fn current_panel_placement(app: &App) -> PanelPlacement {
    let mut placement = if app.compact_mode {
        app.compact_panel
    } else {
        app.full_panel
    };
    placement.docked = app.docked;
    if app.docked {
        // A hidden/animating docked HWND is above the screen. Remember its hot
        // zone and its separate undocked return position, never that rectangle.
        placement.origin = app.dock_return.or(placement.origin);
        if let Some(work) = app.dock_work {
            placement.dock_anchor = Some((app.compact_left, work.top));
        }
    } else if !app.hwnd.is_null() {
        unsafe {
            let mut bounds: RECT = std::mem::zeroed();
            if windows_sys::Win32::UI::WindowsAndMessaging::IsIconic(app.hwnd) == 0
                && GetWindowRect(app.hwnd, &mut bounds) != 0
            {
                placement.origin = Some((bounds.left, bounds.top));
            }
        }
    }
    placement
}

/// Each presentation owns its normal position, dock anchor and dock state.
fn set_compact_mode(app: &mut App, compact: bool) {
    if app.compact_mode == compact {
        return;
    }
    let previous = current_panel_placement(app);
    if app.compact_mode {
        app.compact_panel = previous;
    } else {
        app.full_panel = previous;
    }
    let placement = if compact {
        app.compact_panel
    } else {
        app.full_panel
    };
    app.compact_mode = compact;
    restore_panel_placement(app, placement, previous.origin);
    if app.docked {
        // A mode switch restores the actual docked presentation immediately;
        // it must not wait for a new pointer entry/leave cycle.
        app.dock_wait_for_pointer = false;
        slide_up(app);
    }
}

fn restore_panel_placement(app: &mut App, placement: PanelPlacement, fallback: Option<(i32, i32)>) {
    let saved_height = app.full_height;
    app.tray_hidden = false;
    // Startup restoration waits for pointer entry. Mode switches explicitly
    // start docking after placement; tray restoration has its own wait policy.
    app.dock_wait_for_pointer = placement.docked;
    app.docked = placement.docked;
    app.dock_return = if placement.docked {
        placement.origin.or(fallback)
    } else {
        None
    };
    app.dock_work = None;
    app.anim = None;
    app.visible = true;
    app.hover_row = None;
    app.compact_hover = None;
    app.scroll_drag = None;
    dismiss_menus(app);
    if app.hwnd.is_null() {
        return;
    }
    let origin = if placement.docked {
        placement.dock_anchor.or(placement.origin).or(fallback)
    } else {
        placement.origin.or(fallback)
    };
    let (work, scale) = panel_work_area(app, origin);
    // Changing taskbar style can emit WM_SIZE with the previous mode's bounds.
    set_taskbar_entry(app, !app.docked);
    app.full_height = saved_height;
    app.scale = scale;
    if app.compact_mode {
        update_compact_height_limit(app, &work);
    }
    let (width, height) = window_dimensions(app);
    let (x, y) = restored_panel_position(origin, &work, width, height, app.docked);
    app.compact_left = x;
    if app.docked {
        app.dock_work = Some(work);
    }
    if let Some(graphics) = app.graphics.as_mut() {
        graphics.release_window();
    }
    unsafe {
        SetWindowPos(
            app.hwnd,
            if app.docked {
                HWND_TOPMOST
            } else {
                HWND_NOTOPMOST
            },
            x,
            y,
            width,
            height,
            SWP_NOACTIVATE,
        );
        ShowWindow(app.hwnd, SW_SHOWNOACTIVATE);
        SetTimer(app.hwnd, TIMER_ID, TIMER_INTERVAL, None);
        invalidate_rect(app.hwnd, std::ptr::null(), 0);
    }
}

fn restored_panel_position(
    origin: Option<(i32, i32)>,
    work: &RECT,
    width: i32,
    height: i32,
    docked: bool,
) -> (i32, i32) {
    let (x, y) = origin.unwrap_or((work.left + (work.right - work.left - width) / 2, work.top));
    (
        x.clamp(work.left, (work.right - width).max(work.left)),
        if docked {
            work.top
        } else {
            y.clamp(work.top, (work.bottom - height).max(work.top))
        },
    )
}

fn panel_work_area(app: &App, origin: Option<(i32, i32)>) -> (RECT, f32) {
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    if let Some((x, y)) = origin {
        unsafe {
            let monitor = MonitorFromPoint(
                windows_sys::Win32::Foundation::POINT { x, y },
                MONITOR_DEFAULTTONEAREST,
            );
            let mut info: MONITORINFO = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(monitor, &mut info) != 0 {
                let mut dpi_x = 96;
                let mut dpi_y = 96;
                windows_sys::Win32::UI::HiDpi::GetDpiForMonitor(
                    monitor,
                    windows_sys::Win32::UI::HiDpi::MDT_EFFECTIVE_DPI,
                    &mut dpi_x,
                    &mut dpi_y,
                );
                let work = info.rcWork;
                return (
                    work,
                    fitted_scale(
                        dpi_x,
                        work.right - work.left,
                        work.bottom - work.top,
                        app.compact_mode,
                    ),
                );
            }
        }
    }
    (
        monitor_work_rect(app.hwnd),
        window_scale(app.hwnd, app.compact_mode),
    )
}

fn toggle_dock(app: &mut App) {
    if app.docked {
        let mut placement = current_panel_placement(app);
        placement.docked = false;
        restore_panel_placement(app, placement, None);
    } else {
        app.docked = true;
        app.tray_hidden = false;
        app.dock_wait_for_pointer = false;
        app.anim = None;
        dismiss_menus(app);
        if app.hwnd.is_null() {
            app.visible = false;
            return;
        }
        let saved_height = app.full_height;
        let mut bounds: RECT = unsafe { std::mem::zeroed() };
        unsafe {
            GetWindowRect(app.hwnd, &mut bounds);
        }
        app.dock_return = Some((bounds.left, bounds.top));
        app.dock_work = Some(monitor_work_rect(app.hwnd));
        app.compact_left = bounds.left;
        set_taskbar_entry(app, false);
        app.full_height = saved_height;
        let (width, height) = window_dimensions(app);
        let (left, top) = dock_origin(app);
        unsafe {
            SetWindowPos(
                app.hwnd,
                HWND_TOPMOST,
                left,
                top,
                width,
                height,
                SWP_NOACTIVATE,
            );
        }
        app.visible = true;
        slide_up(app);
    }
    unsafe {
        invalidate_rect(app.hwnd, std::ptr::null(), 0);
    }
}

fn slide_up(app: &mut App) {
    if !app.visible || !app.docked {
        return;
    }
    if app.hwnd.is_null() {
        app.visible = false;
        return;
    }
    let (_, top) = dock_origin(app);
    start_anim(app, top - window_dimensions(app).1);
}

fn slide_down(app: &mut App) {
    if !app.docked || app.tray_hidden {
        return;
    }
    if app.hwnd.is_null() {
        app.visible = true;
        return;
    }
    let (x, top) = dock_origin(app);
    let (width, height) = window_dimensions(app);
    if !app.visible {
        unsafe {
            SetWindowPos(
                app.hwnd,
                HWND_TOPMOST,
                x,
                top - height,
                width,
                height,
                SWP_NOACTIVATE,
            );
            ShowWindow(app.hwnd, SW_SHOWNOACTIVATE);
        }
    }
    app.visible = true;
    start_anim(app, top);
}

fn set_taskbar_entry(app: &App, show: bool) {
    unsafe {
        let current = GetWindowLongPtrW(app.hwnd, GWL_EXSTYLE) as u32;
        let updated = if show {
            (current & !WS_EX_TOOLWINDOW) | WS_EX_APPWINDOW
        } else {
            (current & !WS_EX_APPWINDOW) | WS_EX_TOOLWINDOW
        };
        if updated != current {
            SetWindowLongPtrW(app.hwnd, GWL_EXSTYLE, updated as isize);
            SetWindowPos(
                app.hwnd,
                std::ptr::null_mut(),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
        }
    }
}

const HWND_TOPMOST: HWND = -1isize as HWND;
const HWND_NOTOPMOST: HWND = -2isize as HWND;

fn drain(app: &mut App) -> bool {
    let mut changed = false;
    while let Ok(Message::Usage { id, result }) = app.rx.try_recv() {
        changed = true;
        app.usage_pending.remove(&id);
        let failed = result.is_err();
        if let Some(row) = app.rows.iter_mut().find(|row| row.id == id) {
            row.loading = false;
            match result {
                Ok(value) => {
                    row.error = None;
                    row.expired = false;
                    row.five_hour = value.five_hour_remaining;
                    row.five_hour_reset = value.five_hour_resets_at;
                    row.weekly = value.weekly_remaining;
                    row.weekly_reset = value.weekly_resets_at;
                    row.reset_credits = value.reset_credits;
                }
                Err(error) => {
                    row.expired = looks_like_auth_failure(&error);
                    row.error = Some(error);
                    row.five_hour = None;
                    row.weekly = None;
                    row.reset_credits = None;
                }
            }
        }
        if app.refresh_pending.remove(&id) {
            app.refresh_failed |= failed;
            if app.refresh_pending.is_empty() {
                app.notice = Some(
                    if app.refresh_failed {
                        "刷新失败，请检查账号状态"
                    } else {
                        "刷新成功"
                    }
                    .into(),
                );
                app.notice_at = Instant::now();
            }
        }
    }

    while let Ok(result) = app.switch_rx.try_recv() {
        changed = true;
        let connecting = app.connecting;
        let finished = result.as_ref().map_or(true, |notice| notice.finished);
        if connecting && finished {
            app.connecting = false;
            app.connection_established = result.is_ok();
            // A failed setup gets one automatic attempt. Avoid restarting in a loop.
            app.connection_check = result
                .as_ref()
                .ok()
                .map(|_| Instant::now() + Duration::from_secs(30));
        }
        match result {
            Ok(notice) => {
                app.busy = if notice.finished {
                    None
                } else {
                    Some(
                        if connecting {
                            "连接无感后台"
                        } else {
                            "等待当前轮完成"
                        }
                        .to_string(),
                    )
                };
                if !notice.message.is_empty() {
                    app.notice = Some(notice.message);
                }
                if notice.finished && !connecting {
                    reload(app);
                }
            }
            Err(error) => {
                app.busy = None;
                app.notice = Some(format!(
                    "{}：{error}",
                    if connecting {
                        "连接失败"
                    } else {
                        "切换失败"
                    }
                ));
            }
        }
    }
    while let Ok(result) = app.login_rx.try_recv() {
        changed = true;
        app.login_pending = false;
        match result {
            Ok(label) => {
                app.notice = Some(format!("已登录并导入 {label}"));
                reload(app);
            }
            Err(error) => app.notice = Some(format!("浏览器登录：{error}")),
        }
    }

    changed
}

fn reload(app: &mut App) {
    let (rows, homes, notice) = load_rows();
    app.rows = rows;
    app.homes = homes;
    app.context_row = None;
    app.scroll_drag = None;
    app.compact_hover = None;
    app.hover_row = None;
    if notice.is_some() {
        app.notice = notice;
    }
    app.scroll = app.scroll.min(max_scroll(app));
    app.compact_scroll = app.compact_scroll.min(compact_max_scroll(app));
    if app.compact_mode && !app.hwnd.is_null() {
        let height = px(app, compact_height(app));
        unsafe {
            SetWindowPos(
                app.hwnd,
                std::ptr::null_mut(),
                0,
                0,
                px(app, COMPACT_WIDTH),
                height,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        if app.docked && !app.visible && !app.tray_hidden {
            let (_, top) = dock_origin(app);
            set_y(app, top - height);
        }
    }
    for index in 0..app.rows.len() {
        queue_usage(app, index);
    }
}

fn right_clicked(app: &mut App, x: i32, y: i32) {
    if app.updates.blocked()
        || app.busy.is_some()
        || app.confirm.is_some()
        || app.login_pending
        || app.settings.is_some()
    {
        return;
    }
    let layout = layout(app);
    let inside = |rect: &RECT| x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom;

    // 点在已打开的右键菜单上就不动它。
    if let Some(menu) = layout.context_menu {
        if inside(&menu) {
            return;
        }
    }

    let index = account_at(app, x, y);
    if let Some(index) = index {
        app.context_row = Some(index);
        app.context_at = (x, y);
        app.add_menu = false;
        update_pointer_hover(app, x, y);
    } else {
        app.context_row = None;
    }
}

fn clicked(app: &mut App, x: i32, y: i32) {
    if app.updates.blocked() {
        updates_ui::clicked(app, x, y);
        return;
    }
    let close = if app.compact_mode {
        compact_layout(app).close
    } else {
        layout(app).close
    };
    if point_in(&close, x, y) {
        request_close(app);
        return;
    }
    if app.anim.is_some() {
        return;
    }
    if app.settings.is_some() {
        settings_ui::clicked(app, x, y);
        return;
    }
    if app.compact_mode
        && !app.updates.blocked()
        && app.confirm.is_none()
        && !app.login_pending
        && !app.add_menu
        && app.context_row.is_none()
    {
        compact_clicked(app, x, y);
        return;
    }
    let layout = layout(app);
    let inside = |rect: &RECT| x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom;

    if app.login_pending {
        if inside(&layout.dialog_cancel) {
            app.login_cancel.store(true, Ordering::Relaxed);
            app.notice = Some("正在取消登录…".to_string());
        }
        return;
    }

    if app.confirm.is_some() {
        if inside(&layout.dialog_cancel) {
            app.confirm = None;
        } else if inside(&layout.dialog_ok) {
            if let Some((kind, id)) = app.confirm.take() {
                match kind {
                    Confirm::Switch => {
                        app.busy = Some("切换中".to_string());
                        app.notice = Some("正在连接无感后台，窗口保持打开…".to_string());
                        let sender = app.switch_tx.clone();
                        std::thread::spawn(move || {
                            let result = (|| -> anyhow::Result<SwitchNotice> {
                                let store = Store::discover()?;
                                let outcome = crate::switch::switch_with_progress(
                                    &store,
                                    &id,
                                    false,
                                    |message| {
                                        let _ = sender.send(Ok(SwitchNotice {
                                            message: message.into(),
                                            finished: false,
                                        }));
                                    },
                                )?;
                                if outcome.queued {
                                    let _ = sender.send(Ok(SwitchNotice {
                                        message: QUEUED_SWITCH_NOTICE.into(),
                                        finished: false,
                                    }));
                                    if !crate::bridge::wait_for_switch(&store, &id)? {
                                        return Ok(SwitchNotice {
                                            message: "已取消等待中的换号，账号未改动".into(),
                                            finished: true,
                                        });
                                    }
                                }
                                Ok(SwitchNotice {
                                    message: "已切换，下一轮使用所选账号；窗口保持打开".into(),
                                    finished: true,
                                })
                            })()
                            .map_err(|error| format!("{error:#}"));
                            let _ = sender.send(result);
                        });
                    }
                    Confirm::Delete => {
                        match Store::discover().and_then(|store| store.remove_account(&id)) {
                            Ok(()) => {
                                app.notice = Some("已删除该账号".to_string());
                                reload(app);
                            }
                            Err(error) => app.notice = Some(format!("删除失败：{error:#}")),
                        }
                    }
                }
            }
        }
        unsafe { invalidate_rect(app.hwnd, std::ptr::null(), 0) };
        return;
    }

    if app.context_row.is_some() {
        if inside(&layout.context_refresh) {
            if let Some(index) = app.context_row.take() {
                refresh_one(app, index);
            }
        } else if inside(&layout.context_switch) {
            if let Some(index) = app.context_row.take() {
                let id = app.rows[index].id.clone();
                if app.rows[index].is_active {
                    app.notice = Some("已经是当前账号".to_string());
                } else {
                    app.confirm = Some((Confirm::Switch, id));
                }
            }
        } else if inside(&layout.context_delete) {
            if let Some(index) = app.context_row.take() {
                if app.rows[index].is_active {
                    app.notice = Some("当前账号不能删除，请先切换到其它账号".into());
                } else {
                    app.confirm = Some((Confirm::Delete, app.rows[index].id.clone()));
                }
            }
        } else if layout
            .context_menu
            .map(|menu| !inside(&menu))
            .unwrap_or(true)
        {
            app.context_row = None;
        }
        unsafe { invalidate_rect(app.hwnd, std::ptr::null(), 0) };
        return;
    }

    if app.add_menu {
        if inside(&layout.menu_register) {
            app.add_menu = false;
            match Store::discover().and_then(|store| store.register_current()) {
                Ok(account) => {
                    app.notice = Some(format!("已登记 {}", account.label()));
                    reload(app);
                }
                Err(error) => app.notice = Some(error_notice("登记失败", &error)),
            }
        } else if inside(&layout.menu_login) {
            app.add_menu = false;
            start_login(app);
        } else if !inside(&layout.add) {
            app.add_menu = false;
        }
        unsafe { invalidate_rect(app.hwnd, std::ptr::null(), 0) };
        return;
    }

    if inside(&layout.locate) {
        locate_current(app);
        unsafe {
            invalidate_rect(app.hwnd, std::ptr::null(), 0);
        }
        return;
    }
    if scrollbar_pressed(app, x, y) {
        return;
    }
    if inside(&layout.settings) {
        settings_ui::open(app);
        return;
    }

    if inside(&layout.refresh_all) {
        if app.rows.is_empty() {
            app.notice = Some("还没有账号".to_string());
        } else {
            app.notice = Some("正在刷新全部账号".to_string());
            refresh_all_rows(app);
        }
        unsafe { invalidate_rect(app.hwnd, std::ptr::null(), 0) };
        return;
    }

    if inside(&layout.privacy) {
        app.masked = !app.masked;
        unsafe { invalidate_rect(app.hwnd, std::ptr::null(), 0) };
        return;
    }

    if inside(&layout.min) {
        unsafe { ShowWindow(app.hwnd, SW_MINIMIZE) };
        return;
    }
    if inside(&layout.pin) {
        toggle_dock(app);
        return;
    }
    if inside(&layout.shrink) {
        set_compact_mode(app, true);
        return;
    }
    if inside(&layout.add) {
        if app.busy.is_some() {
            return;
        }
        app.add_menu = true;
        app.context_row = None;
        unsafe { invalidate_rect(app.hwnd, std::ptr::null(), 0) };
        return;
    }

    if y < layout.header.bottom || y >= app.full_height - FOOTER {
        return;
    }
    for (index, area) in layout.switch.iter().enumerate() {
        if inside(area) {
            if app.busy.is_none() && !app.rows[index].is_active {
                app.confirm = Some((Confirm::Switch, app.rows[index].id.clone()));
                unsafe { invalidate_rect(app.hwnd, std::ptr::null(), 0) };
            }
            return;
        }
    }
}

fn double_clicked(app: &mut App, x: i32, y: i32) {
    if app.anim.is_some()
        || app.updates.blocked()
        || app.confirm.is_some()
        || app.login_pending
        || app.add_menu
        || app.context_row.is_some()
        || app.settings.is_some()
    {
        return;
    }
    let index = if app.compact_mode {
        if point_in(&box_rect(14, 62, COMPACT_WIDTH - 28, 144), x, y) {
            app.rows.iter().position(|r| r.is_active)
        } else {
            compact_row_at(app, x, y)
        }
    } else {
        let l = layout(app);
        if y < l.header.bottom || y >= app.full_height - FOOTER {
            None
        } else {
            l.rows.iter().position(|r| point_in(r, x, y))
        }
    };
    if let Some(index) = index {
        refresh_one(app, index);
        unsafe {
            invalidate_rect(app.hwnd, std::ptr::null(), 0);
        }
    }
}

fn layout(app: &App) -> Layout {
    let card = RECT {
        left: 0,
        top: 0,
        right: WIDTH,
        bottom: if app.compact_mode {
            compact_height(app)
        } else {
            app.full_height
        },
    };
    let card = RECT {
        right: if app.compact_mode {
            COMPACT_WIDTH
        } else {
            WIDTH
        },
        ..card
    };
    let add = box_rect(WIDTH - 180, TOOLBAR_TOP, 148, TOOLBAR_HEIGHT);
    let shrink = box_rect(WIDTH - 528, TOOLBAR_TOP, 104, TOOLBAR_HEIGHT);
    let refresh_all = box_rect(WIDTH - 412, TOOLBAR_TOP, 104, TOOLBAR_HEIGHT);
    let privacy = box_rect(WIDTH - 296, TOOLBAR_TOP, 104, TOOLBAR_HEIGHT);
    let pin = box_rect(WIDTH - 224, 10, 44, 36);
    let settings = box_rect(WIDTH - 168, 10, 44, 36);
    let min = box_rect(WIDTH - 112, 10, 44, 36);
    let close = box_rect(WIDTH - 56, 10, 44, 36);
    let header = box_rect(28, TABLE_TOP, WIDTH - 56, HEADER_ROW);

    let mut rows = Vec::new();
    let mut switch = Vec::new();
    let mut top = header.bottom - app.scroll;
    for _ in &app.rows {
        let rect = box_rect(header.left, top, header.right - header.left, DATA_ROW);
        let cells = columns(&rect);
        let center = (rect.top + rect.bottom) / 2;
        switch.push(centered(
            (cells[5].left + cells[5].right) / 2,
            center,
            72,
            36,
        ));
        rows.push(rect);
        top += DATA_ROW;
    }

    let menu = if app.add_menu && app.compact_mode {
        Some(box_rect(
            14,
            compact_height(app) - COMPACT_FOOTER - 104,
            COMPACT_WIDTH - 28,
            98,
        ))
    } else if app.add_menu {
        Some(RECT {
            left: add.right - 240,
            top: add.bottom + 6,
            right: add.right,
            bottom: add.bottom + 98,
        })
    } else {
        None
    };
    let menu_rect = menu.unwrap_or(RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    });
    let menu_register = RECT {
        left: menu_rect.left,
        top: menu_rect.top + 2,
        right: menu_rect.right,
        bottom: menu_rect.top + 46,
    };
    let menu_login = RECT {
        left: menu_rect.left,
        top: menu_rect.top + 46,
        right: menu_rect.right,
        bottom: menu_rect.top + 90,
    };

    let last_column = columns(&header)[5];
    let locate = box_rect(
        last_column.left + 5,
        header.top + 6,
        last_column.right - last_column.left - 10,
        32,
    );

    let context_menu = if app.context_row.is_some() {
        let (click_x, click_y) = app.context_at;
        let width = 150;
        let height = 136;
        let mut left = click_x;
        let mut top = click_y;
        if left + width > card.right - 10 {
            left = card.right - 10 - width;
        }
        if top + height > card.bottom - 10 {
            top = card.bottom - 10 - height;
        }
        left = left.max(card.left + 10);
        top = top.max(card.top + 10);
        Some(RECT {
            left,
            top,
            right: left + width,
            bottom: top + height,
        })
    } else {
        None
    };
    let context_rect = context_menu.unwrap_or(RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    });
    let context_refresh = RECT {
        left: context_rect.left,
        top: context_rect.top + 2,
        right: context_rect.right,
        bottom: context_rect.top + 46,
    };
    let context_switch = RECT {
        left: context_rect.left,
        top: context_rect.top + 46,
        right: context_rect.right,
        bottom: context_rect.top + 90,
    };

    let context_delete = box_rect(
        context_rect.left,
        context_rect.top + 90,
        context_rect.right - context_rect.left,
        44,
    );

    let overlay_width = if app.compact_mode {
        COMPACT_WIDTH
    } else {
        WIDTH
    };
    let overlay_height = if app.compact_mode {
        compact_height(app)
    } else {
        app.full_height
    };
    let dialog = centered(
        overlay_width / 2,
        overlay_height / 2,
        if app.compact_mode {
            COMPACT_WIDTH - 28
        } else {
            580
        },
        if app.compact_mode { 270 } else { 240 },
    );
    let dialog_center_x = (dialog.left + dialog.right) / 2;
    let dialog_cancel = centered(dialog_center_x - 70, dialog.bottom - 44, 120, 38);
    let dialog_ok = centered(dialog_center_x + 70, dialog.bottom - 44, 120, 38);

    Layout {
        refresh_all,
        shrink,
        privacy,
        settings,
        add,
        pin,
        min,
        close,
        header,
        rows,
        switch,
        locate,
        context_menu,
        context_refresh,
        context_switch,
        context_delete,
        menu,
        menu_register,
        menu_login,
        dialog,
        dialog_cancel,
        dialog_ok,
    }
}

fn clickable(app: &App, x: i32, y: i32) -> bool {
    let l = layout(app);
    let hit = |r: &RECT| point_in(r, x, y);
    if app.updates.blocked() {
        return true;
    }
    if app.settings.is_some() {
        return true;
    }
    if app.confirm.is_some() || app.login_pending {
        return hit(&l.dialog_cancel) || hit(&l.dialog_ok);
    }
    if app.add_menu {
        return hit(&l.menu_register) || hit(&l.menu_login);
    }
    if app.context_row.is_some() {
        return hit(&l.context_refresh) || hit(&l.context_switch) || hit(&l.context_delete);
    }
    if app.compact_mode {
        return compact_hit_at(app, x, y).is_some_and(|hit| !matches!(hit, CompactHit::Card(_)));
    }
    [
        l.add,
        l.shrink,
        l.refresh_all,
        l.privacy,
        l.settings,
        l.pin,
        l.min,
        l.close,
        l.locate,
    ]
    .iter()
    .any(hit)
        || (y >= l.header.bottom
            && y < app.full_height - FOOTER
            && l.switch
                .iter()
                .enumerate()
                .any(|(i, r)| hit(r) && !app.rows[i].is_active))
}

fn point_in(rect: &RECT, x: i32, y: i32) -> bool {
    x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom
}

fn centered(cx: i32, cy: i32, width: i32, height: i32) -> RECT {
    RECT {
        left: cx - width / 2,
        top: cy - height / 2,
        right: cx + width / 2,
        bottom: cy + height / 2,
    }
}

fn box_rect(left: i32, top: i32, width: i32, height: i32) -> RECT {
    RECT {
        left,
        top,
        right: left + width,
        bottom: top + height,
    }
}

fn columns(area: &RECT) -> Vec<RECT> {
    let widths = [
        (area.right - area.left - 744).max(180),
        180,
        180,
        92,
        130,
        162,
    ];
    let mut x = area.left;
    widths
        .into_iter()
        .map(|width| {
            let result = box_rect(x, area.top, width, area.bottom - area.top);
            x += width;
            result
        })
        .collect()
}

fn paint_window(app: &mut App) {
    let result = (|| -> Result<()> {
        let mut client: RECT = unsafe { std::mem::zeroed() };
        unsafe {
            GetClientRect(app.hwnd, &mut client);
        }
        let width = (client.right - client.left).max(1) as u32;
        let height = (client.bottom - client.top).max(1) as u32;
        let mut graphics = match app.graphics.take() {
            Some(value) => value,
            None => Graphics::new()?,
        };
        let painted = graphics.render_window(app.hwnd, width, height, app.scale, |canvas| {
            paint(
                app,
                canvas,
                (width as f32 / app.scale).round() as i32,
                (height as f32 / app.scale).round() as i32,
            );
        });
        app.graphics = Some(graphics);
        if !painted? {
            unsafe {
                invalidate_rect(app.hwnd, std::ptr::null(), 0);
            }
        }
        settings_ui::refresh_controls(app);
        Ok(())
    })();
    if let Err(error) = result {
        let message = format!("原生图形绘制失败：{error:#}");
        if app.render_failure.as_deref() != Some(&message) {
            app.render_failure = Some(message.clone());
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
                    app.hwnd,
                    to_wide(&message).as_ptr(),
                    to_wide("Codex切号器").as_ptr(),
                    0x10,
                );
            }
        }
    } else {
        app.render_failure = None;
    }
}

fn paint(app: &App, hdc: Canvas<'_>, width: i32, height: i32) {
    if app.compact_mode {
        paint_compact(app, hdc, width, height);
    } else {
        paint_full(app, hdc, width, height);
    }
    paint_overlays(app, hdc, width, height);
    settings_ui::paint(app, hdc);
    updates_ui::paint(app, hdc);
}

struct CompactLayout {
    pin: RECT,
    close: RECT,
    refresh_all: RECT,
    expand: RECT,
    add: RECT,
    privacy: RECT,
    cards: Vec<RECT>,
}

fn compact_layout(app: &App) -> CompactLayout {
    let bottom = compact_height(app) - COMPACT_FOOTER;
    let mut y = COMPACT_HEADER - app.compact_scroll;
    let cards = app
        .rows
        .iter()
        .map(|row| {
            if row.is_active {
                return box_rect(0, 0, 0, 0);
            }
            let rect = box_rect(14, y, COMPACT_WIDTH - 28, COMPACT_CARD);
            y += COMPACT_CARD;
            rect
        })
        .collect();
    CompactLayout {
        pin: box_rect(COMPACT_WIDTH - 78, 20, 28, 28),
        close: box_rect(COMPACT_WIDTH - 42, 20, 28, 28),
        privacy: box_rect(COMPACT_WIDTH - 48, bottom + 10, 28, 34),
        refresh_all: box_rect(COMPACT_WIDTH - 88, bottom + 10, 28, 34),
        expand: box_rect(COMPACT_WIDTH - 114, 20, 28, 28),
        add: box_rect(20, bottom + 10, 132, 34),
        cards,
    }
}

fn compact_hit_at(app: &App, x: i32, y: i32) -> Option<CompactHit> {
    let layout = compact_layout(app);
    for (rect, hit) in [
        (&layout.close, CompactHit::Close),
        (&layout.pin, CompactHit::Dock),
        (&layout.privacy, CompactHit::Privacy),
        (&layout.refresh_all, CompactHit::Refresh),
        (&layout.expand, CompactHit::Expand),
        (&layout.add, CompactHit::Add),
    ] {
        if point_in(rect, x, y) {
            return Some(hit);
        }
    }
    compact_row_at(app, x, y).map(CompactHit::Card)
}

fn paint_compact(app: &App, hdc: Canvas<'_>, width: i32, height: i32) {
    visual::paint_compact_new(app, hdc, width, height);
}

fn compact_row_at(app: &App, x: i32, y: i32) -> Option<usize> {
    if y < COMPACT_HEADER || y >= compact_height(app) - COMPACT_FOOTER {
        return None;
    }
    let layout = compact_layout(app);
    layout
        .cards
        .iter()
        .position(|card| x >= card.left && x < card.right && y >= card.top && y < card.bottom)
}

fn compact_clicked(app: &mut App, x: i32, y: i32) {
    if scrollbar_pressed(app, x, y) {
        return;
    }
    let layout = compact_layout(app);
    let inside = |rect: &RECT| x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom;

    if inside(&layout.refresh_all) {
        if app.rows.is_empty() {
            app.notice = Some("还没有账号".to_string());
        } else {
            refresh_all_rows(app);
        }
        unsafe { invalidate_rect(app.hwnd, std::ptr::null(), 0) };
        return;
    }
    if inside(&layout.expand) {
        set_compact_mode(app, false);
        return;
    }
    if inside(&layout.pin) {
        toggle_dock(app);
        return;
    }
    if inside(&layout.privacy) {
        app.masked = !app.masked;
        unsafe { invalidate_rect(app.hwnd, std::ptr::null(), 0) };
        return;
    }
    if inside(&layout.add) {
        if app.busy.is_some() {
            return;
        }
        app.add_menu = true;
        unsafe {
            invalidate_rect(app.hwnd, std::ptr::null(), 0);
        }
        return;
    }
}

fn paint_full(app: &App, hdc: Canvas<'_>, width: i32, height: i32) {
    visual::paint_full_base(app, hdc, width, height);
}

fn paint_overlays(app: &App, hdc: Canvas<'_>, width: i32, height: i32) {
    let layout = layout(app);
    let Some(fonts) = &app.fonts else { return };
    unsafe {
        if let Some(menu) = layout.context_menu {
            hdc.shadow(&menu, 10.0);
            round(hdc, &menu, 10, CARD);
            round_border(hdc, &menu, 10, LINE);
            menu_item(
                hdc,
                fonts,
                &layout.context_refresh,
                "刷新",
                TEXT,
                app.hover_item == Some(HoverItem::ContextRefresh),
            );
            menu_item(
                hdc,
                fonts,
                &layout.context_switch,
                "切换",
                TEXT,
                app.hover_item == Some(HoverItem::ContextSwitch),
            );
            menu_item(
                hdc,
                fonts,
                &layout.context_delete,
                "删除",
                rgb(190, 63, 54),
                app.hover_item == Some(HoverItem::ContextDelete),
            );
        }

        if let Some(menu) = layout.menu {
            hdc.shadow(&menu, 10.0);
            round(hdc, &menu, 10, CARD);
            round_border(hdc, &menu, 10, LINE);
            menu_item(
                hdc,
                fonts,
                &layout.menu_register,
                "登记当前Codex账号",
                TEXT,
                app.hover_item == Some(HoverItem::AddRegister),
            );
            menu_item(
                hdc,
                fonts,
                &layout.menu_login,
                "浏览器登录新账号",
                ACCENT,
                app.hover_item == Some(HoverItem::AddLogin),
            );
        }

        if app.login_pending {
            dim_overlay(hdc, width, height);
            hdc.shadow(&layout.dialog, 14.0);
            round(hdc, &layout.dialog, 14, CARD);
            round_border(hdc, &layout.dialog, 14, LINE);
            let title = RECT {
                left: layout.dialog.left + 28,
                top: layout.dialog.top + 24,
                right: layout.dialog.right - 28,
                bottom: layout.dialog.top + 58,
            };
            text_in(
                hdc,
                fonts.body,
                TEXT,
                &title,
                "等待浏览器登录…",
                DT_LEFT | DT_VCENTER | DT_SINGLELINE,
            );
            let hint = RECT {
                left: layout.dialog.left + 28,
                top: layout.dialog.top + 62,
                right: layout.dialog.right - 28,
                bottom: layout.dialog.top + 142,
            };
            text_in(
                hdc,
                fonts.small,
                MUTED,
                &hint,
                "已在浏览器打开登录页，完成授权后会自动导入（不会影响当前账号）。",
                DT_LEFT | windows_sys::Win32::Graphics::Gdi::DT_WORDBREAK | DT_NOPREFIX,
            );
            round(hdc, &layout.dialog_cancel, 8, TRACK);
            text_in(
                hdc,
                fonts.body,
                TEXT,
                &layout.dialog_cancel,
                "取消",
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
        }

        if let Some((kind, id)) = &app.confirm {
            dim_overlay(hdc, width, height);
            let target = app.rows.iter().find(|row| &row.id == id);
            let label = target
                .map(|row| visual::shown(app, row))
                .unwrap_or_default();
            let (title_text, hint_text, ok_text) = match kind {
                Confirm::Switch => (
                    format!("切换到 {label}？"),
                    "在下一次对话时才会使用此账号的额度。未接通时会先自动重新连接Codex",
                    "确认切换",
                ),
                Confirm::Delete => (
                    format!("删除 {label}？"),
                    "会同时删除这个账号保存的凭证快照，无法撤销。",
                    "确认删除",
                ),
            };
            hdc.shadow(&layout.dialog, 14.0);
            round(hdc, &layout.dialog, 14, CARD);
            round_border(hdc, &layout.dialog, 14, LINE);
            let title = RECT {
                left: layout.dialog.left + 28,
                top: layout.dialog.top + 24,
                right: layout.dialog.right - 28,
                bottom: layout.dialog.top + 58,
            };
            text_in(
                hdc,
                fonts.body,
                TEXT,
                &title,
                &title_text,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
            );
            let hint = RECT {
                left: layout.dialog.left + 28,
                top: layout.dialog.top + 62,
                right: layout.dialog.right - 28,
                bottom: layout.dialog.top + 142,
            };
            text_in(
                hdc,
                fonts.small,
                MUTED,
                &hint,
                hint_text,
                DT_LEFT | windows_sys::Win32::Graphics::Gdi::DT_WORDBREAK | DT_NOPREFIX,
            );
            round(hdc, &layout.dialog_cancel, 8, TRACK);
            text_in(
                hdc,
                fonts.body,
                TEXT,
                &layout.dialog_cancel,
                "取消",
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
            hdc.gradient_round(&layout.dialog_ok, 8.0, rgb(57, 133, 108), rgb(40, 105, 84));
            text_in(
                hdc,
                fonts.body,
                rgb(255, 255, 255),
                &layout.dialog_ok,
                ok_text,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
        }
    }
}

fn bar_color(percent: u8) -> u32 {
    match percent {
        0..=4 => QUOTA_RED,
        5..=19 => QUOTA_ORANGE,
        _ => GREEN,
    }
}

fn quota_color(row: &RowView, value: Option<u8>) -> u32 {
    if row.loading || row.error.is_some() {
        MUTED
    } else {
        value.map(bar_color).unwrap_or(MUTED)
    }
}

fn start_login(app: &mut App) {
    if app.login_pending {
        return;
    }
    app.login_pending = true;
    app.login_cancel.store(false, Ordering::Relaxed);
    app.notice = Some("已打开浏览器登录页，请完成授权…".to_string());
    let cancel = Arc::clone(&app.login_cancel);
    let sender = app.login_tx.clone();
    std::thread::spawn(move || {
        let result = crate::login::login_new_account(&cancel)
            .map(|account| account.label())
            .map_err(|error| format!("{error:#}"));
        let _ = sender.send(result);
    });
    unsafe { invalidate_rect(app.hwnd, std::ptr::null(), 0) };
}

fn looks_like_auth_failure(text: &str) -> bool {
    let lowered = text.to_lowercase();
    [
        "401",
        "unauthorized",
        "not logged in",
        "login required",
        "authentication",
        "invalid_grant",
        "未登录",
        "登录失效",
        "凭证失效",
        "refresh token",
    ]
    .iter()
    .any(|needle| lowered.contains(&needle.to_lowercase()))
}

fn status_of(row: &RowView) -> (&'static str, u32, u32) {
    if row.loading {
        return ("读取中", MUTED, TRACK);
    }
    if row.error.is_some() {
        return if row.expired {
            ("需重新登录", AMBER, rgb(250, 240, 221))
        } else {
            ("读取失败", AMBER, rgb(250, 240, 221))
        };
    }
    if row.is_active {
        return ("当前使用", ACCENT, SAGE);
    }
    if let Some(percent) = [row.five_hour, row.weekly].into_iter().flatten().min() {
        if percent < 20 {
            return ("额度较低", bar_color(percent), rgb(250, 240, 221));
        }
    }
    ("可用", ACCENT, SAGE)
}

fn format_day(epoch: i64) -> String {
    match win::local_datetime(epoch) {
        Some((_, month, day, _, _)) => format!("{month}月{day}日"),
        None => "—".to_string(),
    }
}

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs() as i64)
        .unwrap_or(0)
}

fn remaining_days(until: &str) -> Option<i64> {
    let (year, month, day) = parse_date(until)?;
    let (today_year, today_month, today_day, _, _) = win::local_datetime(now_epoch())?;
    let from = days_from_civil(today_year as i64, today_month as u32, today_day as u32);
    let to = days_from_civil(year, month, day);
    Some(to - from)
}

fn parse_date(text: &str) -> Option<(i64, u32, u32)> {
    let mut parts = text.split('-');
    let year = parts.next()?.parse::<i64>().ok()?;
    let month = parts.next()?.parse::<u32>().ok()?;
    let day = parts.next()?.parse::<u32>().ok()?;
    Some((year, month, day))
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_index = ((month + 9) % 12) as i64;
    let day_of_year = (153 * month_index + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn plan_name(plan: &str) -> String {
    match plan {
        "pro" => "PRO".to_string(),
        "plus" => "PLUS".to_string(),
        "team" => "TEAM".to_string(),
        "free" => "FREE".to_string(),
        other => other.to_string(),
    }
}

fn plan_colors(plan: &str) -> (u32, u32) {
    match plan {
        "pro" => (CARD, rgb(77, 85, 80)),
        "plus" => (ACCENT, rgb(223, 239, 228)),
        "team" | "business" => (ACCENT, rgb(232, 237, 226)),
        _ => (MUTED, TRACK),
    }
}

fn create_fonts(_hwnd: HWND) -> Fonts {
    Fonts {
        caption: Font::Caption,
        compact_number: Font::CompactNumber,
        title: Font::Title,
        body: Font::Body,
        small: Font::Small,
        header: Font::Header,
    }
}

#[cfg(test)]
#[path = "ui_tests.rs"]
mod tests;

unsafe fn dim_overlay(canvas: Canvas<'_>, width: i32, height: i32) {
    canvas.fill(&box_rect(0, 0, width, height), rgb(25, 32, 27), 0.36);
}

fn menu_item(hdc: Canvas<'_>, fonts: &Fonts, rect: &RECT, label: &str, color: u32, hovered: bool) {
    if hovered {
        let inner = RECT {
            left: rect.left + 6,
            top: rect.top + 2,
            right: rect.right - 6,
            bottom: rect.bottom - 2,
        };
        unsafe { round(hdc, &inner, 7, HOVER) };
    }
    let text_rect = RECT {
        left: rect.left + 16,
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
    };
    unsafe {
        text_in(
            hdc,
            fonts.body,
            color,
            &text_rect,
            label,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE,
        );
    }
}

unsafe fn fill(canvas: Canvas<'_>, left: i32, top: i32, right: i32, bottom: i32, color: u32) {
    canvas.fill(
        &RECT {
            left,
            top,
            right,
            bottom,
        },
        color,
        1.0,
    );
}
unsafe fn round(canvas: Canvas<'_>, rect: &RECT, radius: i32, color: u32) {
    canvas.round(rect, radius as f32, color, false);
}
unsafe fn round_border(canvas: Canvas<'_>, rect: &RECT, radius: i32, color: u32) {
    canvas.round(rect, radius as f32, color, true);
}
unsafe fn text_in(
    canvas: Canvas<'_>,
    font: Font,
    color: u32,
    rect: &RECT,
    value: &str,
    flags: u32,
) {
    canvas.text(font, color, rect, value, flags);
}

fn to_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn spawn_worker(
    concurrency: u32,
) -> (
    Sender<Request>,
    Receiver<Message>,
    usage_workers::Controller,
) {
    // Resolve once in the pool, not on the UI thread; each child gets its own HOME
    // through Command::env, so concurrent reads never switch the current account.
    let executable = std::sync::OnceLock::new();
    usage_workers::spawn(
        concurrency,
        move |request| match executable.get_or_init(crate::codex::locate) {
            Some(path) => usage::read(path, &request.home).map_err(|error| format!("{error:#}")),
            None => Err("没有找到 codex.exe，无法查询额度".to_string()),
        },
        || {
            let hwnd = find_window();
            if !hwnd.is_null() {
                unsafe {
                    PostMessageW(hwnd, WM_DATA, 0, 0);
                }
            }
        },
    )
}

fn find_window() -> HWND {
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::FindWindowW(
            to_wide("CodexSwitchWindow").as_ptr(),
            std::ptr::null(),
        )
    }
}

fn add_tray(hwnd: HWND, icon: HICON) {
    use windows_sys::Win32::UI::Shell::{
        Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NOTIFYICONDATAW,
    };
    let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = hwnd;
    data.uID = TRAY_ID;
    data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    data.uCallbackMessage = WM_TRAY;
    data.hIcon = icon;
    for (index, code) in to_wide("Codex切号器").iter().take(127).enumerate() {
        data.szTip[index] = *code;
    }
    unsafe { Shell_NotifyIconW(NIM_ADD, &data) };
}

fn remove_tray(hwnd: HWND) {
    use windows_sys::Win32::UI::Shell::{Shell_NotifyIconW, NIM_DELETE, NOTIFYICONDATAW};
    let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = hwnd;
    data.uID = TRAY_ID;
    unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
}

fn update_compact_height_limit(app: &mut App, work: &RECT) {
    let available = ((work.bottom - work.top - 16) as f32 / app.scale).floor() as i32;
    app.compact_height_limit =
        available.clamp(COMPACT_HEADER + COMPACT_FOOTER + 52, COMPACT_MAX_HEIGHT);
    app.compact_scroll = app.compact_scroll.clamp(0, compact_max_scroll(app));
}

fn request_close(app: &mut App) {
    match app.close_action {
        CloseAction::Tray => hide_to_tray(app),
        CloseAction::Exit => {
            if !app.hwnd.is_null() {
                unsafe {
                    DestroyWindow(app.hwnd);
                }
            }
        }
    }
}

fn hide_to_tray(app: &mut App) {
    settings_ui::close(app);
    dismiss_menus(app);
    app.tray_hidden = true;
    app.visible = false;
    app.anim = None;
    app.scroll_drag = None;
    app.dock_wait_for_pointer = false;
    app.compact_hover = None;
    app.hover_row = None;
    if let Some(graphics) = app.graphics.as_mut() {
        graphics.release_window();
    }
    if !app.hwnd.is_null() {
        unsafe {
            windows_sys::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture();
            ShowWindow(app.hwnd, SW_HIDE);
            set_taskbar_entry(app, false);
        }
    }
}

fn restore_from_tray(app: &mut App) {
    app.tray_hidden = false;
    app.anim = None;
    app.visible = true;
    if app.docked {
        app.dock_wait_for_pointer = true;
        app.visible = false;
        if !app.hwnd.is_null()
            && unsafe { windows_sys::Win32::UI::WindowsAndMessaging::IsIconic(app.hwnd) != 0 }
        {
            unsafe {
                ShowWindow(
                    app.hwnd,
                    windows_sys::Win32::UI::WindowsAndMessaging::SW_RESTORE,
                );
            }
        }
        slide_down(app);
    } else if !app.hwnd.is_null() {
        let saved_height = app.full_height;
        set_taskbar_entry(app, true);
        app.full_height = saved_height;
        unsafe {
            ShowWindow(
                app.hwnd,
                windows_sys::Win32::UI::WindowsAndMessaging::SW_RESTORE,
            );
        }
    }
    if !app.hwnd.is_null() {
        unsafe {
            SetForegroundWindow(app.hwnd);
            invalidate_rect(app.hwnd, std::ptr::null(), 0);
        }
    }
}

fn toggle_visibility(app: &mut App) {
    let minimized = !app.hwnd.is_null()
        && unsafe { windows_sys::Win32::UI::WindowsAndMessaging::IsIconic(app.hwnd) != 0 };
    if app.tray_hidden || !app.visible || minimized {
        restore_from_tray(app);
    } else {
        hide_to_tray(app);
    }
}

fn error_notice(context: &str, error: &anyhow::Error) -> String {
    format!("{context}：{error:#}")
}

fn start_connection(app: &mut App, quiet: bool) {
    if app.busy.is_some() {
        return;
    }
    app.busy = Some("连接无感后台".into());
    app.connecting = true;
    app.connection_check = None;
    let sender = app.switch_tx.clone();
    std::thread::spawn(move || {
        let result = (|| -> anyhow::Result<SwitchNotice> {
            let store = Store::discover()?;
            if quiet {
                if !crate::connection::check_ready(&store)? {
                    anyhow::bail!("无感连接暂未就绪；当前 Codex 已保留，切号时会重新检查");
                }
                return Ok(SwitchNotice {
                    message: String::new(),
                    finished: true,
                });
            }
            let changed = std::cell::Cell::new(false);
            crate::connection::ensure(&store, |message| {
                changed.set(true);
                let _ = sender.send(Ok(SwitchNotice {
                    message: message.into(),
                    finished: false,
                }));
            })?;
            Ok(SwitchNotice {
                message: if quiet && !changed.get() {
                    String::new()
                } else {
                    "无感连接已就绪，选择账号即可换号".into()
                },
                finished: true,
            })
        })()
        .map_err(|e| format!("{e:#}"));
        let _ = sender.send(result);
    });
}

fn tray_menu(app: &mut App) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, SetForegroundWindow,
        TrackPopupMenu, MF_STRING, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_NONOTIFY, TPM_RETURNCMD,
    };

    let mut point: windows_sys::Win32::Foundation::POINT = unsafe { std::mem::zeroed() };
    unsafe { GetCursorPos(&mut point) };

    let quit = to_wide("退出");

    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        AppendMenuW(menu, MF_STRING, 1, quit.as_ptr());
        SetForegroundWindow(app.hwnd);
        let command = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_LEFTALIGN | TPM_BOTTOMALIGN,
            point.x,
            point.y,
            0,
            app.hwnd,
            std::ptr::null(),
        );
        DestroyMenu(menu);
        if command == 1 {
            DestroyWindow(app.hwnd);
        }
    }
}

fn load_rows() -> (Vec<RowView>, HashMap<String, PathBuf>, Option<String>) {
    let mut homes = HashMap::new();
    let store = match Store::discover() {
        Ok(store) => store,
        Err(error) => {
            return (
                Vec::new(),
                homes,
                Some(format!("读取数据目录失败：{error:#}")),
            )
        }
    };
    let index = match store.load() {
        Ok(index) => index,
        Err(error) => {
            return (
                Vec::new(),
                homes,
                Some(format!("读取账号列表失败：{error:#}")),
            )
        }
    };

    let rows = index
        .accounts
        .iter()
        .map(|account| {
            let profile = store.profile_auth(&account.id);
            if let Some(home) = profile.parent() {
                homes.insert(account.id.clone(), home.to_path_buf());
            }
            let parsed = std::fs::read(&profile)
                .ok()
                .and_then(|bytes| identity::parse_auth_json(&bytes).ok());
            RowView {
                id: account.id.clone(),
                label: account.label(),
                plan: parsed.as_ref().and_then(|value| value.plan.clone()),
                active_until: parsed.and_then(|value| value.active_until),
                is_active: index.active.as_deref() == Some(account.id.as_str()),
                five_hour: None,
                five_hour_reset: None,
                weekly: None,
                weekly_reset: None,
                reset_credits: None,
                error: None,
                expired: false,
                loading: false,
            }
        })
        .collect();

    (rows, homes, None)
}
