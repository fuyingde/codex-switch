//! Small native settings form. Only explicit Save changes persisted preferences.
use super::*;
use windows_sys::Win32::Graphics::Gdi as gdi;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetFocus, GetKeyState, SetFocus};
use windows_sys::Win32::UI::WindowsAndMessaging as wm;

pub(super) struct SettingsDialog {
    pub active: String,
    pub all: String,
    pub concurrency: String,
    pub close_action: CloseAction,
    radio_focus: Option<usize>,
    pub error: Option<String>,
    edits: [HWND; 3],
    font: gdi::HFONT,
}

pub(super) fn form_rects(app: &App) -> [RECT; 9] {
    let dialog = centered(WIDTH / 2, app.full_height / 2, 600, 322);
    [
        dialog,
        box_rect(dialog.left + 126, dialog.top + 64, 66, 30),
        box_rect(dialog.left + 126, dialog.top + 112, 66, 30),
        box_rect(dialog.right - 190, dialog.bottom - 54, 76, 34),
        box_rect(dialog.right - 100, dialog.bottom - 54, 76, 34),
        box_rect(dialog.left + 126, dialog.top + 208, 144, 30),
        box_rect(dialog.left + 282, dialog.top + 208, 116, 30),
        box_rect(dialog.left + 202, dialog.top + 160, 66, 30),
        box_rect(dialog.right - 330, dialog.bottom - 54, 124, 34),
    ]
}

pub(super) fn validate(active: &str, all: &str) -> Result<(u32, u32)> {
    let active = active
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|v| (2..=120).contains(v))
        .ok_or_else(|| anyhow::anyhow!("当前账号刷新间隔应为 2–120 分钟"))?;
    let all = all
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|v| (20..=300).contains(v))
        .ok_or_else(|| anyhow::anyhow!("全部账号刷新间隔应为 20–300 分钟"))?;
    Ok((active, all))
}

pub(super) fn validate_concurrency(value: &str) -> Result<u32> {
    value
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|v| (1..=usage_workers::MAX_CONCURRENCY).contains(v))
        .ok_or_else(|| anyhow::anyhow!("同时刷新账号数应为 1–16 个"))
}

pub(super) fn open(app: &mut App) {
    if (app.busy.is_some() && !app.connecting) || app.login_pending {
        return;
    }
    app.add_menu = false;
    app.context_row = None;
    app.updates.checker.status = crate::update::Status::Idle;
    app.settings = Some(SettingsDialog {
        active: app.refresh_active_minutes.to_string(),
        all: app.refresh_all_minutes.to_string(),
        concurrency: app.usage_concurrency.to_string(),
        close_action: app.close_action,
        radio_focus: None,
        error: None,
        edits: [std::ptr::null_mut(); 3],
        font: std::ptr::null_mut(),
    });
    if !app.hwnd.is_null() {
        let mut form = app.settings.take().unwrap();
        unsafe {
            for i in 0..3 {
                form.edits[i] = wm::CreateWindowExW(
                    0,
                    to_wide("EDIT").as_ptr(),
                    to_wide([&form.active, &form.all, &form.concurrency][i]).as_ptr(),
                    wm::WS_CHILD
                        | wm::WS_VISIBLE
                        | wm::WS_TABSTOP
                        | wm::ES_NUMBER as u32
                        | wm::ES_CENTER as u32
                        | wm::ES_AUTOHSCROLL as u32,
                    0,
                    0,
                    1,
                    1,
                    app.hwnd,
                    (200 + i) as _,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                );
                wm::SendMessageW(
                    form.edits[i],
                    windows_sys::Win32::UI::Controls::EM_SETLIMITTEXT,
                    3,
                    0,
                );
            }
        }
        app.settings = Some(form);
        reposition(app);
        // Opening the form leaves all inputs unfocused until the user chooses one.
        unsafe {
            SetFocus(app.hwnd);
        }
    }
    unsafe {
        invalidate_rect(app.hwnd, std::ptr::null(), 0);
    }
}

pub(super) fn reposition(app: &mut App) {
    let Some(mut form) = app.settings.take() else {
        return;
    };
    if app.hwnd.is_null() {
        app.settings = Some(form);
        return;
    }
    let rects = form_rects(app);
    unsafe {
        let old = form.font;
        form.font = gdi::CreateFontW(
            -px(app, 16),
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            gdi::DEFAULT_CHARSET as u32,
            gdi::OUT_TT_PRECIS as u32,
            gdi::CLIP_DEFAULT_PRECIS as u32,
            gdi::CLEARTYPE_QUALITY as u32,
            gdi::DEFAULT_PITCH as u32,
            to_wide("Microsoft YaHei UI").as_ptr(),
        );
        for i in 0..3 {
            let rect = [rects[1], rects[2], rects[7]][i];
            wm::SetWindowPos(
                form.edits[i],
                std::ptr::null_mut(),
                px(app, rect.left + 3),
                px(app, rect.top + 5),
                px(app, rect.right - rect.left - 6),
                px(app, rect.bottom - rect.top - 10),
                wm::SWP_NOZORDER | wm::SWP_NOACTIVATE,
            );
            wm::SendMessageW(form.edits[i], wm::WM_SETFONT, form.font as usize, 1);
        }
        if !old.is_null() {
            gdi::DeleteObject(old as _);
        }
    }
    app.settings = Some(form);
    if app.updates.blocked() {
        set_edits_visible(app, false);
    }
}

