use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// File handle shared between the engine (metadata owner) and the background
/// I/O threads that perform the actual reads (issue #55: no synchronous file
/// reads on the UI event-loop thread).
pub type SharedFile = Arc<Mutex<File>>;

/// Bytes scanned per engine tick while building a line index.
pub const INDEX_BYTES_PER_TICK: u64 = 4 * 1024 * 1024;

/// Decode raw line bytes (up to and including `\n`) as UTF-8, replacing
/// invalid sequences (cp1251/latin-1 bytes) with U+FFFD instead of failing
/// the read. Strips the trailing `\n` and `\r` like `read_line` callers did.
pub(crate) fn decode_lossy_line(mut raw: &[u8]) -> String {
    if raw.last() == Some(&b'\n') {
        raw = &raw[..raw.len() - 1];
    }
    if raw.last() == Some(&b'\r') {
        raw = &raw[..raw.len() - 1];
    }
    String::from_utf8_lossy(raw).into_owned()
}

/// Strip a UTF-8 BOM (U+FEFF) from the start of line text read at byte 0.
pub(crate) fn strip_bom(line: &str) -> &str {
    line.strip_prefix('\u{FEFF}').unwrap_or(line)
}

/// Hard cap on bytes buffered for a single line (issue #162): a corrupt or
/// minified single-line file must not balloon memory to multiples of its
/// size. Longer lines are truncated (see [`read_line_bounded`]).
pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

/// Appended to truncated line text so the cut is visible to the user.
pub const LINE_TRUNCATION_MARKER: &str = " …[line truncated]";

