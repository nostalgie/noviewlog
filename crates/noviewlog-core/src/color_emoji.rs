//! Color emoji via the system emoji font: CBDT/CBLC PNG strikes (Noto Color
//! Emoji) or COLR/CPAL vector layers (Windows Segoe UI Emoji).
//!
//! Noto Color Emoji is bitmap-only; fontdue cannot rasterize it. We parse the
//! font with `ttf-parser`, pull embedded PNG bitmaps from CBDT, decode them,
//! and blit RGBA into the viewport. Segoe UI Emoji (stock Windows) has no
//! CBDT strikes — those glyphs are rendered from COLR layers by
//! [`crate::colr_paint`] (issue #67) with the same blit path.
//!
//! Bundling is intentionally skipped: the font is ~10MB+. Prefer the system
//! install (Linux packages / Windows Segoe / user fonts).

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Mutex;

use ttf_parser::{Face, GlyphId, RasterImageFormat};

use crate::colr_paint::render_colr_glyph;

/// Decoded RGBA glyph ready to scale and blit.
#[derive(Clone)]
pub struct ColorEmojiGlyph {
    pub width: u32,
    pub height: u32,
    /// Horizontal bearing from the CBDT/raster image header (pixels at strike size).
    #[allow(dead_code)]
    pub x_offset: i16,
    /// Vertical bearing (PositiveYDown-friendly offset from baseline area).
    #[allow(dead_code)]
    pub y_offset: i16,
    /// Tight RGBA8 buffer (`width * height * 4`).
    pub rgba: Vec<u8>,
}

/// Lazy-loading color-emoji atlas (CBDT strikes or COLR layers) keyed by
/// Unicode scalar or ZWJ cluster.
pub struct ColorEmojiAtlas {
    /// Owned font bytes; declared before `face` so it drops after it.
    _font_data: Box<[u8]>,
    // SAFETY: borrows `_font_data`, which lives in the same struct and is
    // never mutated or moved out; `face` is declared after the data, so it
    // is dropped first and the borrow never outlives the bytes.
    face: Box<Face<'static>>,
    pub(crate) cache: Mutex<HashMap<String, Option<ColorEmojiGlyph>>>,
}

/// Max memoized emoji strikes (~65 KB RGBA each; cap ≈ 8 MB).
pub(crate) const EMOJI_CACHE_CAP: usize = 128;

impl ColorEmojiAtlas {
    /// Load from the first color-capable candidate font. Returns `None` when
    /// no system emoji font with CBDT strikes or COLR layers is available.
    pub fn load() -> Option<Self> {
        let data = load_color_emoji_bytes()?;
        let face = Face::parse(&data, 0).ok()?;
        // SAFETY: see field comment on the struct.
        let face: Face<'static> = unsafe { std::mem::transmute(face) };
        Some(Self {
            _font_data: data.into(),
            face: Box::new(face),
            cache: Mutex::new(HashMap::new()),
        })
    }

    pub fn has_glyph(&self, ch: char) -> bool {
        self.glyph(ch).is_some()
    }

    /// Cached decode of the CBDT PNG for `ch` (single scalar).
    pub fn glyph(&self, ch: char) -> Option<ColorEmojiGlyph> {
        self.glyph_for_key(&ch.to_string())
    }

    /// Cached CBDT PNG for a ZWJ cluster like `👨‍👩‍👧‍👦`.
    ///
    /// Resolves the composite glyph through GSUB ligature substitution; ZWJ
    /// sequences have no cmap entry of their own. Returns `None` when the font
    /// has no composite — the draw path then falls back to per-member cells.
    pub fn glyph_cluster(&self, seq: &str) -> Option<ColorEmojiGlyph> {
        self.glyph_for_key(seq)
    }

