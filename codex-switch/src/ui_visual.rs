//! Direct2D presentation. No file access, account operations, windows, or workers.
use super::*;

const LEFT: u32 = DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS;
const CENTER: u32 = DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX;

pub(super) fn label(hdc: Canvas<'_>, font: Font, color: u32, rect: RECT, text: &str) {
    unsafe {
        text_in(hdc, font, color, &rect, text, LEFT);
    }
}

pub(super) fn shown(app: &App, row: &RowView) -> String {
    if app.masked {
        mask_account(&row.label)
    } else {
        row.label.clone()
    }
}

pub(super) fn connection_label(app: &App) -> (&str, u32) {
    if app.connecting {
        ("正在连接 Codex", AMBER)
    } else if app.connection_established {
        ("无感连接已就绪", ACCENT)
    } else {
        ("无感连接待就绪", MUTED)
    }
}

pub(super) fn dot(hdc: Canvas<'_>, x: i32, y: i32, color: u32) {
    unsafe {
        round(hdc, &box_rect(x, y, 8, 8), 4, color);
    }
}

/// Curved paths share one artboard, round caps, and consistent optical centers.
pub(super) fn icon(canvas: Canvas<'_>, area: &RECT, kind: &str, color: u32) {
    canvas.icon(area, kind, color);
}

pub(super) fn button(hdc: Canvas<'_>, fonts: &Fonts, rect: &RECT, text: &str, primary: bool) {
    unsafe {
        if primary {
            hdc.gradient_round(rect, 9.0, rgb(57, 133, 108), rgb(40, 105, 84));
        } else {
            round(hdc, rect, 9, CARD);
        }
        if !primary {
            round_border(hdc, rect, 9, LINE);
        }
        text_in(
            hdc,
            fonts.body,
            if primary { CARD } else { TEXT },
            rect,
            text,
            CENTER,
        );
    }
}

fn icon_button(canvas: Canvas<'_>, fonts: &Fonts, area: &RECT, kind: &str, title: &str) {
    button(canvas, fonts, area, "", false);
    icon(
        canvas,
        &box_rect(area.left + 14, area.top + 11, 20, 20),
        kind,
        TEXT,
    );
    label(
        canvas,
        fonts.body,
        TEXT,
        box_rect(
            area.left + 44,
            area.top,
            area.right - area.left - 50,
            area.bottom - area.top,
        ),
        title,
    );
}

fn quota_text(row: &RowView, value: Option<u8>) -> String {
    if row.loading {
        "读取中".into()
    } else if row.error.is_some() {
        "—".into()
    } else {
        value.map(|p| format!("{p}%")).unwrap_or_else(|| "—".into())
    }
}

pub(super) fn quota(
    canvas: Canvas<'_>,
    fonts: &Fonts,
    area: RECT,
    row: &RowView,
    weekly: bool,
    compact: bool,
) {
    let value = if weekly { row.weekly } else { row.five_hour };
    let reset = if weekly {
        row.weekly_reset
    } else {
        row.five_hour_reset
    };
    let font = if compact {
        fonts.compact_number
    } else {
        fonts.body
    };
    label(
        canvas,
        font,
        quota_color(row, value),
        box_rect(area.left, area.top, area.right - area.left, 27),
        &quota_text(row, value),
    );
    let reset = reset
        .map(|t| {
            format!(
                "{}重置",
                if weekly {
                    format_day(t)
                } else {
                    short_clock(t)
                }
            )
        })
        .unwrap_or_else(|| "—".into());
    canvas.text(
        fonts.caption,
        MUTED,
        &box_rect(area.left, area.top, area.right - area.left, 27),
        &reset,
        DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
    );
    let track = box_rect(area.left, area.top + 32, area.right - area.left, 6);
    canvas.round(&track, 3.0, TRACK, false);
    if !row.loading && row.error.is_none() {
        if let Some(p) = value {
            let width = (track.right - track.left) * p.min(100) as i32 / 100;
            if width > 0 {
                canvas.round(
                    &box_rect(track.left, track.top, width, 6),
                    3.0,
                    bar_color(p),
                    false,
                );
            }
        }
    }
}

fn short_clock(epoch: i64) -> String {
    win::local_datetime(epoch)
        .map(|(_, _, _, h, m)| format!("{h}:{m:02}"))
        .unwrap_or_else(|| "—".into())
}
fn reset_text(epoch: Option<i64>, weekly: bool) -> String {
    epoch
        .map(|t| {
            format!(
                "{}重置",
                if weekly {
                    format_day(t)
                } else {
                    short_clock(t)
                }
            )
        })
        .unwrap_or_else(|| "—".into())
}
fn days_text(row: &RowView) -> String {
    row.active_until
        .as_deref()
        .and_then(remaining_days)
        .map(|d| d.to_string())
        .unwrap_or_else(|| "—".into())
}
fn credits_text(row: &RowView) -> String {
    row.reset_credits
        .map(|v| v.to_string())
        .unwrap_or_else(|| "—".into())
}

