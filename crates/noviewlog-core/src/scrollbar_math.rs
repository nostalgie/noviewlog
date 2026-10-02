//! Pixel geometry for engine-synced scrollbars (Slint `EngineScrollBar`).
//!
//! The Slint UI must use the same rules: when `min_thumb` applies, travel is
//! `track - thumb_len`, not `1 - page/range`. Otherwise the thumb overflows the
//! track at `value == maximum` and paints into the status bar.

/// Minimum thumb length along the track (matches Slint `max(24px, …)`).
pub const MIN_THUMB_PX: f32 = 24.0;

/// Returns `(offset_along_track, thumb_len)` in the same units as `track`.
///
/// Invariants (within 1e-3): `offset >= 0` and `offset + thumb_len <= track`
/// when `track > 0`.
pub fn thumb_offset_len(track: f32, value: f32, maximum: f32, page: f32) -> (f32, f32) {
    if track <= 0.0 {
        return (0.0, 0.0);
    }
    if maximum <= 0.5 {
        return (0.0, track);
    }
    let page = page.max(1.0);
    let range = maximum + page;
    let ideal = track * (page / range);
    let min_thumb = MIN_THUMB_PX.min(track);
    let thumb_len = ideal.clamp(min_thumb, track);
    let travel = (track - thumb_len).max(0.0);
    let t = value.clamp(0.0, maximum) / maximum;
    let offset = travel * t;
    (offset, thumb_len)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_in_bounds(track: f32, value: f32, maximum: f32, page: f32) {
        let (offset, len) = thumb_offset_len(track, value, maximum, page);
        assert!(
            offset >= -1e-3,
            "offset={offset} track={track} value={value} max={maximum} page={page}"
        );
        assert!(
            offset + len <= track + 1e-3,
            "overflow: offset={offset} len={len} track={track} value={value} max={maximum} page={page}"
        );
        assert!(len >= 0.0 && len <= track + 1e-3);
    }

    #[test]
    fn thumb_at_eof_does_not_overflow_track_when_min_thumb_applies() {
        // Large FILES scroll: page ≪ maximum → ideal thumb << 24px.
        let track = 600.0;
        let maximum = 4_000_000.0;
        let page = 400.0;
        let (offset, len) = thumb_offset_len(track, maximum, maximum, page);
        assert!(
            (len - MIN_THUMB_PX).abs() < 0.01,
            "expected min thumb, got {len}"
        );
        assert!(
            offset + len <= track + 1e-3,
            "EOF thumb must stay in track: offset={offset} len={len} track={track}"
        );
        // Old buggy formula overflowed by nearly MIN_THUMB_PX:
        let range = maximum + page;
        let frac = page / range;
        let buggy_bottom = track * (1.0 - frac) + MIN_THUMB_PX;
        assert!(
            buggy_bottom > track + 1.0,
            "sanity: old formula must overflow (got bottom={buggy_bottom})"
        );
    }

    #[test]
    fn thumb_at_start_stays_in_bounds() {
        let (offset, len) = thumb_offset_len(600.0, 0.0, 4_000_000.0, 400.0);
        assert!((offset - 0.0).abs() < 1e-3);
        assert!(offset + len <= 600.0 + 1e-3);
    }

    #[test]
    fn mid_value_offset_is_between_ends() {
        let track = 500.0;
        let maximum = 10_000.0;
        let page = 200.0;
        let (o0, _) = thumb_offset_len(track, 0.0, maximum, page);
        let (o_mid, _) = thumb_offset_len(track, maximum * 0.5, maximum, page);
        let (o1, _) = thumb_offset_len(track, maximum, maximum, page);
        assert!(o0 <= o_mid + 1e-3 && o_mid <= o1 + 1e-3);
    }

    #[test]
    fn empty_maximum_fills_track() {
        let (offset, len) = thumb_offset_len(400.0, 0.0, 0.0, 400.0);
        assert!((offset - 0.0).abs() < 1e-3);
        assert!((len - 400.0).abs() < 1e-3);
    }

    #[test]
    fn short_track_shorter_than_min_thumb() {
        let track = 16.0;
        let (offset, len) = thumb_offset_len(track, 100.0, 100.0, 8.0);
        assert!(len <= track + 1e-3);
        assert!(offset + len <= track + 1e-3);
    }

    #[test]
    fn value_grid_always_in_bounds() {
        let tracks = [1.0_f32, 16.0, 24.0, 100.0, 600.0, 1200.0];
        let maxima = [0.0_f32, 0.4, 100.0, 10_000.0, 4_000_000.0];
        let pages = [1.0_f32, 40.0, 400.0];
        for &track in &tracks {
            for &maximum in &maxima {
                for &page in &pages {
                    for t in [0.0_f32, 0.25, 0.5, 0.75, 1.0] {
                        let value = if maximum <= 0.5 { 0.0 } else { maximum * t };
                        assert_in_bounds(track, value, maximum, page);
                    }
                }
            }
        }
    }
}
