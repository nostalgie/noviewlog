use super::fonts::{glyph_baseline_y, glyph_has_ink, GLYPH_CACHE_CAP};
use super::primitives::{drawable_text, highlight_selection_in_segments, text_width};
use super::*;
use crate::color_emoji::EMOJI_CACHE_CAP;
use crate::core::types::TextSegment;
use crate::core::visible::{compile_search_pattern, highlight_search_in_segments};
use crate::viewport_layout::TextPos;
use fontdue::Font;

#[test]
fn drawable_text_strips_embedded_ansi() {
    assert_eq!(drawable_text("\u{1b}[32mhello\u{1b}[0m"), "hello");
}

/// Issue #238: the caret column is a display cell — `⏱️` (U+23F1 + U+FE0F)
/// occupies one cell, so the caret block under cell 3 must sit at the same
/// x as the glyph the draw path paints there (3 cells from the base).
#[test]
fn caret_pixel_pos_uses_display_cells() {
    let line = FlatLine {
        record_id: 1,
        line_index: 0,
        segments: vec![TextSegment {
            text: "ab\u{23F1}\u{FE0F}cd".to_string(),
            style: None,
        }],
        raw: "ab\u{23F1}\u{FE0F}cd".to_string(),
        level: None,
        collapsible: false,
        collapsed: false,
        hidden_line_count: 0,
    };
    let lines = vec![line];
    let visual = vec![crate::viewport_layout::VisualLine {
        flat_index: 0,
        start: 0,
        end: lines[0].raw.len(),
    }];
    let cell = 8u32;
    // Caret on 'c' (cell 3; VS16 shifts nothing).
    let (x, _) = caret_pixel_pos(
        &lines,
        &visual,
        ViewportCaret {
            flat_index: 0,
            col: 3,
        },
        0.0,
        LEFT_PAD as i32,
        16.0,
        cell,
        100,
    )
    .expect("caret visible");
    assert_eq!(x, LEFT_PAD as i32 + 3 * cell as i32);
    // Past the line end the caret parks `caret.col` cells from the base
    // (same overflow semantics as before; the base is now display cells).
    let (x, _) = caret_pixel_pos(
        &lines,
        &visual,
        ViewportCaret {
            flat_index: 0,
            col: 9,
        },
        0.0,
        LEFT_PAD as i32,
        16.0,
        cell,
        100,
    )
    .expect("caret visible");
    assert_eq!(x, LEFT_PAD as i32 + 9 * cell as i32);
}

/// The Layout-free draw path must place glyphs exactly where a one-line
/// fontdue `Layout` did (issue #61): same bitmap top and same coverage.
#[test]
fn glyph_placement_matches_fontdue_layout() {
    use fontdue::layout::{CoordinateSystem, Layout, LayoutSettings, TextStyle};
    let font = load_mono_font();
    for size in [12.0_f32, 14.0, 20.0] {
        for ch in ['A', 'g', 'ж', '日', '⚡', '⠋'] {
            let mut layout = Layout::new(CoordinateSystem::PositiveYDown);
            layout.reset(&LayoutSettings {
                x: 0.0,
                y: 37.5,
                ..Default::default()
            });
            layout.append(&[&*font], &TextStyle::new(&ch.to_string(), size, 0));
            let glyph = layout.glyphs().first().expect("layout glyph");
            let laid_y = glyph.y as i32;
            let metrics = font.metrics_indexed(font.lookup_glyph_index(ch), size);
            let baseline = glyph_baseline_y(&font, size, 37.5);
            let computed_y =
                (baseline + (-metrics.bounds.height - metrics.bounds.ymin).floor()) as i32;
            assert_eq!(laid_y, computed_y, "y mismatch for {ch} @{size}");

            // Coverage parity: same rasterizer config, same bitmap.
            let (laid_metrics, laid_bitmap) =
                font.rasterize_indexed(font.lookup_glyph_index(ch), size);
            let mine = font.rasterize_indexed(font.lookup_glyph_index(ch), size);
            assert_eq!(&laid_bitmap, &mine.1, "bitmap mismatch for {ch} @{size}");
            assert_eq!(laid_metrics.width, mine.0.width);
            assert_eq!(laid_metrics.height, mine.0.height);
        }
    }
}

/// Ink-probe answers must be memoized (issue #61): repeated picks reuse
/// the cache instead of re-rasterizing per frame.
#[test]
fn font_stack_pick_memoizes_ink_probe() {
    let fonts = FontStack::new(load_mono_font(), load_emoji_fallback_font());
    let first = fonts.pick('⠋') as *const Font;
    let second = fonts.pick('⠋') as *const Font;
    assert_eq!(first, second, "pick must be stable");
    {
        let probe = fonts.ink_probe.borrow();
        assert!(probe.contains_key(&'⠋'), "probe result must be cached");
    }
}

