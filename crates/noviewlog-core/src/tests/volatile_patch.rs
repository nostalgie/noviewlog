use crate::core::buffer::RecordBuffer;
use crate::core::formats::get_builtin_format;
use crate::core::parser::RecordParser;
use crate::core::terminal::TerminalIngest;
use crate::core::types::LogRecord;
use crate::log_view::LogView;
use chrono::Utc;

fn seed_scrollback(buffer: &mut RecordBuffer, n: usize) {
    for i in 0..n {
        buffer.add(LogRecord {
            id: i as u64,
            lines: vec![format!("line-{i}")],
            text: format!("line-{i}"),
            received_at: Utc::now(),
            level: None,
            overwrite: false,
        });
    }
}

fn apply_overlay(view: &mut LogView, ingest: &TerminalIngest) {
    view.set_live_overlay(ingest.overlay_flat_lines());
}

#[test]
fn many_single_byte_feeds_patch_without_marking_dirty() {
    let mut buffer = RecordBuffer::new(50_000);
    seed_scrollback(&mut buffer, 5_000);
    let format = get_builtin_format("generic");
    let mut parser = RecordParser::new(format);
    let mut ingest = TerminalIngest::new_with_size(80, 24);
    ingest.ensure_live_screen(&mut buffer);

    let mut view = LogView::from_runtime(crate::TERMINAL_TAB_NAME, Vec::new());
    view.rebuild(&mut buffer);
    apply_overlay(&mut view, &ingest);
    assert_eq!(view.flat_lines_record_cursor, buffer.records_len());
    let stable_prefix = view.flat_lines.len() - ingest.volatile_count();
    assert_eq!(stable_prefix, 5_000);

    for b in b"hello world!!!" {
        let old_vol = ingest.volatile_count();
        let old_total = buffer.records_len();
        let (shifted_rec, shifted) = ingest.feed(&[*b], &mut buffer, &mut parser);
        let new_total = buffer.records_len();
        let overlay = ingest.overlay_flat_lines();
        let new_vol = overlay.len();
        assert_eq!(new_total, old_total, "echo must not create Records");
        assert!(
            view.try_patch_committed_and_overlay(
                &mut buffer,
                old_vol,
                old_total,
                &overlay,
                new_total,
                shifted_rec,
                shifted
            ),
            "patch should succeed for unfiltered Terminal tab"
        );
        assert_eq!(view.flat_lines_record_cursor, new_total);
        assert_eq!(
            view.flat_lines.len().saturating_sub(new_vol),
            stable_prefix,
            "stable prefix length must stay put"
        );
        let before_len = view.flat_lines.len();
        view.rebuild(&mut buffer);
        assert_eq!(
            view.flat_lines.len(),
            before_len,
            "rebuild after successful patch must be a no-op on length"
        );
    }
}

#[test]
fn patch_survives_ring_trim_under_cap() {
    let mut buffer = RecordBuffer::new(100);
    seed_scrollback(&mut buffer, 100);
    let format = get_builtin_format("generic");
    let mut parser = RecordParser::new(format);
    let mut ingest = TerminalIngest::new_with_size(80, 10);
    ingest.ensure_live_screen(&mut buffer);

    let mut view = LogView::from_runtime(crate::TERMINAL_TAB_NAME, Vec::new());
    view.rebuild(&mut buffer);
    apply_overlay(&mut view, &ingest);

    let flood = "x".repeat(70) + "\r\n";
    for _ in 0..50 {
        let old_vol = ingest.volatile_count();
        let old_total = buffer.records_len();
        let (shifted_rec, shifted) = ingest.feed(flood.as_bytes(), &mut buffer, &mut parser);
        let overlay = ingest.overlay_flat_lines();
        let new_total = buffer.records_len();
        assert!(
            view.try_patch_committed_and_overlay(
                &mut buffer,
                old_vol,
                old_total,
                &overlay,
                new_total,
                shifted_rec,
                shifted
            ) || {
                view.mark_flat_lines_dirty();
                view.rebuild(&mut buffer);
                apply_overlay(&mut view, &ingest);
                true
            },
            "patch or dirty rebuild must keep flat lines coherent"
        );
        assert_eq!(view.flat_lines_record_cursor, buffer.records_len());
        assert!(buffer.records_len() <= 100);
    }
}

#[test]
fn filter_tab_does_not_see_uncommitted_live_screen() {
    use crate::core::types::{compile_filter, FilterRule, FilterType};

    let mut buffer = RecordBuffer::new(50_000);
    seed_scrollback(&mut buffer, 100);
    let format = get_builtin_format("generic");
    let mut parser = RecordParser::new(format);
    let mut ingest = TerminalIngest::new_with_size(80, 24);
    ingest.ensure_live_screen(&mut buffer);

    let include = compile_filter(FilterRule {
        id: "inc-1".into(),
        name: None,
        filter_type: FilterType::Include,
        pattern: "line-".into(),
        enabled: true,
        use_regex: false,
        regex: None,
    });
    let mut filter_tab = LogView::from_runtime("Tab 1", vec![include]);
    filter_tab.rebuild(&mut buffer);
    let before = filter_tab.flat_lines.len();
    assert!(before > 0);

    ingest.feed(b"spinner-frame-aaaa", &mut buffer, &mut parser);
    assert_eq!(
        buffer.records_len(),
        100,
        "live screen must not add Records"
    );
    filter_tab.rebuild(&mut buffer);
    assert_eq!(
        filter_tab.flat_lines.len(),
        before,
        "filter tab must not see uncommitted spinner/prompt frames"
    );
}