fn frame(hdc: Canvas<'_>, width: i32, height: i32) {
    unsafe {
        fill(hdc, 0, 0, width, height, CARD);
        round_border(hdc, &box_rect(0, 0, width, height), 12, WINDOW_BORDER);
    }
}

pub(super) fn paint_full_base(app: &App, canvas: Canvas<'_>, width: i32, height: i32) {
    let Some(fonts) = &app.fonts else { return };
    let l = layout(app);
    frame(canvas, width, height);
    canvas.logo(&box_rect(28, 12, 32, 32));
    label(
        canvas,
        fonts.title,
        TEXT,
        box_rect(72, 10, 160, 36),
        "Codex切号器",
    );
    label(
        canvas,
        fonts.small,
        MUTED,
        box_rect(248, 10, 150, 36),
        concat!("v", env!("CARGO_PKG_VERSION")),
    );
    let (state, fg) = connection_label(app);
    let badge = box_rect(WIDTH / 2 - 90, 14, 180, 28);
    canvas.round(
        &badge,
        14.0,
        if app.connection_established {
            SAGE
        } else {
            TRACK
        },
        false,
    );
    dot(canvas, badge.left + 12, 24, fg);
    label(
        canvas,
        fonts.small,
        fg,
        box_rect(badge.left + 30, 14, 146, 28),
        state,
    );
    for (rect, kind) in [
        (&l.pin, if app.docked { "undock" } else { "dock" }),
        (&l.settings, "settings"),
        (&l.min, "min"),
        (&l.close, "close"),
    ] {
        icon(
            canvas,
            rect,
            kind,
            if kind == "undock" { ACCENT } else { MUTED },
        );
    }
    canvas.fill(&box_rect(1, TITLE_BAR, WIDTH - 2, 1), LINE, 1.0);
    label(
        canvas,
        fonts.body,
        TEXT,
        box_rect(32, TOOLBAR_TOP, 300, TOOLBAR_HEIGHT),
        &format!("已添加 {} 个账号", app.rows.len()),
    );
    button(canvas, fonts, &l.add, "＋ 添加账号", true);
    icon_button(canvas, fonts, &l.shrink, "shrink", "收起");
    icon_button(canvas, fonts, &l.refresh_all, "refresh", "刷新");
    icon_button(
        canvas,
        fonts,
        &l.privacy,
        if app.masked { "hidden" } else { "eye" },
        "隐私",
    );
    canvas.round(&l.header, 8.0, rgb(242, 242, 238), false);
    for (cell, title) in
        columns(&l.header)
            .iter()
            .zip(["账号", "5 小时额度", "每周额度", "订阅", "状态", ""])
    {
        label(
            canvas,
            fonts.header,
            MUTED,
            box_rect(
                cell.left + 12,
                cell.top,
                cell.right - cell.left - 16,
                HEADER_ROW,
            ),
            title,
        );
    }
    canvas.round(&l.locate, 6.0, CARD, false);
    canvas.round(&l.locate, 6.0, LINE, true);
    canvas.text(fonts.small, ACCENT, &l.locate, "定位到当前账号", CENTER);
    let bottom = height - FOOTER;
    canvas.clip(&box_rect(
        l.header.left,
        l.header.bottom,
        l.header.right - l.header.left,
        bottom - l.header.bottom,
    ));
    if app.rows.is_empty() {
        canvas.text(
            fonts.body,
            MUTED,
            &box_rect(32, l.header.bottom + 12, WIDTH - 64, 180),
            "还没有账号，点击右上角「添加账号」开始",
            CENTER,
        );
    }
    for (index, rect) in l.rows.iter().enumerate() {
        if rect.bottom <= l.header.bottom || rect.top >= bottom {
            continue;
        }
        let row = &app.rows[index];
        let cells = columns(rect);
        if app.hover_row == Some(index) || app.context_row == Some(index) {
            canvas.round(rect, 8.0, HOVER, false);
        }
        label(
            canvas,
            fonts.body,
            TEXT,
            box_rect(
                cells[0].left + 12,
                rect.top + 10,
                cells[0].right - cells[0].left - 24,
                26,
            ),
            &shown(app, row),
        );
        let meta = format!(
            "订阅{}到期，剩余{}天，可重置{}次",
            row.active_until.as_deref().unwrap_or("—"),
            days_text(row),
            credits_text(row)
        );
        label(
            canvas,
            fonts.caption,
            MUTED,
            box_rect(
                cells[0].left + 12,
                rect.top + 40,
                cells[0].right - cells[0].left - 24,
                21,
            ),
            &meta,
        );
        quota(
            canvas,
            fonts,
            box_rect(
                cells[1].left + 12,
                rect.top + 13,
                cells[1].right - cells[1].left - 24,
                44,
            ),
            row,
            false,
            false,
        );
        quota(
            canvas,
            fonts,
            box_rect(
                cells[2].left + 12,
                rect.top + 13,
                cells[2].right - cells[2].left - 24,
                44,
            ),
            row,
            true,
            false,
        );
        let plan = row
            .plan
            .as_deref()
            .map(plan_name)
            .unwrap_or_else(|| "—".into());
        let (fg, bg) = row
            .plan
            .as_deref()
            .map(plan_colors)
            .unwrap_or((MUTED, TRACK));
        let badge = centered(
            (cells[3].left + cells[3].right) / 2,
            (rect.top + rect.bottom) / 2,
            64,
            25,
        );
        canvas.round(&badge, 5.0, bg, false);
        canvas.text(fonts.header, fg, &badge, &plan, CENTER);
        let (state, fg, _) = status_of(row);
        dot(canvas, cells[4].left + 8, rect.top + 32, fg);
        label(
            canvas,
            fonts.header,
            fg,
            box_rect(cells[4].left + 24, rect.top + 20, 104, 32),
            state,
        );
        if row.is_active {
            icon(canvas, &l.switch[index], "check", ACCENT);
        } else {
            button(canvas, fonts, &l.switch[index], "切换", false);
        }
        if index + 1 < app.rows.len() {
            canvas.fill(
                &box_rect(
                    rect.left + 8,
                    rect.bottom - 1,
                    rect.right - rect.left - 16,
                    1,
                ),
                LINE,
                1.0,
            );
        }
    }
    canvas.unclip();
    canvas.fill(&box_rect(1, bottom, WIDTH - 2, 1), LINE, 1.0);
    paint_scrollbar(app, canvas);
    dot(canvas, 33, height - 29, ACCENT);
    label(
        canvas,
        fonts.small,
        MUTED,
        box_rect(50, height - 43, WIDTH - 82, 33),
        app.notice.as_deref().unwrap_or("刷新不会中断当前对话"),
    );
}