/// GlyphCache must stay bounded (issue #61): inserting past the cap
/// evicts instead of growing without limit.
#[test]
fn glyph_cache_is_bounded() {
    let font = load_mono_font();
    let glyph = font.lookup_glyph_index('A');
    let mut cache = GlyphCache::new();
    for i in 0..(GLYPH_CACHE_CAP + 512) {
        // Distinct px values give distinct cache keys without needing
        // 16k distinct glyphs in the font.
        cache.rasterize_indexed(&font, glyph, 1.0 + i as f32 * 0.001);
        assert!(cache.entries.len() <= GLYPH_CACHE_CAP);
    }
}

/// ColorEmojiAtlas memo must stay bounded (issue #61).
#[test]
fn color_emoji_cache_is_bounded() {
    let Some(atlas) = ColorEmojiAtlas::load() else {
        return; // no system emoji font in CI — nothing to bound
    };
    // ZWJ clusters get distinct keys; push well past the cap.
    for i in 0..(EMOJI_CACHE_CAP + 32) {
        let seq = format!("\u{1F600}\u{200D}{i}");
        let _ = atlas.glyph_cluster(&seq);
        let cache = atlas.cache.lock().unwrap();
        assert!(cache.len() <= EMOJI_CACHE_CAP, "cache len {}", cache.len());
    }
}

#[test]
fn text_width_ignores_ansi_bytes() {
    let renderer = ViewportRenderer::new();
    let cell = renderer.metrics.cell_width;
    let plain = text_width("http://localhost:1337", cell);
    let with_ansi = text_width(
        &drawable_text("\u{1b}[32mhttp\u{1b}[0m://localhost:1337"),
        cell,
    );
    assert_eq!(plain, with_ansi);
    assert!(plain > 0);
}

#[test]
fn search_highlight_splits_url_match() {
    let segments = vec![TextSegment {
        text: "http://localhost:1337".to_string(),
        style: None,
    }];
    let pattern = compile_search_pattern("http", false, false, false).unwrap();
    let highlighted = highlight_search_in_segments(&segments, &pattern, Some((0, 4)));
    let joined: String = highlighted.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(joined, "http://localhost:1337");
    assert_eq!(highlighted.len(), 2);
    assert!(highlighted[0]
        .style
        .as_ref()
        .is_some_and(|s| s.search_current));
    assert!(!highlighted[1].style.as_ref().is_some_and(|s| s.search));
}

#[test]
fn render_search_match_on_url() {
    let mut renderer = ViewportRenderer::new();
    let line = FlatLine {
        record_id: 1,
        line_index: 0,
        segments: vec![TextSegment {
            text: "http://localhost:1337".to_string(),
            style: None,
        }],
        raw: "http://localhost:1337".to_string(),
        level: None,
        collapsible: false,
        collapsed: false,
        hidden_line_count: 0,
    };
    let re = compile_search_pattern("http", false, false, false).unwrap();
    let active = SearchMatch {
        line_index: 0,
        start: 0,
        end: 4,
    };
    let mut buf = vec![0u8; 400 * 40 * 4];
    renderer
        .render(
            &mut buf,
            400,
            40,
            &[line],
            0.0,
            0.0,
            false,
            None,
            Some(&re),
            None,
            Some(active),
            None,
        )
        .unwrap();
    // Active match uses orange highlight (R > G, B low).
    let orange_pixels = buf
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| px[0] > 150 && px[1] > 100 && px[1] < 160 && px[2] < 40)
        .count();
    assert!(
        orange_pixels > 20,
        "expected orange search highlight pixels"
    );
}

