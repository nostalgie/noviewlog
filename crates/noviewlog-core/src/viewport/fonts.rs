use std::collections::HashMap;

use fontdue::layout::GlyphRasterConfig;
use fontdue::Font;

use super::ViewportMetrics;
use crate::core::types::DEFAULT_VIEWPORT_FONT_SIZE;

/// Probe size when checking whether a fallback font has ink for a glyph.
const FONT_PROBE_SIZE: f32 = DEFAULT_VIEWPORT_FONT_SIZE;
pub(super) struct FontStack {
    pub(super) primary: std::sync::Arc<Font>,
    pub(super) fallback: Option<std::sync::Arc<Font>>,
    /// Memoized `glyph_has_ink` answers for the fallback font. A probe is a
    /// full rasterization whose result was discarded; braille spinners (npm /
    /// ora) re-probe the same chars every frame (issue #61). Bounded by the
    /// fallback font's charset — one bool per distinct char.
    pub(super) ink_probe: std::cell::RefCell<std::collections::HashMap<char, bool>>,
}

impl FontStack {
    pub(super) fn new(
        primary: std::sync::Arc<Font>,
        fallback: Option<std::sync::Arc<Font>>,
    ) -> Self {
        Self {
            primary,
            fallback,
            ink_probe: std::cell::RefCell::new(std::collections::HashMap::new()),
        }
    }

    fn has_fallback_ink(&self, ch: char) -> bool {
        let Some(fb) = &self.fallback else {
            return false;
        };
        if !fb.has_glyph(ch) {
            return false;
        }
        let mut probe = self.ink_probe.borrow_mut();
        *probe.entry(ch).or_insert_with(|| glyph_has_ink(fb, ch))
    }

    pub(super) fn pick(&self, ch: char) -> &Font {
        if let Some(fb) = &self.fallback {
            if emoji_or_symbol(ch) && self.has_fallback_ink(ch) {
                return fb;
            }
            if !self.primary.has_glyph(ch) && self.has_fallback_ink(ch) {
                return fb;
            }
        }
        &self.primary
    }
}

pub(super) struct CachedGlyph {
    pub(super) width: usize,
    pub(super) height: usize,
    pub(super) bitmap: Vec<u8>,
}

/// Fontdue raster cache for the current font size (cleared on size change).
/// Bounded: CJK-heavy sessions rasterize thousands of distinct glyphs; an
/// unbounded map grew for the whole session (issue #61).
pub(super) struct GlyphCache {
    pub(super) entries: HashMap<(usize, u16, u32), CachedGlyph>,
}

/// Max rasterized glyphs kept. Typical Latin workloads sit well below; CJK
/// scrolls evict ~1/8 of the map (arbitrary victims — recency tracking is not
/// worth its cost for ~300-byte coverage masks).
pub(super) const GLYPH_CACHE_CAP: usize = 16_384;

impl GlyphCache {
    pub(super) fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
    }

    fn rasterize(&mut self, font: &Font, key: GlyphRasterConfig) -> &CachedGlyph {
        let cache_key = (key.font_hash, key.glyph_index, key.px.to_bits());
        if !self.entries.contains_key(&cache_key) && self.entries.len() >= GLYPH_CACHE_CAP {
            self.evict(GLYPH_CACHE_CAP / 8);
        }
        self.entries.entry(cache_key).or_insert_with(|| {
            let (metrics, bitmap) = font.rasterize_config(key);
            CachedGlyph {
                width: metrics.width,
                height: metrics.height,
                bitmap,
            }
        })
    }

    /// Rasterize by glyph index (no Layout needed; issue #61).
    pub(super) fn rasterize_indexed(
        &mut self,
        font: &Font,
        glyph_index: u16,
        px: f32,
    ) -> &CachedGlyph {
        let key = GlyphRasterConfig {
            glyph_index,
            px,
            font_hash: font.file_hash(),
        };
        self.rasterize(font, key)
    }

    /// Drop `n` arbitrary entries (HashMap order — effectively random).
    fn evict(&mut self, n: usize) {
        let victims: Vec<_> = self.entries.keys().take(n).copied().collect();
        for key in victims {
            self.entries.remove(&key);
        }
    }
}
pub(super) fn compute_metrics(primary: &Font, font_size: f32) -> ViewportMetrics {
    let line_metrics = primary.horizontal_line_metrics(font_size);
    let ascent = line_metrics
        .map(|m| m.ascent)
        .filter(|a| *a > 0.0)
        .unwrap_or(font_size);
    let row_height = line_metrics
        .map(|m| m.new_line_size)
        .filter(|h| *h > 0.0)
        .unwrap_or(font_size + 2.0);
    let row_stride = row_height.max(1.0);
    let cell_width = mono_cell_width(primary, font_size);
    ViewportMetrics {
        row_height: row_stride,
        row_stride,
        ascent,
        cell_width,
    }
}

fn mono_cell_width(font: &Font, font_size: f32) -> u32 {
    // Monospace fonts should share one advance; sample ASCII + box-drawing for safety.
    const SAMPLES: &[char] = &['M', ' ', '│', '┬', '─', '╭', '╮', '┐', '┘'];
    SAMPLES
        .iter()
        .map(|&ch| font.metrics(ch, font_size).advance_width.ceil() as u32)
        .max()
        .unwrap_or(8)
        .max(1)
}

