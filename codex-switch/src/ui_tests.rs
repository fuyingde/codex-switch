//! Offscreen Direct2D / DirectWrite checks. Never creates a visible window, worker, or connection.
use super::*;

fn fixture() -> App {
    let (tx, rx) = mpsc::channel();
    let (switch_tx, switch_rx) = mpsc::channel();
    let (login_tx, login_rx) = mpsc::channel();
    let (usage_tx, _) = mpsc::channel();
    // Keep channels correctly typed without starting the real usage worker.
    drop(tx);
    let rows = [
        "lin@example.com",
        "studio@example.com",
        "dev@example.com",
        "archive@example.com",
    ]
    .into_iter()
    .enumerate()
    .map(|(i, label)| RowView {
        id: format!("synthetic-{i}"),
        label: label.into(),
        plan: Some(
            if i == 1 {
                "pro"
            } else if i == 3 {
                "free"
            } else {
                "plus"
            }
            .into(),
        ),
        active_until: Some("2026-11-13".into()),
        is_active: i == 0,
        five_hour: [Some(82), Some(96), Some(12), None][i],
        five_hour_reset: Some(now_epoch() + 3600),
        weekly: [Some(64), Some(88), Some(37), None][i],
        weekly_reset: Some(now_epoch() + 86400 * 4),
        reset_credits: Some(2),
        error: if i == 3 {
            Some("synthetic login required".into())
        } else {
            None
        },
        expired: i == 3,
        loading: false,
    })
    .collect();
    App {
        hwnd: std::ptr::null_mut(),
        icon: std::ptr::null_mut(),
        rows,
        homes: HashMap::new(),
        notice: None,
        notice_at: Instant::now(),
        last_notice: None,
        compact_mode: false,
        full_panel: PanelPlacement::default(),
        compact_panel: PanelPlacement::default(),
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
        connection_established: true,
        connection_check: None,
        add_menu: false,
        context_row: None,
        context_at: (0, 0),
        hover_item: None,
        compact_hover: None,
        compact_left: 0,
        masked: false,
        scroll: 0,
        full_height: HEIGHT,
        scroll_drag: None,
        compact_scroll: 0,
        compact_height_limit: COMPACT_MAX_HEIGHT,
        hover_row: None,
        refresh_active_minutes: 5,
        refresh_all_minutes: 60,
        usage_concurrency: usage_workers::DEFAULT_CONCURRENCY,
        usage_control: None,
        close_action: CloseAction::Tray,
        settings: None,
        updates: updates_ui::State::default(),
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
        tx: usage_tx,
        rx,
        switch_tx,
        switch_rx,
        login_pending: false,
        login_cancel: Arc::new(AtomicBool::new(false)),
        login_tx,
        login_rx,
    }
}

#[test]
fn scrolling_keeps_compact_summary_and_footer_out_of_row_hit_targets() {
    let mut app = fixture();
    for i in 0..16 {
        let mut more = fixture().rows.remove(1);
        more.id = format!("extra-{i}");
        app.rows.push(more);
    }
    app.compact_mode = true;
    app.compact_scroll = compact_max_scroll(&app);
    let layout = compact_layout(&app);
    assert_eq!(compact_height(&app), COMPACT_MAX_HEIGHT);
    assert!(compact_row_at(&app, 30, 100).is_none());
    assert!(compact_row_at(&app, 30, COMPACT_MAX_HEIGHT - 20).is_none());
    assert_eq!(
        compact_hit_at(&app, 30, COMPACT_MAX_HEIGHT - 20),
        Some(CompactHit::Add)
    );
    let visible = layout
        .cards
        .iter()
        .find(|c| c.top >= COMPACT_HEADER && c.bottom <= COMPACT_MAX_HEIGHT - COMPACT_FOOTER)
        .unwrap();
    assert!(matches!(
        compact_hit_at(&app, visible.left + 4, visible.top + 4),
        Some(CompactHit::Card(_))
    ));
    assert!(
        layout.cards[0].right == 0,
        "current account belongs in fixed summary"
    );
    let last = layout.cards.last().unwrap();
    assert_eq!(last.bottom, COMPACT_MAX_HEIGHT - COMPACT_FOOTER);
}

#[test]
fn full_table_actions_and_columns_stay_inside_the_visible_table() {
    let app = fixture();
    let layout = layout(&app);
    assert_eq!(
        columns(&layout.header).last().unwrap().right,
        layout.header.right
    );
    for (i, row) in layout.rows.iter().enumerate() {
        assert!(row.bottom < HEIGHT - FOOTER);
        assert!(layout.switch[i].right <= row.right);
    }
    assert!(!point_in(
        &layout.add,
        layout.privacy.left + 1,
        layout.privacy.top + 1
    ));
}

/// Runs the same presentation through a Windows WIC software target, no HWND.
fn render(app: &App, name: &str) {
    render_scaled(app, name, 1.0);
}
fn render_scaled(app: &App, name: &str, scale: f32) {
    let (logical_w, logical_h) = if app.compact_mode {
        (COMPACT_WIDTH, compact_height(app))
    } else {
        (WIDTH, app.full_height)
    };
    let width = (logical_w as f32 * scale).round() as u32;
    let height = (logical_h as f32 * scale).round() as u32;
    let graphics = Graphics::new().unwrap();
    let pixels = graphics
        .render_bitmap(width, height, scale, |canvas| {
            paint(app, canvas, logical_w, logical_h)
        })
        .unwrap();
    assert!(
        pixels.chunks_exact(4).any(|p| p[0] != p[1]),
        "drawing must include colored elements"
    );
    save_preview(name, width, height, pixels);
}

fn save_preview(name: &str, width: u32, height: u32, pixels: Vec<u8>) {
    if let Some(folder) = std::env::var_os("CODEX_SWITCH_UI_RENDER_DIR") {
        let folder = PathBuf::from(folder);
        std::fs::create_dir_all(&folder).unwrap();
        let size = width * height * 4;
        let mut bytes = Vec::new();
        bytes.extend(b"BM");
        bytes.extend((54 + size).to_le_bytes());
        bytes.extend([0u8; 4]);
        bytes.extend(54u32.to_le_bytes());
        bytes.extend(40u32.to_le_bytes());
        bytes.extend((width as i32).to_le_bytes());
        bytes.extend((-(height as i32)).to_le_bytes());
        bytes.extend(1u16.to_le_bytes());
        bytes.extend(32u16.to_le_bytes());
        bytes.extend(0u32.to_le_bytes());
        bytes.extend(size.to_le_bytes());
        bytes.extend([0u8; 16]);
        bytes.extend(pixels);
        std::fs::write(folder.join(format!("{name}.bmp")), bytes).unwrap();
    }
}

/// Compose the same premultiplied outer-shadow pixels uploaded by the real
/// native surface, so the outer effect is inspectable without creating an HWND.
fn render_with_shadow(app: &App, name: &str, scale: f32) {
    let (lw, lh) = if app.compact_mode {
        (COMPACT_WIDTH, compact_height(app))
    } else {
        (WIDTH, app.full_height)
    };
    let width = (lw as f32 * scale).round() as u32;
    let height = (lh as f32 * scale).round() as u32;
    let graphics = Graphics::new().unwrap();
    let body = graphics
        .render_bitmap(width, height, scale, |canvas| paint(app, canvas, lw, lh))
        .unwrap();
    let geometry = window_shadow::Geometry::new(width as i32, height as i32, scale);
    let mut output = window_shadow::shadow_pixels(geometry);
    for y in 0..geometry.height {
        for x in 0..geometry.width {
            let i = (y as usize * geometry.width as usize + x as usize) * 4;
            let alpha = output[i + 3];
            output[i..i + 3].fill(255 - alpha);
            output[i + 3] = 255;
            let bx = x - geometry.padding;
            let by = y - geometry.padding;
            if bx >= 0
                && bx < width as i32
                && by >= 0
                && by < height as i32
                && window_shadow::rounded_distance(
                    bx as f32 + 0.5,
                    by as f32 + 0.5,
                    width as f32,
                    height as f32,
                    geometry.radius as f32,
                ) <= 0.0
            {
                let source = (by as usize * width as usize + bx as usize) * 4;
                output[i..i + 4].copy_from_slice(&body[source..source + 4]);
            }
        }
    }
    save_preview(name, geometry.width as u32, geometry.height as u32, output);
}