pub(super) fn close(app: &mut App) {
    if let Some(form) = app.settings.take() {
        // Results belong to the settings session that requested them. Keep the
        // network request and 24-hour clock running, but detach its old feedback.
        app.updates.checker.status = crate::update::Status::Idle;
        if let Some(job) = app.updates.checker.job.as_mut() {
            job.manual = false;
        }
        if let Some((_, manual)) = app.updates.checker.pending.as_mut() {
            *manual = false;
        }
        unsafe {
            for edit in form.edits {
                if !edit.is_null() && wm::IsWindow(edit) != 0 {
                    wm::DestroyWindow(edit);
                }
            }
            if !form.font.is_null() {
                gdi::DeleteObject(form.font as _);
            }
        }
    }
    if !app.hwnd.is_null() {
        unsafe {
            invalidate_rect(app.hwnd, std::ptr::null(), 0);
        }
    }
}

pub(super) fn refresh_controls(app: &App) {
    if let Some(form) = &app.settings {
        unsafe {
            for edit in form.edits {
                if !edit.is_null() {
                    invalidate_rect(edit, std::ptr::null(), 0);
                    gdi::UpdateWindow(edit);
                }
            }
        }
    }
}

fn edit_text(edit: HWND, fallback: &str) -> String {
    if edit.is_null() {
        return fallback.into();
    }
    let mut buffer = [0u16; 32];
    let count = unsafe { wm::GetWindowTextW(edit, buffer.as_mut_ptr(), buffer.len() as i32) };
    String::from_utf16_lossy(&buffer[..count.max(0) as usize])
}

pub(super) fn save(app: &mut App) {
    let Some(form) = &app.settings else { return };
    let active = edit_text(form.edits[0], &form.active);
    let all = edit_text(form.edits[1], &form.all);
    let concurrency = edit_text(form.edits[2], &form.concurrency);
    let close_action = form.close_action;
    let result = (|| -> Result<(u32, u32, u32)> {
        let (active, all) = validate(&active, &all)?;
        let concurrency = validate_concurrency(&concurrency)?;
        let mut settings = app_settings(app);
        settings.active_minutes = active;
        settings.all_minutes = all;
        settings.usage_concurrency = concurrency;
        settings.close_action = close_action;
        save_ui_settings(settings)?;
        Ok((active, all, concurrency))
    })();
    match result {
        Ok((active, all, concurrency)) => {
            app.refresh_active_minutes = active;
            app.refresh_all_minutes = all;
            app.usage_concurrency = concurrency;
            if let Some(control) = app.usage_control.as_mut() {
                control.set_limit(concurrency);
            }
            app.close_action = close_action;
            app.last_active_refresh = Instant::now();
            app.last_all_refresh = Instant::now();
            close(app);
            app.notice = Some("设置已保存".into());
        }
        Err(error) => {
            if let Some(form) = app.settings.as_mut() {
                form.error = Some(error.to_string());
            }
        }
    }
    if !app.hwnd.is_null() {
        unsafe {
            invalidate_rect(app.hwnd, std::ptr::null(), 0);
        }
    }
}

pub(super) fn clicked(app: &mut App, x: i32, y: i32) {
    let rects = form_rects(app);
    if let Some(i) = [rects[5], rects[6]].iter().position(|r| point_in(r, x, y)) {
        if let Some(form) = app.settings.as_mut() {
            form.close_action = if i == 0 {
                CloseAction::Tray
            } else {
                CloseAction::Exit
            };
            form.radio_focus = Some(i);
            if !app.hwnd.is_null() {
                unsafe {
                    SetFocus(app.hwnd);
                }
            }
        }
        unsafe {
            invalidate_rect(app.hwnd, std::ptr::null(), 0);
        }
    } else if point_in(&rects[8], x, y) {
        updates_ui::manual(app);
    } else if point_in(&rects[3], x, y) {
        close(app);
    } else if point_in(&rects[4], x, y) {
        save(app);
    }
}

pub(super) fn set_edits_visible(app: &App, visible: bool) {
    if let Some(form) = &app.settings {
        for edit in form.edits {
            if !edit.is_null() {
                unsafe {
                    wm::ShowWindow(
                        edit,
                        if visible {
                            wm::SW_SHOWNOACTIVATE
                        } else {
                            wm::SW_HIDE
                        },
                    );
                }
            }
        }
    }
}

