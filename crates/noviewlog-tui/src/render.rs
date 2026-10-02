//! ANSI frame painter: tab bar, content rows with severity cues, and the
//! status/input lines. Layout lives here so mouse hit-testing can reuse it
//! (row 0 = tab bar; rows 1.. = content lines in paint order).
//!
//! Painting is diff-based: every logical row is rendered into a buffer and
//! written only when its bytes differ from the previous frame. Rewriting an
//! unchanged line is what makes emulators flicker, so a static screen must
//! produce zero output.

use std::io::Write;

use crossterm::style::{Color, Print, SetBackgroundColor, SetForegroundColor};
use crossterm::{cursor::MoveTo, queue, terminal::Clear, terminal::ClearType};

use noviewlog_core::core::types::{FlatLine, LogLevel, TextSegment};

use crate::App;

fn level_color(level: LogLevel) -> Color {
    match level {
        LogLevel::Error => Color::Red,
        LogLevel::Warn => Color::Yellow,
        LogLevel::Info => Color::DarkGreen,
        LogLevel::Debug => Color::Grey,
    }
}

fn level_tag(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Error => "E",
        LogLevel::Warn => "W",
        LogLevel::Info => "I",
        LogLevel::Debug => "D",
    }
}

fn rgb(c: (u8, u8, u8)) -> Color {
    Color::Rgb {
        r: c.0,
        g: c.1,
        b: c.2,
    }
}

/// Append one row (MoveTo + line clear + content) to `buf`.
/// Colors are set explicitly per segment — never inherited from the previous
/// row or segment, or unstyled text prints in the last used color.
fn build_row(buf: &mut Vec<u8>, y: u16, paint: impl FnOnce(&mut Vec<u8>)) {
    let _ = queue!(buf, MoveTo(0, y), Clear(ClearType::CurrentLine));
    paint(buf);
    let _ = queue!(
        buf,
        SetForegroundColor(Color::Reset),
        SetBackgroundColor(Color::Reset)
    );
}

/// Append segments; chars within `hl` (start..end col) get the selection
/// background. Chars are printed in runs of the same style.
fn queue_segments(
    buf: &mut Vec<u8>,
    segments: &[TextSegment],
    cols: usize,
    hl: Option<(usize, usize)>,
) {
    let mut col = 0usize;
    // SGR background persists across cells until changed: leaving the
    // selected span must emit an explicit reset, or every following cell of
    // the row would stay DarkBlue and the highlight would grow to EOL.
    let mut selected_prev = false;
    for seg in segments {
        if col >= cols {
            break;
        }
        for ch in seg.text.chars() {
            if col >= cols {
                break;
            }
            let selected = hl.is_some_and(|(s, e)| col >= s && col < e);
            let fg = match &seg.style {
                Some(style) => style.fg.map(rgb).unwrap_or(Color::Reset),
                None => Color::Reset,
            };
            let _ = if selected {
                queue!(
                    buf,
                    SetForegroundColor(fg),
                    SetBackgroundColor(Color::DarkBlue),
                    Print(ch)
                )
            } else if selected_prev {
                queue!(
                    buf,
                    SetBackgroundColor(Color::Reset),
                    SetForegroundColor(fg),
                    Print(ch)
                )
            } else {
                queue!(buf, SetForegroundColor(fg), Print(ch))
            };
            selected_prev = selected;
            // Wide glyphs occupy two cells in the emulator; zero-width marks
            // occupy none — count cells, not chars, so selection spans and
            // truncation line up with what the user sees (#199).
            col += noviewlog_terminal::terminal::width::char_width(ch);
        }
    }
    let _ = queue!(buf, SetBackgroundColor(Color::Reset));
}

/// Display-cell width of a label: the emulator advances the cursor in cells,
/// so hit spans must count the same cells the paint cursor moved (wide CJK
/// chars take two cells), not `chars().count()` (#241).
fn label_width(s: &str) -> u16 {
    s.chars()
        .map(|ch| noviewlog_terminal::terminal::width::char_width(ch) as u16)
        .fold(0u16, u16::saturating_add)
}

/// Span of the "+" (new-tab) button starting at cell `x`: drawn only when
/// all three cells fit, ending at the last column at the latest (`x + 3 <=
/// cols`) — the old `x + 3 < cols` bound suppressed it one column early
/// (#254).
fn tab_add_span(x: u16, cols: u16) -> Option<(u16, u16)> {
    if x.saturating_add(3) <= cols {
        Some((x, 3))
    } else {
        None
    }
}