#[test]
fn scale_125_150_200_percent_uses_the_same_logical_layout() {
    let mut app = fixture();
    for scale in [1.25, 1.5, 2.0] {
        render_scaled(&app, &format!("full-{}", (scale * 100.0) as i32), scale);
        app.compact_mode = true;
        render_scaled(&app, &format!("compact-{}", (scale * 100.0) as i32), scale);
        app.compact_mode = false;
    }
}

#[test]
fn dpi_hit_targets_match_rendering_and_small_screens_keep_footer_visible() {
    let mut app = fixture();
    for scale in [1.0, 1.25, 1.5, 2.0] {
        app.scale = scale;
        let layout = layout(&app);
        for area in [
            layout.add,
            layout.privacy,
            layout.settings,
            layout.switch[1],
        ] {
            let x = (area.left + area.right) / 2;
            let y = (area.top + area.bottom) / 2;
            assert!(point_in(
                &area,
                dip(&app, px(&app, x)),
                dip(&app, px(&app, y))
            ));
        }
        app.compact_mode = true;
        let panel = compact_layout(&app);
        let x = (panel.expand.left + panel.expand.right) / 2;
        let y = (panel.expand.top + panel.expand.bottom) / 2;
        assert_eq!(
            compact_hit_at(&app, dip(&app, px(&app, x)), dip(&app, px(&app, y))),
            Some(CompactHit::Expand)
        );
        app.compact_mode = false;
    }
    for (dpi, w, h) in [(96, 1366, 728), (144, 1920, 1040), (192, 3840, 2080)] {
        let scale = fitted_scale(dpi, w, h, false);
        assert!((WIDTH as f32 * scale).round() as i32 <= w - 16);
        assert!((HEIGHT as f32 * scale).round() as i32 <= h - 16);
    }
}

#[test]
fn offscreen_visual_states() {
    let mut app = fixture();
    render(&app, "full");
    app.compact_mode = true;
    render(&app, "compact");
    app.confirm = Some((Confirm::Switch, app.rows[1].id.clone()));
    render(&app, "compact-confirm");
    app.confirm = None;
    app.add_menu = true;
    render(&app, "compact-add-menu");
    app.add_menu = false;
    app.login_pending = true;
    render(&app, "compact-login");
    app.login_pending = false;
    app.compact_mode = false;
    settings_ui::open(&mut app);
    render(&app, "settings");
    settings_ui::close(&mut app);
    app.masked = true;
    render(&app, "privacy");
    app.masked = false;
    app.confirm = Some((Confirm::Switch, app.rows[1].id.clone()));
    render(&app, "confirm");
    app.confirm = None;
    app.add_menu = true;
    render(&app, "add-menu");
    app.add_menu = false;
    app.rows[1].label = "very-long-account-name-for-layout-check@example.com".into();
    app.rows[1].loading = true;
    app.connecting = true;
    app.busy = Some("等待当前轮结束".into());
    render(&app, "long-loading");
    app.compact_mode = true;
    render(&app, "compact-long-loading");
    app.compact_mode = false;
    app.rows.clear();
    app.connecting = false;
    app.connection_established = false;
    app.busy = None;
    render(&app, "empty");
    app.compact_mode = true;
    render(&app, "compact-empty");
}

#[test]
fn compact_custom_menu_switch_delete_and_add_stay_inside_the_panel() {
    let mut app = fixture();
    app.compact_mode = true;
    let l = compact_layout(&app);
    let card = l.cards[1];
    clicked(&mut app, 30, card.top + 18);
    assert!(app.confirm.is_none() && app.context_row.is_none());
    // The whole row is hoverable, but left-clicking its former ellipsis area does nothing.
    clicked(&mut app, card.right - 8, card.top + 18);
    assert!(app.context_row.is_none());
    right_clicked(&mut app, card.right - 8, card.top + 18);
    assert_eq!(app.context_row, Some(1));
    assert!(app.compact_mode && app.confirm.is_none());
    let menu = layout(&app);
    let bounds = menu.context_menu.unwrap();
    assert!(bounds.right <= COMPACT_WIDTH && bounds.bottom <= compact_height(&app));
    clicked(
        &mut app,
        menu.context_switch.left + 8,
        menu.context_switch.top + 8,
    );
    assert_eq!(app.confirm, Some((Confirm::Switch, app.rows[1].id.clone())));
    assert!(app.compact_mode);
    let dialog = layout(&app);
    assert!(dialog.dialog.right <= COMPACT_WIDTH && dialog.dialog.bottom <= compact_height(&app));
    clicked(
        &mut app,
        dialog.dialog_cancel.left + 4,
        dialog.dialog_cancel.top + 4,
    );
    assert!(app.confirm.is_none());
    right_clicked(&mut app, 30, card.top + 20);
    assert_eq!(app.context_row, Some(1));
    let menu = layout(&app);
    clicked(
        &mut app,
        menu.context_delete.left + 8,
        menu.context_delete.top + 8,
    );
    assert_eq!(app.confirm, Some((Confirm::Delete, app.rows[1].id.clone())));
    app.confirm = None;
    clicked(&mut app, l.add.left + 4, l.add.top + 4);
    assert!(app.compact_mode && app.add_menu);
    assert!(layout(&app).menu.unwrap().right <= COMPACT_WIDTH);
    app.add_menu = false;
    right_clicked(&mut app, 30, 80);
    assert_eq!(app.context_row, Some(0));
    let menu = layout(&app);
    clicked(
        &mut app,
        menu.context_delete.left + 8,
        menu.context_delete.top + 8,
    );
    assert!(
        app.confirm.is_none(),
        "the active account cannot be deleted"
    );
    assert!(app.compact_mode);
}

#[test]
fn row_hover_progress_and_top_controls_fit_the_350_panel() {
    let mut app = fixture();
    app.compact_mode = true;
    assert_eq!(COMPACT_WIDTH, 350);
    let l = compact_layout(&app);
    assert!(l.refresh_all.right < l.privacy.left);
    let card = l.cards[1];
    update_pointer_hover(&mut app, card.left + 4, card.top + 4);
    assert_eq!(app.compact_hover, Some(CompactHit::Card(1)));
    let [five, seven] = visual::compact_quota_areas(&card);
    assert!(five.right < seven.left);
    assert_eq!(seven.right, card.right - 8);
    assert!(seven.left - five.right >= 24);
    assert!(five.right - five.left >= 130 && seven.right - seven.left >= 130);
    render(&app, "compact-hover");
    right_clicked(&mut app, card.right - 10, card.bottom - 5);
    let menu = layout(&app);
    update_pointer_hover(
        &mut app,
        menu.context_delete.left + 4,
        menu.context_delete.top + 4,
    );
    assert!(app.hover_item == Some(HoverItem::ContextDelete));
    render(&app, "compact-context-menu");
}

fn many_rows(app: &mut App) {
    for i in 0..24 {
        let mut row = fixture().rows.remove(1);
        row.id = format!("many-{i}");
        row.label = format!("account-{i}@example.com");
        app.rows.push(row);
    }
    app.rows[0].is_active = false;
    app.rows.last_mut().unwrap().is_active = true;
}

