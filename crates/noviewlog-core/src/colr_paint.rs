//! COLRv0/v1 color-glyph rasterizer (issue #67).
//!
//! Windows ships Segoe UI Emoji as a COLR/CPAL vector font — no CBDT PNG
//! strikes, so the bitmap path in [`crate::color_emoji`] finds nothing there.
//! This module renders COLR glyphs via `ttf-parser`'s [`Painter`] callback
//! API: outlines are scanline-rasterized with 4x vertical supersampling
//! (nonzero winding), layers are composited in premultiplied float space,
//! and gradients (a COLRv1 Segoe specialty) are evaluated per pixel.

use ttf_parser::colr::{ClipBox, ColorStop, CompositeMode, GradientExtend, Paint, Painter};
use ttf_parser::{Face, GlyphId, OutlineBuilder, RgbaColor, Transform};

/// Render a COLR color glyph at `size_px` em size. Returns a tight RGBA8
/// buffer (premultiplied-composited, straight alpha out) of `width`×`height`.
pub fn render_colr_glyph(
    face: &Face,
    glyph_id: GlyphId,
    size_px: f32,
) -> Option<(Vec<u8>, u32, u32)> {
    let upem = face.units_per_em().max(1) as f32;
    let scale = size_px / upem;

    // Pass 1: overall bounds over every outline bbox / clip box under its
    // transform, so the canvas can be allocated once, tightly.
    let mut bounds = BoundsPainter::new(face, scale);
    face.paint_color_glyph(glyph_id, 0, foreground(), &mut bounds)?;
    let (min_fx, max_fx, min_fy, max_fy) = bounds.bounds()?;

    let pad_px = 2.0;
    let min_fx = min_fx - pad_px / scale;
    let max_fx = max_fx + pad_px / scale;
    let min_fy = min_fy - pad_px / scale;
    let max_fy = max_fy + pad_px / scale;
    let width = ((max_fx - min_fx) * scale).ceil().max(1.0) as i32;
    let height = ((max_fy - min_fy) * scale).ceil().max(1.0) as i32;
    // Hard cap: a corrupt paint graph must not OOM the viewport thread.
    if width > 1024 || height > 1024 || width < 1 || height < 1 {
        return None;
    }
    let view = View {
        scale,
        min_fx,
        max_fy,
        width,
        height,
    };

    // Pass 2: real paint.
    let mut painter = RasterPainter::new(face, view);
    face.paint_color_glyph(glyph_id, 0, foreground(), &mut painter)?;
    let canvas = painter.finish();
    Some((canvas.to_rgba8(), width as u32, height as u32))
}

fn foreground() -> RgbaColor {
    // CPAL palette 0 foreground placeholder; emoji fonts never key to it.
    RgbaColor::new(0, 0, 0, 255)
}

/// Apply a 2x3 affine to a point (`ttf_parser::Transform` has no public
/// point-mapping helper).
#[inline]
fn apply_t(t: Transform, x: f32, y: f32) -> (f32, f32) {
    (t.a * x + t.c * y + t.e, t.b * x + t.d * y + t.f)
}

/// Font-space → pixel mapping. Pixels are y-down, font units y-up:
/// `px = (fx - min_fx) * scale`, `py = (max_fy - fy) * scale`.
#[derive(Clone, Copy)]
struct View {
    scale: f32,
    min_fx: f32,
    max_fy: f32,
    width: i32,
    height: i32,
}

impl View {
    #[inline]
    fn as_px(&self, fx: f32, fy: f32) -> (f32, f32) {
        (
            (fx - self.min_fx) * self.scale,
            (self.max_fy - fy) * self.scale,
        )
    }
    #[inline]
    fn as_font(&self, px: f32, py: f32) -> (f32, f32) {
        (px / self.scale + self.min_fx, self.max_fy - py / self.scale)
    }
}

/// Premultiplied RGBA float canvas (4 f32 per pixel).
#[derive(Clone)]
struct Canvas {
    px: Vec<f32>,
}

impl Canvas {
    fn new(w: i32, h: i32) -> Self {
        Self {
            px: vec![0.0; (w * h * 4) as usize],
        }
    }
    fn to_rgba8(&self) -> Vec<u8> {
        // Premultiplied → straight alpha.
        let mut out = vec![0u8; self.px.len()];
        for i in (0..self.px.len()).step_by(4) {
            let a = self.px[i + 3].clamp(0.0, 1.0);
            let k = if a > 0.0 { 1.0 / a } else { 0.0 };
            out[i] = (self.px[i].clamp(0.0, 1.0) * k * 255.0).round() as u8;
            out[i + 1] = (self.px[i + 1].clamp(0.0, 1.0) * k * 255.0).round() as u8;
            out[i + 2] = (self.px[i + 2].clamp(0.0, 1.0) * k * 255.0).round() as u8;
            out[i + 3] = (a * 255.0).round() as u8;
        }
        out
    }
}

/// Per-pixel alpha mask (clip paths, outline coverage).
#[derive(Clone)]
struct Mask {
    a: Vec<f32>,
}