/// Selection column span for content row `i`, from the drag state.
fn highlight_span(app: &App, i: usize) -> Option<(usize, usize)> {
    let a = app.sel_anchor?;
    let c = app.sel_current?;
    let ((r0, c0), (r1, c1)) = if a <= c { (a, c) } else { (c, a) };
    if i < r0 || i > r1 {
        return None;
    }
    Some((
        if i == r0 { c0 } else { 0 },
        if i == r1 { c1 } else { usize::MAX },
    ))
}

/// Diff-based frame paint. `lines` is the visible slice from the engine.
/// `full` forces one full repaint (used on menu open/close, since the menu
/// is a floating overlay outside the steady row layout).
pub fn frame(
    out: &mut impl Write,
    app: &mut App,
    lines: &[FlatLine],
    full: bool,
) -> std::io::Result<()> {
    let cols = usize::from(app.cols.max(1));
    let content_rows = app.content_rows().max(1) as usize;
    let mut rows: Vec<Vec<u8>> = Vec::new();
    if full {
        queue!(out, Clear(ClearType::All))?;
        app.frame_prev.clear();
    }

    // Row 0: tab bar with tabs then a "+" (new filter tab). Column spans are
    // recorded for mouse hit-testing. The labels are queued via the
    // build_row paint closure: emitting them before build_row would print
    // them at the stale cursor position and then have the row cleared.
    let mut row0 = Vec::new();
    let mut x: u16 = 0;
    let tabs: Vec<(usize, String, bool)> = app
        .stats
        .as_ref()
        .map(|s| {
            s.tabs
                .iter()
                .map(|t| (t.index, t.name.clone(), t.index == s.active_tab))
                .collect()
        })
        .unwrap_or_default();
    app.tab_spans.clear();
    let paint_tabs = |buf: &mut Vec<u8>| {
        for (index, name, active) in &tabs {
            let label = if *active {
                format!(" [{name}] ")
            } else {
                format!("  {name}  ")
            };
            if x >= cols as u16 {
                break;
            }
            let color = if *active {
                Color::White
            } else {
                Color::DarkGrey
            };
            let width = label_width(&label);
            let _ = queue!(buf, SetForegroundColor(color), Print(&label));
            app.tab_spans.push((x, width, *index));
            x = x.saturating_add(width);
        }
        if let Some((x, len)) = tab_add_span(x, cols as u16) {
            let _ = queue!(buf, SetForegroundColor(Color::Cyan), Print(" + "));
            app.tab_add_span = Some((x, len));
        } else {
            app.tab_add_span = None;
        }
    };
    build_row(&mut row0, 0, paint_tabs);
    rows.push(row0);

    // Content rows 1..1+content_rows.
    app.row_records.clear();
    for (i, line) in lines.iter().enumerate().take(content_rows) {
        app.row_records.push(Some(line.record_id));
        let hl = highlight_span(app, i);
        let mut buf = Vec::new();
        let content = |buf: &mut Vec<u8>| {
            let mark = if line.collapsible {
                let _ = queue!(buf, SetForegroundColor(Color::DarkGrey));
                if line.collapsed {
                    '+'
                } else {
                    '-'
                }
            } else {
                ' '
            };
            let _ = queue!(buf, Print(mark));
            if let Some(level) = line.level {
                let _ = queue!(
                    buf,
                    SetForegroundColor(level_color(level)),
                    Print(level_tag(level))
                );
            } else {
                let _ = queue!(buf, SetForegroundColor(Color::Reset), Print(' '));
            }
            queue_segments(buf, &line.segments, cols.saturating_sub(2), hl);
        };
        build_row(&mut buf, (i + 1) as u16, content);
        rows.push(buf);
    }
    // Rows the previous frame used but this one doesn't (content shrank):
    // emit clear-row buffers; the diff writes them exactly once. A previous
    // frame is [tab, N content, input, status], so its content occupied
    // rows 0..prev_len-3 — the old `-2` bound cleared into the input row
    // and misaligned the frame diff (#199).
    let prev_len = app.frame_prev.len();
    for row in lines.len().min(content_rows)..prev_len.saturating_sub(3) {
        let y = (row + 1) as u16;
        if y.saturating_add(1) < app.rows {
            let mut buf = Vec::new();
            let _ = queue!(buf, MoveTo(0, y), Clear(ClearType::CurrentLine));
            rows.push(buf);
        }
    }

    // Input line (second to last): filter prompt while focused, else a hint.
    let status_row = app.rows.saturating_sub(1);
    let input_row = status_row.saturating_sub(1);
    let mut input_buf = Vec::new();
    build_row(&mut input_buf, input_row, |buf| {
        if app.input_focus {
            // When the buffer outgrows the line, show its tail so the caret
            // marker (`_`) always sits at the true end of the input.
            let prefix = "filter include: ";
            let suffix = "_  (Enter apply, Esc cancel)";
            let body_cols = cols
                .saturating_sub(usize::from(label_width(prefix)))
                .saturating_sub(usize::from(label_width(suffix)));
            let _ = queue!(
                buf,
                SetForegroundColor(Color::DarkCyan),
                Print(format!(
                    "{prefix}{}{suffix}",
                    truncate_tail(&app.filter_buf, body_cols)
                ))
            );
        } else if let Some(banner) = &app.exited {
            let _ = queue!(
                buf,
                SetForegroundColor(Color::Yellow),
                Print(truncate(
                    &format!("{banner} — R reconnect · N new · Q quit"),
                    cols
                ))
            );
        } else if app.confirm_quit {
            let _ = queue!(
                buf,
                SetForegroundColor(Color::Yellow),
                Print(truncate("Quit NoViewLog? Ctrl+Q again to confirm", cols))
            );
        } else {
            let _ = queue!(
                buf,
                SetForegroundColor(Color::DarkCyan),
                Print(truncate(hint_text(), cols))
            );
        }
    });
    rows.push(input_buf);

    let status = match app.stats.as_ref() {
        Some(s) => format!(
            "{} | lines {} | follow {} | filters {}{}",
            if s.status.is_empty() {
                if s.running {
                    "running"
                } else {
                    "stopped"
                }
            } else {
                &s.status
            },
            s.lines,
            if s.auto_follow { "on" } else { "off" },
            s.filters.len(),
            if s.dropped > 0 {
                format!(" | dropped {}", s.dropped)
            } else {
                String::new()
            }
        ),
        None => "starting…".to_string(),
    };
    let mut status_buf = Vec::new();
    build_row(&mut status_buf, status_row, |buf| {
        let _ = queue!(
            buf,
            SetForegroundColor(Color::DarkGrey),
            Print(status_line(app.session_label.as_deref(), &status, cols))
        );
    });
    rows.push(status_buf);

    // Write only rows whose bytes changed.
    for (i, buf) in rows.iter().enumerate() {
        if app.frame_prev.get(i) != Some(buf) {
            out.write_all(buf)?;
        }
    }
    app.frame_prev = rows;

    // Context menu overlay (drawn last, on top).
    if let Some(menu) = &app.menu {
        let width = usize::from(crate::App::menu_width(menu.col, app.cols.max(1)));
        let mut buf = Vec::new();
        let _ = queue!(
            buf,
            SetForegroundColor(Color::White),
            SetBackgroundColor(Color::DarkBlue)
        );
        let top: String = format!("+{:-<width$}+", "");
        let _ = queue!(buf, MoveTo(menu.col, menu.row), Print(top));
        for (idx, item) in menu.items.iter().enumerate() {
            // Truncate before the fill format: `{:<width$}` never shortens,
            // so an over-long item would expand past the box (#241).
            let line = format!("| {:<width$} |", truncate(item, width.saturating_sub(2)));
            let _ = queue!(
                buf,
                MoveTo(menu.col, menu.row + idx as u16 + 1),
                Print(line)
            );
        }
        let bottom: String = format!("+{:-<width$}+", "");
        let _ = queue!(
            buf,
            MoveTo(menu.col, menu.row + menu.items.len() as u16 + 1),
            Print(bottom)
        );
        let _ = queue!(buf, SetBackgroundColor(Color::Reset));
        out.write_all(&buf)?;
    }

    // Connect overlay: centered profile list (same geometry as
    // App::connect_geo — 40 cols, items = profiles + "local shell").
    if app.connect_open {
        let items = app.connect_items();
        let (col, row, count) = app.connect_geo();
        let width = usize::from(crate::App::connect_box_width(col, app.cols.max(1)));
        let mut buf = Vec::new();
        let _ = queue!(
            buf,
            SetForegroundColor(Color::White),
            SetBackgroundColor(Color::DarkBlue)
        );
        let top: String = format!("+{:-<width$}+", "");
        let _ = queue!(buf, MoveTo(col, row), Print(top));
        let title = if app.profiles.is_empty() {
            " no ssh profiles ".to_string()
        } else {
            " connect ".to_string()
        };
        // Truncate before the centering fill: `{:^width$}` never shortens,
        // so a title wider than the box would expand past it and wrap (#241).
        let title = truncate(&title, width);
        let _ = queue!(
            buf,
            MoveTo(col, row + 1),
            Print(format!("|{title:^width$}|"))
        );
        let sel = app.connect_sel.min(count.saturating_sub(1));
        for (idx, item) in items.iter().enumerate().take(count) {
            // The selected row paints reversed with a `>` marker; the marker
            // replaces a pad space so the box geometry (and mouse hit test)
            // stays identical.
            let selected = idx == sel;
            let marker = if selected { '>' } else { ' ' };
            let body = format!("{marker}{}", truncate(item, width.saturating_sub(3)));
            let line = format!("| {:<width$} |", body);
            let _ = queue!(buf, MoveTo(col, row + idx as u16 + 2));
            if selected {
                let _ = queue!(
                    buf,
                    SetForegroundColor(Color::Black),
                    SetBackgroundColor(Color::White),
                    Print(line),
                    SetForegroundColor(Color::White),
                    SetBackgroundColor(Color::DarkBlue)
                );
            } else {
                let _ = queue!(buf, Print(line));
            }
        }
        let bottom: String = format!("+{:-<width$}+", "");
        let _ = queue!(buf, MoveTo(col, row + count as u16 + 2), Print(bottom));
        // Empty-profile hint: the actual config path on a line below the box
        // (not interactive — a click there is an outside click, same as Esc).
        if app.profiles.is_empty() {
            let body = truncate_tail(&crate::config_path_label(), width.saturating_sub(2));
            let _ = queue!(
                buf,
                MoveTo(col, row + count as u16 + 3),
                Print(format!(" {body}"))
            );
        }
        let _ = queue!(buf, SetBackgroundColor(Color::Reset));
        out.write_all(&buf)?;
    }
    Ok(())
}