#[test]
fn failed_patch_does_not_strip_overlay() {
    use crate::core::types::{compile_filter, FilterRule, FilterType};

    let mut buffer = RecordBuffer::new(50_000);
    seed_scrollback(&mut buffer, 50);
    let mut ingest = TerminalIngest::new_with_size(80, 24);
    ingest.ensure_live_screen(&mut buffer);

    let include = compile_filter(FilterRule {
        id: "inc-1".into(),
        name: None,
        filter_type: FilterType::Include,
        pattern: "line-".into(),
        enabled: true,
        use_regex: false,
        regex: None,
    });
    // Filters on Terminal tab force patch to fail (must not mutate first).
    let mut view = LogView::from_runtime(crate::TERMINAL_TAB_NAME, vec![include]);
    view.rebuild(&mut buffer);
    apply_overlay(&mut view, &ingest);
    let before = view.flat_lines.len();
    assert!(before > 0);
    let overlay = ingest.overlay_flat_lines();
    let overlay_n = view.overlay_len();
    let recs = buffer.records_len();
    assert!(
        !view.try_patch_committed_and_overlay(&mut buffer, overlay_n, recs, &overlay, recs, 0, 0),
        "filtered view must refuse patch"
    );
    assert_eq!(
        view.flat_lines.len(),
        before,
        "failed patch must not drop the live overlay (Follow jump)"
    );
}

#[test]
fn first_patch_with_zero_overlay_len_keeps_committed_prefix() {
    let mut buffer = RecordBuffer::new(50_000);
    seed_scrollback(&mut buffer, 80);
    let format = get_builtin_format("generic");
    let mut parser = RecordParser::new(format);
    let mut ingest = TerminalIngest::new_with_size(80, 24);
    ingest.ensure_live_screen(&mut buffer);

    let mut view = LogView::from_runtime(crate::TERMINAL_TAB_NAME, Vec::new());
    view.rebuild(&mut buffer);
    assert_eq!(view.overlay_len(), 0);
    assert_eq!(view.flat_lines.len(), 80);

    let (shifted_rec, shifted) = ingest.feed(b"hello", &mut buffer, &mut parser);
    let overlay = ingest.overlay_flat_lines();
    let new_total = buffer.records_len();
    assert!(
        view.try_patch_committed_and_overlay(
            &mut buffer,
            0,
            80,
            &overlay,
            new_total,
            shifted_rec,
            shifted
        ),
        "first overlay apply must patch"
    );
    assert_eq!(
        view.flat_lines.len().saturating_sub(view.overlay_len()),
        80,
        "must not treat ingest overlay cache as already on the view"
    );
}

#[test]
fn patch_survives_multiline_record_ring_trim() {
    // Regression for issue #187: a ring trim that drops one multiline record
    // removes several flat lines. The patch must drain the flat-line count
    // while re-appending records from the record-space cursor.
    let mut buffer = RecordBuffer::new(30);
    let format = get_builtin_format("generic");
    let mut parser = RecordParser::new(format);
    let mut ingest = TerminalIngest::new_with_size(80, 5);
    ingest.ensure_live_screen(&mut buffer);

    // Seed with 3-line records so record count and flat-line count diverge.
    for i in 0..30 {
        buffer.add(LogRecord {
            id: i as u64,
            lines: vec![format!("a-{i}-1"), format!("a-{i}-2"), format!("a-{i}-3")],
            text: format!("a-{i}"),
            received_at: Utc::now(),
            level: None,
            overwrite: false,
        });
    }

    let mut view = LogView::from_runtime(crate::TERMINAL_TAB_NAME, Vec::new());
    view.rebuild(&mut buffer);
    apply_overlay(&mut view, &ingest);

    // Flood enough committed lines that multi-line records are ring-trimmed.
    let mut flood = String::new();
    for i in 0..80 {
        flood.push_str(&format!("flood line {i}\r\n"));
    }
    let (shifted_rec, shifted_lines) = ingest.feed(flood.as_bytes(), &mut buffer, &mut parser);
    assert!(shifted_rec > 0, "flood must trim the ring");
    assert!(
        shifted_lines > shifted_rec,
        "multiline records must make flat-line trim exceed record trim"
    );

    let overlay = ingest.overlay_flat_lines();
    let new_total = buffer.records_len();
    let old_total = view.flat_lines_record_cursor;
    assert!(
        view.try_patch_committed_and_overlay(
            &mut buffer,
            view.overlay_len(),
            old_total,
            &overlay,
            new_total,
            shifted_rec,
            shifted_lines
        ),
        "patch must survive a multiline ring trim"
    );

    // Ground truth: a fresh view rebuilt from the same buffer + overlay.
    let mut reference = LogView::from_runtime(crate::TERMINAL_TAB_NAME, Vec::new());
    reference.rebuild(&mut buffer);
    reference.set_live_overlay(overlay);
    assert_eq!(
        view.flat_lines.len(),
        reference.flat_lines.len(),
        "patched flat-line count must match a full rebuild"
    );
    for (patched, expected) in view.flat_lines.iter().zip(reference.flat_lines.iter()) {
        assert_eq!(
            patched.raw, expected.raw,
            "patched line diverged from rebuild"
        );
    }
}
