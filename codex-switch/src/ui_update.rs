//! Native update overlay. Network work always stays off the UI thread.
use super::*;
use crate::update::{self, Status};

pub(super) struct Dialog {
    pub version: String,
    pub ignore: bool,
    pub error: Option<String>,
    focus: usize,
}
pub(super) struct State {
    pub checker: update::Checker,
    pub dialog: Option<Dialog>,
    download: Option<Receiver<std::result::Result<(), String>>>,
    pub exit_ready: bool,
}
impl Default for State {
    fn default() -> Self {
        Self {
            checker: update::Checker::default(),
            dialog: None,
            download: None,
            exit_ready: false,
        }
    }
}
impl State {
    pub fn load() -> Self {
        Self {
            checker: update::Checker::load(),
            ..Self::default()
        }
    }
    pub fn blocked(&self) -> bool {
        self.dialog.is_some()
    }
}

pub(super) fn tick(app: &mut App) -> bool {
    let mut changed = app.updates.checker.poll(Instant::now());
    if app.updates.checker.auto_due(Instant::now()) {
        changed |= app.updates.checker.begin(false);
    }
    if let Some(rx) = &app.updates.download {
        if let Ok(result) = rx.try_recv() {
            app.updates.download = None;
            changed = true;
            match result {
                Ok(()) => {
                    app.updates.exit_ready = true;
                    if !app.hwnd.is_null() {
                        unsafe {
                            DestroyWindow(app.hwnd);
                        }
                    }
                    return true;
                }
                Err(error) => {
                    if let Some(dialog) = &mut app.updates.dialog {
                        dialog.error = Some(error);
                    }
                }
            }
        }
    }
    let available = app.visible
        && !app.tray_hidden
        && app.anim.is_none()
        && app.busy.is_none()
        && !app.login_pending
        && app.confirm.is_none()
        && app.updates.dialog.is_none();
    let manual = app
        .updates
        .checker
        .pending
        .as_ref()
        .is_some_and(|(_, manual)| *manual);
    if available && (app.settings.is_none() || manual) {
        if let Some((version, _)) = app.updates.checker.pending.take() {
            app.add_menu = false;
            app.context_row = None;
            app.updates.dialog = Some(Dialog {
                version,
                ignore: false,
                error: None,
                focus: 1,
            });
            settings_ui::set_edits_visible(app, false);
            changed = true;
        }
    }
    changed
}
pub(super) fn manual(app: &mut App) {
    // An automatic request in progress is joined; never start a duplicate request.
    if let Some(job) = app.updates.checker.job.as_mut() {
        job.manual = true;
        app.updates.checker.status = Status::Checking;
    } else {
        app.updates.checker.begin(true);
    }
    unsafe {
        invalidate_rect(app.hwnd, std::ptr::null(), 0);
    }
}
pub(super) fn rects(app: &App) -> [RECT; 4] {
    let width = if app.compact_mode {
        COMPACT_WIDTH
    } else {
        WIDTH
    };
    let height = if app.compact_mode {
        compact_height(app)
    } else {
        app.full_height
    };
    let dialog = centered(
        width / 2,
        height / 2,
        if app.compact_mode { 326 } else { 520 },
        236,
    );
    [
        dialog,
        box_rect(
            dialog.left + 20,
            dialog.bottom - 54,
            if app.compact_mode { 140 } else { 245 },
            34,
        ),
        box_rect(dialog.right - 160, dialog.bottom - 54, 64, 34),
        box_rect(dialog.right - 86, dialog.bottom - 54, 64, 34),
    ]
}
fn dismiss(app: &mut App) {
    let Some(dialog) = &app.updates.dialog else {
        return;
    };
    if dialog.ignore {
        if let Err(error) = app.updates.checker.ignore(&dialog.version) {
            app.updates.dialog.as_mut().unwrap().error = Some(format!("保存失败：{error:#}"));
            return;
        }
    }
    app.updates.dialog = None;
    settings_ui::set_edits_visible(app, true);
}
fn begin_download(app: &mut App) {
    if app.updates.download.is_some() {
        return;
    }
    let Some(dialog) = &mut app.updates.dialog else {
        return;
    };
    if dialog.ignore {
        if let Err(error) = app.updates.checker.ignore(&dialog.version) {
            dialog.error = Some(format!("保存失败：{error:#}"));
            return;
        }
    }
    dialog.error = None;
    let version = dialog.version.clone();
    let (tx, rx) = mpsc::channel();
    app.updates.download = Some(rx);
    std::thread::spawn(move || {
        let result = update::stage(&version)
            .and_then(update::launch_helper)
            .map_err(|e| format!("{e:#}"));
        let _ = tx.send(result);
    });
}
pub(super) fn clicked(app: &mut App, x: i32, y: i32) {
    if app.updates.download.is_some() {
        return;
    }
    let [_, check, cancel, ok] = rects(app);
    if point_in(&check, x, y) {
        if let Some(dialog) = &mut app.updates.dialog {
            dialog.ignore = !dialog.ignore;
            dialog.focus = 0;
        }
    } else if point_in(&cancel, x, y) {
        dismiss(app);
    } else if point_in(&ok, x, y) {
        begin_download(app);
    }
    unsafe {
        invalidate_rect(app.hwnd, std::ptr::null(), 0);
    }
}
pub(super) fn keyboard(app: &mut App, key: u32) -> bool {
    if app.updates.dialog.is_none() {
        return false;
    }
    if app.updates.download.is_some() {
        return true;
    }
    match key {
        27 => dismiss(app),
        9 => {
            let backwards =
                unsafe { windows_sys::Win32::UI::Input::KeyboardAndMouse::GetKeyState(0x10) } < 0;
            let d = app.updates.dialog.as_mut().unwrap();
            d.focus = (d.focus + if backwards { 2 } else { 1 }) % 3;
        }
        13 | 32 => match app.updates.dialog.as_ref().unwrap().focus {
            0 => {
                let d = app.updates.dialog.as_mut().unwrap();
                d.ignore = !d.ignore;
            }
            1 => dismiss(app),
            _ => begin_download(app),
        },
        _ => (),
    }
    unsafe {
        invalidate_rect(app.hwnd, std::ptr::null(), 0);
    }
    true
}
pub(super) fn paint(app: &App, canvas: Canvas<'_>) {
    let Some(dialog) = &app.updates.dialog else {
        return;
    };
    let Some(fonts) = &app.fonts else { return };
    let [area, check, cancel, ok] = rects(app);
    let width = if app.compact_mode {
        COMPACT_WIDTH
    } else {
        WIDTH
    };
    let height = if app.compact_mode {
        compact_height(app)
    } else {
        app.full_height
    };
    unsafe {
        dim_overlay(canvas, width, height);
    }
    canvas.shadow(&area, 14.0);
    canvas.round(&area, 14.0, CARD, false);
    canvas.round(&area, 14.0, LINE, true);
    visual::label(
        canvas,
        fonts.title,
        TEXT,
        box_rect(
            area.left + 24,
            area.top + 20,
            area.right - area.left - 48,
            36,
        ),
        "是否更新版本？",
    );
    visual::label(
        canvas,
        fonts.small,
        MUTED,
        box_rect(
            area.left + 24,
            area.top + 63,
            area.right - area.left - 48,
            24,
        ),
        &format!("v{} → v{}", env!("CARGO_PKG_VERSION"), dialog.version),
    );
    let downloading = app.updates.download.is_some();
    let message = if downloading {
        "正在下载，完成后自动重启切号器"
    } else {
        dialog.error.as_deref().unwrap_or("")
    };
    canvas.text(
        fonts.caption,
        if dialog.error.is_some() {
            QUOTA_RED
        } else {
            MUTED
        },
        &box_rect(
            area.left + 24,
            area.top + 93,
            area.right - area.left - 48,
            32,
        ),
        message,
        DT_LEFT | DT_VCENTER | DT_END_ELLIPSIS | DT_SINGLELINE,
    );
    let square = box_rect(check.left + 2, check.top + 6, 16, 16);
    canvas.round(
        &square,
        3.0,
        if dialog.ignore { ACCENT } else { LINE },
        dialog.ignore == false,
    );
    if dialog.ignore {
        visual::icon(canvas, &square, "check", CARD);
    }
    visual::label(
        canvas,
        fonts.small,
        if downloading { MUTED } else { TEXT },
        box_rect(
            check.left + 27,
            check.top,
            check.right - check.left - 27,
            28,
        ),
        "此版本不再提示",
    );
    if dialog.focus == 0 {
        canvas.round(&check, 5.0, LINE, true);
    }
    visual::button(canvas, fonts, &cancel, "取消", false);
    visual::button(
        canvas,
        fonts,
        &ok,
        if downloading { "更新中" } else { "更新" },
        true,
    );
}
pub(super) fn paint_check_button(app: &App, canvas: Canvas<'_>, area: &RECT) {
    let Some(fonts) = &app.fonts else { return };
    let (text, color) = match app.updates.checker.status {
        Status::Idle => ("检查新版本", TEXT),
        Status::Checking => ("正在检查...", MUTED),
        Status::Latest => ("已是最新版", GREEN),
        Status::Failed => ("请求失败", QUOTA_RED),
    };
    canvas.round(area, 8.0, LINE, true);
    canvas.text(
        fonts.small,
        color,
        area,
        text,
        DT_CENTER | DT_VCENTER | DT_SINGLELINE,
    );
}