/// Bounded `BufRead::read_until(b'\n')`. Appends at most [`MAX_LINE_BYTES`]
/// bytes to `out`; when the line is longer, the rest is drained (discarded in
/// bounded chunks) so the reader still lands on the next line start and line
/// accounting stays aligned with the newline-scanning index. Returns
/// `(bytes consumed from the reader including the trailing `\n` if any,
/// line was truncated)`.
pub(crate) fn read_line_bounded(
    reader: &mut impl BufRead,
    out: &mut Vec<u8>,
) -> std::io::Result<(usize, bool)> {
    let mut total = 0usize;
    let mut truncated = false;
    loop {
        let available = match reader.fill_buf() {
            Ok(buf) => buf,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        if available.is_empty() {
            return Ok((total, truncated));
        }
        let newline = available.iter().position(|&b| b == b'\n');
        let consumed = match newline {
            Some(pos) => {
                // Include the trailing '\n' so callers can mirror read_until.
                let upto = pos + 1;
                let keep = upto.min(MAX_LINE_BYTES.saturating_sub(total));
                out.extend_from_slice(&available[..keep]);
                truncated |= keep < upto;
                upto
            }
            None => {
                let keep = available.len().min(MAX_LINE_BYTES.saturating_sub(total));
                out.extend_from_slice(&available[..keep]);
                // Flag truncation only when bytes were actually dropped — a
                // line of exactly MAX_LINE_BYTES is intact (#197).
                truncated |= keep < available.len();
                available.len()
            }
        };
        reader.consume(consumed);
        total += consumed;
        if newline.is_some() {
            return Ok((total, truncated));
        }
    }
}

/// Deletes the wrapped temp file when dropped (UTF-16 transcoded sessions).
pub(crate) struct TempFileGuard(pub std::path::PathBuf);

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Raw file lines kept in the in-memory sliding window.
pub const WINDOW_RAW_LINES: usize = 50_000;

/// Raw lines loaded when the user scrolls near a window edge.
pub const PREFETCH_RAW_LINES: usize = 5_000;

/// Store a checkpoint every N lines (plus line 0). Keeps RAM ~O(lines/stride).
pub const LINE_INDEX_STRIDE: u64 = 256;

/// Sparse line index: checkpoints + on-demand walk within a stride.
#[derive(Clone, Debug)]
pub struct LineIndex {
    /// `(line_number, byte_offset)` sorted by line number.
    checkpoints: Vec<(u64, u64)>,
    line_count: u64,
    file_size: u64,
    stride: u64,
}

impl Default for LineIndex {
    fn default() -> Self {
        Self::new(0)
    }
}

impl LineIndex {
    pub fn new(file_size: u64) -> Self {
        Self {
            checkpoints: Vec::new(),
            line_count: 0,
            file_size,
            stride: LINE_INDEX_STRIDE.max(1),
        }
    }

    pub fn file_size(&self) -> u64 {
        self.file_size
    }

    pub fn total_lines(&self) -> u64 {
        self.line_count
    }

    pub fn checkpoint_count(&self) -> usize {
        self.checkpoints.len()
    }

    pub fn is_complete(&self, bytes_indexed: u64) -> bool {
        bytes_indexed >= self.file_size
    }

    pub fn progress(&self, bytes_indexed: u64) -> f32 {
        if self.file_size == 0 {
            1.0
        } else {
            (bytes_indexed as f32 / self.file_size as f32).clamp(0.0, 1.0)
        }
    }

    fn push_checkpoint(&mut self, line: u64, offset: u64) {
        if let Some(&(last_line, _)) = self.checkpoints.last() {
            if line <= last_line {
                return;
            }
        }
        self.checkpoints.push((line, offset));
    }

    fn should_checkpoint(&self, line: u64) -> bool {
        line == 0 || line.is_multiple_of(self.stride)
    }

    /// Approximate line whose start is `<= byte` (checkpoint interpolation).
    pub fn line_at_offset(&self, byte: u64) -> u64 {
        if self.line_count == 0 || self.checkpoints.is_empty() {
            return 0;
        }
        let mut lo = 0usize;
        let mut hi = self.checkpoints.len();
        while lo + 1 < hi {
            let mid = (lo + hi) / 2;
            if self.checkpoints[mid].1 <= byte {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let (line, off) = self.checkpoints[lo];
        if off >= byte {
            return line.min(self.line_count.saturating_sub(1));
        }
        if lo + 1 < self.checkpoints.len() {
            let (next_line, next_off) = self.checkpoints[lo + 1];
            if next_off <= byte {
                return next_line.min(self.line_count.saturating_sub(1));
            }
            if next_off > off && next_line > line {
                let span = next_off - off;
                let lines = next_line - line;
                let progress = ((byte - off) as f64 / span as f64).clamp(0.0, 1.0);
                return (line + (progress * lines as f64) as u64)
                    .min(self.line_count.saturating_sub(1));
            }
        }
        line.min(self.line_count.saturating_sub(1))
    }

    /// Last checkpoint with line number `<= line` (`None` before the first).
    /// Binary search: the vector is sorted by line number; a linear reverse
    /// scan was O(checkpoints) per read (issue #238).
    fn checkpoint_at_or_before(&self, line: u64) -> Option<(u64, u64)> {
        let idx = self.checkpoints.partition_point(|(l, _)| *l <= line);
        idx.checked_sub(1).map(|i| self.checkpoints[i])
    }

    /// Exact byte offset of `line`, walking from the nearest checkpoint at or before it.
    pub fn offset_of_exact(&self, file: &mut File, line: u64) -> Result<Option<u64>, String> {
        if line >= self.line_count {
            return Ok(None);
        }
        let Some((cp_line, cp_off)) = self.checkpoint_at_or_before(line) else {
            return Ok(None);
        };
        if cp_line == line {
            return Ok(Some(cp_off));
        }
        file.seek(SeekFrom::Start(cp_off))
            .map_err(|e| format!("Seek failed: {e}"))?;
        let mut reader = BufReader::new(file);
        let mut cur = cp_line;
        let mut pos = cp_off;
        while cur < line {
            let mut buf = Vec::new();
            match read_line_bounded(&mut reader, &mut buf) {
                Ok((0, _)) => return Ok(None),
                Ok((n, _)) => {
                    pos += n as u64;
                    cur += 1;
                }
                Err(err) => return Err(format!("Read error: {err}")),
            }
        }
        Ok(Some(pos))
    }

    /// Checkpoint hit only (no file walk). Prefer [`Self::offset_of_exact`] for reads.
    pub fn offset_of(&self, line: u64) -> Option<u64> {
        if line >= self.line_count {
            return None;
        }
        self.checkpoint_at_or_before(line)
            .and_then(|(l, o)| if l == line { Some(o) } else { None })
    }

    /// Exact line number of the line that starts at `byte` (`byte` must be a
    /// line start). Walks newlines from the nearest checkpoint at or before
    /// `byte` — unlike [`Self::line_at_offset`], which interpolates.
    pub fn line_at_byte_exact(&self, file: &mut File, byte: u64) -> Result<u64, String> {
        if self.checkpoints.is_empty() || byte == 0 {
            return Ok(0);
        }
        let idx = self.checkpoints.partition_point(|&(_, off)| off <= byte);
        let (line, off) = if idx == 0 {
            (0, 0)
        } else {
            self.checkpoints[idx - 1]
        };
        if off == byte {
            return Ok(line);
        }
        file.seek(SeekFrom::Start(off))
            .map_err(|e| format!("Seek failed: {e}"))?;
        let mut reader = BufReader::new(file);
        let mut cur = off;
        let mut cur_line = line;
        while cur < byte {
            let mut buf = Vec::new();
            match read_line_bounded(&mut reader, &mut buf) {
                Ok((0, _)) => return Ok(cur_line),
                Ok((n, _)) => {
                    cur += n as u64;
                    cur_line += 1;
                }
                Err(err) => return Err(format!("Read error: {err}")),
            }
        }
        Ok(cur_line)
    }

    pub fn scan_chunk(
        &mut self,
        file: &mut File,
        from_byte: u64,
        max_bytes: u64,
    ) -> Result<(u64, bool), String> {
        if from_byte >= self.file_size {
            return Ok((from_byte, true));
        }

        file.seek(SeekFrom::Start(from_byte))
            .map_err(|e| format!("Seek failed: {e}"))?;

        // First chunk (may start past a stripped UTF-8 BOM): line 0 begins at
        // the scan start, not necessarily byte 0.
        if self.checkpoints.is_empty() {
            self.push_checkpoint(0, from_byte);
            self.line_count = 1;
        }

        let end_byte = (from_byte + max_bytes).min(self.file_size);
        let mut buf = vec![0u8; (end_byte - from_byte) as usize];
        file.read_exact(&mut buf)
            .map_err(|e| format!("Read failed: {e}"))?;

        for (i, &b) in buf.iter().enumerate() {
            if b != b'\n' {
                continue;
            }
            let next = from_byte + i as u64 + 1;
            if next >= self.file_size {
                continue;
            }
            // New line starts at `next`.
            let new_line = self.line_count;
            self.line_count += 1;
            if self.should_checkpoint(new_line) {
                self.push_checkpoint(new_line, next);
            }
        }

        let done = end_byte >= self.file_size;
        Ok((end_byte, done))
    }

    pub fn read_lines(
        &self,
        file: &mut File,
        start_line: u64,
        count: usize,
    ) -> Result<Vec<String>, String> {
        if count == 0 || start_line >= self.total_lines() {
            return Ok(Vec::new());
        }

        let start = self
            .offset_of_exact(file, start_line)?
            .ok_or_else(|| format!("Line {start_line} not in index"))?;
        file.seek(SeekFrom::Start(start))
            .map_err(|e| format!("Seek failed: {e}"))?;

        let mut reader = BufReader::new(file);
        let mut out = Vec::with_capacity(count.min(256));
        for _ in 0..count {
            let line_index = start_line + out.len() as u64;
            if line_index >= self.total_lines() {
                break;
            }
            let mut buf = Vec::new();
            match read_line_bounded(&mut reader, &mut buf) {
                Ok((0, _)) => break,
                Ok((_, truncated)) => {
                    let mut text = decode_lossy_line(&buf);
                    if truncated {
                        text.push_str(LINE_TRUNCATION_MARKER);
                    }
                    out.push(text);
                }
                Err(err) => return Err(format!("Read error: {err}")),
            }
        }
        // Line 0 may still carry a BOM when reached through a byte-0 offset
        // (match-index reads); BOM-skipped index checkpoints never include it.
        if start_line == 0 {
            if let Some(first) = out.first_mut() {
                *first = strip_bom(first).into();
            }
        }
        Ok(out)
    }
}

/// Retained after load: path, index, and a shared file handle for on-demand
/// reads. The handle is shared with background I/O threads (issue #55); all
/// reads go through the engine's worker, never the UI thread.
pub struct FileBackedLog {
    pub path: String,
    pub file: SharedFile,
    pub index: LineIndex,
    /// On-disk size of `path` when this session was opened (issue #151). Stored
    /// explicitly — for UTF-16 sessions `index.file_size()` covers the temp
    /// UTF-8 copy, not the watched original.
    pub watch_size: u64,
    /// `path`'s mtime at open; `None` when the platform has no mtime (the
    /// watcher then degrades to size-only comparison).
    pub watch_mtime: Option<SystemTime>,
    /// Temp UTF-8 copy backing a UTF-16 session; removed when this is dropped.
    /// Never read — kept alive for [`TempFileGuard`]'s Drop (file removal).
    #[allow(dead_code)]
    pub(crate) temp: Option<TempFileGuard>,
}

impl FileBackedLog {
    pub fn read_lines(&self, start_line: u64, count: usize) -> Result<Vec<String>, String> {
        read_lines_shared(&self.file, &self.index, start_line, count)
    }

    /// True when the on-disk file no longer matches the snapshot this log was
    /// opened from (truncated, appended, rewritten, or the path is gone) —
    /// issue #151. Cheap single stat; the engine polls it on the tick.
    pub fn changed_on_disk(&self) -> bool {
        let Ok(meta) = std::fs::metadata(&self.path) else {
            return true;
        };
        meta.len() != self.watch_size || meta.modified().ok() != self.watch_mtime
    }
}

/// Read `count` lines starting at `start_line` through a shared handle.
/// Used by background I/O threads that own a clone of the handle + index.
pub fn read_lines_shared(
    file: &SharedFile,
    index: &LineIndex,
    start_line: u64,
    count: usize,
) -> Result<Vec<String>, String> {
    let mut file = file.lock().unwrap_or_else(|e| e.into_inner());
    // Stale-index guard (issue #108): a truncated/rewritten file makes the
    // index offsets point at wrong bytes — surface it instead of silently
    // returning wrong lines.
    let current = file
        .metadata()
        .map_err(|e| format!("Stat failed: {e}"))?
        .len();
    if current < index.file_size() {
        return Err("File changed on disk (truncated) — reload the session".to_string());
    }
    index.read_lines(&mut file, start_line, count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_lines(path: &std::path::Path, n: usize) {
        let mut f = std::fs::File::create(path).unwrap();
        for i in 0..n {
            writeln!(f, "line {i:06}").unwrap();
        }
    }

    /// Issue #238: the binary-search checkpoint lookup must agree with the old
    /// reverse linear scan on every interesting case.
    #[test]
    fn checkpoint_lookup_matches_reverse_scan_reference() {
        let mut index = LineIndex::new(64 * 1024);
        // Synthetic set: first checkpoint not at line 0, exact hits, gaps.
        index.checkpoints = vec![(512, 4096), (1024, 8192), (1536, 16_384)];
        index.line_count = 2000;
        let reference = |line: u64| -> Option<(u64, u64)> {
            index
                .checkpoints
                .iter()
                .rev()
                .find(|(l, _)| *l <= line)
                .copied()
        };
        for line in [
            0, 1, 100, 511, 512, 513, 600, 1023, 1024, 1025, 1535, 1536, 1600, 1999, 2000, 5000,
        ] {
            assert_eq!(
                index.checkpoint_at_or_before(line),
                reference(line),
                "lookup drift at line {line}"
            );
        }
        // Before the first checkpoint there is no checkpoint to walk from.
        assert_eq!(index.checkpoint_at_or_before(0), None);
        assert_eq!(index.checkpoint_at_or_before(511), None);
        assert_eq!(index.offset_of(0), None);
        // Exact checkpoint hits resolve without a walk.
        assert_eq!(index.offset_of(1024), Some(8192));
        // Non-checkpoint line and beyond-last both miss in `offset_of`.
        assert_eq!(index.offset_of(1500), None);
        assert_eq!(index.offset_of(5000), None);
    }

    #[test]
    fn index_and_read_range() {
        let path = std::env::temp_dir().join(format!("noviewlog-idx-{}", std::process::id()));
        write_lines(&path, 500);
        let size = std::fs::metadata(&path).unwrap().len();
        let mut file = File::open(&path).unwrap();
        let mut index = LineIndex::new(size);
        let (end, done) = index.scan_chunk(&mut file, 0, size).unwrap();
        assert!(done);
        assert_eq!(end, size);
        assert_eq!(index.total_lines(), 500);
        assert!(index.checkpoint_count() < 500);
        assert!(index.checkpoint_count() >= 2);

        let lines = index.read_lines(&mut file, 10, 3).unwrap();
        assert_eq!(lines, vec!["line 000010", "line 000011", "line 000012"]);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn sparse_index_scales_checkpoints() {
        let path = std::env::temp_dir().join(format!("noviewlog-sparse-{}", std::process::id()));
        write_lines(&path, 10_000);
        let size = std::fs::metadata(&path).unwrap().len();
        let mut file = File::open(&path).unwrap();
        let mut index = LineIndex::new(size);
        index.scan_chunk(&mut file, 0, size).unwrap();
        assert_eq!(index.total_lines(), 10_000);
        let expected_cps = 1 + (10_000 - 1) / LINE_INDEX_STRIDE as usize;
        assert!(
            index.checkpoint_count() <= expected_cps + 2,
            "checkpoints={} expected~{}",
            index.checkpoint_count(),
            expected_cps
        );
        let lines = index.read_lines(&mut file, 9_000, 2).unwrap();
        assert_eq!(lines[0], "line 009000");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn line_at_offset_finds_tail_start() {
        let path = std::env::temp_dir().join(format!("noviewlog-idx2-{}", std::process::id()));
        write_lines(&path, 100);
        let size = std::fs::metadata(&path).unwrap().len();
        let tail_byte = size / 2;
        let mut file = File::open(&path).unwrap();
        let mut index = LineIndex::new(size);
        index.scan_chunk(&mut file, 0, size).unwrap();
        let line = index.line_at_offset(tail_byte);
        assert!(line < 100);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn read_line_bounded_truncates_and_stays_line_aligned() {
        // Oversized single line, no trailing newline.
        let data = vec![b'x'; MAX_LINE_BYTES + 4096];
        let mut cursor = std::io::Cursor::new(data);
        let mut out = Vec::new();
        let (consumed, truncated) = read_line_bounded(&mut cursor, &mut out).unwrap();
        assert!(truncated);
        assert_eq!(out.len(), MAX_LINE_BYTES);
        assert_eq!(consumed, MAX_LINE_BYTES + 4096, "whole line drained");

        // After the oversized line, the next line is read intact: the drain
        // landed the reader on the next line start.
        let mut data = vec![b'y'; MAX_LINE_BYTES + 10];
        data.push(b'\n');
        data.extend_from_slice(b"after\n");
        let mut cursor = std::io::Cursor::new(data);
        let mut first = Vec::new();
        let (n1, truncated) = read_line_bounded(&mut cursor, &mut first).unwrap();
        assert!(truncated);
        let mut second = Vec::new();
        let (n2, truncated2) = read_line_bounded(&mut cursor, &mut second).unwrap();
        assert!(!truncated2);
        assert_eq!(second, b"after\n");
        assert_eq!(n1 + n2, MAX_LINE_BYTES + 10 + 1 + 6);

        // A normal line under the cap is untouched.
        let mut cursor = std::io::Cursor::new(b"hello\nworld".to_vec());
        let mut line = Vec::new();
        let (n, truncated) = read_line_bounded(&mut cursor, &mut line).unwrap();
        assert!(!truncated);
        assert_eq!(line, b"hello\n");
        assert_eq!(n, 6);
    }

    #[test]
    fn single_line_huge_file_indexes_one_line_and_truncates_reads() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("noviewlog-huge-line-{stamp}.log"));
        {
            let mut f = std::fs::File::create(&path).unwrap();
            f.write_all(&vec![b'z'; MAX_LINE_BYTES + 2048]).unwrap();
        }
        let size = std::fs::metadata(&path).unwrap().len();
        let mut file = File::open(&path).unwrap();
        let mut index = LineIndex::new(size);
        index.scan_chunk(&mut file, 0, size).unwrap();
        assert_eq!(index.total_lines(), 1, "newline-free file is one line");

        // Window reads surface the truncation marker instead of ballooning.
        let lines = index.read_lines(&mut file, 0, 10).unwrap();
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].ends_with(LINE_TRUNCATION_MARKER),
            "truncation must be visible to the user"
        );
        assert_eq!(
            lines[0].len(),
            MAX_LINE_BYTES + LINE_TRUNCATION_MARKER.len()
        );

        let _ = std::fs::remove_file(path);
    }

    /// Open a temp log, load it fully, and return the retained [`FileBackedLog`].
    #[cfg(test)]
    fn open_backed(path: &std::path::Path) -> FileBackedLog {
        let mut state = crate::file_load::FileLoadState::open(path.to_str().unwrap()).unwrap();
        while !state.is_finished() {
            state.tick().unwrap();
        }
        state.into_backed().unwrap()
    }

    #[test]
    fn changed_on_disk_unchanged_file_not_flagged() {
        // Issue #151: a file untouched since open must stay quiet.
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("noviewlog-watch-same-{stamp}.log"));
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "one").unwrap();
            writeln!(f, "two").unwrap();
        }
        let backed = open_backed(&path);
        assert!(!backed.changed_on_disk());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn changed_on_disk_flags_append() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("noviewlog-watch-append-{stamp}.log"));
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "one").unwrap();
        }
        let backed = open_backed(&path);
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            writeln!(f, "two").unwrap();
        }
        assert!(backed.changed_on_disk(), "appended file must be flagged");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn changed_on_disk_flags_truncate() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("noviewlog-watch-trunc-{stamp}.log"));
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "one").unwrap();
            writeln!(f, "two").unwrap();
        }
        let backed = open_backed(&path);
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "tiny").unwrap();
        }
        assert!(backed.changed_on_disk(), "truncated file must be flagged");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn changed_on_disk_flags_missing_file() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("noviewlog-watch-gone-{stamp}.log"));
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "one").unwrap();
        }
        let backed = open_backed(&path);
        let _ = std::fs::remove_file(&path);
        assert!(backed.changed_on_disk(), "deleted file must be flagged");
    }

    #[test]
    fn changed_on_disk_flags_same_size_mtime_bump() {
        // Rewrite in place with an identical byte count: only mtime drifts.
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("noviewlog-watch-mtime-{stamp}.log"));
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "one").unwrap();
        }
        let backed = open_backed(&path);
        let later =
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert!(
            backed.changed_on_disk(),
            "same-size rewrite must be flagged via mtime"
        );
        let _ = std::fs::remove_file(path);
    }
}