fn try_load_font(path: &str) -> Option<Font> {
    let data = std::fs::read(path).ok()?;
    Font::from_bytes(data.as_slice(), fontdue::FontSettings::default()).ok()
}

/// fontdue's `Font::from_bytes` parses the whole TTF (~200 ms for a system
/// mono font). Engines are built per test / per app boot, so the parsed
/// fonts are process-wide singletons behind `Arc` — rasterization is `&self`.
static MONO_FONT: std::sync::OnceLock<std::sync::Arc<Font>> = std::sync::OnceLock::new();
static EMOJI_FALLBACK_FONT: std::sync::OnceLock<Option<std::sync::Arc<Font>>> =
    std::sync::OnceLock::new();

pub(super) fn load_mono_font() -> std::sync::Arc<Font> {
    MONO_FONT
        .get_or_init(|| std::sync::Arc::new(load_mono_font_from_disk()))
        .clone()
}

fn load_mono_font_from_disk() -> Font {
    let mut candidates: Vec<String> = Vec::new();
    if let Some(home) = dirs::home_dir() {
        let home_str = home.to_string_lossy();
        for name in [
            "JetBrainsMono-Regular.ttf",
            "FiraCode-Regular.ttf",
            "CascadiaMono.ttf",
            "CascadiaCode-Regular.ttf",
        ] {
            candidates.push(format!("{home_str}/.local/share/fonts/{name}"));
            candidates.push(format!("{home_str}/.fonts/{name}"));
            // Windows user fonts folder
            candidates.push(format!(
                "{home_str}/AppData/Local/Microsoft/Windows/Fonts/{name}"
            ));
        }
    }
    for path in [
        // Linux
        "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Regular.ttf",
        "/usr/share/fonts/truetype/JetBrainsMono/JetBrainsMono-Regular.ttf",
        "/usr/share/fonts/truetype/firacode/FiraCode-Regular.ttf",
        "/usr/share/fonts/truetype/FiraCode/FiraCode-Regular.ttf",
        "/usr/share/fonts/truetype/cascadia-code/CascadiaMono.ttf",
        "/usr/share/fonts/truetype/cascadia/CascadiaMono.ttf",
        "/usr/share/fonts/opentype/cascadia-code/CascadiaMono.ttf",
        "/usr/share/fonts/truetype/noto/NotoSansMono-Regular.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
        // Windows system fonts
        "C:\\Windows\\Fonts\\CascadiaMono.ttf",
        "C:\\Windows\\Fonts\\cascadiamono.ttf",
        "C:\\Windows\\Fonts\\CascadiaCode.ttf",
        "C:\\Windows\\Fonts\\consola.ttf",
        "C:\\Windows\\Fonts\\lucon.ttf",
    ] {
        candidates.push(path.to_string());
    }
    for path in &candidates {
        if let Some(font) = try_load_font(path) {
            return font;
        }
    }
    let bundled = include_bytes!("../../../../assets/NotoSansMono-Regular.ttf");
    Font::from_bytes(&bundled[..], fontdue::FontSettings::default())
        .expect("failed to load bundled NotoSansMono-Regular.ttf")
}

/// Monochrome emoji/symbol fallback (Noto Sans Symbols 2).
/// Color pictographs use `ColorEmojiAtlas` (CBDT) when the system font is present;
/// this path covers symbols fontdue can rasterize (⚡, ✔, braille spinners, etc.).
pub(super) fn load_emoji_fallback_font() -> Option<std::sync::Arc<Font>> {
    EMOJI_FALLBACK_FONT
        .get_or_init(|| load_emoji_fallback_font_from_disk().map(std::sync::Arc::new))
        .clone()
}

fn load_emoji_fallback_font_from_disk() -> Option<Font> {
    const CANDIDATES: &[&str] = &[
        "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
        "/usr/share/fonts/truetype/noto/NotoSansSymbols-Regular.ttf",
    ];
    for path in CANDIDATES {
        if let Some(font) = try_load_font(path) {
            if glyph_has_ink(&font, '⚡') {
                return Some(font);
            }
        }
    }
    let bundled = include_bytes!("../../../../assets/NotoSansSymbols2-Regular.ttf");
    Font::from_bytes(&bundled[..], fontdue::FontSettings::default()).ok()
}

fn emoji_or_symbol(ch: char) -> bool {
    // Shared classifier (issue #85): color_emoji.rs owns the range tables so
    // the color draw path and this font-fallback pick cannot drift apart.
    crate::color_emoji::is_symbol_font_candidate(ch)
}

pub(super) fn glyph_has_ink(font: &Font, ch: char) -> bool {
    let (_, bitmap) = font.rasterize(ch, FONT_PROBE_SIZE);
    bitmap.iter().any(|&a| a > 0)
}
/// Baseline of a single-line fontdue layout with pen origin `row_top`
/// (`LayoutSettings { y: row_top }`, default alignment). Fonts without
/// horizontal line metrics contribute zero ascent (matching fontdue).
pub(super) fn glyph_baseline_y(font: &Font, font_size: f32, row_top: f32) -> f32 {
    let ascent = font
        .horizontal_line_metrics(font_size)
        .map(|m| m.ascent.ceil())
        .unwrap_or(0.0);
    row_top + ascent
}