/// Composite `src` onto `dst` (same size) in premultiplied space.
fn composite(dst: &mut Canvas, src: &Canvas, mode: CompositeMode) {
    // Canvas is 4 f32 per pixel: iterate pixels, not bytes.
    for i in (0..dst.px.len()).step_by(4) {
        let (sr, sg, sb, sa) = (src.px[i], src.px[i + 1], src.px[i + 2], src.px[i + 3]);
        let (dr, dg, db, da) = (dst.px[i], dst.px[i + 1], dst.px[i + 2], dst.px[i + 3]);
        let (r, g, b, a) = match mode {
            CompositeMode::Clear => (0.0, 0.0, 0.0, 0.0),
            CompositeMode::Source => (sr, sg, sb, sa),
            CompositeMode::Destination => (dr, dg, db, da),
            CompositeMode::SourceOver => (
                sr + dr * (1.0 - sa),
                sg + dg * (1.0 - sa),
                sb + db * (1.0 - sa),
                sa + da * (1.0 - sa),
            ),
            CompositeMode::DestinationOver => (
                dr + sr * (1.0 - da),
                dg + sg * (1.0 - da),
                db + sb * (1.0 - da),
                da + sa * (1.0 - da),
            ),
            CompositeMode::SourceIn => (sr * da, sg * da, sb * da, sa * da),
            CompositeMode::DestinationIn => (dr * sa, dg * sa, db * sa, da * sa),
            CompositeMode::SourceOut => (
                sr * (1.0 - da),
                sg * (1.0 - da),
                sb * (1.0 - da),
                sa * (1.0 - da),
            ),
            CompositeMode::DestinationOut => (
                dr * (1.0 - sa),
                dg * (1.0 - sa),
                db * (1.0 - sa),
                da * (1.0 - sa),
            ),
            CompositeMode::SourceAtop => (
                sr * da + dr * (1.0 - sa),
                sg * da + dg * (1.0 - sa),
                sb * da + db * (1.0 - sa),
                da,
            ),
            CompositeMode::DestinationAtop => (
                dr * sa + sr * (1.0 - da),
                dg * sa + sg * (1.0 - da),
                db * sa + sb * (1.0 - da),
                sa,
            ),
            CompositeMode::Xor => (
                sr * (1.0 - da) + dr * (1.0 - sa),
                sg * (1.0 - da) + dg * (1.0 - sa),
                sb * (1.0 - da) + db * (1.0 - sa),
                sa + da - 2.0 * sa * da,
            ),
            CompositeMode::Plus => (sr + dr, sg + dg, sb + db, sa + da),
            CompositeMode::Multiply => blend(sa, da, sr * dr, sg * dg, sb * db),
            CompositeMode::Screen => blend(
                sa,
                da,
                sr + dr - sr * dr,
                sg + dg - sg * dg,
                sb + db - sb * db,
            ),
            CompositeMode::Darken => blend(sa, da, sr.min(dr), sg.min(dg), sb.min(db)),
            CompositeMode::Lighten => blend(sa, da, sr.max(dr), sg.max(dg), sb.max(db)),
            CompositeMode::Difference => {
                blend(sa, da, (sr - dr).abs(), (sg - dg).abs(), (sb - db).abs())
            }
            CompositeMode::Exclusion => blend(
                sa,
                da,
                sr + dr - 2.0 * sr * dr,
                sg + dg - 2.0 * sg * dg,
                sb + db - 2.0 * sb * db,
            ),
            // Blend modes Segoe emoji does not rely on (overlay, dodge, burn,
            // soft-light, HSL…) — approximate as source-over.
            _ => (
                sr + dr * (1.0 - sa),
                sg + dg * (1.0 - sa),
                sb + db * (1.0 - sa),
                sa + da * (1.0 - sa),
            ),
        };
        dst.px[i] = r;
        dst.px[i + 1] = g;
        dst.px[i + 2] = b;
        dst.px[i + 3] = a;
    }
}

/// W3C blend with premultiplied inputs: result keeps source-over alpha.
fn blend(sa: f32, da: f32, r: f32, g: f32, b: f32) -> (f32, f32, f32, f32) {
    let a = sa + da * (1.0 - sa);
    let k = if a > 0.0 { 1.0 / a } else { 0.0 };
    (r * k * a, g * k * a, b * k * a, a)
}

/// A flattened outline in pixel space.
struct Outline {
    /// Contours of (x, y) points; closed implicitly.
    contours: Vec<Vec<(f32, f32)>>,
}

impl Outline {
    /// Anti-aliased coverage via 4x supersampled scanlines, nonzero winding.
    fn coverage(&self, view: View) -> Mask {
        const SUB: usize = 4;
        let w = view.width as usize;
        let h = view.height as usize;
        let mut a = vec![0.0f32; w * h];
        let mut crossings: Vec<(f32, i32)> = Vec::new();
        for row in 0..h {
            let mut acc = vec![0.0f32; w];
            for sub in 0..SUB {
                let y = row as f32 + (sub as f32 + 0.5) / SUB as f32;
                crossings.clear();
                for contour in &self.contours {
                    let n = contour.len();
                    if n < 2 {
                        continue;
                    }
                    for i in 0..n {
                        let (x0, y0) = contour[i];
                        let (x1, y1) = contour[(i + 1) % n];
                        if (y0 <= y && y1 > y) || (y1 <= y && y0 > y) {
                            let t = (y - y0) / (y1 - y0);
                            crossings.push((x0 + t * (x1 - x0), if y1 > y0 { 1 } else { -1 }));
                        }
                    }
                }
                if crossings.len() < 2 {
                    continue;
                }
                crossings
                    .sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
                let mut winding = 0i32;
                let mut prev = 0.0f32;
                for &(x, dir) in &crossings {
                    if winding != 0 {
                        add_span(&mut acc, prev, x, winding.unsigned_abs() as f32);
                    }
                    winding += dir;
                    prev = x;
                }
            }
            for (dst, src) in a[row * w..row * w + w].iter_mut().zip(&acc) {
                *dst = (*src / SUB as f32).clamp(0.0, 1.0);
            }
        }
        Mask { a }
    }
}

/// Add coverage `w` to `acc` over the pixel span [x0, x1) with fractional ends.
fn add_span(acc: &mut [f32], x0: f32, x1: f32, w: f32) {
    if acc.is_empty() {
        return;
    }
    let last = acc.len() - 1;
    let (a, b) = (x0.max(0.0), x1.min(acc.len() as f32));
    if b <= a {
        return;
    }
    let a0 = (a as usize).min(last);
    let b0 = (b as usize).min(last);
    if a0 == b0 {
        acc[a0] += w * (b - a);
    } else {
        acc[a0] += w * (a0 as f32 + 1.0 - a);
        for p in acc.iter_mut().take(b0).skip(a0 + 1) {
            *p += w;
        }
        acc[b0] += w * (b - b0 as f32);
    }
}

/// OutlineBuilder that applies the current transform and flattens curves
/// directly into pixel space.
struct PathBuilder {
    transform: Transform,
    view: View,
    contours: Vec<Vec<(f32, f32)>>,
    current: Vec<(f32, f32)>,
}

impl PathBuilder {
    /// Transform a font-space point into pixel space.
    #[inline]
    fn pt(&self, x: f32, y: f32) -> (f32, f32) {
        let (tx, ty) = apply_t(self.transform, x, y);
        self.view.as_px(tx, ty)
    }
    fn push_transformed(&mut self, x: f32, y: f32) {
        let (tx, ty) = apply_t(self.transform, x, y);
        let p = self.view.as_px(tx, ty);
        self.current.push(p);
    }
}