#[test]
fn vertical_resize_scrollbar_and_locate_keep_header_and_footer_fixed() {
    let mut app = fixture();
    many_rows(&mut app);
    for height in [MIN_HEIGHT, 480, HEIGHT] {
        app.full_height = height;
        app.scroll = 0;
        let header = layout(&app).header;
        let bar = scrollbar(&app).unwrap();
        assert!(bar.thumb.bottom < bar.track.bottom);
        assert!(scrollbar_pressed(
            &mut app,
            bar.thumb.left,
            bar.thumb.top + 4
        ));
        drag_scrollbar(&mut app, bar.track.bottom + 100);
        assert_eq!(app.scroll, max_scroll(&app));
        assert_eq!(layout(&app).header.top, header.top);
        app.scroll_drag = None;
        app.scroll = 0;
        let locate = layout(&app).locate;
        clicked(&mut app, locate.left + 4, locate.top + 4);
        let l = layout(&app);
        let active = *l.rows.last().unwrap();
        assert!(active.top >= l.header.bottom && active.bottom <= height - FOOTER);
        assert!(account_at(&app, 30, l.header.bottom - 1).is_none());
        assert!(account_at(&app, 30, height - FOOTER + 1).is_none());
        right_clicked(&mut app, 35, l.header.bottom - 1);
        assert!(app.context_row.is_none());
        right_clicked(&mut app, WIDTH - 45, active.bottom - 4);
        assert_eq!(app.context_row, Some(app.rows.len() - 1));
        assert!(layout(&app).context_menu.unwrap().bottom <= height - 10);
        app.context_row = None;
        render(&app, &format!("full-scroll-{height}"));
        settings_ui::open(&mut app);
        let form = settings_ui::form_rects(&app)[0];
        assert!(form.top >= 0 && form.bottom <= height);
        render(&app, &format!("settings-{height}"));
        settings_ui::close(&mut app);
    }
    for scale in [1.0, 1.25, 1.5, 2.0] {
        app.scale = scale;
        let (width, min, max) = window_size_limits(&app);
        assert_eq!(
            (width, min, max),
            (px(&app, WIDTH), px(&app, MIN_HEIGHT), px(&app, HEIGHT))
        );
        let mut bounds = box_rect(100, 100, 1700, 150);
        constrain_resize(
            &app,
            &mut bounds,
            windows_sys::Win32::UI::WindowsAndMessaging::WMSZ_BOTTOM,
        );
        assert_eq!(bounds.right - bounds.left, width);
        assert_eq!(bounds.bottom - bounds.top, min);
        let mut bounds = box_rect(100, 100, 1700, 1900);
        let bottom = bounds.bottom;
        constrain_resize(
            &app,
            &mut bounds,
            windows_sys::Win32::UI::WindowsAndMessaging::WMSZ_TOP,
        );
        assert_eq!(bounds.bottom, bottom);
        assert_eq!(bounds.bottom - bounds.top, max);
    }
    assert!(vertical_resize_hit(&app, 0).is_some());
    assert!(vertical_resize_hit(&app, HEIGHT - 1).is_some());
    assert!(vertical_resize_hit(&app, HEIGHT / 2).is_none());
    app.compact_mode = true;
    assert!(vertical_resize_hit(&app, 0).is_none());
}

#[test]
fn nonclient_calculation_preserves_the_new_window_rect() {
    use windows_sys::Win32::UI::WindowsAndMessaging::NCCALCSIZE_PARAMS;
    let mut params: NCCALCSIZE_PARAMS = unsafe { std::mem::zeroed() };
    params.rgrc[0] = box_rect(100, 100, WIDTH, HEIGHT);
    params.rgrc[1] = box_rect(100, 100, WIDTH - 20, HEIGHT - 20);
    let result = unsafe {
        window_proc(
            std::ptr::null_mut(),
            WM_NCCALCSIZE,
            1,
            &mut params as *mut _ as isize,
        )
    };
    assert_eq!(result, 0);
    assert_eq!(params.rgrc[0].right - params.rgrc[0].left, WIDTH);
    assert_eq!(params.rgrc[0].bottom - params.rgrc[0].top, HEIGHT);
}

#[test]
fn double_click_refreshes_current_card_and_refresh_result_is_reported() {
    let mut app = fixture();
    app.compact_mode = true;
    let (tx, requests) = mpsc::channel();
    app.tx = tx;
    let (results, rx) = mpsc::channel();
    app.rx = rx;
    app.homes
        .insert(app.rows[0].id.clone(), PathBuf::from("synthetic-home"));
    double_clicked(&mut app, 40, 88);
    let request = requests.try_recv().unwrap();
    assert_eq!(request.id, app.rows[0].id);
    assert_eq!(app.refresh_pending.len(), 1);
    assert!(!app.notice.as_deref().unwrap().ends_with('…'));
    results
        .send(Message::Usage {
            id: request.id,
            result: Ok(usage::Usage::default()),
        })
        .unwrap();
    drain(&mut app);
    assert_eq!(app.notice.as_deref(), Some("刷新成功"));
    double_clicked(&mut app, 40, 88);
    let request = requests.try_recv().unwrap();
    results
        .send(Message::Usage {
            id: request.id,
            result: Err("synthetic failure".into()),
        })
        .unwrap();
    drain(&mut app);
    assert!(app.notice.as_deref().unwrap().starts_with("刷新失败"));
}

