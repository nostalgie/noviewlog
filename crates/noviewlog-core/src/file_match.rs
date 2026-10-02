//! Whole-file match index for file-session filter tabs.
//!
//! Scans the source file for include/exclude (+ severity) hits and stores
//! matching line **byte offsets**. The viewport seeks those offsets instead of
//! copying the filtered text into a second log file.

use std::fs::File;
use std::io::{BufReader, Seek, SeekFrom};

use crate::core::filter::FilterEngine;
use crate::core::types::{detect_level, SeverityFilter};
use crate::file_index::{decode_lossy_line, read_line_bounded, strip_bom, LINE_TRUNCATION_MARKER};

/// Bytes scanned per engine tick while building a match index.
pub const MATCH_SCAN_BYTES_PER_TICK: u64 = 512 * 1024;

/// How many match lines to keep materialized for the active filter tab.
pub const MATCH_WINDOW_LINES: usize = 10_000;

/// Hard cap on stored match offsets (~16 MB of u64). A broad filter over a
/// huge file stops the scan here instead of growing without bound; the
/// viewport already pages in only [`MATCH_WINDOW_LINES`] at a time.
pub const MAX_MATCH_OFFSETS: usize = 2_000_000;

/// Returns true when a file filter tab should use a whole-file match index
/// instead of the shared sliding window buffer.
pub fn view_needs_match_index(filters: &FilterEngine, severity: SeverityFilter) -> bool {
    let has_rules = filters.filters().iter().any(|f| f.enabled);
    has_rules || severity != SeverityFilter::All
}

/// Scan up to `max_bytes` from `from_byte`, appending matching line start offsets.
/// Returns `(next_pos, done, capped)`: the next byte position to continue from,
/// whether the scan is finished (file end or the offset cap reached), and
/// whether stored offsets were truncated at the cap (`done && capped` means the
/// match set is incomplete — callers must surface that to the user, issue #150).
pub fn scan_match_chunk(
    file: &mut File,
    file_size: u64,
    from_byte: u64,
    max_bytes: u64,
    filters: &FilterEngine,
    severity: SeverityFilter,
    offsets: &mut Vec<u64>,
) -> Result<(u64, bool, bool), String> {
    scan_match_chunk_with_cap(
        file,
        file_size,
        from_byte,
        max_bytes,
        filters,
        severity,
        offsets,
        MAX_MATCH_OFFSETS,
    )
}

pub(crate) fn scan_match_chunk_with_cap(
    file: &mut File,
    file_size: u64,
    from_byte: u64,
    max_bytes: u64,
    filters: &FilterEngine,
    severity: SeverityFilter,
    offsets: &mut Vec<u64>,
    cap: usize,
) -> Result<(u64, bool, bool), String> {
    if from_byte >= file_size {
        return Ok((from_byte, true, false));
    }
    // Stale-size guard (issue #108): the file shrank since the session opened.
    // Without this the scan would spin forever re-reading the same position.
    let current_size = file
        .metadata()
        .map_err(|e| format!("Stat failed: {e}"))?
        .len();
    if current_size < file_size {
        return Err("File changed on disk (truncated) — reload the session".to_string());
    }
    file.seek(SeekFrom::Start(from_byte))
        .map_err(|e| format!("Seek failed: {e}"))?;

    let end_byte = (from_byte + max_bytes).min(file_size);
    let mut reader = BufReader::new(file);
    let mut pos = from_byte;
    let mut capped = false;
    let mut hit_eof = false;
    while pos < end_byte {
        if offsets.len() >= cap {
            capped = true;
            break;
        }
        let line_start = pos;
        let mut raw = Vec::new();
        match read_line_bounded(&mut reader, &mut raw) {
            // EOF before the stale size: the file shrank mid-scan (issue #108).
            // Finish the scan — continuing would livelock on the same position.
            Ok((0, _)) => {
                hit_eof = true;
                break;
            }
            Ok((n, truncated)) => {
                pos += n as u64;
                let mut decoded = decode_lossy_line(&raw);
                if truncated {
                    decoded.push_str(LINE_TRUNCATION_MARKER);
                }
                let line = if line_start == 0 {
                    strip_bom(&decoded)
                } else {
                    &decoded
                };
                // &str predicate: no per-line record clone, no clock read, and
                // level regexes only run when a severity filter needs them.
                let visible = filters.is_visible_text(line)
                    && (severity == SeverityFilter::All || severity.allows(detect_level(line)));
                if visible {
                    offsets.push(line_start);
                }
                if pos >= end_byte {
                    break;
                }
            }
            Err(err) => return Err(format!("Read error: {err}")),
        }
    }
    let done = capped || hit_eof || pos >= file_size;
    // "Capped" only when results were actually truncated (issue #113):
    // hitting the cap exactly at EOF is a complete scan.
    let capped = capped && pos < file_size;
    Ok((pos, done, capped))
}