impl OutlineBuilder for PathBuilder {
    fn move_to(&mut self, x: f32, y: f32) {
        if self.current.len() > 1 {
            self.contours.push(std::mem::take(&mut self.current));
        }
        self.current.clear();
        self.push_transformed(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.push_transformed(x, y);
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        let (px, py) = *self.current.last().expect("quad without move");
        let (cx, cy) = self.pt(cx, cy);
        let (ex, ey) = self.pt(x, y);
        const STEPS: usize = 16;
        for i in 1..=STEPS {
            let t = i as f32 / STEPS as f32;
            let mt = 1.0 - t;
            self.current.push((
                mt * mt * px + 2.0 * mt * t * cx + t * t * ex,
                mt * mt * py + 2.0 * mt * t * cy + t * t * ey,
            ));
        }
    }
    fn curve_to(&mut self, cx: f32, cy: f32, dx: f32, dy: f32, x: f32, y: f32) {
        let (px, py) = *self.current.last().expect("cubic without move");
        let (cx, cy) = self.pt(cx, cy);
        let (dx, dy) = self.pt(dx, dy);
        let (ex, ey) = self.pt(x, y);
        const STEPS: usize = 24;
        for i in 1..=STEPS {
            let t = i as f32 / STEPS as f32;
            let mt = 1.0 - t;
            self.current.push((
                mt * mt * mt * px + 3.0 * mt * mt * t * cx + 3.0 * mt * t * t * dx + t * t * t * ex,
                mt * mt * mt * py + 3.0 * mt * mt * t * cy + 3.0 * mt * t * t * dy + t * t * t * ey,
            ));
        }
    }
    fn close(&mut self) {
        if self.current.len() > 1 {
            self.contours.push(std::mem::take(&mut self.current));
        }
        self.current.clear();
    }
}

/// First pass: bounds of everything the paint graph may draw.
struct BoundsPainter<'f> {
    face: &'f Face<'f>,
    transform: Transform,
    transform_stack: Vec<Transform>,
    min_x: Option<f32>,
    max_x: Option<f32>,
    min_y: Option<f32>,
    max_y: Option<f32>,
}

impl<'f> BoundsPainter<'f> {
    fn new(face: &'f Face<'f>, _scale: f32) -> Self {
        Self {
            face,
            transform: Transform::default(),
            transform_stack: Vec::new(),
            min_x: None,
            max_x: None,
            min_y: None,
            max_y: None,
        }
    }

    fn bounds(&self) -> Option<(f32, f32, f32, f32)> {
        Some((self.min_x?, self.max_x?, self.min_y?, self.max_y?))
    }

    fn include(&mut self, x: f32, y: f32) {
        let (tx, ty) = apply_t(self.transform, x, y);
        let min_x = self.min_x.get_or_insert(tx);
        if tx < *min_x {
            *min_x = tx;
        }
        let max_x = self.max_x.get_or_insert(tx);
        if tx > *max_x {
            *max_x = tx;
        }
        let min_y = self.min_y.get_or_insert(ty);
        if ty < *min_y {
            *min_y = ty;
        }
        let max_y = self.max_y.get_or_insert(ty);
        if ty > *max_y {
            *max_y = ty;
        }
    }
}

impl Painter<'_> for BoundsPainter<'_> {
    fn outline_glyph(&mut self, glyph_id: GlyphId) {
        if let Some(bbox) = self.face.glyph_bounding_box(glyph_id) {
            self.include(bbox.x_min as f32, bbox.y_min as f32);
            self.include(bbox.x_max as f32, bbox.y_max as f32);
        }
    }
    fn paint(&mut self, _: Paint) {}
    fn push_clip(&mut self) {}
    fn push_clip_box(&mut self, clipbox: ClipBox) {
        self.include(clipbox.x_min, clipbox.y_min);
        self.include(clipbox.x_max, clipbox.y_max);
    }
    fn pop_clip(&mut self) {}
    fn push_layer(&mut self, _: CompositeMode) {}
    fn pop_layer(&mut self) {}
    fn push_transform(&mut self, transform: Transform) {
        self.transform_stack.push(self.transform);
        self.transform = Transform::combine(self.transform, transform);
    }
    fn pop_transform(&mut self) {
        if let Some(prev) = self.transform_stack.pop() {
            self.transform = prev;
        }
    }
}

/// Second pass: real rasterization.
struct RasterPainter<'f> {
    face: &'f Face<'f>,
    view: View,
    transform: Transform,
    transform_stack: Vec<Transform>,
    /// Layer stack; on pop, the top canvas is composited onto the one below
    /// with the top's recorded mode. The bottom entry becomes the result.
    layers: Vec<(CompositeMode, Canvas)>,
    clip_stack: Vec<Mask>,
    /// Current outline awaiting `paint`.
    outline: Option<Outline>,
}

impl<'f> RasterPainter<'f> {
    fn new(face: &'f Face<'f>, view: View) -> Self {
        Self {
            face,
            view,
            transform: Transform::default(),
            transform_stack: Vec::new(),
            layers: vec![(
                CompositeMode::SourceOver,
                Canvas::new(view.width, view.height),
            )],
            clip_stack: Vec::new(),
            outline: None,
        }
    }

    fn finish(mut self) -> Canvas {
        while self.layers.len() > 1 {
            self.pop_layer();
        }
        self.layers.pop().map(|(_, c)| c).unwrap()
    }

    fn pop_layer(&mut self) {
        if self.layers.len() < 2 {
            return;
        }
        let (mode, top) = self.layers.pop().expect("len checked");
        let (_, below) = self.layers.last_mut().expect("len checked");
        composite(below, &top, mode);
    }
}

/// Invert a 2x3 affine (x' = a·x + c·y + e). Returns `None` when singular.
fn invert_t(t: Transform) -> Option<Transform> {
    let det = t.a * t.d - t.b * t.c;
    if det.abs() <= f32::EPSILON {
        return None;
    }
    let inv = 1.0 / det;
    Some(Transform {
        a: t.d * inv,
        b: -t.b * inv,
        c: -t.c * inv,
        d: t.a * inv,
        e: (t.c * t.f - t.d * t.e) * inv,
        f: (t.b * t.e - t.a * t.f) * inv,
    })
}