pub(super) fn paint_compact_new(app: &App, canvas: Canvas<'_>, width: i32, height: i32) {
    let Some(fonts) = &app.fonts else { return };
    let l = compact_layout(app);
    frame(canvas, width, height);
    canvas.round(&box_rect(width / 2 - 22, 7, 44, 4), 2.0, LINE, false);
    canvas.logo(&box_rect(16, 21, 26, 26));
    label(
        canvas,
        fonts.title,
        TEXT,
        box_rect(48, 19, 176, 32),
        "Codex切号器",
    );
    for (area, kind) in [
        (&l.expand, "expand"),
        (&l.pin, if app.docked { "undock" } else { "dock" }),
        (&l.close, "close"),
    ] {
        icon(
            canvas,
            &box_rect(area.left + 4, area.top + 4, 20, 20),
            kind,
            if app.docked && kind == "undock" {
                ACCENT
            } else {
                MUTED
            },
        );
    }
    let current = box_rect(14, 62, width - 28, 144);
    canvas.gradient_round(&current, 11.0, rgb(233, 244, 237), rgb(239, 246, 241));
    canvas.round(&current, 11.0, rgb(224, 236, 227), true);
    if app
        .context_row
        .is_some_and(|i| app.rows.get(i).is_some_and(|r| r.is_active))
    {
        canvas.round(&current, 11.0, ACCENT, true);
    }
    let half = (width - 64) / 2;
    if let Some(row) = app.rows.iter().find(|r| r.is_active) {
        label(
            canvas,
            fonts.body,
            TEXT,
            box_rect(26, 73, width - 52, 26),
            &shown(app, row),
        );
        label(
            canvas,
            fonts.small,
            ACCENT,
            box_rect(26, 101, width - 52, 21),
            &format!(
                "当前 · {} · 剩余 {} 天 · 可重置 {} 次",
                row.plan
                    .as_deref()
                    .map(plan_name)
                    .unwrap_or_else(|| "—".into()),
                days_text(row),
                credits_text(row)
            ),
        );
        label(
            canvas,
            fonts.small,
            MUTED,
            box_rect(26, 131, half, 18),
            "5 小时",
        );
        label(
            canvas,
            fonts.small,
            MUTED,
            box_rect(38 + half, 131, half, 18),
            "每周",
        );
        quota(canvas, fonts, box_rect(26, 152, half, 44), row, false, true);
        quota(
            canvas,
            fonts,
            box_rect(38 + half, 152, half, 44),
            row,
            true,
            true,
        );
    } else {
        label(
            canvas,
            fonts.body,
            TEXT,
            box_rect(26, 83, width - 52, 28),
            "还没有当前账号",
        );
        label(
            canvas,
            fonts.small,
            MUTED,
            box_rect(26, 122, width - 52, 54),
            "点击下方添加账号开始使用",
        );
    }
    let bottom = height - COMPACT_FOOTER;
    canvas.clip(&box_rect(
        10,
        COMPACT_HEADER,
        width - 20,
        bottom - COMPACT_HEADER,
    ));
    let last = app.rows.iter().rposition(|r| !r.is_active);
    for (index, card) in l.cards.iter().enumerate() {
        if app.rows[index].is_active || card.bottom <= COMPACT_HEADER || card.top >= bottom {
            continue;
        }
        let row = &app.rows[index];
        if app.context_row == Some(index)
            || matches!(app.compact_hover, Some(CompactHit::Card(i)) if i == index)
        {
            canvas.round(card, 8.0, HOVER, false);
        }
        label(
            canvas,
            fonts.body,
            TEXT,
            box_rect(22, card.top + 8, width - 44, 25),
            &shown(app, row),
        );
        let areas = compact_quota_areas(card);
        thin_quota(canvas, fonts, areas[0], row, false);
        thin_quota(canvas, fonts, areas[1], row, true);
        let plan = row
            .plan
            .as_deref()
            .map(plan_name)
            .unwrap_or_else(|| "—".into());
        label(
            canvas,
            fonts.caption,
            MUTED,
            box_rect(22, card.top + 65, width - 44, 20),
            &format!(
                "{plan} · 剩余 {} 天 · 可重置 {} 次",
                days_text(row),
                credits_text(row)
            ),
        );
        if last != Some(index) {
            canvas.fill(&box_rect(22, card.bottom - 1, width - 44, 1), LINE, 1.0);
        }
    }
    if last.is_none() {
        label(
            canvas,
            fonts.small,
            MUTED,
            box_rect(22, COMPACT_HEADER + 18, width - 44, 30),
            "添加更多账号，即可快速切换",
        );
    }
    canvas.unclip();
    paint_scrollbar(app, canvas);
    canvas.fill(&box_rect(1, bottom, width - 2, 1), LINE, 1.0);
    label(canvas, fonts.small, ACCENT, l.add, "＋ 添加账号");
    icon(
        canvas,
        &centered(
            (l.refresh_all.left + l.refresh_all.right) / 2,
            (l.refresh_all.top + l.refresh_all.bottom) / 2,
            16,
            16,
        ),
        "refresh",
        MUTED,
    );
    icon(
        canvas,
        &centered(
            (l.privacy.left + l.privacy.right) / 2,
            (l.privacy.top + l.privacy.bottom) / 2,
            16,
            16,
        ),
        if app.masked { "hidden" } else { "eye" },
        MUTED,
    );
}