/// Idle hint line: only shortcuts that actually exist. ASCII only, short
/// enough for an 80-column terminal (the painter truncates cell-safely).
fn hint_text() -> &'static str {
    "wheel=scroll drag=copy Ctrl+F filter Ctrl+W close Alt+N tab Ctrl+Q quit"
}

fn truncate(s: &str, cols: usize) -> String {
    // Cut by cell width, not char count: CJK glyphs take two columns (#199).
    let mut out = String::new();
    let mut width = 0usize;
    for ch in s.chars() {
        let w = noviewlog_terminal::terminal::width::char_width(ch);
        if width + w > cols {
            break;
        }
        out.push(ch);
        width += w;
    }
    out
}

/// Last `cols` display cells of `s` (char-boundary safe, wide-char safe).
/// A glyph wider than the remaining budget is dropped whole, never split.
fn truncate_tail(s: &str, cols: usize) -> String {
    let mut picked: Vec<char> = Vec::new();
    let mut width = 0usize;
    for ch in s.chars().rev() {
        let w = noviewlog_terminal::terminal::width::char_width(ch);
        if width + w > cols {
            break;
        }
        width += w;
        picked.push(ch);
    }
    picked.reverse();
    picked.into_iter().collect()
}

/// Status bar composition: an optional session label before the engine
/// status, with the label tail-truncated so the right-side counters always
/// stay inside `cols`.
fn status_line(label: Option<&str>, status: &str, cols: usize) -> String {
    let Some(label) = label.filter(|l| !l.is_empty()) else {
        return truncate(status, cols);
    };
    let budget = cols
        .saturating_sub(usize::from(label_width(status)))
        .saturating_sub(3); // " | "
    format!("{} | {}", truncate_tail(label, budget), status)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cells(s: &str) -> usize {
        s.chars()
            .map(noviewlog_terminal::terminal::width::char_width)
            .sum()
    }

    #[test]
    fn label_width_counts_display_cells() {
        // Wide CJK glyphs occupy two cells: span width is 2× chars, matching
        // the cursor advance the emulator applies (#241).
        assert_eq!(label_width("日志"), 4);
        assert_eq!(label_width(" [日志] "), 8);
        assert_eq!(label_width("abc"), 3);
    }

    #[test]
    fn label_width_saturates_instead_of_overflowing() {
        // A label wider than u16::MAX cells must saturate, not wrap/panic
        // in debug builds (#254).
        let huge = "a".repeat(70_000);
        assert_eq!(label_width(&huge), u16::MAX);
    }

    #[test]
    fn tab_add_span_fits_at_last_column() {
        // "+" ending exactly at the last column renders; one cell later it
        // does not (#254).
        assert_eq!(tab_add_span(7, 10), Some((7, 3)));
        assert_eq!(tab_add_span(0, 3), Some((0, 3)));
        assert_eq!(tab_add_span(8, 10), None);
        assert_eq!(tab_add_span(u16::MAX, 10), None);
    }

    #[test]
    fn truncate_connect_title_stays_in_box() {
        // The 42-char connect title at a 38-wide box: truncation before the
        // centering fill keeps the drawn line inside the overlay (#241).
        let title = " no ssh profiles — add tui_ssh_profiles ".to_string();
        assert!(cells(&title) > 38);
        let cut = truncate(&title, 38);
        assert!(cells(&cut) <= 38);
        let line = format!("|{cut:^38}|");
        assert!(cells(&line) <= 40);
    }

    #[test]
    fn truncate_is_char_boundary_safe() {
        // Wide chars at an odd width: stop before the glyph, never mid-char.
        let cut = truncate("日日日", 3);
        assert_eq!(cut, "日");
        assert!(std::str::from_utf8(cut.as_bytes()).is_ok());
    }

    #[test]
    fn hint_line_fits_80_cols_and_is_ascii() {
        // The hint must fit an 80-column terminal in full and degrade safely
        // on narrower ones (cell-based truncation, ASCII source).
        let hint = hint_text();
        assert!(hint.is_ascii(), "hint must be ASCII: {hint:?}");
        assert!(cells(hint) <= 80, "hint is {} cells: {hint:?}", cells(hint));
        for width in [10usize, 40, 71, 79, 80] {
            let cut = truncate(hint, width);
            assert!(cells(&cut) <= width);
        }
        assert_eq!(truncate(hint, 80), hint);
    }

    #[test]
    fn truncate_tail_keeps_the_last_cells() {
        assert_eq!(truncate_tail("hello world", 5), "world");
        // Fits: unchanged.
        assert_eq!(truncate_tail("abc", 10), "abc");
        // Zero budget: empty.
        assert_eq!(truncate_tail("abc", 0), "");
    }

    #[test]
    fn truncate_tail_is_wide_char_safe() {
        // "日志ab" = 6 cells; last 4 cells = "志ab" (a straddling 日 is
        // dropped whole, never split).
        assert_eq!(truncate_tail("日志ab", 4), "志ab");
        // Odd budget on wide glyphs: only glyphs that fully fit.
        assert_eq!(truncate_tail("日日", 3), "日");
    }

    #[test]
    fn filter_input_tail_shows_end_of_long_buffer() {
        // 200-char paste on an 80-col line: the prefix, the visible tail and
        // the caret suffix must together fit the line, and the tail must end
        // at the true end of the buffer (the caret sits on the last char).
        let prefix = "filter include: ";
        let suffix = "_  (Enter apply, Esc cancel)";
        let cols = 80usize;
        let body_cols = cols
            .saturating_sub(usize::from(label_width(prefix)))
            .saturating_sub(usize::from(label_width(suffix)));
        let buf: String = (0..200)
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect();
        let line = format!("{prefix}{}{suffix}", truncate_tail(&buf, body_cols));
        assert!(cells(&line) <= cols, "line is {} cells", cells(&line));
        assert!(line.starts_with(prefix));
        assert!(line.ends_with(suffix));
        // The tail ends with the buffer's last char, right before the caret.
        let body = &line[prefix.len()..line.len() - suffix.len()];
        assert_eq!(body.len(), body_cols);
        assert!(buf.ends_with(body), "rendered body must be the buffer tail");
        assert_eq!(body.chars().last(), buf.chars().last());
    }

    #[test]
    fn status_line_without_label_is_plain_truncation() {
        assert_eq!(
            status_line(None, "running | lines 3", 80),
            "running | lines 3"
        );
        // An empty label behaves like no label.
        assert_eq!(status_line(Some(""), "running", 80), "running");
    }

    #[test]
    fn status_line_keeps_counters_with_long_label() {
        // A label wider than the line is tail-truncated; the engine status
        // (with the right-side counters) must survive in full.
        let label = "ssh very-long-profile-name-on-a-slow-host (user@host.example.com)";
        let status = "running | lines 1234 | follow on | filters 2 | dropped 0";
        let line = status_line(Some(label), status, 80);
        assert!(cells(&line) <= 80, "status line is {} cells", cells(&line));
        assert!(line.ends_with(status), "counters must be intact: {line:?}");
        assert!(
            line.contains(" | "),
            "label and status joined by a separator"
        );
    }
}