/// Color of `paint` at pixel (px, py). Outlines are recorded in the base
/// space as `T(raw_point)` with the cumulative transform `T`; gradient
/// endpoints are raw. So map the pixel's base-space point back through
/// `T⁻¹` into the gradient's own space before comparing.
fn eval_color(transform: Transform, view: View, paint: &Paint, px: f32, py: f32) -> [f32; 4] {
    let (bx, by) = view.as_font(px, py);
    let (fx, fy) = invert_t(transform).map_or((bx, by), |inv| apply_t(inv, bx, by));
    match paint {
        Paint::Solid(c) => rgba_to_f32(*c),
        Paint::LinearGradient(g) => {
            let (dx, dy) = (g.x1 - g.x0, g.y1 - g.y0);
            let len2 = dx * dx + dy * dy;
            let t = if len2 <= f32::EPSILON {
                0.0
            } else {
                ((fx - g.x0) * dx + (fy - g.y0) * dy) / len2
            };
            gradient_color(g.stops(0, &[]), t, g.extend)
        }
        Paint::RadialGradient(g) => {
            let dist = ((fx - g.x1).powi(2) + (fy - g.y1).powi(2)).sqrt();
            let denom = (g.r1 - g.r0).abs();
            let t = if denom <= f32::EPSILON {
                0.0
            } else {
                (dist - g.r0) / (g.r1 - g.r0)
            };
            gradient_color(g.stops(0, &[]), t, g.extend)
        }
        Paint::SweepGradient(g) => {
            // Sweep gradients are rare in emoji fonts; mid-stop color.
            let stops: Vec<_> = g.stops(0, &[]).collect();
            stops
                .get(stops.len() / 2)
                .map(|s| rgba_to_f32(s.color))
                .unwrap_or([0.0, 0.0, 0.0, 1.0])
        }
    }
}

fn rgba_to_f32(c: RgbaColor) -> [f32; 4] {
    [
        c.red as f32 / 255.0,
        c.green as f32 / 255.0,
        c.blue as f32 / 255.0,
        c.alpha as f32 / 255.0,
    ]
}