/// Read one line starting at `offset` (does not require a full line index).
pub fn read_line_at(file: &mut File, offset: u64) -> Result<String, String> {
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| format!("Seek failed: {e}"))?;
    let mut reader = BufReader::new(file);
    let mut raw = Vec::new();
    match read_line_bounded(&mut reader, &mut raw) {
        Ok((0, _)) => Ok(String::new()),
        Ok((_, truncated)) => {
            let mut decoded = decode_lossy_line(&raw);
            if truncated {
                decoded.push_str(LINE_TRUNCATION_MARKER);
            }
            if offset == 0 {
                Ok(strip_bom(&decoded).into())
            } else {
                Ok(decoded)
            }
        }
        Err(err) => Err(format!("Read error: {err}")),
    }
}

/// Materialize a window of match lines as plain strings.
pub fn read_match_window(
    file: &mut File,
    offsets: &[u64],
    start: usize,
    count: usize,
) -> Result<Vec<String>, String> {
    if start >= offsets.len() || count == 0 {
        return Ok(Vec::new());
    }
    let end = (start + count).min(offsets.len());
    let mut out = Vec::with_capacity(end - start);
    for &off in &offsets[start..end] {
        out.push(read_line_at(file, off)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::{compile_filter, FilterRule, FilterType};
    use std::io::Write;

    #[test]
    fn scan_finds_include_matches() {
        let path = std::env::temp_dir().join(format!("noviewlog-match-{}", std::process::id()));
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "alpha").unwrap();
            writeln!(f, "error boom").unwrap();
            writeln!(f, "beta").unwrap();
            writeln!(f, "error again").unwrap();
        }
        let size = std::fs::metadata(&path).unwrap().len();
        let mut file = File::open(&path).unwrap();
        let rule = compile_filter(FilterRule {
            id: "1".into(),
            name: None,
            filter_type: FilterType::Include,
            pattern: "error".into(),
            enabled: true,
            use_regex: false,
            regex: None,
        });
        let engine = FilterEngine::new(vec![rule]);
        let mut offsets = Vec::new();
        let (pos, done, capped) = scan_match_chunk(
            &mut file,
            size,
            0,
            size,
            &engine,
            SeverityFilter::All,
            &mut offsets,
        )
        .unwrap();
        assert!(done);
        assert!(!capped, "complete scan must not flag truncation");
        assert_eq!(pos, size);
        assert_eq!(offsets.len(), 2);
        let lines = read_match_window(&mut file, &offsets, 0, 10).unwrap();
        assert_eq!(lines, vec!["error boom", "error again"]);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn scan_stops_at_offset_cap() {
        // Issue #52: a broad filter must not grow `offsets` without bound.
        let path = std::env::temp_dir().join(format!("noviewlog-match-cap-{}", std::process::id()));
        {
            let mut f = std::fs::File::create(&path).unwrap();
            for i in 0..10 {
                writeln!(f, "error {i}").unwrap();
            }
        }
        let size = std::fs::metadata(&path).unwrap().len();
        let mut file = File::open(&path).unwrap();
        let engine = FilterEngine::default();
        let mut offsets = Vec::new();
        let (pos, done, capped) = scan_match_chunk_with_cap(
            &mut file,
            size,
            0,
            size,
            &engine,
            SeverityFilter::All,
            &mut offsets,
            5,
        )
        .unwrap();
        assert_eq!(offsets.len(), 5, "cap must bound stored offsets");
        assert!(done, "cap reached must finish the scan");
        assert!(
            capped,
            "truncated scan must surface the cap flag (issue #150)"
        );
        assert!(pos < size, "scan position stops early: {pos} < {size}");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn scan_cap_hit_exactly_at_eof_is_complete() {
        // Issue #113 / #150: matching lines count == cap with EOF reached is a
        // full result set — the cap flag must stay false.
        let path =
            std::env::temp_dir().join(format!("noviewlog-match-cap-eof-{}", std::process::id()));
        {
            let mut f = std::fs::File::create(&path).unwrap();
            for i in 0..5 {
                writeln!(f, "error {i}").unwrap();
            }
        }
        let size = std::fs::metadata(&path).unwrap().len();
        let mut file = File::open(&path).unwrap();
        let engine = FilterEngine::default();
        let mut offsets = Vec::new();
        let (pos, done, capped) = scan_match_chunk_with_cap(
            &mut file,
            size,
            0,
            size,
            &engine,
            SeverityFilter::All,
            &mut offsets,
            5,
        )
        .unwrap();
        assert_eq!(offsets.len(), 5);
        assert!(done);
        assert!(!capped, "cap == match count at EOF is not truncation");
        assert_eq!(pos, size);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn scan_strips_utf8_bom_from_line_zero() {
        // Issue #74: a filter for the first token of line 1 must match even
        // when the file starts with a UTF-8 BOM.
        let path = std::env::temp_dir().join(format!("noviewlog-match-bom-{}", std::process::id()));
        {
            let mut f = std::fs::File::create(&path).unwrap();
            f.write_all(b"\xEF\xBB\xBFalpha start\nbeta\n").unwrap();
        }
        let size = std::fs::metadata(&path).unwrap().len();
        let mut file = File::open(&path).unwrap();
        let rule = compile_filter(FilterRule {
            id: "1".into(),
            name: None,
            filter_type: FilterType::Include,
            pattern: "alpha".into(),
            enabled: true,
            use_regex: false,
            regex: None,
        });
        let engine = FilterEngine::new(vec![rule]);
        let mut offsets = Vec::new();
        let (_, done, capped) = scan_match_chunk(
            &mut file,
            size,
            0,
            size,
            &engine,
            SeverityFilter::All,
            &mut offsets,
        )
        .unwrap();
        assert!(done);
        assert!(!capped);
        assert_eq!(offsets, vec![0]);
        let lines = read_match_window(&mut file, &offsets, 0, 10).unwrap();
        assert_eq!(lines, vec!["alpha start"]);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn scan_survives_non_utf8_bytes() {
        // Issue #51: an invalid byte must not abort the match scan.
        let path =
            std::env::temp_dir().join(format!("noviewlog-match-lossy-{}", std::process::id()));
        {
            let mut f = std::fs::File::create(&path).unwrap();
            f.write_all(b"error \xED boom\nplain\nerror two\n").unwrap();
        }
        let size = std::fs::metadata(&path).unwrap().len();
        let mut file = File::open(&path).unwrap();
        let rule = compile_filter(FilterRule {
            id: "1".into(),
            name: None,
            filter_type: FilterType::Include,
            pattern: "error".into(),
            enabled: true,
            use_regex: false,
            regex: None,
        });
        let engine = FilterEngine::new(vec![rule]);
        let mut offsets = Vec::new();
        let (_, done, _) = scan_match_chunk(
            &mut file,
            size,
            0,
            size,
            &engine,
            SeverityFilter::All,
            &mut offsets,
        )
        .unwrap();
        assert!(done);
        assert_eq!(offsets.len(), 2);
        let lines = read_match_window(&mut file, &offsets, 0, 10).unwrap();
        assert_eq!(lines[0], "error \u{FFFD} boom");
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod stale_tests {
    use super::*;

    use std::io::Write;

    #[test]
    fn scan_errors_when_file_shrank_under_stale_size() {
        // Issue #108: stale file_size (session opened before truncation) must
        // surface an error instead of scanning past EOF forever.
        let path = std::env::temp_dir().join(format!("noviewlog-stale-{}", std::process::id()));
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "short").unwrap();
        }
        let mut file = File::open(&path).unwrap();
        let engine = FilterEngine::default();
        let mut offsets = Vec::new();
        let stale_size = 10 * 1024 * 1024;
        let result = scan_match_chunk(
            &mut file,
            stale_size,
            0,
            1024 * 1024,
            &engine,
            SeverityFilter::All,
            &mut offsets,
        );
        assert!(
            result.unwrap_err().contains("truncated"),
            "stale size must surface a reload message"
        );
        let _ = std::fs::remove_file(path);
    }
}