pub(super) fn keyboard(app: &mut App, key: u32) -> bool {
    if app.settings.is_none() {
        return false;
    }
    match key {
        13 => {
            save(app);
            true
        }
        27 => {
            close(app);
            true
        }
        9 => {
            if let Some(form) = app.settings.as_mut() {
                unsafe {
                    let focus = GetFocus();
                    let current = form
                        .edits
                        .iter()
                        .position(|edit| !edit.is_null() && *edit == focus)
                        .or_else(|| form.radio_focus.map(|i| i + 3));
                    let backwards = GetKeyState(0x10) < 0;
                    let next = current.map_or(if backwards { 4 } else { 0 }, |current| {
                        (current + if backwards { 4 } else { 1 }) % 5
                    });
                    if next < 3 {
                        form.radio_focus = None;
                        SetFocus(form.edits[next]);
                        wm::SendMessageW(
                            form.edits[next],
                            windows_sys::Win32::UI::Controls::EM_SETSEL,
                            0,
                            -1,
                        );
                    } else {
                        form.radio_focus = Some(next - 3);
                        SetFocus(app.hwnd);
                    }
                }
            }
            unsafe {
                invalidate_rect(app.hwnd, std::ptr::null(), 0);
            }
            true
        }
        32 | 37 | 38 | 39 | 40 => {
            if let Some(form) = app.settings.as_mut() {
                if let Some(index) = form.radio_focus {
                    if unsafe { GetFocus() } == app.hwnd {
                        let next = if key == 32 { index } else { 1 - index };
                        form.radio_focus = Some(next);
                        form.close_action = if next == 0 {
                            CloseAction::Tray
                        } else {
                            CloseAction::Exit
                        };
                        unsafe {
                            invalidate_rect(app.hwnd, std::ptr::null(), 0);
                        }
                        return true;
                    }
                }
            }
            false
        }
        _ => false,
    }
}

pub(super) fn paint(app: &App, canvas: Canvas<'_>) {
    let Some(form) = &app.settings else { return };
    let Some(fonts) = &app.fonts else { return };
    let [dialog, active, all, cancel, save, tray, exit, concurrency, check_update] =
        form_rects(app);
    unsafe {
        dim_overlay(canvas, WIDTH, app.full_height);
    }
    canvas.shadow(&dialog, 14.0);
    canvas.round(&dialog, 14.0, CARD, false);
    canvas.round(&dialog, 14.0, LINE, true);
    visual::label(
        canvas,
        fonts.title,
        TEXT,
        box_rect(dialog.left + 28, dialog.top + 19, 400, 36),
        "设置",
    );
    for (i, rect) in [active, all, concurrency].into_iter().enumerate() {
        visual::label(
            canvas,
            fonts.body,
            TEXT,
            box_rect(dialog.left + 28, rect.top, 94, 32),
            ["当前账号", "全部账号", "批量刷新"][i],
        );
        if i == 2 {
            visual::label(
                canvas,
                fonts.body,
                TEXT,
                box_rect(dialog.left + 126, rect.top, 76, 30),
                "同时刷新",
            );
        }
        canvas.round(&rect, 6.0, 0xffffff, false);
        canvas.round(&rect, 6.0, LINE, true);
        if form.edits[i].is_null() {
            canvas.text(
                fonts.body,
                TEXT,
                &rect,
                [&form.active, &form.all, &form.concurrency][i],
                DT_CENTER | DT_VCENTER,
            );
        }
        visual::label(
            canvas,
            fonts.body,
            TEXT,
            box_rect(rect.right + 10, rect.top, if i == 2 { 54 } else { 112 }, 30),
            if i == 2 {
                "个账号"
            } else {
                "分钟自动刷新"
            },
        );
        visual::label(
            canvas,
            fonts.caption,
            MUTED,
            box_rect(dialog.left + 338, rect.top, 234, 30),
            [
                "（范围2-120，建议5分钟）",
                "（范围20-300，建议60分钟）",
                "（同时刷新范围1-16个，建议5个）",
            ][i],
        );
    }
    visual::label(
        canvas,
        fonts.body,
        TEXT,
        box_rect(dialog.left + 28, tray.top, 94, 32),
        "关闭按钮",
    );
    for (area, value, title) in [
        (tray, CloseAction::Tray, "最小化到托盘"),
        (exit, CloseAction::Exit, "退出软件"),
    ] {
        let selected = form.close_action == value;
        let x = (area.left + 10) as f32;
        let y = (area.top + 16) as f32;
        canvas.ellipse(x, y, 7.0, 7.0, if selected { ACCENT } else { MUTED }, false);
        if selected {
            canvas.ellipse(x, y, 3.5, 3.5, ACCENT, true);
        }
        visual::label(
            canvas,
            fonts.body,
            TEXT,
            box_rect(area.left + 27, area.top, area.right - area.left - 27, 32),
            title,
        );
    }
    if let Some(error) = &form.error {
        visual::label(
            canvas,
            fonts.caption,
            AMBER,
            box_rect(dialog.left + 112, dialog.top + 25, 460, 22),
            error,
        );
    }
    visual::label(
        canvas,
        fonts.small,
        MUTED,
        box_rect(
            dialog.left + 28,
            cancel.top,
            check_update.left - dialog.left - 44,
            34,
        ),
        &format!("当前版本号：{}", env!("CARGO_PKG_VERSION")),
    );
    updates_ui::paint_check_button(app, canvas, &check_update);
    visual::button(canvas, fonts, &cancel, "取消", false);
    visual::button(canvas, fonts, &save, "保存", true);
}