#[test]
fn refresh_settings_validate_cancel_and_persist_without_touching_user_files() {
    assert!(settings_ui::validate("1", "60").is_err());
    assert!(settings_ui::validate("121", "60").is_err());
    assert!(settings_ui::validate("5", "19").is_err());
    assert!(settings_ui::validate("5", "301").is_err());
    assert!(settings_ui::validate("", "60").is_err());
    assert_eq!(settings_ui::validate("2", "300").unwrap(), (2, 300));
    for invalid in ["", "0", "17", "-1", "1.5", "abc", "999999999999"] {
        assert!(settings_ui::validate_concurrency(invalid).is_err());
    }
    for valid in [1, 5, 16] {
        assert_eq!(
            settings_ui::validate_concurrency(&valid.to_string()).unwrap(),
            valid
        );
    }
    let old = parse_ui_settings(r#"{"masked":true,"compact_left":80}"#);
    assert_eq!((old.active_minutes, old.all_minutes), (5, 60));
    assert_eq!(old.close_action, CloseAction::Tray);
    assert_eq!(old.usage_concurrency, 5);
    for value in [0, 17] {
        let invalid = parse_ui_settings(&format!(r#"{{"usage_concurrency":{value}}}"#));
        assert_eq!(invalid.usage_concurrency, 5);
    }
    let unknown =
        parse_ui_settings(r#"{"active_minutes":7,"all_minutes":90,"close_action":"unknown"}"#);
    assert_eq!(unknown.close_action, CloseAction::Tray);
    assert_eq!(unknown.active_minutes, 7);
    let invalid = parse_ui_settings(r#"{"active_minutes":0,"all_minutes":900}"#);
    assert_eq!((invalid.active_minutes, invalid.all_minutes), (5, 60));
    let mut app = fixture();
    settings_ui::open(&mut app);
    app.settings.as_mut().unwrap().active = "9".into();
    app.settings.as_mut().unwrap().concurrency = "16".into();
    let exit = settings_ui::form_rects(&app)[6];
    settings_ui::clicked(&mut app, exit.left + 4, exit.top + 4);
    assert_eq!(
        app.settings.as_ref().unwrap().close_action,
        CloseAction::Exit
    );
    assert_eq!(app.close_action, CloseAction::Tray, "editing is not saving");
    settings_ui::close(&mut app);
    assert_eq!(app.refresh_active_minutes, 5);
    assert_eq!(
        app.usage_concurrency, 5,
        "cancel must not change the pool limit"
    );
    assert_eq!(app.close_action, CloseAction::Tray);
    let path = std::env::temp_dir().join(format!(
        "codex-ui-settings-{}-{}.json",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let previous = std::env::var_os("CODEX_SWITCH_UI_FILE");
    std::env::set_var("CODEX_SWITCH_UI_FILE", &path);
    settings_ui::open(&mut app);
    let form = app.settings.as_mut().unwrap();
    form.active = "7".into();
    form.all = "90".into();
    form.concurrency = "16".into();
    let exit = settings_ui::form_rects(&app)[6];
    settings_ui::clicked(&mut app, exit.left + 4, exit.top + 4);
    render(&app, "settings-exit-selected");
    settings_ui::save(&mut app);
    assert!(app.settings.is_none());
    assert_eq!(
        (app.refresh_active_minutes, app.refresh_all_minutes),
        (7, 90)
    );
    let saved = parse_ui_settings(&std::fs::read_to_string(&path).unwrap());
    assert_eq!((saved.active_minutes, saved.all_minutes), (7, 90));
    assert_eq!(saved.close_action, CloseAction::Exit);
    assert_eq!(saved.usage_concurrency, 16);
    assert_eq!(app.usage_concurrency, 16);
    assert_eq!(app.close_action, CloseAction::Exit);
    settings_ui::open(&mut app);
    assert_eq!(
        app.settings.as_ref().unwrap().close_action,
        CloseAction::Exit
    );
    assert_eq!(app.settings.as_ref().unwrap().concurrency, "16");
    let tray = settings_ui::form_rects(&app)[5];
    settings_ui::clicked(&mut app, tray.left + 4, tray.top + 4);
    assert_eq!(
        app.settings.as_ref().unwrap().close_action,
        CloseAction::Tray
    );
    settings_ui::save(&mut app);
    assert_eq!(
        parse_ui_settings(&std::fs::read_to_string(&path).unwrap()).close_action,
        CloseAction::Tray
    );
    settings_ui::open(&mut app);
    let previous_file = std::fs::read(&path).unwrap();
    app.settings.as_mut().unwrap().concurrency = "17".into();
    settings_ui::save(&mut app);
    assert!(app.settings.as_ref().unwrap().error.is_some());
    assert_eq!(app.usage_concurrency, 16);
    assert_eq!(std::fs::read(&path).unwrap(), previous_file);
    render(&app, "settings-invalid-concurrency");
    settings_ui::close(&mut app);
    std::fs::remove_file(&path).unwrap();
    if let Some(previous) = previous {
        std::env::set_var("CODEX_SWITCH_UI_FILE", previous);
    } else {
        std::env::remove_var("CODEX_SWITCH_UI_FILE");
    }
}

#[test]
fn full_and_compact_context_refresh_routes_only_the_selected_account() {
    for compact in [false, true] {
        let mut app = fixture();
        app.compact_mode = compact;
        let (tx, requests) = mpsc::channel();
        app.tx = tx;
        app.homes
            .insert(app.rows[1].id.clone(), PathBuf::from("synthetic-home"));
        let row = if compact {
            compact_layout(&app).cards[1]
        } else {
            layout(&app).rows[1]
        };
        right_clicked(&mut app, row.left + 8, row.top + 8);
        assert_eq!(app.context_row, Some(1));
        let menu = layout(&app);
        clicked(
            &mut app,
            menu.context_refresh.left + 8,
            menu.context_refresh.top + 8,
        );
        assert_eq!(requests.try_recv().unwrap().id, app.rows[1].id);
        assert!(requests.try_recv().is_err());
        assert!(app.rows[1].loading && app.context_row.is_none());
        right_clicked(&mut app, row.left + 8, row.top + 8);
        render(
            &app,
            if compact {
                "compact-context-refresh"
            } else {
                "full-context-menu"
            },
        );
        dismiss_menus(&mut app);
        assert!(app.context_row.is_none() && !app.add_menu);
    }
}

#[test]
fn compact_height_grows_to_800_and_only_the_list_scrolls() {
    let mut app = fixture();
    app.compact_mode = true;
    app.rows.truncate(1);
    let one = compact_height(&app);
    assert!(one < 400 && scrollbar(&app).is_none());
    app.rows.push(fixture().rows.remove(1));
    let two = compact_height(&app);
    assert!(two > one && two < 800 && scrollbar(&app).is_none());
    render(&app, "compact-two-accounts");
    many_rows(&mut app);
    assert_eq!(compact_height(&app), 800);
    let before = compact_layout(&app);
    let bar = scrollbar(&app).unwrap();
    assert!(bar.track.top >= COMPACT_HEADER && bar.track.bottom <= 800 - COMPACT_FOOTER);
    assert!(scrollbar_pressed(
        &mut app,
        bar.thumb.left,
        bar.thumb.top + 4
    ));
    drag_scrollbar(&mut app, 900);
    assert_eq!(app.compact_scroll, compact_max_scroll(&app));
    let after = compact_layout(&app);
    assert_eq!(before.add.top, after.add.top);
    assert_eq!(before.refresh_all.top, after.refresh_all.top);
    assert_eq!(before.close.top, after.close.top);
    assert!(compact_row_at(&app, 30, 100).is_none());
    assert!(
        account_at(&app, 30, 100).is_some(),
        "current account remains pinned"
    );
    app.scroll_drag = None;
    render(&app, "compact-scroll-800");
    app.compact_scroll = 0;
    app.scale = 1.0;
    update_compact_height_limit(&mut app, &box_rect(0, 0, 1366, 728));
    assert!(compact_height(&app) <= 728 - 16);
    assert!(scrollbar(&app).is_some());
    assert_eq!(
        fitted_scale(96, 1366, 728, true),
        1.0,
        "short panels retain normal width/font size"
    );
    render(&app, "compact-small-work-area");
}

#[test]
fn context_menu_keeps_the_account_highlight_when_pointer_moves_to_menu() {
    for compact in [false, true] {
        let mut app = fixture();
        app.compact_mode = compact;
        let row = if compact {
            compact_layout(&app).cards[1]
        } else {
            layout(&app).rows[1]
        };
        update_pointer_hover(&mut app, row.left + 8, row.top + 8);
        right_clicked(&mut app, row.left + 8, row.top + 8);
        let menu = layout(&app);
        update_pointer_hover(
            &mut app,
            menu.context_delete.right - 8,
            menu.context_delete.bottom - 8,
        );
        if compact {
            assert_eq!(app.compact_hover, Some(CompactHit::Card(1)));
        } else {
            assert_eq!(app.hover_row, Some(1));
        }
        update_pointer_hover(&mut app, 2, 2);
        if compact {
            assert_eq!(app.compact_hover, Some(CompactHit::Card(1)));
        } else {
            assert_eq!(app.hover_row, Some(1));
        }
        render(
            &app,
            if compact {
                "compact-context-highlight"
            } else {
                "full-context-highlight"
            },
        );
        dismiss_menus(&mut app);
        update_pointer_hover(&mut app, 2, 2);
        assert!(app.compact_hover.is_none() && app.hover_row.is_none());
    }
}

#[test]
fn close_button_hides_both_views_to_tray_and_tray_click_restores_the_mode() {
    for compact in [false, true] {
        let mut app = fixture();
        app.compact_mode = compact;
        assert_eq!(app.close_action, CloseAction::Tray);
        app.confirm = Some((Confirm::Switch, app.rows[1].id.clone()));
        let close = if compact {
            compact_layout(&app).close
        } else {
            layout(&app).close
        };
        clicked(&mut app, close.left + 4, close.top + 4);
        assert!(app.tray_hidden && !app.visible && app.anim.is_none());
        tick(&mut app);
        assert!(
            app.tray_hidden && !app.visible,
            "edge hot zone must not wake a tray-hidden window"
        );
        toggle_visibility(&mut app);
        assert!(!app.tray_hidden && app.visible);
        assert_eq!(app.compact_mode, compact);
        assert!(
            !app.dock_wait_for_pointer,
            "undocked compact mode has no edge auto-hide"
        );
        assert!(
            app.confirm.is_some(),
            "hiding the window must not execute a pending confirmation"
        );
        assert!(app.switch_rx.try_recv().is_err());
    }
}

#[test]
fn quota_colors_match_percentage_and_fill_in_all_three_views() {
    let mut app = fixture();
    let graphics = Graphics::new().unwrap();
    let fonts = app.fonts.as_ref().unwrap();
    for (value, expected) in [
        (0, QUOTA_RED),
        (1, QUOTA_RED),
        (4, QUOTA_RED),
        (5, QUOTA_ORANGE),
        (19, QUOTA_ORANGE),
        (20, GREEN),
        (100, GREEN),
    ] {
        assert_eq!(bar_color(value), expected);
        app.rows[0].five_hour = Some(value);
        app.rows[0].weekly = Some(value);
        let row = &app.rows[0];
        assert_eq!(quota_color(row, Some(value)), expected);
        for (compact, thin) in [(false, false), (true, false), (true, true)] {
            let width = 180;
            let height = 48;
            let pixels = graphics
                .render_bitmap(width, height, 1.0, |canvas| {
                    canvas.fill(&box_rect(0, 0, width as i32, height as i32), CARD, 1.0);
                    if thin {
                        visual::thin_quota(canvas, fonts, box_rect(0, 0, 180, 40), row, false);
                    } else {
                        visual::quota(canvas, fonts, box_rect(0, 0, 180, 44), row, false, compact);
                    }
                })
                .unwrap();
            let is_color = |pixel: &[u8], color: u32| {
                let channels = [(color >> 16) as u8, (color >> 8) as u8, color as u8];
                pixel[..3]
                    .iter()
                    .zip(channels)
                    .all(|(a, b)| a.abs_diff(b) <= 3)
            };
            let text = &pixels[..(width * 20 * 4) as usize];
            assert!(
                text.chunks_exact(4).any(|p| is_color(p, expected)),
                "percentage must have exact fill color at {value}%"
            );
            let track_top = if thin { 23 } else { 32 };
            let track_height = if thin { 3 } else { 6 };
            let bar = &pixels[(width * track_top * 4) as usize
                ..(width * (track_top + track_height) * 4) as usize];
            if value == 0 {
                assert!(
                    !bar.chunks_exact(4).any(|p| is_color(p, QUOTA_RED)),
                    "zero is an empty track"
                );
            } else if value >= 5 {
                assert!(
                    bar.chunks_exact(4).any(|p| is_color(p, expected)),
                    "fill color must agree with number"
                );
            }
        }
    }
}

#[test]
fn missing_loading_and_failed_quotas_never_use_stale_colored_values() {
    let mut row = fixture().rows.remove(0);
    for state in 0..3 {
        row.loading = state == 1;
        row.error = if state == 2 {
            Some("synthetic failure".into())
        } else {
            None
        };
        row.five_hour = if state == 0 { None } else { Some(1) };
        assert_eq!(quota_color(&row, row.five_hour), MUTED);
        let graphics = Graphics::new().unwrap();
        let app = fixture();
        let fonts = app.fonts.as_ref().unwrap();
        for thin in [false, true] {
            let pixels = graphics
                .render_bitmap(180, 48, 1.0, |canvas| {
                    canvas.fill(&box_rect(0, 0, 180, 48), CARD, 1.0);
                    if thin {
                        visual::thin_quota(canvas, fonts, box_rect(0, 0, 180, 40), &row, false);
                    } else {
                        visual::quota(canvas, fonts, box_rect(0, 0, 180, 44), &row, false, false);
                    }
                })
                .unwrap();
            assert!(
                pixels
                    .chunks_exact(4)
                    .all(|p| p[0].abs_diff(p[1]) < 20 && p[1].abs_diff(p[2]) < 20),
                "unknown data has only neutral text and an empty gray track"
            );
        }
    }
}

#[test]
fn offscreen_quota_boundaries_and_explicit_outer_shadow() {
    let mut app = fixture();
    app.rows.clear();
    for (i, value) in [
        Some(100),
        Some(20),
        Some(19),
        Some(5),
        Some(4),
        Some(1),
        Some(0),
        None,
    ]
    .into_iter()
    .enumerate()
    {
        let mut row = fixture().rows.remove(0);
        row.id = format!("boundary-{i}");
        row.label = format!(
            "quota-{}@example.com",
            value.map(|v| v.to_string()).unwrap_or("unknown".into())
        );
        row.five_hour = value;
        row.weekly = value;
        row.is_active = i == 6;
        app.rows.push(row);
    }
    render_with_shadow(&app, "full-quota-colors-shadow", 1.0);
    app.compact_mode = true;
    render_with_shadow(&app, "compact-quota-colors-shadow", 1.0);
    app.compact_scroll = compact_max_scroll(&app);
    render_with_shadow(&app, "compact-zero-unknown-shadow", 1.0);
    for scale in [1.25, 1.5, 2.0] {
        render_with_shadow(
            &app,
            &format!("compact-shadow-{}", (scale * 100.0) as u32),
            scale,
        );
    }
}

#[test]
fn presentation_size_switches_restore_each_panels_docking_and_preserve_the_full_height() {
    let mut app = fixture();
    app.full_height = 480;
    let full = layout(&app);
    clicked(&mut app, full.shrink.left + 4, full.shrink.top + 4);
    assert!(app.compact_mode && !app.docked && app.visible);
    assert_eq!(app.full_height, 480);
    let panel = compact_layout(&app);
    clicked(&mut app, panel.expand.left + 4, panel.expand.top + 4);
    assert!(!app.compact_mode && !app.docked && app.visible);
    assert_eq!(app.full_height, 480);
    for compact in [false, true] {
        // Start each scenario with two previously undocked panels.
        app.full_panel.docked = false;
        app.compact_panel.docked = false;
        app.docked = false;
        set_compact_mode(&mut app, compact);
        toggle_dock(&mut app);
        assert!(app.docked && !app.visible);
        assert_eq!(app.compact_mode, compact);
        let size = window_dimensions(&app);
        assert_eq!(size.0, if compact { COMPACT_WIDTH } else { WIDTH });
        slide_down(&mut app);
        assert!(app.visible && app.docked);
        let control = if compact {
            compact_layout(&app).expand
        } else {
            layout(&app).shrink
        };
        clicked(&mut app, control.left + 4, control.top + 4);
        assert_eq!(app.compact_mode, !compact);
        assert!(!app.docked && app.visible && app.anim.is_none());
        assert_eq!(app.full_height, 480);
        set_compact_mode(&mut app, compact);
        assert!(app.docked && !app.visible && !app.dock_wait_for_pointer);
        assert_eq!(app.full_height, 480);
    }
}

#[test]
fn restored_positions_use_the_target_panels_coordinates_and_fit_changed_work_areas() {
    let work = box_rect(0, 40, 2400, 1400);
    assert_eq!(
        restored_panel_position(Some((600, 300)), &work, WIDTH, 480, false),
        (600, 300)
    );
    assert_eq!(
        restored_panel_position(Some((1500, 80)), &work, COMPACT_WIDTH, 600, false),
        (1500, 80)
    );
    assert_eq!(
        restored_panel_position(Some((1500, 80)), &work, COMPACT_WIDTH, 600, true),
        (1500, 40)
    );
    let left_monitor = box_rect(-1920, -120, 1920, 1080);
    assert_eq!(
        restored_panel_position(Some((-1600, 100)), &left_monitor, WIDTH, 480, false),
        (-1600, 100)
    );
    let smaller_work = box_rect(0, 40, 1280, 720);
    assert_eq!(
        restored_panel_position(Some((3000, -900)), &smaller_work, COMPACT_WIDTH, 600, false),
        (1280 - COMPACT_WIDTH, 40)
    );
    assert_eq!(
        restored_panel_position(
            Some((-3000, 2000)),
            &smaller_work,
            COMPACT_WIDTH,
            600,
            false
        ),
        (0, 160)
    );
    assert_eq!(
        restored_panel_position(None, &work, COMPACT_WIDTH, 600, false),
        ((2400 - COMPACT_WIDTH) / 2, 40)
    );
}

#[test]
fn native_position_capture_uses_the_window_and_keeps_hidden_dock_coordinates_separate() {
    // Only a synthetic, permanently hidden STATIC window. No app entry point,
    // workers, timer, account files or real client are involved.
    unsafe {
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            to_wide("STATIC").as_ptr(),
            to_wide("synthetic panel placement").as_ptr(),
            windows_sys::Win32::UI::WindowsAndMessaging::WS_POPUP,
            600,
            300,
            WIDTH,
            480,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        );
        assert!(!hwnd.is_null());
        let mut app = fixture();
        app.hwnd = hwnd;
        assert_eq!(current_panel_placement(&app).origin, Some((600, 300)));
        app.full_panel = current_panel_placement(&app);
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            1500,
            80,
            COMPACT_WIDTH,
            600,
            SWP_NOACTIVATE | SWP_NOZORDER,
        );
        app.compact_mode = true;
        assert_eq!(current_panel_placement(&app).origin, Some((1500, 80)));
        app.compact_panel = current_panel_placement(&app);
        app.docked = true;
        app.dock_return = Some((1500, 80));
        app.compact_left = 1450;
        app.dock_work = Some(box_rect(0, 40, 2400, 1400));
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            1450,
            -560,
            0,
            0,
            SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER,
        );
        let saved = app_settings(&app);
        assert_eq!(saved.full_panel.origin, Some((600, 300)));
        assert_eq!(saved.compact_panel.origin, Some((1500, 80)));
        assert_eq!(saved.compact_panel.dock_anchor, Some((1450, 40)));
        assert_ne!(saved.compact_panel.origin, Some((1450, -560)));
        assert_eq!(
            windows_sys::Win32::UI::WindowsAndMessaging::IsWindowVisible(hwnd),
            0
        );
        DestroyWindow(hwnd);
    }
}

#[test]
fn panel_positions_and_dock_anchors_survive_round_trips_and_settings_serialization() {
    let mut app = fixture();
    app.full_height = 480;
    app.full_panel.origin = Some((600, 300));
    app.compact_panel.origin = Some((1500, 80));
    app.compact_panel.dock_anchor = Some((1500, 40));
    app.compact_panel.docked = true;
    set_compact_mode(&mut app, true);
    assert!(app.compact_mode && app.docked && !app.visible);
    assert_eq!(app.dock_return, Some((1500, 80)));
    // The panel is hidden/animating when switching away. Its offscreen position
    // must never replace its return position or hot zone.
    app.visible = false;
    app.compact_left = 1480;
    app.dock_work = Some(box_rect(0, 40, 2400, 1400));
    set_compact_mode(&mut app, false);
    assert!(!app.docked && app.visible);
    assert_eq!(current_panel_placement(&app).origin, Some((600, 300)));
    assert_eq!(app.compact_panel.origin, Some((1500, 80)));
    assert_eq!(app.compact_panel.dock_anchor, Some((1480, 40)));
    // Both panels can independently stay docked.
    app.docked = true;
    app.dock_return = Some((600, 300));
    app.compact_left = 500;
    app.dock_work = Some(box_rect(0, 40, 2400, 1400));
    set_compact_mode(&mut app, true);
    assert!(app.docked && !app.visible && !app.dock_wait_for_pointer);
    let work = box_rect(0, 40, 2400, 1400);
    let bounds = box_rect(1480, 40, COMPACT_WIDTH, compact_height(&app));
    slide_down(&mut app);
    assert_eq!(
        dock_pointer_action(&mut app, 20, 1000, &bounds, &work),
        DockAction::Hide
    );
    assert_eq!(
        dock_pointer_action(&mut app, 1490, 60, &bounds, &work),
        DockAction::None
    );
    assert_eq!(
        dock_pointer_action(&mut app, 20, 1000, &bounds, &work),
        DockAction::Hide
    );
    let saved = app_settings(&app);
    let reloaded = parse_ui_settings(&serde_json::to_string(&saved).unwrap());
    assert_eq!(reloaded.full_panel, saved.full_panel);
    assert_eq!(reloaded.compact_panel, saved.compact_panel);
    assert!(reloaded.full_panel.docked && reloaded.compact_panel.docked);
    assert_eq!(reloaded.full_panel.origin, Some((600, 300)));
    assert_eq!(reloaded.compact_panel.origin, Some((1500, 80)));
    toggle_dock(&mut app);
    assert!(!app.docked);
    set_compact_mode(&mut app, false);
    assert!(app.docked && !app.visible);
    assert_eq!(app.dock_return, Some((600, 300)));
    set_compact_mode(&mut app, true);
    assert!(!app.docked && app.visible);
    assert_eq!(app.full_height, 480);
    let old = parse_ui_settings(r#"{"compact_left":80,"masked":true}"#);
    assert_eq!(old.full_panel, PanelPlacement::default());
    assert_eq!(old.compact_panel, PanelPlacement::default());
}

#[test]
fn switching_to_either_docked_panel_hides_immediately_without_pointer_entry() {
    let work = box_rect(0, 40, 2400, 1400);
    for compact in [false, true] {
        let mut app = fixture();
        app.compact_mode = !compact;
        let placement = PanelPlacement {
            origin: Some((600, 300)),
            dock_anchor: Some((500, 40)),
            docked: true,
        };
        if compact {
            app.compact_panel = placement;
        } else {
            app.full_panel = placement;
        }
        set_compact_mode(&mut app, compact);
        // A null HWND completes slide_up synchronously. No pointer event is
        // delivered between the size switch and this check.
        assert!(app.docked && !app.visible && !app.dock_wait_for_pointer);
        assert_eq!(app.dock_return, placement.origin);
        assert_eq!(app.compact_mode, compact);
        app.compact_left = 500;
        let bounds = box_rect(
            500,
            40,
            window_dimensions(&app).0,
            window_dimensions(&app).1,
        );
        assert_eq!(
            dock_pointer_action(&mut app, 510, 40, &bounds, &work),
            DockAction::Show
        );
        slide_down(&mut app);
        assert!(app.visible && app.docked);
        assert_eq!(
            dock_pointer_action(&mut app, 20, 1000, &bounds, &work),
            DockAction::Hide
        );
    }
}

#[test]
fn both_dock_buttons_hide_and_restore_the_same_mode_with_the_right_hot_zone() {
    let work = box_rect(100, 40, 2400, 1400);
    for compact in [false, true] {
        let mut app = fixture();
        app.compact_mode = compact;
        app.compact_left = 500;
        let control = if compact {
            compact_layout(&app).pin
        } else {
            layout(&app).pin
        };
        clicked(&mut app, control.left + 4, control.top + 4);
        assert!(app.docked && !app.visible);
        assert_eq!(app.compact_mode, compact);
        let (width, height) = window_dimensions(&app);
        let bounds = box_rect(500, 40, width, height);
        // Full mode's hot zone includes places beyond the small panel's width.
        assert_eq!(
            dock_pointer_action(&mut app, 500 + width - 1, 40, &bounds, &work),
            DockAction::Show
        );
        assert_eq!(
            dock_pointer_action(&mut app, 500 + width + 1, 40, &bounds, &work),
            DockAction::None
        );
        slide_down(&mut app);
        assert!(app.visible && app.docked);
        assert_eq!(app.compact_mode, compact);
        assert_eq!(
            dock_pointer_action(&mut app, 520, 80, &bounds, &work),
            DockAction::None
        );
        assert_eq!(
            dock_pointer_action(&mut app, 20, 80, &bounds, &work),
            DockAction::Hide
        );
        app.confirm = Some((Confirm::Switch, app.rows[1].id.clone()));
        assert_eq!(
            dock_pointer_action(&mut app, 20, 80, &bounds, &work),
            DockAction::None
        );
        app.confirm = None;
        toggle_dock(&mut app);
        assert!(!app.docked && app.visible);
        assert_eq!(app.compact_mode, compact);
        assert_eq!(
            dock_pointer_action(&mut app, 20, 80, &bounds, &work),
            DockAction::None
        );
        render_with_shadow(
            &app,
            if compact {
                "compact-undocked"
            } else {
                "full-undocked"
            },
            1.0,
        );
        app.docked = true;
        render_with_shadow(
            &app,
            if compact {
                "compact-docked-icon"
            } else {
                "full-docked-icon"
            },
            1.0,
        );
    }
}

#[test]
fn docking_survives_tray_restore_without_changing_the_mode() {
    for compact in [false, true] {
        let mut app = fixture();
        app.compact_mode = compact;
        toggle_dock(&mut app);
        hide_to_tray(&mut app);
        assert!(app.tray_hidden && app.docked);
        toggle_visibility(&mut app);
        assert!(!app.tray_hidden && app.docked && app.visible && app.dock_wait_for_pointer);
        assert_eq!(app.compact_mode, compact);
        let work = box_rect(0, 0, 2400, 1400);
        let bounds = box_rect(0, 0, window_dimensions(&app).0, window_dimensions(&app).1);
        assert_eq!(
            dock_pointer_action(&mut app, 2300, 1300, &bounds, &work),
            DockAction::None
        );
        assert_eq!(
            dock_pointer_action(&mut app, 20, 20, &bounds, &work),
            DockAction::None
        );
        assert!(!app.dock_wait_for_pointer);
        assert_eq!(
            dock_pointer_action(&mut app, 2300, 1300, &bounds, &work),
            DockAction::Hide
        );
    }
}

#[test]
fn mode_and_dock_hit_targets_do_not_overlap_and_small_footer_controls_stay_fixed() {
    let mut app = fixture();
    let l = layout(&app);
    assert!(l.shrink.right < l.refresh_all.left);
    assert!(l.refresh_all.right < l.privacy.left);
    assert!(l.privacy.right < l.add.left);
    app.compact_mode = true;
    let l = compact_layout(&app);
    assert!(l.expand.right < l.pin.left && l.pin.right < l.close.left);
    assert!(l.refresh_all.top >= compact_height(&app) - COMPACT_FOOTER);
    assert!(l.privacy.top == l.refresh_all.top && l.add.top == l.privacy.top);
    assert!(l.add.right < l.refresh_all.left && l.refresh_all.right < l.privacy.left);
    for (rect, hit) in [
        (l.expand, CompactHit::Expand),
        (l.pin, CompactHit::Dock),
        (l.close, CompactHit::Close),
        (l.refresh_all, CompactHit::Refresh),
        (l.privacy, CompactHit::Privacy),
    ] {
        assert_eq!(compact_hit_at(&app, rect.left + 4, rect.top + 4), Some(hit));
    }
}

#[test]
fn queued_switch_message_is_exact_in_full_view_and_invisible_in_compact_view() {
    let mut app = fixture();
    app.compact_mode = true;
    let graphics = Graphics::new().unwrap();
    let draw = |app: &App| {
        graphics
            .render_bitmap(
                COMPACT_WIDTH as u32,
                compact_height(app) as u32,
                1.0,
                |canvas| paint(app, canvas, COMPACT_WIDTH, compact_height(app)),
            )
            .unwrap()
    };
    let before = draw(&app);
    app.switch_tx
        .send(Ok(SwitchNotice {
            message: QUEUED_SWITCH_NOTICE.into(),
            finished: false,
        }))
        .unwrap();
    assert!(drain(&mut app));
    assert_eq!(
        app.notice.as_deref(),
        Some("正在切号，等待当前对话结束后自动切换")
    );
    assert!(app.busy.is_some());
    assert_eq!(
        before,
        draw(&app),
        "compact view does not show busy/queued text"
    );
    render(&app, "compact-queued-no-notice");
    app.compact_mode = false;
    render(&app, "full-queued-notice");
}

#[test]
fn refresh_all_manual_auto_and_reloaded_rows_share_one_inflight_query_per_account() {
    let mut app = fixture();
    let (tx, requests) = mpsc::channel();
    let (results, rx) = mpsc::channel();
    app.tx = tx;
    app.rx = rx;
    for row in &app.rows {
        app.homes.insert(
            row.id.clone(),
            PathBuf::from(format!("isolated-{}", row.id)),
        );
    }
    // Startup queries are not a manual batch, but Refresh All joins them.
    for i in 0..app.rows.len() {
        assert!(queue_usage(&mut app, i));
    }
    let queued: Vec<_> = requests.try_iter().collect();
    assert_eq!(queued.len(), app.rows.len());
    refresh_all_rows(&mut app);
    refresh_active_row(&mut app);
    refresh_one(&mut app, 1);
    refresh_all_rows(&mut app);
    assert!(
        requests.try_recv().is_err(),
        "repeated clicks/timers must not spawn duplicate profile queries"
    );
    assert_eq!(app.refresh_pending.len(), app.rows.len());
    // Store reload creates fresh RowViews while in-flight requests remain valid.
    app.rows = fixture().rows;
    for i in 0..app.rows.len() {
        assert!(queue_usage(&mut app, i));
    }
    assert!(
        requests.try_recv().is_err(),
        "reloading rows must not bypass request deduplication"
    );
    assert!(app.rows.iter().all(|r| r.loading));
    for (step, index) in [3, 1, 0, 2].into_iter().enumerate() {
        let result = if index == 1 {
            Err("synthetic single-account error".into())
        } else {
            Ok(usage::Usage {
                five_hour_remaining: Some(70),
                weekly_remaining: Some(80),
                ..Default::default()
            })
        };
        results
            .send(Message::Usage {
                id: app.rows[index].id.clone(),
                result,
            })
            .unwrap();
        assert!(drain(&mut app));
        assert!(!app.rows[index].loading);
        assert_eq!(app.usage_pending.len(), app.rows.len() - step - 1);
        if index == 1 {
            assert!(app.rows[index].error.is_some());
        } else {
            assert_eq!(app.rows[index].five_hour, Some(70));
        }
        if step < 3 {
            assert_eq!(app.notice.as_deref(), Some("正在刷新全部账号"));
        }
    }
    assert!(app.refresh_pending.is_empty() && app.usage_pending.is_empty());
    assert_eq!(app.notice.as_deref(), Some("刷新失败，请检查账号状态"));
    // Once the previous request completes the account is refreshable again.
    refresh_one(&mut app, 1);
    assert_eq!(requests.try_recv().unwrap().id, app.rows[1].id);
}

#[test]
fn concurrent_refresh_reports_success_only_after_the_last_account_and_handles_shutdown() {
    let mut app = fixture();
    let (tx, requests) = mpsc::channel();
    let (results, rx) = mpsc::channel();
    app.tx = tx;
    app.rx = rx;
    for row in &app.rows {
        app.homes.insert(
            row.id.clone(),
            PathBuf::from(format!("isolated-{}", row.id)),
        );
    }
    refresh_all_rows(&mut app);
    let mut queued: Vec<_> = requests.try_iter().collect();
    queued.reverse();
    for (i, request) in queued.into_iter().enumerate() {
        results
            .send(Message::Usage {
                id: request.id,
                result: Ok(usage::Usage::default()),
            })
            .unwrap();
        drain(&mut app);
        assert_eq!(
            app.notice.as_deref(),
            Some(if i + 1 == app.rows.len() {
                "刷新成功"
            } else {
                "正在刷新全部账号"
            })
        );
    }
    assert!(app.usage_pending.is_empty());
    drop(requests);
    refresh_one(&mut app, 0);
    assert!(!app.rows[0].loading);
    assert!(app.usage_pending.is_empty() && app.refresh_pending.is_empty());
    assert_eq!(app.notice.as_deref(), Some("刷新失败，刷新服务不可用"));
}

#[test]
fn update_dialog_fits_both_views_and_never_obscures_settings_actions() {
    for compact in [false, true] {
        let mut app = fixture();
        app.compact_mode = compact;
        app.updates.checker.pending = Some(("0.5.0".into(), false));
        assert!(updates_ui::tick(&mut app));
        assert!(!app.updates.dialog.as_ref().unwrap().ignore);
        let [dialog, check, cancel, ok] = updates_ui::rects(&app);
        let width = if compact { COMPACT_WIDTH } else { WIDTH };
        let height = if compact {
            compact_height(&app)
        } else {
            app.full_height
        };
        assert!(
            dialog.left >= 0 && dialog.right <= width && dialog.top >= 0 && dialog.bottom <= height
        );
        assert!(check.right <= cancel.left && cancel.right < ok.left);
        assert_eq!(check.top, cancel.top);
        assert_eq!(cancel.top, ok.top);
        render(
            &app,
            if compact {
                "compact-update-dialog"
            } else {
                "full-update-dialog"
            },
        );
        updates_ui::clicked(&mut app, check.left + 5, check.top + 10);
        assert!(app.updates.dialog.as_ref().unwrap().ignore);
        // Uncheck before cancel so this pure UI fixture never persists user settings.
        updates_ui::clicked(&mut app, check.left + 5, check.top + 10);
        updates_ui::clicked(&mut app, cancel.left + 4, cancel.top + 4);
        assert!(app.updates.dialog.is_none());
        app.updates.checker.pending = Some(("0.6.0".into(), false));
        app.busy = Some("synthetic switch".into());
        updates_ui::tick(&mut app);
        assert!(app.updates.dialog.is_none());
        app.busy = None;
        app.tray_hidden = true;
        updates_ui::tick(&mut app);
        assert!(app.updates.dialog.is_none());
    }
    let mut app = fixture();
    settings_ui::open(&mut app);
    let rects = settings_ui::form_rects(&app);
    assert!(rects[8].right < rects[3].left && rects[3].right < rects[4].left);
    assert_eq!(rects[8].top - rects[5].bottom, 30);
    assert_eq!(rects[6].left - rects[5].right, 12);
    for pair in [rects[1], rects[2], rects[7], rects[5]].windows(2) {
        assert_eq!(pair[1].top - pair[0].bottom, 18);
    }
    for (status, name) in [
        (crate::update::Status::Idle, "settings-check-idle"),
        (crate::update::Status::Checking, "settings-check-running"),
        (crate::update::Status::Latest, "settings-check-latest"),
        (crate::update::Status::Failed, "settings-check-failed"),
    ] {
        app.updates.checker.status = status;
        render(&app, name);
    }
    app.updates.checker.pending = Some(("0.5.0".into(), false));
    updates_ui::tick(&mut app);
    assert!(
        app.updates.dialog.is_none(),
        "automatic checks defer during settings editing"
    );
    app.updates.checker.pending = Some(("0.5.0".into(), true));
    updates_ui::tick(&mut app);
    assert!(
        app.updates.dialog.is_some(),
        "manual result opens above settings"
    );
    render(&app, "settings-update-dialog");
    assert!(updates_ui::keyboard(&mut app, 27));
    assert!(app.updates.dialog.is_none());
    assert!(app.settings.is_some());
    app.full_height = MIN_HEIGHT;
    app.updates.checker.pending = Some(("0.5.0".into(), true));
    updates_ui::tick(&mut app);
    render(&app, "settings-update-350");
}

#[test]
fn reopening_settings_resets_feedback_and_detaches_late_check_results() {
    use crate::update::Status;
    let mut app = fixture();
    for status in [Status::Latest, Status::Failed] {
        settings_ui::open(&mut app);
        app.updates.checker.status = status;
        settings_ui::close(&mut app);
        settings_ui::open(&mut app);
        assert_eq!(app.updates.checker.status, Status::Idle);
        settings_ui::close(&mut app);
    }
    // Only synthetic worker responses: closing must neither restart checks nor
    // let the previous settings session change the reopened button's feedback.
    for failed in [false, true] {
        settings_ui::open(&mut app);
        let (release, gate) = mpsc::channel();
        let clock = app.updates.checker.last_auto;
        assert!(app.updates.checker.begin_with(true, move || {
            gate.recv().unwrap();
            if failed {
                anyhow::bail!("synthetic request failure");
            }
            Ok(env!("CARGO_PKG_VERSION").into())
        }));
        settings_ui::close(&mut app);
        settings_ui::open(&mut app);
        assert_eq!(app.updates.checker.status, Status::Idle);
        assert!(!app.updates.checker.job.as_ref().unwrap().manual);
        assert_eq!(app.updates.checker.last_auto, clock);
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while app.updates.checker.job.is_some() && Instant::now() < deadline {
            app.updates.checker.poll(Instant::now());
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(app.updates.checker.job.is_none());
        assert_eq!(app.updates.checker.status, Status::Idle);
        settings_ui::close(&mut app);
    }
    // A new explicit click can join the still-running request and gets its own
    // visible result; repeated clicks must preserve that original worker.
    settings_ui::open(&mut app);
    let (release, gate) = mpsc::channel();
    app.updates.checker.begin_with(true, move || {
        gate.recv().unwrap();
        Ok(env!("CARGO_PKG_VERSION").into())
    });
    let started = app.updates.checker.job.as_ref().unwrap().started;
    settings_ui::close(&mut app);
    settings_ui::open(&mut app);
    updates_ui::manual(&mut app);
    updates_ui::manual(&mut app);
    assert_eq!(app.updates.checker.status, Status::Checking);
    assert!(app.updates.checker.job.as_ref().unwrap().manual);
    assert_eq!(app.updates.checker.job.as_ref().unwrap().started, started);
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.updates.checker.job.is_some() && Instant::now() < deadline {
        app.updates.checker.poll(Instant::now());
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(app.updates.checker.job.is_none());
    assert_eq!(app.updates.checker.status, Status::Latest);
    settings_ui::close(&mut app);
}

#[test]
fn settings_stays_available_during_connection_and_radio_clicks_never_add_a_background() {
    for failed in [false, true] {
        let mut app = fixture();
        app.connection_established = false;
        app.connecting = true;
        app.busy = Some("连接无感后台".into());
        let gear = layout(&app).settings;
        clicked(&mut app, gear.left + 5, gear.top + 5);
        assert!(
            app.settings.is_some(),
            "startup connection must not block settings"
        );
        assert!(app.connecting && app.busy.is_some());
        app.settings.as_mut().unwrap().active = "9".into();
        let graphics = Graphics::new().unwrap();
        let draw = |app: &App| {
            graphics
                .render_bitmap(WIDTH as u32, app.full_height as u32, 1.0, |canvas| {
                    paint(app, canvas, WIDTH, app.full_height)
                })
                .unwrap()
        };
        for (index, action) in [(5, CloseAction::Tray), (6, CloseAction::Exit)] {
            app.settings.as_mut().unwrap().close_action = action;
            let before = draw(&app);
            let radio = settings_ui::form_rects(&app)[index];
            settings_ui::clicked(&mut app, radio.left + 5, radio.top + 5);
            assert_eq!(app.settings.as_ref().unwrap().close_action, action);
            assert_eq!(before, draw(&app), "radio focus must not add a background");
        }
        render(
            &app,
            if failed {
                "settings-connecting-exit"
            } else {
                "settings-connecting-tray"
            },
        );
        app.switch_tx
            .send(Ok(SwitchNotice {
                message: "synthetic connection progress".into(),
                finished: false,
            }))
            .unwrap();
        assert!(drain(&mut app));
        assert!(app.settings.is_some() && app.connecting);
        app.switch_tx
            .send(if failed {
                Err("synthetic connection failure".into())
            } else {
                Ok(SwitchNotice {
                    message: "synthetic ready".into(),
                    finished: true,
                })
            })
            .unwrap();
        assert!(drain(&mut app));
        assert!(!app.connecting && app.busy.is_none());
        assert_eq!(app.connection_established, !failed);
        assert_eq!(app.settings.as_ref().unwrap().active, "9");
        settings_ui::close(&mut app);
        app.busy = Some("synthetic account switch".into());
        settings_ui::open(&mut app);
        assert!(
            app.settings.is_none(),
            "account switch retains its existing guard"
        );
    }
}