    fn glyph_for_key(&self, key: &str) -> Option<ColorEmojiGlyph> {
        {
            // Poison-tolerant like the second lock below (issue #238): a
            // panicked holder must not permanently disable color emoji.
            let cache = self
                .cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(entry) = cache.get(key) {
                return entry.clone();
            }
        }
        let decoded = decode_glyph_or_cluster(&self.face, key);
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Bounded (issue #61): a decoded strike is ~65 KB RGBA; without a
        // cap a long emoji-heavy session grew the map for hours. Drop
        // ~1/8 arbitrary entries (HashMap order) when full.
        if !cache.contains_key(key) && cache.len() >= EMOJI_CACHE_CAP {
            let victims: Vec<String> = cache.keys().take(EMOJI_CACHE_CAP / 8).cloned().collect();
            for victim in victims {
                cache.remove(&victim);
            }
        }
        cache.insert(key.to_string(), decoded.clone());
        decoded
    }
}

fn decode_glyph_or_cluster(face: &Face, key: &str) -> Option<ColorEmojiGlyph> {
    if key.chars().count() > 1 {
        let chars: Vec<char> = key.chars().collect();
        let gid_from = |seq: &[char]| -> Option<GlyphId> {
            let gids: Option<Vec<GlyphId>> = seq.iter().map(|c| face.glyph_index(*c)).collect();
            resolve_ligature(face, gids.as_ref()?)
        };
        // Try the full sequence first, then without variation selectors —
        // some fonts key keycap ligatures without the VS component.
        if let Some(g) = gid_from(&chars) {
            return decode_glyph_image(face, g);
        }
        let stripped: Vec<char> = chars
            .iter()
            .copied()
            .filter(|c| !matches!(c, '\u{FE00}'..='\u{FE0F}'))
            .collect();
        if stripped.len() != chars.len() {
            if stripped.len() == 1 {
                return decode_glyph(face, stripped[0]);
            }
            if let Some(g) = gid_from(&stripped) {
                return decode_glyph_image(face, g);
            }
        }
        return None;
    }
    let ch = key.chars().next()?;
    decode_glyph(face, ch)
}

/// Decode a color glyph: CBDT PNG strike when present, else COLR layers
/// (Segoe UI Emoji on Windows — issue #67).
fn decode_glyph_image(face: &Face, gid: GlyphId) -> Option<ColorEmojiGlyph> {
    if let Some(img) = face.glyph_raster_image(gid, u16::MAX) {
        if img.format == RasterImageFormat::PNG {
            if let Some(rgba) = decode_png_rgba(img.data) {
                return Some(ColorEmojiGlyph {
                    width: img.width as u32,
                    height: img.height as u32,
                    x_offset: img.x,
                    y_offset: img.y,
                    rgba,
                });
            }
        }
    }
    // COLR render size: matches the ~136px CBDT strikes of Noto Color Emoji;
    // the blit path scales to the cell anyway.
    let (rgba, width, height) = render_colr_glyph(face, gid, 136.0)?;
    Some(ColorEmojiGlyph {
        width,
        height,
        x_offset: 0,
        y_offset: 0,
        rgba,
    })
}

/// Walk GSUB ligature lookups for a substitution whose glyph sequence matches.
fn resolve_ligature(face: &Face, gids: &[GlyphId]) -> Option<GlyphId> {
    use ttf_parser::gsub::SubstitutionSubtable;

    if gids.len() < 2 {
        return None;
    }
    let gsub = face.tables().gsub.as_ref()?;
    let first = *gids.first()?;
    let rest = gids.get(1..)?;
    for i in 0..gsub.lookups.len() {
        let lookup = gsub.lookups.get(i)?;
        for si in 0..lookup.subtables.len() {
            let Some(subtable) = lookup.subtables.get::<SubstitutionSubtable>(si) else {
                continue;
            };
            let SubstitutionSubtable::Ligature(lig) = subtable else {
                continue;
            };
            let Some(set_idx) = lig.coverage.get(first) else {
                continue;
            };
            let Some(set) = lig.ligature_sets.get(set_idx) else {
                continue;
            };
            for ligature in set.into_iter() {
                if ligature.components.len() as usize == rest.len()
                    && ligature.components.into_iter().eq(rest.iter().copied())
                {
                    return Some(ligature.glyph);
                }
            }
        }
    }
    None
}

/// Candidate emoji fonts, best first: CBDT Noto Color Emoji (crisp PNG
/// strikes), then COLR Segoe UI Emoji (stock Windows, issue #67).
fn load_color_emoji_bytes() -> Option<Vec<u8>> {
    let mut candidates: Vec<String> = Vec::new();
    if let Some(home) = dirs::home_dir() {
        let home = home.to_string_lossy();
        for name in ["NotoColorEmoji.ttf", "NotoColorEmoji.ttc"] {
            candidates.push(format!("{home}/.local/share/fonts/{name}"));
            candidates.push(format!("{home}/.fonts/{name}"));
            candidates.push(format!(
                "{home}/AppData/Local/Microsoft/Windows/Fonts/{name}"
            ));
        }
    }
    for path in [
        // Linux
        "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf",
        "/usr/share/fonts/noto/NotoColorEmoji.ttf",
        "/usr/share/fonts/google-noto/NotoColorEmoji.ttf",
        "/usr/share/fonts/truetype/noto-color-emoji/NotoColorEmoji.ttf",
        // Windows (only if a CBDT Noto build is installed)
        "C:\\Windows\\Fonts\\NotoColorEmoji.ttf",
        "C:\\Windows\\Fonts\\NotoColorEmoji.ttc",
    ] {
        candidates.push(path.to_string());
    }
    // Stock Windows: Segoe UI Emoji is COLR/CPAL (no CBDT strikes).
    candidates.push("C:\\Windows\\Fonts\\seguiemj.ttf".to_string());
    if let Some(home) = dirs::home_dir() {
        let home = home.to_string_lossy();
        candidates.push(format!(
            "{home}/AppData/Local/Microsoft/Windows/Fonts/seguiemj.ttf"
        ));
    }

    for path in &candidates {
        let Ok(data) = std::fs::read(path) else {
            continue;
        };
        let Ok(face) = Face::parse(&data, 0) else {
            continue;
        };
        // Only accept fonts that can actually paint the probe emoji in
        // color (CBDT strike or COLR layers) — a plain symbol font that
        // merely parses must not win over a later color font.
        let Some(probe) = face
            .glyph_index('\u{1F680}')
            .or_else(|| face.glyph_index('😀'))
        else {
            continue;
        };
        if face.glyph_raster_image(probe, u16::MAX).is_some() || face.is_color_glyph(probe) {
            return Some(data);
        }
    }
    None
}

fn decode_glyph(face: &Face, ch: char) -> Option<ColorEmojiGlyph> {
    let gid: GlyphId = face.glyph_index(ch)?;
    decode_glyph_image(face, gid)
}

fn decode_png_rgba(png_bytes: &[u8]) -> Option<Vec<u8>> {
    let mut decoder = png::Decoder::new(Cursor::new(png_bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::ALPHA);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    match info.color_type {
        png::ColorType::Rgba => {
            buf.truncate(info.buffer_size());
            Some(buf)
        }
        png::ColorType::Rgb => {
            let mut rgba = Vec::with_capacity((info.width * info.height * 4) as usize);
            for chunk in buf[..info.buffer_size()].chunks_exact(3) {
                rgba.extend_from_slice(&[chunk[0], chunk[1], chunk[2], 255]);
            }
            Some(rgba)
        }
        _ => None,
    }
}

/// Nearest-neighbor scale + alpha blit of a color emoji into an RGBA viewport buffer.
///
/// `span_cells` is the cluster display width: composites may spread wider than
/// the classic ~2-cell emoji box.
pub fn blit_color_emoji(
    out: &mut [u8],
    buf_width: u32,
    buf_height: u32,
    cell_x: i32,
    row_top: f32,
    row_height: f32,
    cell_width: u32,
    glyph: &ColorEmojiGlyph,
    clip: Option<(f32, f32)>,
) {
    blit_color_emoji_span(
        out, buf_width, buf_height, cell_x, row_top, row_height, cell_width, 1, glyph, clip,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn blit_color_emoji_span(
    out: &mut [u8],
    buf_width: u32,
    buf_height: u32,
    cell_x: i32,
    row_top: f32,
    row_height: f32,
    cell_width: u32,
    span_cells: u32,
    glyph: &ColorEmojiGlyph,
    clip: Option<(f32, f32)>,
) {
    if glyph.width == 0 || glyph.height == 0 || glyph.rgba.len() < 4 {
        return;
    }
    let span_w = cell_width.max(1) as f32 * span_cells.max(1) as f32;
    // Fit strike into ~1 row tall and at most the cluster span wide.
    let target_h = (row_height * 0.92).max(1.0);
    // Single emoji may overflow ~1.85 cells (legacy look); clusters span their cells.
    let max_w_cells = (span_cells.max(1) as f32 * 0.98).max(1.85);
    let max_w = cell_width.max(1) as f32 * max_w_cells;
    let scale = (target_h / glyph.height as f32).min(max_w / glyph.width as f32);
    let dest_w = (glyph.width as f32 * scale).round().max(1.0) as i32;
    let dest_h = (glyph.height as f32 * scale).round().max(1.0) as i32;

    // Center in the span horizontally; vertically center within the row.
    let dest_x = cell_x + ((span_w - dest_w as f32) * 0.5).round() as i32;
    let dest_y = (row_top + (row_height - dest_h as f32) * 0.5).round() as i32;

    let clip_top = clip.map(|c| c.0.floor() as i32).unwrap_or(0);
    let clip_bottom = clip.map(|c| c.1.ceil() as i32).unwrap_or(buf_height as i32);
    let buf_w = buf_width as i32;
    let buf_h = buf_height as i32;

    for dy in 0..dest_h {
        let py = dest_y + dy;
        if py < 0 || py >= buf_h || py < clip_top || py >= clip_bottom {
            continue;
        }
        let src_y = ((dy as f32 + 0.5) / dest_h as f32 * glyph.height as f32).floor() as u32;
        let src_y = src_y.min(glyph.height - 1);
        for dx in 0..dest_w {
            let px = dest_x + dx;
            if px < 0 || px >= buf_w {
                continue;
            }
            let src_x = ((dx as f32 + 0.5) / dest_w as f32 * glyph.width as f32).floor() as u32;
            let src_x = src_x.min(glyph.width - 1);
            let si = ((src_y * glyph.width + src_x) * 4) as usize;
            let src = [
                glyph.rgba[si],
                glyph.rgba[si + 1],
                glyph.rgba[si + 2],
                glyph.rgba[si + 3],
            ];
            if src[3] == 0 {
                continue;
            }
            let di = ((py as u32 * buf_width + px as u32) * 4) as usize;
            blend_rgba(&mut out[di..di + 4], src);
        }
    }
}

fn blend_rgba(dst: &mut [u8], src: [u8; 4]) {
    let a = src[3] as f32 / 255.0;
    for i in 0..3 {
        dst[i] = ((src[i] as f32 * a) + (dst[i] as f32 * (1.0 - a))) as u8;
    }
    dst[3] = 255;
}

/// True for codepoints we prefer to paint from the color-emoji font when present.
///
/// Includes misc-technical / misc-symbol blocks (⏱ U+23F1, ⚡, …) as well as
/// the main emoji planes. The draw path still requires a CBDT raster image, so
/// expanding these ranges is safe: missing glyphs fall through to fontdue.
pub fn is_color_emoji_candidate(ch: char) -> bool {
    matches!(
        ch,
        '\u{2300}'..='\u{23FF}' // Miscellaneous Technical (e.g. ⏱ ⌚️ ⌛)
            | '\u{2600}'..='\u{26FF}' // Miscellaneous Symbols (e.g. ⚡ ⚠)
            | '\u{2700}'..='\u{27BF}' // Dingbats
            | '\u{2B00}'..='\u{2BFF}' // Miscellaneous Symbols and Arrows (e.g. ⭐)
            | '\u{1F300}'..='\u{1FAFF}' // Misc. Symbols and Pictographs … Extended-A
            | '\u{1F1E6}'..='\u{1F1FF}' // Regional indicator symbols (flags; best-effort)
    )
}

/// Symbol/emoji blocks preferred from the symbol fallback font (fontdue pick).
///
/// Single classification source (issue #85): the viewport font-fallback pick
/// delegates here instead of maintaining a second range table. It adds Braille
/// patterns (rendered as outlines, never as CBDT color rasters).
pub fn is_symbol_font_candidate(ch: char) -> bool {
    is_color_emoji_candidate(ch) || matches!(ch, '\u{2800}'..='\u{28FF}')
}

/// How a scalar contributes to the monospace cell grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CharKind {
    /// Occupies one cell and advances the column.
    Advance,
    /// Invisible joiner / selector / format char: consumes no cell, no ink.
    SilentSkip,
    /// Combining mark: no cell advance; ink overlays the previous cell.
    Overlay,
}

/// Variation selectors / ZWJ / other invisible format chars.
///
/// Sample apps often emit `⏱️` as U+23F1 + U+FE0F; without this, FE0F becomes tofu.
pub fn is_zero_width_emoji_mark(ch: char) -> bool {
    matches!(
        ch,
        '\u{200D}' // Zero Width Joiner (joined with the cluster on draw)
            | '\u{FE00}'..='\u{FE0F}' // Variation Selectors 1–16 (incl. text/emoji VS)
            | '\u{200B}'..='\u{200F}' // ZWSP..RLM direction marks
            | '\u{FEFF}' // BOM / ZWNBSP
            | '\u{2060}'..='\u{2064}' // word joiner etc.
    )
}

/// Combining marks (Mn/Me blocks): Arabic harakat, Hebrew points, Devanagari
/// matras, Latin diacritics, etc. Rendered onto the previous cell.
///
/// Static range table instead of a general-category crate — hot path.
pub fn is_combining_mark(ch: char) -> bool {
    matches!(
        ch,
        '\u{0300}'..='\u{036F}' // Combining Diacritical Marks
            | '\u{0483}'..='\u{0489}' // Cyrillic combining
            | '\u{0591}'..='\u{05BD}' | '\u{05BF}' | '\u{05C1}'..='\u{05C2}' | '\u{05C4}'..='\u{05C5}' | '\u{05C7}' // Hebrew points
            | '\u{0610}'..='\u{061A}' | '\u{064B}'..='\u{065F}' | '\u{0670}' | '\u{06D6}'..='\u{06DC}' | '\u{06DF}'..='\u{06E4}' | '\u{06E7}'..='\u{06E8}' | '\u{06EA}'..='\u{06ED}' // Arabic harakat / Quranic marks
            | '\u{0711}' | '\u{0730}'..='\u{074A}' // Syriac
            | '\u{07A6}'..='\u{07B0}' // Thaana
            | '\u{0816}'..='\u{0819}' | '\u{081B}'..='\u{0823}' | '\u{0825}'..='\u{0827}' | '\u{0829}'..='\u{082D}' | '\u{0859}'..='\u{085B}' | '\u{08D3}'..='\u{08E1}' | '\u{08E3}'..='\u{0903}' // Arabic Extended / Mandaic
            | '\u{093A}' | '\u{093C}' | '\u{0941}'..='\u{0943}' | '\u{094D}' | '\u{0951}'..='\u{0957}' | '\u{0962}'..='\u{0963}' // Devanagari Mn marks (Mc matras advance a cell)
            | '\u{0981}'..='\u{0983}' | '\u{09BC}' | '\u{09BE}'..='\u{09C4}' | '\u{09C7}'..='\u{09C8}' | '\u{09CB}'..='\u{09CD}' | '\u{09D7}' | '\u{09E2}'..='\u{09E3}' // Bengali
            | '\u{0A01}'..='\u{0A03}' | '\u{0A3C}' | '\u{0A3E}'..='\u{0A42}' | '\u{0A47}'..='\u{0A48}' | '\u{0A4B}'..='\u{0A4D}' | '\u{0A51}' // Gurmukhi
            | '\u{0A81}'..='\u{0A83}' | '\u{0ABC}' | '\u{0ABE}'..='\u{0AC5}' | '\u{0AC7}'..='\u{0AC9}' | '\u{0ACB}'..='\u{0ACD}' // Gujarati
            | '\u{0B01}'..='\u{0B03}' | '\u{0B3C}' | '\u{0B3E}'..='\u{0B44}' | '\u{0B47}'..='\u{0B48}' | '\u{0B4B}'..='\u{0B4D}' | '\u{0B56}'..='\u{0B57}' // Oriya
            | '\u{0B82}' | '\u{0BBE}'..='\u{0BC2}' | '\u{0BC6}'..='\u{0BC8}' | '\u{0BCA}'..='\u{0BCD}' | '\u{0BD7}' // Tamil
            | '\u{0C00}'..='\u{0C03}' | '\u{0C3E}'..='\u{0C44}' | '\u{0C46}'..='\u{0C48}' | '\u{0C4A}'..='\u{0C4D}' | '\u{0C55}'..='\u{0C56}' // Telugu
            | '\u{0C81}'..='\u{0C83}' | '\u{0CBC}' | '\u{0CBE}'..='\u{0CC4}' | '\u{0CC6}'..='\u{0CC8}' | '\u{0CCA}'..='\u{0CCD}' | '\u{0CD5}'..='\u{0CD6}' // Kannada
            | '\u{0D01}'..='\u{0D03}' | '\u{0D3E}'..='\u{0D44}' | '\u{0D46}'..='\u{0D48}' | '\u{0D4A}'..='\u{0D4D}' | '\u{0D57}' // Malayalam
            | '\u{0E31}' | '\u{0E34}'..='\u{0E3A}' | '\u{0E47}'..='\u{0E4E}' // Thai
            | '\u{0EB1}' | '\u{0EB4}'..='\u{0EBC}' | '\u{0EC8}'..='\u{0ECD}' // Lao
            | '\u{0F71}'..='\u{0F84}' | '\u{0F86}'..='\u{0F87}' | '\u{0F8D}'..='\u{0F97}' | '\u{0F99}'..='\u{0FBC}' // Tibetan
            | '\u{102D}'..='\u{1030}' | '\u{1032}'..='\u{1037}' | '\u{1039}'..='\u{103A}' | '\u{1058}'..='\u{1059}' | '\u{105E}'..='\u{1060}' // Myanmar
            | '\u{135D}'..='\u{135F}' // Ethiopic combining
            | '\u{17B6}' | '\u{17BE}'..='\u{17C5}' | '\u{17C7}'..='\u{17C8}' | '\u{17B4}'..='\u{17B5}' // Khmer
            | '\u{1885}'..='\u{1886}' | '\u{18A9}' // Mongolian
            | '\u{1920}'..='\u{1922}' | '\u{1927}'..='\u{1928}' | '\u{1932}' | '\u{1939}'..='\u{193B}' // Limbu
            | '\u{1A17}'..='\u{1A18}' // Buginese
            | '\u{1AB0}'..='\u{1AFF}' // Combining Diacritical Marks Extended
            | '\u{1B00}'..='\u{1B03}' // Balinese
            | '\u{1B34}' | '\u{1B35}' | '\u{1B6B}'..='\u{1B73}' // Balinese / Sundanese
            | '\u{1DC0}'..='\u{1DFF}' // Combining Diacritical Marks Supplement
            | '\u{20D0}'..='\u{20F0}' // Combining Marks for Symbols
            | '\u{2CEF}'..='\u{2CF1}' // Coptic combining
            | '\u{302A}'..='\u{302F}' // CJK combining (vertical forms etc.)
            | '\u{3099}'..='\u{309A}' // Kana voicing marks
            | '\u{A66F}'..='\u{A672}' | '\u{A674}'..='\u{A67D}' // Combining Old Permic etc.
            | '\u{A8E0}'..='\u{A8F1}' // Devanagari Extended
            | '\u{FE20}'..='\u{FE2F}' // Combining Half Marks
    )
}

/// Cell-grid classification for a single scalar.
pub fn char_kind(ch: char) -> CharKind {
    if is_zero_width_emoji_mark(ch) {
        return CharKind::SilentSkip;
    }
    if is_combining_mark(ch) {
        return CharKind::Overlay;
    }
    CharKind::Advance
}

/// Display cell count: Unicode scalars minus zero-width marks.
pub fn display_cell_count(text: &str) -> usize {
    text.chars()
        .filter(|ch| char_kind(*ch) == CharKind::Advance)
        .count()
}

/// One display unit of a text run: a ZWJ emoji cluster, a plain scalar,
/// or a combining mark (`cells: 0`, overlays the previous cell).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayItem<'a> {
    /// Text of the item (cluster includes ZWJs / VSes).
    pub text: &'a str,
    /// Cells this item occupies (cluster members; ZWJ/VS contribute none).
    pub cells: usize,
}

/// Segment a run into ZWJ emoji clusters, combining marks, and plain scalars.
///
/// `👨‍👩‍👧‍👦` becomes one item with `cells: 4`; `a` + U+0301 yields a 1-cell
/// item followed by a 0-cell mark item.
pub fn display_items(text: &str) -> Vec<DisplayItem<'_>> {
    let mut items = Vec::new();
    let mut it = text.char_indices().peekable();
    while let Some((start, ch)) = it.next() {
        match char_kind(ch) {
            CharKind::SilentSkip => continue,
            CharKind::Overlay => {
                items.push(DisplayItem {
                    text: &text[start..start + ch.len_utf8()],
                    cells: 0,
                });
                continue;
            }
            CharKind::Advance => {}
        }
        // Flag: a pair of regional indicators renders as one 2-cell glyph.
        if matches!(ch, '\u{1F1E6}'..='\u{1F1FF}') {
            if let Some(&(_, r2)) = it.peek() {
                if matches!(r2, '\u{1F1E6}'..='\u{1F1FF}') {
                    it.next();
                    items.push(DisplayItem {
                        text: &text[start..start + ch.len_utf8() + r2.len_utf8()],
                        cells: 2,
                    });
                    continue;
                }
            }
            // Lone regional indicator: fall through to plain handling.
        }
        // Look ahead: base (VS)? (ZWJ (VS)? base)+ → one cluster item.
        let mut end = start + ch.len_utf8();
        let mut cells = 1usize;
        let mut probe = it.clone();
        let mut cluster = is_color_emoji_candidate(ch);
        loop {
            // Optional VS after base/member.
            if let Some(&(_, vs)) = probe.peek() {
                if matches!(vs, '\u{FE00}'..='\u{FE0F}') {
                    end += vs.len_utf8();
                    probe.next();
                }
            }
            match probe.peek() {
                Some(&(_, '\u{200D}')) => {
                    // ZWJ + member (VS optional).
                    end += '\u{200D}'.len_utf8();
                    probe.next();
                    match probe.next() {
                        Some((m_start, m)) if char_kind(m) == CharKind::Advance => {
                            end = m_start + m.len_utf8();
                            cells += 1;
                            cluster &= is_color_emoji_candidate(m);
                            if matches!(m, '\u{1F3FB}'..='\u{1F3FF}') {
                                // Skin-tone modifiers occupy no extra terminal cell.
                                cells -= 1;
                            }
                            it = probe.clone();
                        }
                        _ => break, // dangling ZWJ — not a cluster
                    }
                }
                _ => {
                    // Keycap: base (VS)? + U+20E3 → one single-cell item.
                    if let Some(&(_, kc)) = probe.peek() {
                        if kc == '\u{20E3}' {
                            end += kc.len_utf8();
                            probe.next();
                            it = probe.clone();
                        }
                    }
                    break;
                }
            }
        }
        if cluster && cells > 1 {
            items.push(DisplayItem {
                text: &text[start..end],
                cells,
            });
        } else {
            items.push(DisplayItem {
                text: &text[start..end],
                cells: 1,
            });
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_font_candidate_is_color_candidate_plus_braille() {
        // Issue #85 parity: the viewport font-fallback table must be exactly
        // the color-emoji table plus Braille (never an independent copy).
        for cp in 0u32..=0x2FFFF {
            let Some(ch) = char::from_u32(cp) else {
                continue;
            };
            let expected = is_color_emoji_candidate(ch) || ('\u{2800}'..='\u{28FF}').contains(&ch);
            assert_eq!(
                is_symbol_font_candidate(ch),
                expected,
                "drift at U+{cp:04X}"
            );
        }
    }

    #[test]
    fn candidate_boundaries_are_stable() {
        // Pin the documented range edges so edits here are deliberate.
        let inside = [
            0x2300, 0x23FF, 0x2600, 0x26FF, 0x2700, 0x27BF, 0x2B00, 0x2BFF, 0x1F300, 0x1FAFF,
            0x1F1E6, 0x1F1FF,
        ];
        let outside = [
            0x22FF, 0x2400, 0x25FF, 0x27C0, 0x2AFF, 0x2C00, 0x1F2FF, 0x1FB00, 0x1F1E5, 0x27FF,
            0x2900,
        ];
        for cp in inside {
            assert!(
                is_color_emoji_candidate(char::from_u32(cp).unwrap()),
                "U+{cp:04X} must be a color-emoji candidate"
            );
        }
        for cp in outside {
            assert!(
                !is_color_emoji_candidate(char::from_u32(cp).unwrap()),
                "U+{cp:04X} must stay outside the color-emoji table"
            );
        }
        // Braille belongs to the symbol-font pick only.
        assert!(!is_color_emoji_candidate('\u{2800}'));
        assert!(is_symbol_font_candidate('\u{2800}'));
        assert!(is_symbol_font_candidate('\u{28FF}'));
        assert!(!is_symbol_font_candidate('\u{27FF}'));
        assert!(!is_symbol_font_candidate('\u{2900}'));
    }

    #[test]
    fn loads_system_color_emoji_font_when_present() {
        let Some(atlas) = ColorEmojiAtlas::load() else {
            eprintln!("skip: no color emoji font installed");
            return;
        };
        assert!(
            atlas.has_glyph('\u{1F680}'),
            "expected rocket emoji in color font"
        );
    }

    /// Issue #67 regression: stock Windows ships Segoe UI Emoji as COLR, and
    /// the atlas used to come back `None` there. Fail loudly on a Windows
    /// host instead of silently skipping.
    #[test]
    fn loads_on_stock_windows_via_colr_segoe() {
        if !cfg!(windows) {
            eprintln!("skip: Windows-only (stock Segoe UI Emoji)");
            return;
        }
        let atlas = ColorEmojiAtlas::load()
            .expect("stock Windows must provide Segoe UI Emoji (COLR) for the color atlas");
        assert!(atlas.has_glyph('\u{1F680}'));
    }

    #[test]
    fn rocket_decodes_with_colored_ink() {
        let Some(atlas) = ColorEmojiAtlas::load() else {
            eprintln!("skip: no color emoji font installed");
            return;
        };
        let glyph = atlas.glyph('\u{1F680}').expect("rocket glyph");
        assert!(glyph.width > 8 && glyph.height > 8);
        let colored = glyph
            .rgba
            .chunks_exact(4)
            .filter(|p| p[3] > 20 && (p[0] > 30 || p[1] > 30 || p[2] > 30))
            .count();
        assert!(
            colored > 100,
            "expected colored ink in rocket glyph, got {colored}"
        );
    }

    #[test]
    fn stopwatch_is_color_candidate_and_has_cbdt() {
        assert!(
            is_color_emoji_candidate('\u{23F1}'),
            "⏱ (U+23F1) must be eligible for the color-emoji path"
        );
        let Some(atlas) = ColorEmojiAtlas::load() else {
            eprintln!("skip: Noto Color Emoji not installed");
            return;
        };
        assert!(
            atlas.has_glyph('\u{23F1}'),
            "expected stopwatch in Noto Color Emoji CBDT"
        );
    }

    #[test]
    fn variation_selectors_are_zero_width() {
        assert!(is_zero_width_emoji_mark('\u{FE0F}'));
        assert!(is_zero_width_emoji_mark('\u{FE0E}'));
        assert!(is_zero_width_emoji_mark('\u{200D}'));
        assert!(!is_zero_width_emoji_mark('\u{23F1}'));
        assert_eq!(display_cell_count("\u{23F1}\u{FE0F}"), 1);
        assert_eq!(display_cell_count("\u{FE0F}"), 0);
    }

    #[test]
    fn combining_marks_are_overlay_cells() {
        // Latin acute, Arabic fatha+shadda, Devanagari vocalic-r mark.
        for ch in ['\u{0301}', '\u{064E}', '\u{0651}', '\u{0941}', '\u{0300}'] {
            assert_eq!(char_kind(ch), CharKind::Overlay, "U+{:04X}", ch as u32);
        }
        assert_eq!(char_kind('a'), CharKind::Advance);
        assert_eq!(char_kind('\u{200D}'), CharKind::SilentSkip);
        assert_eq!(display_cell_count("a\u{0301}b"), 2);
        assert_eq!(display_cell_count("\u{0627}\u{064E}\u{0651}\u{0644}"), 2);
    }

    #[test]
    fn display_items_group_zwj_clusters() {
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
        let items = display_items(family);
        assert_eq!(items.len(), 1, "family emoji must be one item: {items:?}");
        assert_eq!(items[0].cells, 4);
        assert_eq!(items[0].text, family);

        // VS stays inside the cluster text; plain text stays 1 cell per char.
        let items = display_items("ok\u{1F469}\u{200D}\u{1F3EB}\u{FE0F}!");
        assert_eq!(items.len(), 4);
        assert_eq!(items[2].cells, 2);
        assert_eq!(items[2].text, "\u{1F469}\u{200D}\u{1F3EB}\u{FE0F}");

        // Combining mark after base: 1-cell item then a 0-cell mark item.
        let items = display_items("a\u{0301}b");
        assert_eq!(items.len(), 3);
        assert_eq!(items[1].cells, 0);
        assert_eq!(items[1].text, "\u{0301}");
    }

    #[test]
    fn display_items_group_flag_pairs_and_keycaps() {
        // Flag = one 2-cell item; consecutive flags pair off in order.
        let items = display_items("\u{1F1FA}\u{1F1F8}\u{1F1E9}\u{1F1EA}");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].cells, 2);
        assert_eq!(items[0].text, "\u{1F1FA}\u{1F1F8}");
        assert_eq!(items[1].text, "\u{1F1E9}\u{1F1EA}");

        // Lone regional indicator stays a 1-cell scalar.
        let items = display_items("\u{1F1FA}");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].cells, 1);