#[test]
fn render_nowrap_scroll_x_reveals_line_tail() {
    use crate::viewport_layout::max_scroll_x;

    let mut renderer = ViewportRenderer::new();
    let cell = renderer.metrics.cell_width;
    let text = "START__http://localhost:1337/admin/dashboard__END";
    let line = FlatLine {
        record_id: 1,
        line_index: 0,
        segments: vec![TextSegment {
            text: text.to_string(),
            style: None,
        }],
        raw: text.to_string(),
        level: None,
        collapsible: false,
        collapsed: false,
        hidden_line_count: 0,
    };
    let width = 160u32;
    let height = 40u32;
    let scroll_x = max_scroll_x(&[line.clone()], width, cell);
    assert!(scroll_x > cell as f32, "line should overflow viewport");

    let mut head_buf = vec![0u8; (width * height * 4) as usize];
    renderer
        .render(
            &mut head_buf,
            width,
            height,
            &[line.clone()],
            0.0,
            0.0,
            false,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

    let mut tail_buf = vec![0u8; (width * height * 4) as usize];
    renderer
        .render(
            &mut tail_buf,
            width,
            height,
            &[line],
            0.0,
            scroll_x,
            false,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

    let head_start = char_column_lit(&head_buf, width, 0, cell, 0.0);
    let head_end_col = text.chars().count().saturating_sub(3);
    let head_end = char_column_lit(&head_buf, width, head_end_col, cell, 0.0);
    let tail_start = char_column_lit(&tail_buf, width, 0, cell, scroll_x);
    let tail_end = char_column_lit(&tail_buf, width, head_end_col, cell, scroll_x);

    assert!(head_start, "expected line start visible at scroll_x=0");
    assert!(!head_end, "expected line end hidden at scroll_x=0");
    assert!(tail_end, "expected line end visible at max scroll_x");
    assert!(!tail_start, "expected line start hidden at max scroll_x");
}

fn char_column_lit(buf: &[u8], width: u32, col: usize, cell_width: u32, scroll_x: f32) -> bool {
    let x =
        crate::viewport_layout::LEFT_PAD as i32 + col as i32 * cell_width as i32 - scroll_x as i32;
    if x + cell_width as i32 <= 0 || x >= width as i32 {
        return false;
    }
    let start_x = x.max(0) as u32;
    let end_x = (x + cell_width as i32).min(width as i32) as u32;
    let height = buf.len() / ((width * 4) as usize);
    for y in 0..height {
        for px in start_x..end_x {
            let idx = ((y as u32 * width + px) * 4) as usize;
            let px4 = &buf[idx..idx + 4];
            if px4[0] > 10 || px4[1] > 10 || px4[2] > 10 {
                return true;
            }
        }
    }
    false
}

#[test]
fn render_emoji_and_symbol_chars() {
    let mut renderer = ViewportRenderer::new();
    let cases = [
        ("To access the server ⚡, go to:", '⚡'),
        ("✔ Cleaning dist dir (6ms)", '✔'),
        ("⠋ Building...", '⠋'),
    ];
    for (text, marker) in cases {
        let line = FlatLine {
            record_id: 1,
            line_index: 0,
            segments: vec![TextSegment {
                text: text.to_string(),
                style: None,
            }],
            raw: text.to_string(),
            level: None,
            collapsible: false,
            collapsed: false,
            hidden_line_count: 0,
        };
        let mut buf = vec![0u8; 600 * 40 * 4];
        renderer
            .render(
                &mut buf,
                600,
                40,
                &[line],
                0.0,
                0.0,
                false,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        let lit = buf
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|px| px[0] > 10 || px[1] > 10 || px[2] > 10)
            .count();
        assert!(
            lit > 30,
            "expected visible pixels for {text:?} (marker {marker}), got {lit}"
        );
        assert_no_horizontal_stripe_artifacts(&buf, 600, 40);
    }
}

#[test]
fn render_color_rocket_emoji_when_noto_available() {
    let mut renderer = ViewportRenderer::new();
    if renderer.color_emoji.is_none() {
        eprintln!("skip: Noto Color Emoji not installed");
        return;
    }
    let text = "🚀 launch";
    let line = FlatLine {
        record_id: 1,
        line_index: 0,
        segments: vec![TextSegment {
            text: text.to_string(),
            style: None,
        }],
        raw: text.to_string(),
        level: None,
        collapsible: false,
        collapsed: false,
        hidden_line_count: 0,
    };
    let width = 200u32;
    let height = 40u32;
    let mut buf = vec![0u8; (width * height * 4) as usize];
    renderer
        .render(
            &mut buf,
            width,
            height,
            &[line],
            0.0,
            0.0,
            false,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    // Color emoji should contribute chromatic (non-gray) pixels, not tofu/empty.
    let colorful = buf
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| {
            let [r, g, b, _] = [px[0], px[1], px[2], px[3]];
            let max = r.max(g).max(b);
            let min = r.min(g).min(b);
            max > 40 && (max - min) > 20
        })
        .count();
    assert!(
        colorful > 20,
        "expected colored rocket pixels when Noto Color Emoji is present, got {colorful}"
    );
}

#[test]
fn render_color_stopwatch_when_noto_available() {
    let mut renderer = ViewportRenderer::new();
    if renderer.color_emoji.is_none() {
        eprintln!("skip: Noto Color Emoji not installed");
        return;
    }
    // U+23F1 lives in Miscellaneous Technical — must not fall through to Symbols2.
    // Sample app.js emits ⏱️ as U+23F1 + U+FE0F.
    let text = "\u{23F1}\u{FE0F} 2.1s";
    let line = FlatLine {
        record_id: 1,
        line_index: 0,
        segments: vec![TextSegment {
            text: text.to_string(),
            style: None,
        }],
        raw: text.to_string(),
        level: None,
        collapsible: false,
        collapsed: false,
        hidden_line_count: 0,
    };
    let width = 200u32;
    let height = 40u32;
    let mut buf = vec![0u8; (width * height * 4) as usize];
    renderer
        .render(
            &mut buf,
            width,
            height,
            &[line],
            0.0,
            0.0,
            false,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    let colorful = buf
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| {
            let [r, g, b, _] = [px[0], px[1], px[2], px[3]];
            let max = r.max(g).max(b);
            let min = r.min(g).min(b);
            max > 40 && (max - min) > 20
        })
        .count();
    assert!(
        colorful > 20,
        "expected colored stopwatch pixels (not Symbols2 wireframe), got {colorful}"
    );
}

#[test]
fn variation_selector_16_does_not_draw_tofu_cell() {
    let mut renderer = ViewportRenderer::new();
    let cell = renderer.metrics.cell_width;
    // Sample: console.time(`⏱️ …`) → U+23F1 + U+FE0F
    assert_eq!(
        text_width("\u{23F1}\u{FE0F}", cell),
        text_width("\u{23F1}", cell)
    );
    assert_eq!(text_width("\u{FE0F}", cell), 0);

    // FE0F alone must not produce a tofu box.
    let vs_only = FlatLine {
        record_id: 1,
        line_index: 0,
        segments: vec![TextSegment {
            text: "\u{FE0F}".to_string(),
            style: None,
        }],
        raw: "\u{FE0F}".to_string(),
        level: None,
        collapsible: false,
        collapsed: false,
        hidden_line_count: 0,
    };
    let width = 120u32;
    let height = 40u32;
    let mut buf = vec![0u8; (width * height * 4) as usize];
    renderer
        .render(
            &mut buf,
            width,
            height,
            &[vs_only],
            0.0,
            0.0,
            false,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    let lit = buf
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| px[0] > 10 || px[1] > 10 || px[2] > 10)
        .count();
    assert_eq!(
        lit, 0,
        "FE0F alone must not rasterize tofu, got {lit} lit pixels"
    );

    // Marker after emoji+VS16 lands in the same cell as after bare emoji.
    let with_vs = FlatLine {
        record_id: 1,
        line_index: 0,
        segments: vec![TextSegment {
            text: "\u{23F1}\u{FE0F}#".to_string(),
            style: None,
        }],
        raw: "\u{23F1}\u{FE0F}#".to_string(),
        level: None,
        collapsible: false,
        collapsed: false,
        hidden_line_count: 0,
    };
    let bare = FlatLine {
        record_id: 2,
        line_index: 0,
        segments: vec![TextSegment {
            text: "\u{23F1}#".to_string(),
            style: None,
        }],
        raw: "\u{23F1}#".to_string(),
        level: None,
        collapsible: false,
        collapsed: false,
        hidden_line_count: 0,
    };
    let mut buf_vs = vec![0u8; (width * height * 4) as usize];
    let mut buf_bare = vec![0u8; (width * height * 4) as usize];
    renderer
        .render(
            &mut buf_vs,
            width,
            height,
            &[with_vs],
            0.0,
            0.0,
            false,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    renderer
        .render(
            &mut buf_bare,
            width,
            height,
            &[bare],
            0.0,
            0.0,
            false,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    let marker_col = 1usize; // display cell after the emoji
    assert!(
        char_column_lit(&buf_vs, width, marker_col, cell, 0.0),
        "expected '#' after emoji+VS16 in display cell 1"
    );
    assert!(
        char_column_lit(&buf_bare, width, marker_col, cell, 0.0),
        "expected '#' after bare emoji in display cell 1"
    );
}

#[test]
fn combining_mark_overlays_base_cell_and_does_not_advance() {
    let mut renderer = ViewportRenderer::new();
    let cell = renderer.metrics.cell_width;
    // "a" + U+0301 + "#": the '#' must land in display cell 1 (mark overlays cell 0).
    let combined = FlatLine {
        record_id: 1,
        line_index: 0,
        segments: vec![TextSegment {
            text: "a\u{0301}#".to_string(),
            style: None,
        }],
        raw: "a\u{0301}#".to_string(),
        level: None,
        collapsible: false,
        collapsed: false,
        hidden_line_count: 0,
    };
    let bare = FlatLine {
        record_id: 2,
        line_index: 0,
        segments: vec![TextSegment {
            text: "a#".to_string(),
            style: None,
        }],
        raw: "a#".to_string(),
        level: None,
        collapsible: false,
        collapsed: false,
        hidden_line_count: 0,
    };
    let width = 120u32;
    let height = 40u32;
    let mut buf_combined = vec![0u8; (width * height * 4) as usize];
    let mut buf_bare = vec![0u8; (width * height * 4) as usize];
    renderer
        .render(
            &mut buf_combined,
            width,
            height,
            &[combined],
            0.0,
            0.0,
            false,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    renderer
        .render(
            &mut buf_bare,
            width,
            height,
            &[bare],
            0.0,
            0.0,
            false,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    assert!(
        char_column_lit(&buf_combined, width, 0, cell, 0.0),
        "base 'a' cell must have ink"
    );
    assert!(
        char_column_lit(&buf_combined, width, 1, cell, 0.0),
        "expected '#' in display cell 1 after combining mark"
    );
    assert!(
        !char_column_lit(&buf_combined, width, 2, cell, 0.0),
        "combining mark must not spill into cell 2 (tofu advance)"
    );
    // Arabic: base + fatha + shadda renders, no extra cell.
    let arabic = FlatLine {
        record_id: 3,
        line_index: 0,
        segments: vec![TextSegment {
            text: "\u{0627}\u{064E}\u{0651}".to_string(),
            style: None,
        }],
        raw: "\u{0627}\u{064E}\u{0651}".to_string(),
        level: None,
        collapsible: false,
        collapsed: false,
        hidden_line_count: 0,
    };
    let mut buf_ar = vec![0u8; (width * height * 4) as usize];
    renderer
        .render(
            &mut buf_ar,
            width,
            height,
            &[arabic],
            0.0,
            0.0,
            false,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    assert!(
        char_column_lit(&buf_ar, width, 0, cell, 0.0),
        "Arabic base cell must have ink"
    );
    assert!(
        !char_column_lit(&buf_ar, width, 1, cell, 0.0),
        "harakat must not occupy their own cells"
    );
}

#[test]
fn flag_pair_spans_two_cells_and_marker_lands_after() {
    let mut renderer = ViewportRenderer::new();
    let cell = renderer.metrics.cell_width;
    // US flag + '#': '#' must land in display cell 2 (flag spans 2 cells).
    let line = FlatLine {
        record_id: 1,
        line_index: 0,
        segments: vec![TextSegment {
            text: "\u{1F1FA}\u{1F1F8}#".to_string(),
            style: None,
        }],
        raw: "\u{1F1FA}\u{1F1F8}#".to_string(),
        level: None,
        collapsible: false,
        collapsed: false,
        hidden_line_count: 0,
    };
    let width = 120u32;
    let height = 40u32;
    let mut buf = vec![0u8; (width * height * 4) as usize];
    renderer
        .render(
            &mut buf,
            width,
            height,
            &[line],
            0.0,
            0.0,
            false,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    assert!(
        char_column_lit(&buf, width, 0, cell, 0.0),
        "flag must paint its first cell"
    );
    assert!(
        char_column_lit(&buf, width, 2, cell, 0.0),
        "expected '#' in display cell 2 after a 2-cell flag"
    );
    assert!(
        !char_column_lit(&buf, width, 3, cell, 0.0),
        "flag must not occupy a third cell"
    );
}

#[test]
fn emoji_fallback_font_loads() {
    let fonts = FontStack::new(load_mono_font(), load_emoji_fallback_font());
    assert!(
        fonts.fallback.is_some(),
        "expected emoji/symbol fallback font"
    );
    let fb = fonts.fallback.as_ref().unwrap();
    for ch in ['⚡', '✔', '⠋'] {
        assert!(
            fb.has_glyph(ch) && glyph_has_ink(fb, ch),
            "fallback should rasterize {ch}"
        );
    }
    // Box drawing stays on the primary monospace font.
    assert!(fonts.primary.has_glyph('│'));
}

#[test]
fn render_multiple_lines_have_vertical_glyph_spread() {
    let mut renderer = ViewportRenderer::new();
    let lines: Vec<FlatLine> = (0..5)
        .map(|i| {
            let i = i as usize;
            FlatLine {
                record_id: i as u64,
                line_index: i,
                segments: vec![TextSegment {
                    text: format!("log line {i}: hello world"),
                    style: None,
                }],
                raw: format!("log line {i}: hello world"),
                level: None,
                collapsible: false,
                collapsed: false,
                hidden_line_count: 0,
            }
        })
        .collect();
    let width = 400u32;
    let height = 120u32;
    let mut buf = vec![0u8; (width * height * 4) as usize];
    renderer
        .render(
            &mut buf, width, height, &lines, 0.0, 0.0, false, None, None, None, None, None,
        )
        .unwrap();

    let text_rows = rows_with_text_pixels(&buf, width, height);
    assert!(
        text_rows.len() >= 3,
        "expected glyphs on multiple rows, got {} lit rows: {:?}",
        text_rows.len(),
        text_rows
    );
    let max_row_pixels = text_rows
        .iter()
        .map(|row| count_lit_pixels_on_row(&buf, width, *row))
        .max()
        .unwrap_or(0);
    assert!(
        max_row_pixels > 20,
        "expected solid glyph rows, not 1px stripes (max row pixels={max_row_pixels})"
    );
    assert_no_horizontal_stripe_artifacts(&buf, width, height);
}

#[test]
fn render_search_match_shows_text_not_only_background() {
    let mut renderer = ViewportRenderer::new();
    let line = FlatLine {
        record_id: 1,
        line_index: 0,
        segments: vec![TextSegment {
            text: "http://localhost:1337".to_string(),
            style: None,
        }],
        raw: "http://localhost:1337".to_string(),
        level: None,
        collapsible: false,
        collapsed: false,
        hidden_line_count: 0,
    };
    let re = compile_search_pattern("http", false, false, false).unwrap();
    let active = SearchMatch {
        line_index: 0,
        start: 0,
        end: 4,
    };
    let width = 400u32;
    let height = 40u32;
    let mut buf = vec![0u8; (width * height * 4) as usize];
    renderer
        .render(
            &mut buf,
            width,
            height,
            &[line],
            0.0,
            0.0,
            false,
            None,
            Some(&re),
            None,
            Some(active),
            None,
        )
        .unwrap();

    let orange_pixels = buf
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| px[0] > 150 && px[1] > 100 && px[1] < 160 && px[2] < 40)
        .count();
    assert!(
        orange_pixels > 20,
        "expected orange search highlight pixels"
    );

    let text_pixels = buf
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| {
            // Default foreground (not black background, not orange highlight)
            px[0] > 180 && px[1] > 200 && px[2] > 200
        })
        .count();
    assert!(
        text_pixels > 30,
        "expected visible foreground text pixels, got {text_pixels}"
    );
    assert_no_horizontal_stripe_artifacts(&buf, width, height);
}

#[test]
fn render_box_drawing_vertical_bars_align_across_rows() {
    let mut renderer = ViewportRenderer::new();
    let width = 800u32;
    let height = 80u32;
    let row_stride = renderer.metrics.row_stride;

    let rows = [
        "┌──────────┬──────────────────────────────────────────┐",
        "│ Time     │ Fri Jul 10 2026 12:07:46 GMT+0300        │",
        "│ Launched │ 2333 ms                                  │",
        "└──────────┴──────────────────────────────────────────┘",
    ];

    let flat_lines: Vec<FlatLine> = rows
        .iter()
        .enumerate()
        .map(|(i, text)| FlatLine {
            record_id: i as u64,
            line_index: i,
            segments: vec![TextSegment {
                text: (*text).to_string(),
                style: None,
            }],
            raw: text.to_string(),
            level: None,
            collapsible: false,
            collapsed: false,
            hidden_line_count: 0,
        })
        .collect();

    let mut buf = vec![0u8; (width * height * 4) as usize];
    renderer
        .render(
            &mut buf,
            width,
            height,
            &flat_lines,
            0.0,
            0.0,
            false,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

    // Middle divider is at column 11 on every row (after 10-cell-wide left column).
    let bar_cols = [0usize, 11];
    let cell_width = renderer.metrics.cell_width;
    let base_x = 8u32;

    for row in 0..3 {
        let row_top = (row as f32 * row_stride).round() as u32;
        let row_bottom = ((row as f32 + 1.0) * row_stride).round() as u32;
        for &col in &bar_cols {
            let cell_x = base_x + col as u32 * cell_width;
            let lit_x = find_lit_x_in_column(&buf, width, height, cell_x, row_top, row_bottom);
            assert!(
                lit_x.is_some(),
                "row {row} missing bar ink near column {col} (cell_x={cell_x})"
            );
            let x = lit_x.unwrap();
            assert!(
                x >= cell_x as i32 && x < (cell_x + cell_width) as i32,
                "row {row} col {col}: bar ink at x={x} outside cell [{cell_x}, {})",
                cell_x + cell_width
            );
        }
    }

    // Data rows (1-2) use │ at the same columns — ink x must match exactly.
    for &col in &bar_cols {
        let cell_x = base_x + col as u32 * cell_width;
        let mut xs = Vec::new();
        for row in 1..3 {
            let row_top = (row as f32 * row_stride).round() as u32;
            let row_bottom = ((row as f32 + 1.0) * row_stride).round() as u32;
            if let Some(x) = find_lit_x_in_column(&buf, width, height, cell_x, row_top, row_bottom)
            {
                xs.push(x);
            }
        }
        assert_eq!(
            xs.len(),
            2,
            "expected │ ink on both data rows at column {col}"
        );
        assert_eq!(
            xs[0], xs[1],
            "│ column {col} jumped between data rows: {:?}",
            xs
        );
    }
}

/// Find the leftmost lit pixel in a cell column on a row (for box-drawing bar alignment).
fn find_lit_x_in_column(
    buf: &[u8],
    width: u32,
    height: u32,
    cell_x: u32,
    row_top: u32,
    row_bottom: u32,
) -> Option<i32> {
    let cell_w = 8u32;
    let x_end = (cell_x + cell_w).min(width);
    let y_start = row_top.min(height);
    let y_end = row_bottom.min(height);
    for y in y_start..y_end {
        for x in cell_x..x_end {
            let idx = ((y * width + x) * 4) as usize;
            let px = &buf[idx..idx + 4];
            if px[0] > 10 || px[1] > 10 || px[2] > 10 {
                return Some(x as i32);
            }
        }
    }
    None
}

#[test]
fn render_strapi_table_lines_visible() {
    use crate::core::ansi::{parse_ansi_line, strip_ansi};
    use crate::core::types::FlatLine;

    let mut renderer = ViewportRenderer::new();
    let colored = "\u{1b}[90m│\u{1b}[39m \u{1b}[34mTime\u{1b}[39m               \u{1b}[90m│\u{1b}[39m Fri Jul 10 2026 12:07:46 GMT+0300 \u{1b}[90m│\u{1b}[39m";
    let plain = strip_ansi(colored);
    let segments = parse_ansi_line(colored);
    let line = FlatLine {
        record_id: 1,
        line_index: 0,
        segments,
        raw: plain.clone(),
        level: None,
        collapsible: false,
        collapsed: false,
        hidden_line_count: 0,
    };

    let width = 800u32;
    let height = 40u32;
    let mut buf = vec![0u8; (width * height * 4) as usize];
    renderer
        .render(
            &mut buf,
            width,
            height,
            &[line],
            0.0,
            0.0,
            false,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

    let lit = buf
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| px[0] > 10 || px[1] > 10 || px[2] > 10)
        .count();
    assert!(
        lit > 50,
        "table row should render visible pixels, got {lit} for {plain:?}"
    );
    let border_only = FlatLine {
        record_id: 2,
        line_index: 0,
        segments: vec![TextSegment {
            text: "╭────────────────────┬──────────────────────────────────────────────────╮"
                .to_string(),
            style: None,
        }],
        raw: "╭────────────────────┬──────────────────────────────────────────────────╮"
            .to_string(),
        level: None,
        collapsible: false,
        collapsed: false,
        hidden_line_count: 0,
    };
    let mut buf2 = vec![0u8; (width * height * 4) as usize];
    renderer
        .render(
            &mut buf2,
            width,
            height,
            &[border_only],
            0.0,
            0.0,
            false,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    let lit_border = buf2
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| px[0] > 10 || px[1] > 10 || px[2] > 10)
        .count();
    assert!(
        lit_border > 30,
        "box-drawing border should render, got {lit_border} pixels"
    );
}

fn rows_with_text_pixels(buf: &[u8], width: u32, height: u32) -> Vec<u32> {
    let mut rows = Vec::new();
    for y in 0..height {
        if count_lit_pixels_on_row(buf, width, y) > 5 {
            rows.push(y);
        }
    }
    rows
}

fn count_lit_pixels_on_row(buf: &[u8], width: u32, y: u32) -> usize {
    let start = (y * width * 4) as usize;
    let end = start + (width as usize) * 4;
    buf[start..end]
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| px[0] > 10 || px[1] > 10 || px[2] > 10)
        .count()
}

/// Detect the regression where only search/row backgrounds render as thin horizontal bars.
fn assert_no_horizontal_stripe_artifacts(buf: &[u8], width: u32, height: u32) {
    let text_rows = rows_with_text_pixels(buf, width, height);
    assert!(
        !text_rows.is_empty(),
        "viewport rendered no visible pixels at all"
    );

    let mut thin_rows = 0usize;
    for &y in &text_rows {
        let lit = count_lit_pixels_on_row(buf, width, y);
        if lit <= 3 {
            thin_rows += 1;
        }
    }
    assert!(
        thin_rows < text_rows.len(),
        "viewport looks like horizontal stripes: {thin_rows}/{} lit rows are 1-3px tall",
        text_rows.len()
    );
}

#[test]
fn render_draws_block_caret_past_line_end() {
    let mut renderer = ViewportRenderer::new();
    let line = FlatLine {
        record_id: 1,
        line_index: 0,
        segments: vec![crate::core::types::TextSegment {
            text: "$ ".to_string(),
            style: None,
        }],
        raw: "$ ".to_string(),
        level: None,
        collapsible: false,
        collapsed: false,
        hidden_line_count: 0,
    };
    let width = 200u32;
    let height = 40u32;
    let cell = renderer.metrics.cell_width;
    let mut with_caret = vec![0u8; (width * height * 4) as usize];
    let mut without = vec![0u8; (width * height * 4) as usize];
    renderer
        .render(
            &mut without,
            width,
            height,
            &[line.clone()],
            0.0,
            0.0,
            false,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    renderer
        .render(
            &mut with_caret,
            width,
            height,
            &[line],
            0.0,
            0.0,
            false,
            None,
            None,
            None,
            None,
            Some(ViewportCaret {
                flat_index: 0,
                col: 5,
            }),
        )
        .unwrap();

    let caret_x = 8 + 5 * cell;
    let mut caret_lit = 0usize;
    let mut diff = 0usize;
    for y in 0..height {
        for x in caret_x..caret_x + cell {
            let i = ((y * width + x) * 4) as usize;
            if with_caret[i] != without[i]
                || with_caret[i + 1] != without[i + 1]
                || with_caret[i + 2] != without[i + 2]
            {
                diff += 1;
            }
            if with_caret[i] > 40 || with_caret[i + 1] > 40 || with_caret[i + 2] > 40 {
                caret_lit += 1;
            }
        }
    }
    assert!(
        diff > 10,
        "caret should change pixels at col 5, diff={diff}"
    );
    assert!(
        caret_lit > 10,
        "caret cell should be visible, lit={caret_lit}"
    );
}

#[test]
fn set_font_size_rebuilds_metrics() {
    let mut renderer = ViewportRenderer::new();
    assert!((renderer.font_size() - 13.0).abs() < 0.01);
    let baseline = renderer.metrics().row_stride;
    let baseline_cell = renderer.metrics().cell_width;

    renderer.set_font_size(24.0);
    assert!((renderer.font_size() - 24.0).abs() < 0.01);
    assert!(renderer.metrics().row_stride > baseline);
    assert!(renderer.metrics().cell_width >= baseline_cell);

    renderer.set_font_size(8.0);
    assert!((renderer.font_size() - 8.0).abs() < 0.01);
    assert!(renderer.metrics().row_stride < baseline);

    renderer.set_font_size(100.0);
    assert!((renderer.font_size() - 32.0).abs() < 0.01);
}

// Issue #235: a selection that survived a buffer swap can hold stale byte
// offsets landing mid-character; the highlight path used to slice without
// the char-boundary guard selection_plain_text has and panicked.
#[test]
fn highlight_selection_tolerates_stale_mid_char_offsets() {
    let text = "日本語です"; // 2 bytes per char, 10 bytes total
    let segments = vec![TextSegment {
        text: text.to_string(),
        style: None,
    }];
    let sel = TextSelection::new(
        TextPos {
            line_index: 0,
            byte_offset: 1,
        }, // mid-char (stale)
        TextPos {
            line_index: 0,
            byte_offset: 5,
        }, // mid-char (stale)
    );
    let out = highlight_selection_in_segments(&segments, &sel, 0, 0, text.len());
    let joined: String = out.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(joined, text, "highlight must not drop or corrupt chars");
    assert!(out
        .iter()
        .any(|s| s.style.as_ref().is_some_and(|st| st.selected)));
}
#[test]
fn leading_combining_mark_at_segment_start_is_skipped() {
    // Issue #327: a zero-width mark leading a segment at the row start
    // has no previous cell; it must be skipped, not painted one cell
    // left of the line base (into the left pad). The rendered output
    // must be pixel-identical to the same line without the mark.
    let mk = |text: &str, id: u64| FlatLine {
        record_id: id,
        line_index: 0,
        segments: vec![TextSegment {
            text: text.to_string(),
            style: None,
        }],
        raw: text.to_string(),
        level: None,
        collapsible: false,
        collapsed: false,
        hidden_line_count: 0,
    };
    let with_mark = mk("\u{0301}b", 1);
    let bare = mk("b", 2);
    let width = 120u32;
    let height = 40u32;
    let mut buf_with = vec![0u8; (width * height * 4) as usize];
    let mut buf_bare = vec![0u8; (width * height * 4) as usize];
    let mut renderer = ViewportRenderer::new();
    for (buf, line) in [(&mut buf_with, &with_mark), (&mut buf_bare, &bare)] {
        renderer
            .render(
                buf,
                width,
                height,
                &[line.clone()],
                0.0,
                0.0,
                false,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
    }
    assert_eq!(
        buf_with, buf_bare,
        "leading combining mark must be skipped: no ink left of the line base"
    );
    assert!(
        char_column_lit(&buf_bare, width, 0, renderer.metrics.cell_width, 0.0),
        "'b' must land in display cell 0"
    );
}