fn gradient_color(
    stops: impl Iterator<Item = ColorStop>,
    t: f32,
    extend: GradientExtend,
) -> [f32; 4] {
    let t = match extend {
        GradientExtend::Pad => t.clamp(0.0, 1.0),
        GradientExtend::Repeat => t.rem_euclid(1.0),
        GradientExtend::Reflect => {
            let r = t.rem_euclid(2.0);
            if r > 1.0 {
                2.0 - r
            } else {
                r
            }
        }
    };
    let mut stops: Vec<_> = stops.collect();
    stops.sort_by(|a, b| {
        a.stop_offset
            .partial_cmp(&b.stop_offset)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    if stops.is_empty() {
        return [0.0, 0.0, 0.0, 1.0];
    }
    if t <= stops[0].stop_offset {
        return rgba_to_f32(stops[0].color);
    }
    if t >= stops[stops.len() - 1].stop_offset {
        return rgba_to_f32(stops[stops.len() - 1].color);
    }
    for w in stops.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        if t >= a.stop_offset && t <= b.stop_offset {
            let span = b.stop_offset - a.stop_offset;
            let k = if span <= f32::EPSILON {
                0.0
            } else {
                (t - a.stop_offset) / span
            };
            let ca = rgba_to_f32(a.color);
            let cb = rgba_to_f32(b.color);
            return [
                ca[0] + (cb[0] - ca[0]) * k,
                ca[1] + (cb[1] - ca[1]) * k,
                ca[2] + (cb[2] - ca[2]) * k,
                ca[3] + (cb[3] - ca[3]) * k,
            ];
        }
    }
    [0.0, 0.0, 0.0, 1.0]
}

impl Painter<'_> for RasterPainter<'_> {
    fn outline_glyph(&mut self, glyph_id: GlyphId) {
        let mut builder = PathBuilder {
            transform: self.transform,
            view: self.view,
            contours: Vec::new(),
            current: Vec::new(),
        };
        if self.face.outline_glyph(glyph_id, &mut builder).is_some() {
            if builder.current.len() > 1 {
                builder.contours.push(std::mem::take(&mut builder.current));
            }
            self.outline = Some(Outline {
                contours: builder.contours,
            });
        } else {
            self.outline = None;
        }
    }

    fn paint(&mut self, paint: Paint) {
        // COLRv0 paints a pending outline; COLRv1 `PaintGlyph` pushes the
        // glyph outline as a clip and the nested paint then fills *that*
        // region (no pending outline of its own).
        let (mask, outer_clips) = if let Some(outline) = self.outline.take() {
            (outline.coverage(self.view), self.clip_stack.len())
        } else if let Some(top) = self.clip_stack.last() {
            // The top clip IS the fill region; only clips below it apply.
            (top.clone(), self.clip_stack.len() - 1)
        } else {
            return;
        };
        let (_, canvas) = self.layers.last_mut().expect("base layer");
        let view = self.view;
        let transform = self.transform;
        for y in 0..view.height {
            for x in 0..view.width {
                let i = (y * view.width + x) as usize;
                let mut cov = mask.a[i];
                for clip in &self.clip_stack[..outer_clips] {
                    cov *= clip.a[i];
                    if cov <= 0.0 {
                        break;
                    }
                }
                if cov <= 0.0 {
                    continue;
                }
                let [r, g, b, a] =
                    eval_color(transform, view, &paint, x as f32 + 0.5, y as f32 + 0.5);
                let sa = a * cov;
                let d = i * 4;
                canvas.px[d] += r * sa;
                canvas.px[d + 1] += g * sa;
                canvas.px[d + 2] += b * sa;
                canvas.px[d + 3] += sa;
            }
        }
    }

    fn push_clip(&mut self) {
        if let Some(outline) = self.outline.take() {
            self.clip_stack.push(outline.coverage(self.view));
        } else {
            // No outline ⇒ clip to everything.
            self.clip_stack.push(Mask {
                a: vec![1.0; (self.view.width * self.view.height) as usize],
            });
        }
    }

    fn push_clip_box(&mut self, clipbox: ClipBox) {
        let corners = [
            apply_t(self.transform, clipbox.x_min, clipbox.y_min),
            apply_t(self.transform, clipbox.x_max, clipbox.y_min),
            apply_t(self.transform, clipbox.x_max, clipbox.y_max),
            apply_t(self.transform, clipbox.x_min, clipbox.y_max),
        ];
        let px: Vec<(f32, f32)> = corners
            .iter()
            .map(|(fx, fy)| self.view.as_px(*fx, *fy))
            .collect();
        let x0 = px.iter().map(|p| p.0).fold(f32::INFINITY, f32::min);
        let x1 = px.iter().map(|p| p.0).fold(f32::NEG_INFINITY, f32::max);
        let y0 = px.iter().map(|p| p.1).fold(f32::INFINITY, f32::min);
        let y1 = px.iter().map(|p| p.1).fold(f32::NEG_INFINITY, f32::max);
        let mut mask = Mask {
            a: vec![0.0; (self.view.width * self.view.height) as usize],
        };
        let xs = x0.ceil().max(0.0) as i32..x1.floor().min(self.view.width as f32) as i32;
        let ys = y0.ceil().max(0.0) as i32..y1.floor().min(self.view.height as f32) as i32;
        for y in ys {
            for x in xs.clone() {
                mask.a[(y * self.view.width + x) as usize] = 1.0;
            }
        }
        self.clip_stack.push(mask);
    }

    fn pop_clip(&mut self) {
        self.clip_stack.pop();
    }

    fn push_layer(&mut self, mode: CompositeMode) {
        self.layers
            .push((mode, Canvas::new(self.view.width, self.view.height)));
    }
    fn pop_layer(&mut self) {
        RasterPainter::pop_layer(self);
    }

    fn push_transform(&mut self, transform: Transform) {
        self.transform_stack.push(self.transform);
        self.transform = Transform::combine(self.transform, transform);
    }
    fn pop_transform(&mut self) {
        if let Some(prev) = self.transform_stack.pop() {
            self.transform = prev;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(w: i32, h: i32) -> View {
        View {
            scale: 1.0,
            min_fx: 0.0,
            max_fy: h as f32,
            width: w,
            height: h,
        }
    }

    fn canvas_1x1(px: [f32; 4]) -> Canvas {
        let mut canvas = Canvas::new(1, 1);
        canvas.px.copy_from_slice(&px);
        canvas
    }

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 1e-4,
            "expected {expected}, got {actual}"
        );
    }

    fn assert_pixel(actual: [f32; 4], expected: [f32; 4]) {
        for (a, e) in actual.iter().zip(expected) {
            assert_close(*a, e);
        }
    }

    fn composite_pixel(mode: CompositeMode, dst: [f32; 4], src: [f32; 4]) -> [f32; 4] {
        let mut out = canvas_1x1(dst);
        composite(&mut out, &canvas_1x1(src), mode);
        out.px.try_into().expect("1x1 canvas")
    }

    fn stop(offset: f32, color: RgbaColor) -> ColorStop {
        ColorStop {
            stop_offset: offset,
            color,
        }
    }

    fn stops(colors: &[(f32, RgbaColor)]) -> impl Iterator<Item = ColorStop> + '_ {
        colors.iter().map(|(offset, color)| stop(*offset, *color))
    }

    #[test]
    fn view_maps_font_space_y_up_to_pixels_y_down() {
        let view = view(4, 4);
        // Top of the font box maps to pixel row 0.
        assert_pair(view.as_px(0.0, 4.0), (0.0, 0.0));
        assert_pair(view.as_px(0.0, 0.0), (0.0, 4.0));
        // Roundtrip through both directions is lossless.
        for (fx, fy) in [(1.5, 2.25), (0.0, 4.0), (4.0, 0.0)] {
            let (px, py) = view.as_px(fx, fy);
            let (fx2, fy2) = view.as_font(px, py);
            assert_close(fx2, fx);
            assert_close(fy2, fy);
        }
    }

    fn assert_pair(actual: (f32, f32), expected: (f32, f32)) {
        assert_close(actual.0, expected.0);
        assert_close(actual.1, expected.1);
    }

    #[test]
    fn canvas_to_rgba8_unpremultiplies_and_guards_zero_alpha() {
        let canvas = canvas_1x1([0.5, 0.25, 0.0, 0.5]);
        assert_eq!(
            canvas.to_rgba8(),
            vec![255, 128, 0, 128],
            "straight alpha = color / alpha"
        );
        // Fully transparent pixels must not divide by zero (no NaN panic).
        let transparent = canvas_1x1([0.3, 0.3, 0.3, 0.0]);
        assert_eq!(transparent.to_rgba8(), vec![0, 0, 0, 0]);
    }

    #[test]
    fn blend_with_zero_alpha_stays_finite() {
        let (r, g, b, a) = blend(0.0, 0.0, 1.0, 1.0, 1.0);
        assert_pixel([r, g, b, a], [0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn composite_modes_match_premultiplied_algebra() {
        let dst = [1.0, 0.0, 0.0, 1.0];
        let src = [0.0, 0.5, 0.0, 0.5];
        assert_pixel(composite_pixel(CompositeMode::Clear, dst, src), [0.0; 4]);
        assert_pixel(composite_pixel(CompositeMode::Source, dst, src), src);
        assert_pixel(composite_pixel(CompositeMode::Destination, dst, src), dst);
        assert_pixel(
            composite_pixel(CompositeMode::SourceOver, dst, src),
            [0.5, 0.5, 0.0, 1.0],
        );
        assert_pixel(
            composite_pixel(CompositeMode::DestinationOver, dst, src),
            [1.0, 0.0, 0.0, 1.0],
        );
        assert_pixel(
            composite_pixel(CompositeMode::SourceIn, dst, src),
            [0.0, 0.5, 0.0, 0.5],
        );
        assert_pixel(
            composite_pixel(CompositeMode::DestinationIn, dst, src),
            [0.5, 0.0, 0.0, 0.5],
        );
        assert_pixel(
            composite_pixel(CompositeMode::SourceOut, dst, src),
            [0.0, 0.0, 0.0, 0.0],
        );
        assert_pixel(
            composite_pixel(CompositeMode::DestinationOut, dst, src),
            [0.5, 0.0, 0.0, 0.5],
        );
        assert_pixel(
            composite_pixel(CompositeMode::SourceAtop, dst, src),
            [0.5, 0.5, 0.0, 1.0],
        );
        assert_pixel(
            composite_pixel(CompositeMode::DestinationAtop, dst, src),
            [0.5, 0.0, 0.0, 0.5],
        );
        assert_pixel(
            composite_pixel(CompositeMode::Xor, dst, src),
            [0.5, 0.0, 0.0, 0.5],
        );
        assert_pixel(
            composite_pixel(CompositeMode::Plus, dst, src),
            [1.0, 0.5, 0.0, 1.5],
        );
        // Blend modes route through the W3C helper.
        assert_pixel(
            composite_pixel(CompositeMode::Multiply, dst, src),
            [0.0, 0.0, 0.0, 1.0],
        );
        assert_pixel(
            composite_pixel(CompositeMode::Screen, dst, src),
            [1.0, 0.5, 0.0, 1.0],
        );
        assert_pixel(
            composite_pixel(CompositeMode::Darken, dst, src),
            [0.0, 0.0, 0.0, 1.0],
        );
        assert_pixel(
            composite_pixel(CompositeMode::Lighten, dst, src),
            [1.0, 0.5, 0.0, 1.0],
        );
        assert_pixel(
            composite_pixel(CompositeMode::Difference, dst, src),
            [1.0, 0.5, 0.0, 1.0],
        );
        assert_pixel(
            composite_pixel(CompositeMode::Exclusion, dst, src),
            [1.0, 0.5, 0.0, 1.0],
        );
        // Unhandled Segoe-rare modes approximate as source-over.
        assert_pixel(
            composite_pixel(CompositeMode::Overlay, dst, src),
            [0.5, 0.5, 0.0, 1.0],
        );
    }

    #[test]
    fn gradient_color_empty_stops_falls_back_to_black() {
        assert_pixel(
            gradient_color(std::iter::empty(), 0.5, GradientExtend::Pad),
            [0.0, 0.0, 0.0, 1.0],
        );
    }

    #[test]
    fn gradient_color_pads_clamps_out_of_range_t() {
        let red = RgbaColor::new(255, 0, 0, 255);
        let blue = RgbaColor::new(0, 0, 255, 255);
        let grad = [(0.0, red), (1.0, blue)];
        assert_pixel(
            gradient_color(stops(&grad), -0.3, GradientExtend::Pad),
            [1.0, 0.0, 0.0, 1.0],
        );
        assert_pixel(
            gradient_color(stops(&grad), 1.7, GradientExtend::Pad),
            [0.0, 0.0, 1.0, 1.0],
        );
        assert_pixel(
            gradient_color(stops(&grad), 0.5, GradientExtend::Pad),
            [0.5, 0.0, 0.5, 1.0],
        );
    }

    #[test]
    fn gradient_color_repeat_and_reflect_wrap_t() {
        let red = RgbaColor::new(255, 0, 0, 255);
        let blue = RgbaColor::new(0, 0, 255, 255);
        let grad = [(0.0, red), (1.0, blue)];
        assert_pixel(
            gradient_color(stops(&grad), 1.25, GradientExtend::Repeat),
            [0.75, 0.0, 0.25, 1.0],
        );
        assert_pixel(
            gradient_color(stops(&grad), 1.75, GradientExtend::Reflect),
            [0.75, 0.0, 0.25, 1.0],
        );
    }

    #[test]
    fn gradient_color_sorts_unsorted_stops() {
        let red = RgbaColor::new(255, 0, 0, 255);
        let blue = RgbaColor::new(0, 0, 255, 255);
        // Wire order reversed: the stop sort must still bracket t correctly.
        let grad = [(0.8, blue), (0.2, red)];
        assert_pixel(
            gradient_color(stops(&grad), 0.1, GradientExtend::Pad),
            [1.0, 0.0, 0.0, 1.0],
        );
        assert_pixel(
            gradient_color(stops(&grad), 0.5, GradientExtend::Pad),
            [0.5, 0.0, 0.5, 1.0],
        );
        assert_pixel(
            gradient_color(stops(&grad), 0.9, GradientExtend::Pad),
            [0.0, 0.0, 1.0, 1.0],
        );
    }

    #[test]
    fn gradient_color_degenerate_duplicate_offsets_do_not_divide_by_zero() {
        let red = RgbaColor::new(255, 0, 0, 255);
        let blue = RgbaColor::new(0, 0, 255, 255);
        let grad = [(0.5, red), (0.5, blue)];
        // Exactly on the zero-width span: keeps the lower stop.
        assert_pixel(
            gradient_color(stops(&grad), 0.5, GradientExtend::Pad),
            [1.0, 0.0, 0.0, 1.0],
        );
        // Past both (identical) stops: the last-stop guard wins.
        assert_pixel(
            gradient_color(stops(&grad), 0.7, GradientExtend::Pad),
            [0.0, 0.0, 1.0, 1.0],
        );
    }

    #[test]
    fn invert_t_rejects_singular_and_roundtrips() {
        assert!(invert_t(Transform::default()).is_some());
        let degenerate = Transform {
            a: 0.0,
            b: 0.0,
            c: 0.0,
            d: 0.0,
            e: 1.0,
            f: 1.0,
        };
        assert!(invert_t(degenerate).is_none());

        let t = Transform {
            a: 2.0,
            b: 0.0,
            c: 0.0,
            d: 4.0,
            e: 5.0,
            f: 7.0,
        };
        let inv = invert_t(t).expect("nonsingular");
        let (tx, ty) = apply_t(t, 3.0, -1.0);
        let (x, y) = apply_t(inv, tx, ty);
        assert_close(x, 3.0);
        assert_close(y, -1.0);
    }

    #[test]
    fn add_span_clamps_out_of_range_and_fractional_spans() {
        // Empty accumulator: no panic.
        let mut acc: Vec<f32> = Vec::new();
        add_span(&mut acc, 0.0, 5.0, 1.0);

        acc = vec![0.0; 4];
        // Fractional single-pixel span.
        add_span(&mut acc, 1.2, 1.8, 1.0);
        assert_close(acc[1], 0.6);
        assert_close(acc[0] + acc[2] + acc[3], 0.0);

        // Multi-pixel span with fractional ends.
        add_span(&mut acc, 1.5, 2.5, 2.0);
        assert_close(acc[1], 1.6);
        assert_close(acc[2], 1.0);

        // Span starting left of the canvas and ending inside.
        add_span(&mut acc, -3.0, 1.0, 1.0);
        assert_close(acc[0], 1.0);

        // Span extending past the canvas end clamps without panicking.
        add_span(&mut acc, 3.0, 9.0, 1.0);
        assert_close(acc[3], 1.0);
    }

    #[test]
    fn outline_coverage_ignores_empty_and_degenerate_contours() {
        let view = view(4, 4);
        let empty = Outline {
            contours: Vec::new(),
        };
        assert!(empty.coverage(view).a.iter().all(|&a| a == 0.0));

        // A single-point contour has no edges and must not index out of bounds.
        let point_only = Outline {
            contours: vec![vec![(2.0, 2.0)]],
        };
        assert!(point_only.coverage(view).a.iter().all(|&a| a == 0.0));
    }

    #[test]
    fn outline_coverage_nonzero_winding_fills_and_cancels() {
        let view = view(4, 4);
        let square = Outline {
            contours: vec![vec![(1.0, 1.0), (3.0, 1.0), (3.0, 3.0), (1.0, 3.0)]],
        };
        let mask = square.coverage(view);
        assert_close(mask.a[4 + 1], 1.0);
        assert_close(mask.a[2 * 4 + 2], 1.0);
        // Ring around the square stays empty.
        assert_close(mask.a[0], 0.0);
        assert_close(mask.a[4], 0.0);
        assert_close(mask.a[4 + 3], 0.0);

        // Second contour wound with the outer one punches a hole (nonzero rule).
        let with_hole = Outline {
            contours: vec![
                vec![(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)],
                vec![(1.0, 1.0), (1.0, 3.0), (3.0, 3.0), (3.0, 1.0)],
            ],
        };
        let mask = with_hole.coverage(view);
        assert_close(mask.a[0], 1.0);
        assert_close(mask.a[2], 1.0);
        assert_close(mask.a[2 * 4 + 2], 0.0);
        assert_close(mask.a[2 * 4], 1.0);
        assert_close(mask.a[2 * 4 + 3], 1.0);
        assert_close(mask.a[3 * 4 + 1], 1.0);
    }

    #[test]
    fn path_builder_splits_contours_on_move_and_close() {
        let mut builder = PathBuilder {
            transform: Transform::default(),
            view: view(8, 8),
            contours: Vec::new(),
            current: Vec::new(),
        };
        builder.move_to(1.0, 1.0);
        builder.line_to(3.0, 1.0);
        // A second move_to flushes the pending contour.
        builder.move_to(1.0, 3.0);
        assert_eq!(builder.contours.len(), 1);
        assert_eq!(builder.contours[0].len(), 2);
        // close() with a single point must not push a degenerate contour.
        builder.close();
        assert_eq!(builder.contours.len(), 1);
        assert!(builder.current.is_empty());
    }

    #[test]
    fn path_builder_flattens_quad_and_cubic_curves() {
        let mut builder = PathBuilder {
            transform: Transform::default(),
            view: view(8, 8),
            contours: Vec::new(),
            current: Vec::new(),
        };
        builder.move_to(0.0, 0.0);
        builder.quad_to(4.0, 8.0, 8.0, 0.0);
        assert_eq!(
            builder.current.len(),
            1 + 16,
            "quad subdivides into 16 steps"
        );
        assert!(builder
            .current
            .iter()
            .all(|(x, y)| x.is_finite() && y.is_finite()));

        builder.close();
        builder.move_to(0.0, 0.0);
        builder.curve_to(2.0, 8.0, 6.0, 8.0, 8.0, 0.0);
        assert_eq!(
            builder.current.len(),
            1 + 24,
            "cubic subdivides into 24 steps"
        );
    }

    #[cfg(windows)]
    fn with_segoe_face<T>(body: impl FnOnce(&Face<'_>) -> T) -> Option<T> {
        let data = std::fs::read("C:\\Windows\\Fonts\\seguiemj.ttf").ok()?;
        let face = Face::parse(&data, 0).ok()?;
        Some(body(&face))
    }

    #[cfg(windows)]
    #[test]
    fn render_colr_glyph_renders_emoji_and_enforces_size_cap() {
        let Some(result) = with_segoe_face(|face| {
            let gid = face
                .glyph_index('\u{1F680}')
                .expect("Segoe UI Emoji has the rocket glyph");
            let rendered = render_colr_glyph(face, gid, 136.0);
            let capped = render_colr_glyph(face, gid, 100_000.0);
            let not_colr = render_colr_glyph(face, GlyphId(0), 136.0);
            (rendered, capped, not_colr)
        }) else {
            eprintln!("skip: Segoe UI Emoji not available");
            return;
        };
        let (rendered, capped, not_colr) = result;

        let (rgba, width, height) = rendered.expect("COLR rocket must rasterize");
        assert!(width >= 1 && height >= 1);
        assert!(width <= 1024 && height <= 1024, "tight canvas stays capped");
        assert_eq!(rgba.len(), (width * height * 4) as usize);
        assert!(
            rgba.iter().skip(3).step_by(4).any(|&a| a > 0),
            "glyph must paint opaque pixels"
        );

        // A corrupt/huge paint request must not OOM the viewport thread.
        assert!(capped.is_none(), "oversized render must hit the 1024px cap");
        // Glyph 0 (.notdef) has no COLR layers.
        assert!(not_colr.is_none());
    }

    #[cfg(windows)]
    #[test]
    fn raster_painter_layer_stack_survives_unbalanced_and_overpops() {
        let painted = with_segoe_face(|face| {
            let mut painter = RasterPainter::new(face, view(8, 8));
            // Unbalanced pushes, then over-pop: the guard keeps the base layer.
            Painter::push_layer(&mut painter, CompositeMode::Lighten);
            Painter::push_layer(&mut painter, CompositeMode::SourceOver);
            painter.pop_layer();
            painter.pop_layer();
            painter.pop_layer(); // no-op on the base layer
                                 // Clip stack underflow is equally harmless.
            Painter::push_clip(&mut painter); // no pending outline: full mask
            assert!(painter
                .clip_stack
                .last()
                .expect("clip pushed")
                .a
                .iter()
                .all(|&a| a == 1.0));
            painter.pop_clip();
            painter.pop_clip(); // no-op on empty stack
            painter.finish()
        });
        let Some(canvas) = painted else {
            eprintln!("skip: Segoe UI Emoji not available");
            return;
        };
        assert_eq!(canvas.px.len(), 8 * 8 * 4);
        assert!(canvas.px.iter().all(|&v| v == 0.0), "nothing was painted");
    }

    #[cfg(windows)]
    /// A View scaled so the glyph's bounding box fits the canvas (raw font
    /// units are ~2048/em — far larger than a small test canvas).
    fn glyph_fitted_view(face: &Face<'_>, gid: GlyphId, canvas: i32) -> View {
        let bbox = face.glyph_bounding_box(gid).expect("glyph bbox");
        let extent = (bbox.x_max - bbox.x_min)
            .max(bbox.y_max - bbox.y_min)
            .max(1) as f32;
        View {
            scale: (canvas as f32 - 4.0) / extent,
            min_fx: bbox.x_min as f32,
            max_fy: bbox.y_max as f32,
            width: canvas,
            height: canvas,
        }
    }

    #[cfg(windows)]
    #[test]
    fn raster_painter_paint_uses_outline_then_clip_as_fill_region() {
        let painted = with_segoe_face(|face| {
            let gid = face.glyph_index('\u{1F680}').expect("rocket glyph");
            let red = RgbaColor::new(255, 0, 0, 255);
            let green = RgbaColor::new(0, 255, 0, 255);

            // COLRv0: a pending outline is painted directly.
            let mut v0 = RasterPainter::new(face, glyph_fitted_view(face, gid, 32));
            Painter::outline_glyph(&mut v0, gid);
            Painter::paint(&mut v0, Paint::Solid(red));
            let v0_done = v0.finish();

            // COLRv1 PaintGlyph: push_clip consumes the outline and the nested
            // paint fills the clip region instead.
            let mut v1 = RasterPainter::new(face, glyph_fitted_view(face, gid, 32));
            Painter::outline_glyph(&mut v1, gid);
            Painter::push_clip(&mut v1);
            Painter::paint(&mut v1, Paint::Solid(green));
            let v1_done = v1.finish();
            (v0_done.px, v1_done.px)
        });
        let Some((v0, v1)) = painted else {
            eprintln!("skip: Segoe UI Emoji not available");
            return;
        };
        let has_red = v0.chunks_exact(4).any(|px| px[0] > 0.0 && px[3] > 0.0);
        let has_green = v1.chunks_exact(4).any(|px| px[1] > 0.0 && px[3] > 0.0);
        assert!(has_red, "pending-outline paint path must fill the glyph");
        assert!(has_green, "clip-as-fill-region paint path must paint");
    }

    #[cfg(windows)]
    #[test]
    fn paint_without_outline_or_clip_is_a_noop() {
        let painted = with_segoe_face(|face| {
            let mut painter = RasterPainter::new(face, view(8, 8));
            // A paint with no pending outline and no clip on the stack has no
            // fill region: it must neither panic nor touch the canvas.
            Painter::paint(&mut painter, Paint::Solid(RgbaColor::new(255, 0, 0, 255)));
            painter.finish()
        });
        let Some(canvas) = painted else {
            eprintln!("skip: Segoe UI Emoji not available");
            return;
        };
        assert!(
            canvas.px.iter().all(|&v| v == 0.0),
            "paint without a fill region must stay a no-op"
        );
    }

    #[cfg(windows)]
    #[test]
    fn pending_outline_paint_is_intersected_with_outer_clips() {
        let painted = with_segoe_face(|face| {
            let gid = face.glyph_index('\u{1F680}').expect("rocket glyph");
            let fitted = glyph_fitted_view(face, gid, 32);
            // Clip everything right of pixel column 16 (ClipBox is in font
            // units, so convert the pixel cut back into font space).
            let cut_fx = fitted.min_fx + 16.0 / fitted.scale;
            let mut painter = RasterPainter::new(face, fitted);
            Painter::outline_glyph(&mut painter, gid);
            Painter::push_clip_box(
                &mut painter,
                ClipBox {
                    x_min: fitted.min_fx - 16.0,
                    y_min: fitted.max_fy - 64.0 / fitted.scale,
                    x_max: cut_fx,
                    y_max: fitted.max_fy,
                },
            );
            // The pending outline paints *through* the pushed box clip.
            Painter::outline_glyph(&mut painter, gid);
            Painter::paint(&mut painter, Paint::Solid(RgbaColor::new(255, 0, 0, 255)));
            painter.finish()
        });
        let Some(canvas) = painted else {
            eprintln!("skip: Segoe UI Emoji not available");
            return;
        };
        let painted_cols: Vec<i32> = canvas
            .px
            .chunks_exact(4)
            .enumerate()
            .filter(|(_, px)| px[3] > 0.0)
            .map(|(i, _)| (i % 32) as i32)
            .collect();
        assert!(
            !painted_cols.is_empty(),
            "glyph's left half must survive the clip"
        );
        assert!(
            painted_cols.iter().all(|&x| x < 16),
            "no pixel right of the clip may be painted: max={}",
            painted_cols.iter().max().expect("non-empty")
        );
    }

    #[cfg(windows)]
    #[test]
    fn clip_box_masks_only_the_box_and_handles_degenerate_ranges() {
        let done = with_segoe_face(|face| {
            let mut painter = RasterPainter::new(face, view(8, 8));
            Painter::push_clip_box(
                &mut painter,
                ClipBox {
                    x_min: 2.0,
                    y_min: 2.0,
                    x_max: 5.0,
                    y_max: 5.0,
                },
            );
            let mask = painter.clip_stack.pop().expect("clip pushed");
            // y is flipped: font y 2..5 covers pixel rows 3..6.
            let inside = mask.a[3 * 8 + 2];
            let outside = mask.a[0];
            // A box entirely outside the canvas must yield an empty mask
            // (empty x/y ranges) instead of an out-of-range index panic.
            Painter::push_clip_box(
                &mut painter,
                ClipBox {
                    x_min: 100.0,
                    y_min: 100.0,
                    x_max: 200.0,
                    y_max: 200.0,
                },
            );
            let offscreen = painter.clip_stack.pop().expect("clip pushed");
            (inside, outside, offscreen)
        });
        let Some((inside, outside, offscreen)) = done else {
            eprintln!("skip: Segoe UI Emoji not available");
            return;
        };
        assert_close(inside, 1.0);
        assert_close(outside, 0.0);
        assert!(
            offscreen.a.iter().all(|&a| a == 0.0),
            "offscreen clip box must mask everything out"
        );
    }

    #[cfg(windows)]
    #[test]
    fn bounds_painter_tracks_min_max_and_transform_stack() {
        let done = with_segoe_face(|face| {
            let mut painter = BoundsPainter::new(face, 1.0);
            let empty = painter.bounds().is_none();
            painter.include(5.0, 7.0);
            let first = painter.bounds();
            painter.include(1.0, 9.0);
            let grown = painter.bounds();
            painter.push_transform(Transform {
                a: 2.0,
                b: 0.0,
                c: 0.0,
                d: 2.0,
                e: 0.0,
                f: 0.0,
            });
            painter.include(1.0, 1.0);
            let scaled = painter.bounds();
            painter.pop_transform();
            painter.pop_transform(); // no-op on empty stack
            (empty, first, grown, scaled)
        });
        let Some((empty, first, grown, scaled)) = done else {
            eprintln!("skip: Segoe UI Emoji not available");
            return;
        };
        assert!(empty, "no bounds before anything is included");
        let Some((min_x, max_x, min_y, max_y)) = first else {
            panic!("bounds after include");
        };
        assert_close(min_x, 5.0);
        assert_close(max_x, 5.0);
        assert_close(min_y, 7.0);
        assert_close(max_y, 7.0);
        let Some((min_x, max_x, min_y, max_y)) = grown else {
            panic!("bounds grow monotonically");
        };
        assert_close(min_x, 1.0);
        assert_close(max_x, 5.0);
        assert_close(min_y, 7.0);
        assert_close(max_y, 9.0);
        // (1,1) under the 2x transform lands at (2,2) and must not shrink
        // the already-recorded bounds.
        let Some((min_x, _, min_y, _)) = scaled else {
            panic!("bounds survive transform push/pop");
        };
        assert_close(min_x, 1.0);
        assert_close(min_y, 2.0);
    }
}