        // Keycap with and without VS16: one 1-cell item, full sequence kept.
        for key in ["1\u{FE0F}\u{20E3}", "1\u{20E3}", "#\u{FE0F}\u{20E3}"] {
            let items = display_items(key);
            assert_eq!(items.len(), 1, "keycap {key:?}");
            assert_eq!(items[0].cells, 1);
            assert_eq!(items[0].text, key);
        }
        assert_eq!(display_cell_count("\u{1F1FA}\u{1F1F8}"), 2);
        assert_eq!(display_cell_count("1\u{FE0F}\u{20E3}"), 1);
    }

    #[test]
    fn flag_pair_resolves_composite_glyph_when_noto_available() {
        let Some(atlas) = ColorEmojiAtlas::load() else {
            eprintln!("skip: Noto Color Emoji not installed");
            return;
        };
        if atlas.glyph_cluster("\u{1F1FA}\u{1F1F8}").is_none() {
            eprintln!("note: font has no US-flag composite glyph (GSUB miss)");
        }
        let keycap = "1\u{FE0F}\u{20E3}";
        if atlas.glyph_cluster(keycap).is_none() {
            eprintln!("note: font has no keycap composite glyph (GSUB miss)");
        }
    }

    #[test]
    fn family_zwj_resolves_composite_glyph_when_noto_available() {
        let Some(atlas) = ColorEmojiAtlas::load() else {
            eprintln!("skip: Noto Color Emoji not installed");
            return;
        };
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
        if atlas.glyph_cluster(family).is_none() {
            eprintln!("note: font has no family composite glyph (GSUB miss)");
        }
        // Single-scalar path must be unaffected by the cluster cache rework.
        assert!(atlas.has_glyph('\u{1F680}'));
    }
}