/// Quotas fill the row width, with a clear gap and the weekly track on the right.
pub(super) fn compact_quota_areas(card: &RECT) -> [RECT; 2] {
    let left = card.left + 8;
    let right = card.right - 8;
    let gap = 24;
    let width = (right - left - gap) / 2;
    [
        box_rect(left, card.top + 34, width, 26),
        box_rect(right - width, card.top + 34, width, 26),
    ]
}

fn paint_scrollbar(app: &App, canvas: Canvas<'_>) {
    if let Some(bar) = scrollbar(app) {
        canvas.round(&bar.track, 3.0, TRACK, false);
        canvas.round(
            &bar.thumb,
            3.0,
            if app.scroll_drag.is_some() {
                ACCENT
            } else {
                rgb(178, 190, 180)
            },
            false,
        );
    }
}

pub(super) fn thin_quota(
    canvas: Canvas<'_>,
    fonts: &Fonts,
    area: RECT,
    row: &RowView,
    weekly: bool,
) {
    let value = if weekly { row.weekly } else { row.five_hour };
    let reset = if weekly {
        row.weekly_reset
    } else {
        row.five_hour_reset
    };
    let text_area = box_rect(area.left, area.top, area.right - area.left, 20);
    let fg = quota_color(row, value);
    // Caption font fits both labels and the longest reset date at 350 DIPs.
    canvas.text(
        fonts.caption,
        fg,
        &text_area,
        &format!(
            "{}·{}",
            if weekly { "7d" } else { "5h" },
            quota_text(row, value)
        ),
        LEFT,
    );
    canvas.text(
        fonts.caption,
        MUTED,
        &text_area,
        &reset_text(reset, weekly),
        DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
    );
    let track = box_rect(area.left, area.top + 23, area.right - area.left, 3);
    canvas.round(&track, 1.5, TRACK, false);
    if !row.loading && row.error.is_none() {
        if let Some(p) = value {
            let width = (track.right - track.left) * p.min(100) as i32 / 100;
            if width > 0 {
                canvas.round(
                    &box_rect(track.left, track.top, width, 3),
                    1.5,
                    bar_color(p),
                    false,
                );
            }
        }
    }
}
