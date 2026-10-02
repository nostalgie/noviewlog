use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use crate::file_index::{
    decode_lossy_line, read_line_bounded, FileBackedLog, LineIndex, TempFileGuard,
    INDEX_BYTES_PER_TICK, LINE_TRUNCATION_MARKER,
};

/// Files above this size show a small tail window immediately while the line
/// index is built in the background.
pub const FILE_LARGE_BYTES: u64 = 8 * 1024 * 1024;

/// Seek near EOF by this many bytes when picking the initial window for large files.
/// (Actual ingested lines are capped by [`FILE_INITIAL_WINDOW_LINES`].)
pub const FILE_INITIAL_TAIL_BYTES: u64 = 2 * 1024 * 1024;

/// Max lines materialized into the buffer on first open of a large file.
/// Keeps first paint cheap even when the tail has very long lines.
pub const FILE_INITIAL_WINDOW_LINES: usize = 800;

/// Soft cap for file sliding windows (independent of live PTY scrollback setting).
/// Long-line logs (access logs, URLs) explode memory/CPU under wrap if this is 10–30k.
pub const FILE_VIEW_WINDOW_LINES: usize = 2_000;

/// Lines ingested per engine tick while file content is loading.
pub const FILE_LOAD_LINES_PER_TICK: usize = 2_000;

/// Refuse to transcode UTF-16 files larger than this (temp UTF-8 copy would
/// double disk usage; PowerShell `Out-File` logs are far below this).
const UTF16_TRANSCODE_MAX_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq)]
enum FileEncoding {
    Plain,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
}

fn sniff_encoding(prefix: &[u8]) -> Result<FileEncoding, String> {
    if prefix.starts_with(&[0xEF, 0xBB, 0xBF]) {
        Ok(FileEncoding::Utf8Bom)
    } else if prefix.starts_with(&[0xFF, 0xFE, 0x00, 0x00]) {
        // UTF-32LE BOM would transcode as NUL-riddled UTF-16 (issue #111).
        Err("UTF-32LE (BOM FF FE 00 00) log files are not supported; re-encode to UTF-8".into())
    } else if prefix.starts_with(&[0xFF, 0xFE]) {
        Ok(FileEncoding::Utf16Le)
    } else if prefix.starts_with(&[0xFE, 0xFF]) {
        Ok(FileEncoding::Utf16Be)
    } else {
        Ok(FileEncoding::Plain)
    }
}

/// Transcode a UTF-16 (BOM-prefixed) file to a UTF-8 temp file and return its
/// path + size. Unpaired surrogates become U+FFFD instead of failing the load.
fn transcode_utf16_to_temp(file: &mut File, big_endian: bool) -> Result<(PathBuf, u64), String> {
    static TRANSCODE_SEQ: AtomicU64 = AtomicU64::new(0);
    let out_path = std::env::temp_dir().join(format!(
        "noviewlog-utf16-{}-{}.log",
        std::process::id(),
        TRANSCODE_SEQ.fetch_add(1, Ordering::Relaxed)
    ));

    // Skip the 2-byte BOM.
    file.seek(SeekFrom::Start(2))
        .map_err(|e| format!("Seek failed: {e}"))?;
    let mut reader = BufReader::new(&mut *file);
    let mut out = std::io::BufWriter::new(
        std::fs::File::create(&out_path).map_err(|e| format!("Temp file create failed: {e}"))?,
    );

    // Stage keeps chunk boundaries pair-aligned; a trailing high surrogate is
    // held back so pairs split across reads still decode.
    let mut stage: Vec<u8> = Vec::with_capacity(65_536 + 1);
    let mut chunk = vec![0u8; 65_536];
    loop {
        let n = reader
            .read(&mut chunk)
            .map_err(|e| format!("Read error: {e}"))?;
        if n == 0 {
            break;
        }
        stage.extend_from_slice(&chunk[..n]);
        let usable = stage.len() & !1;
        if usable == 0 {
            continue;
        }
        let mut units = Vec::with_capacity(usable / 2);
        for pair in stage[..usable].chunks_exact(2) {
            units.push(if big_endian {
                u16::from_be_bytes([pair[0], pair[1]])
            } else {
                u16::from_le_bytes([pair[0], pair[1]])
            });
        }
        let mut take = units.len();
        if let Some(&last) = units.last() {
            if (0xD800..0xDC00).contains(&last) {
                take -= 1; // maybe a pair continues in the next chunk
            }
        }
        let text = String::from_utf16_lossy(&units[..take]);
        out.write_all(text.as_bytes())
            .map_err(|e| format!("Temp file write failed: {e}"))?;
        stage.drain(..take * 2);
    }
    if !stage.is_empty() {
        // Dangling byte or unpaired surrogate at EOF.
        out.write_all("\u{FFFD}".as_bytes())
            .map_err(|e| format!("Temp file write failed: {e}"))?;
    }
    out.flush()
        .map_err(|e| format!("Temp file write failed: {e}"))?;
    drop(out);

    let size = std::fs::metadata(&out_path)
        .map_err(|e| format!("Temp file stat failed: {e}"))?
        .len();
    Ok((out_path, size))
}

pub struct FileLoadState {
    pub path: String,
    pub file_size: u64,
    /// Content reader (tail-first for large files, start-to-end for small).
    content_reader: Option<BufReader<File>>,
    pub content_lines_read: u64,
    pub content_finished: bool,
    /// Stop content ingest after this many lines (`None` = read until EOF).
    content_line_limit: Option<usize>,
    /// Byte offset where the content reader started (for window placement).
    pub content_start_byte: u64,
    /// Background index construction.
    index_file: Option<File>,
    pub index: LineIndex,
    pub index_bytes_done: u64,
    pub index_finished: bool,
    /// File the IO actually targets (temp UTF-8 copy for UTF-16 sessions).
    source_path: PathBuf,
    /// On-disk identity of the watched original at open time (issue #151),
    /// carried into the derived [`FileBackedLog`] for change detection.
    watch_size: u64,
    watch_mtime: Option<SystemTime>,
    /// Deletes the temp copy when this state (or the derived FileBackedLog) drops.
    temp: Option<TempFileGuard>,
}

impl FileLoadState {
    pub fn open(path: &str) -> Result<Self, String> {
        let path = crate::core::config::expand_path(path);
        let mut file = File::open(&path).map_err(|e| format!("Failed to open {path}: {e}"))?;
        let raw_meta = file.metadata().map_err(|e| e.to_string())?;
        let raw_size = raw_meta.len();
        let watch_mtime = raw_meta.modified().ok();

        // Sniff a BOM on a cloned handle so `file` stays at position 0.
        let encoding = {
            let mut sniff = file.try_clone().map_err(|e| e.to_string())?;
            let mut prefix = [0u8; 4];
            let mut got = 0usize;
            while got < prefix.len() {
                match sniff.read(&mut prefix[got..]) {
                    Ok(0) => break,
                    Ok(k) => got += k,
                    Err(e) => return Err(format!("Read error in {path}: {e}")),
                }
            }
            sniff_encoding(&prefix[..got])?
        };

        let (source_path, file_size, temp, bom_skip) = match encoding {
            FileEncoding::Utf16Le | FileEncoding::Utf16Be => {
                if raw_size > UTF16_TRANSCODE_MAX_BYTES {
                    return Err(format!(
                        "UTF-16 file too large to transcode ({raw_size} bytes): {path}"
                    ));
                }
                let (tmp, size) =
                    transcode_utf16_to_temp(&mut file, encoding == FileEncoding::Utf16Be)?;
                (tmp.clone(), size, Some(TempFileGuard(tmp)), 0u64)
            }
            FileEncoding::Utf8Bom => (PathBuf::from(&path), raw_size, None, 3u64),
            FileEncoding::Plain => (PathBuf::from(&path), raw_size, None, 0u64),
        };

        let content_file =
            File::open(&source_path).map_err(|e| format!("Failed to open {path}: {e}"))?;
        let large = file_size > FILE_LARGE_BYTES;
        let (content_reader, content_start_byte) = if large {
            open_tail_reader(content_file, file_size)?
        } else {
            let mut content_file = content_file;
            if bom_skip > 0 {
                content_file
                    .seek(SeekFrom::Start(bom_skip))
                    .map_err(|e| format!("Seek failed: {e}"))?;
            }
            (Some(BufReader::new(content_file)), bom_skip)
        };

        let index_file =
            File::open(&source_path).map_err(|e| format!("Failed to open {path}: {e}"))?;

        Ok(Self {
            path,
            file_size,
            content_reader,
            content_lines_read: 0,
            content_finished: file_size == 0,
            content_line_limit: if large {
                Some(FILE_INITIAL_WINDOW_LINES)
            } else {
                None
            },
            content_start_byte,
            index_file: Some(index_file),
            index: LineIndex::new(file_size),
            index_bytes_done: bom_skip,
            index_finished: file_size == 0,
            source_path,
            watch_size: raw_size,
            watch_mtime,
            temp,
        })
    }

    /// Advance content load and/or index scan. Returns `(content_lines, content_done, index_done)`.
    pub fn tick(&mut self) -> Result<(Vec<String>, bool, bool), String> {
        let mut lines = Vec::new();

        if let Some(reader) = self.content_reader.as_mut() {
            if !self.content_finished {
                let limit = self.content_line_limit.unwrap_or(usize::MAX);
                let budget = FILE_LOAD_LINES_PER_TICK
                    .min(limit.saturating_sub(self.content_lines_read as usize));
                for _ in 0..budget {
                    let mut raw = Vec::new();
                    match read_line_bounded(reader, &mut raw) {
                        Ok((0, _)) => {
                            self.content_finished = true;
                            break;
                        }
                        Ok((_, truncated)) => {
                            let mut text = decode_lossy_line(&raw);
                            if truncated {
                                text.push_str(LINE_TRUNCATION_MARKER);
                            }
                            lines.push(text);
                            self.content_lines_read += 1;
                        }
                        Err(err) => {
                            return Err(format!("Read error in {}: {err}", self.path));
                        }
                    }
                }
                if self
                    .content_line_limit
                    .is_some_and(|lim| self.content_lines_read as usize >= lim)
                {
                    self.content_finished = true;
                }
            }
        } else {
            self.content_finished = true;
        }

        // Index in parallel with content so large files become scrollable sooner.
        if let Some(file) = self.index_file.as_mut() {
            if !self.index_finished {
                let (next, done) =
                    self.index
                        .scan_chunk(file, self.index_bytes_done, INDEX_BYTES_PER_TICK)?;
                self.index_bytes_done = next;
                if done {
                    self.index_finished = true;
                }
            }
        }

        Ok((lines, self.content_finished, self.index_finished))
    }

    pub fn index_progress(&self) -> f32 {
        self.index.progress(self.index_bytes_done)
    }

    pub fn into_backed(self) -> Result<FileBackedLog, String> {
        let file = File::open(&self.source_path)
            .map_err(|e| format!("Failed to open {}: {e}", self.path))?;
        Ok(FileBackedLog {
            path: self.path,
            file: Arc::new(std::sync::Mutex::new(file)),
            index: self.index,
            watch_size: self.watch_size,
            watch_mtime: self.watch_mtime,
            temp: self.temp,
        })
    }

    pub fn is_finished(&self) -> bool {
        self.content_finished && self.index_finished
    }

    /// Content window is ready to show (index may still be running).
    pub fn content_ready(&self) -> bool {
        self.content_finished
    }
}

fn open_tail_reader(
    mut file: File,
    file_size: u64,
) -> Result<(Option<BufReader<File>>, u64), String> {
    let seek_pos = file_size.saturating_sub(FILE_INITIAL_TAIL_BYTES);
    file.seek(SeekFrom::Start(seek_pos))
        .map_err(|e| format!("Seek failed: {e}"))?;

    // True when the byte just before the seek offset is a '\n' (or the seek
    // is at the file start): the run we read below is then a *complete* line
    // rather than the tail of a longer one.
    let boundary_before = if seek_pos > 0 {
        let mut prev = [0u8; 1];
        let at_newline = file
            .seek(SeekFrom::Start(seek_pos - 1))
            .and_then(|_| file.read_exact(&mut prev))
            .map(|_| prev[0] == b'\n')
            .unwrap_or(false);
        file.seek(SeekFrom::Start(seek_pos))
            .map_err(|e| format!("Seek failed: {e}"))?;
        at_newline
    } else {
        false
    };

    let mut reader = BufReader::new(file);
    let mut content_start = seek_pos;
    if seek_pos > 0 {
        // The seek offset is arbitrary and may land inside a multi-byte UTF-8
        // sequence (Cyrillic / CJK / emoji logs). Discard a *partial* line as
        // raw bytes: `read_line` would fail UTF-8 validation and abort the
        // whole file open. Content starts after the discarded bytes, at the
        // next line start (issue #161). When the seek lands exactly on a
        // line boundary the line is intact — re-seek and keep it instead of
        // silently dropping one full line (#197).
        let mut discard = Vec::new();
        let (consumed, truncated) = read_line_bounded(&mut reader, &mut discard)
            .map_err(|err| format!("Read error after seek: {err}"))?;
        // Intact = boundary before, complete run (newline present, nothing
        // capped) and valid UTF-8. A seek landing mid-line still reads up to
        // the next '\n', but that run starts inside a character or mid-text
        // and must be dropped.
        let intact_line = boundary_before
            && !truncated
            && discard.last() == Some(&b'\n')
            && std::str::from_utf8(&discard).is_ok();
        if intact_line {
            reader
                .seek(SeekFrom::Start(seek_pos))
                .map_err(|e| format!("Seek failed: {e}"))?;
        } else {
            content_start = seek_pos + consumed as u64;
        }
    }

    Ok((Some(reader), content_start))
}

/// One step of background load progress (issue #55): the load/index pipeline
/// runs on a worker thread and reports through a channel the engine drains
/// per tick. The UI thread performs zero filesystem work during a load.
pub enum LoadEvent {
    /// A content batch plus index state after one worker tick.
    Progress {
        lines: Vec<String>,
        content_done: bool,
        index_done: bool,
        content_lines_read: u64,
        index_progress: f32,
        /// `Some(line)` once the index can map the tail window start byte;
        /// `Some(0)` immediately when content starts at byte 0.
        tail_start_line: Option<u64>,
    },
    /// Load finished; `backed` carries the index + shared read handle.
    Done {
        backed: Box<FileBackedLog>,
        content_lines_read: u64,
        tail_start_line: u64,
    },
    Failed(String),
}

/// Engine-side handle to a background load. Dropping it stops the worker
/// (the channel send fails on the next tick boundary).
pub struct FileLoadHandle {
    pub path: String,
    rx: mpsc::Receiver<LoadEvent>,
    /// Pending-event gauge the worker increments before each send and the
    /// receiver decrements in [`Self::drain`]. Test hook for the #238
    /// backpressure bound; the sync channel itself is the enforcement.
    in_flight: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Mirrored worker state for stats (`file_index_progress`).
    pub index_progress: f32,
    pub content_lines_read: u64,
    pub index_done: bool,
}

impl FileLoadHandle {
    /// Events sent by the worker and not yet drained. May briefly overcount
    /// by one while the sender is parked inside a blocking send.
    #[cfg(test)]
    fn in_flight(&self) -> usize {
        use std::sync::atomic::Ordering::Relaxed;
        self.in_flight.load(Relaxed)
    }

    /// Take at most `max` pending events (never blocks).
    pub fn drain(&mut self, max: usize) -> Vec<LoadEvent> {
        use std::sync::atomic::Ordering::Relaxed;
        let mut out = Vec::new();
        while out.len() < max {
            match self.rx.try_recv() {
                Ok(event) => {
                    self.in_flight.fetch_sub(1, Relaxed);
                    self.note(&event);
                    out.push(event);
                }
                Err(_) => break,
            }
        }
        out
    }

    /// True once the worker channel is disconnected: the worker thread
    /// dropped its sender without ever posting Done/Failed (e.g. it panicked
    /// after channel setup). Such a load can never finish, so the engine
    /// converts it to `Failed` instead of waiting forever (issue #253).
    ///
    /// Drains and RETURNS any events that landed meanwhile instead of
    /// consuming them silently: a bare `try_recv` probe could eat a `Done` /
    /// `Failed` that arrived between the caller's drain and this check,
    /// misclassifying a healthy finished load as stalled.
    pub fn probe(&mut self) -> (bool, Vec<LoadEvent>) {
        use std::sync::atomic::Ordering::Relaxed;
        use std::sync::mpsc::TryRecvError;
        let mut out = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(event) => {
                    self.in_flight.fetch_sub(1, Relaxed);
                    self.note(&event);
                    out.push(event);
                }
                Err(TryRecvError::Empty) => return (false, out),
                Err(TryRecvError::Disconnected) => return (true, out),
            }
        }
    }

    fn note(&mut self, event: &LoadEvent) {
        if let LoadEvent::Progress {
            content_lines_read,
            index_progress,
            index_done,
            ..
        } = event
        {
            self.content_lines_read = *content_lines_read;
            self.index_progress = *index_progress;
            self.index_done = *index_done;
        }
    }
}

/// Cap on load events applied per engine tick: bounds UI work per tick while
/// the worker free-runs ahead (the channel buffers the rest in order).
pub const LOAD_EVENTS_PER_TICK: usize = 32;

/// Last-resort backstop for a background load whose worker neither posts
/// events nor disconnects (e.g. a blocked network-share `open`) — issue #253.
/// A healthy worker sends Progress/Done far more often than this; when the
/// quiet period lapses the engine fails the load so `host_work_pending`
/// cannot wedge the fast tick cadence forever. A *disconnected* worker is
/// failed immediately without waiting this long.
pub const FILE_LOAD_STALL_TIMEOUT: Duration = Duration::from_secs(120);

/// Worker→UI channel capacity (issue #238): the worker blocks in `send` once
/// this many [`LoadEvent`]s are pending, so a stalled UI can no longer let the
/// queue grow with the whole file. Payload skipping is not an option here —
/// each Progress event's lines extend the contiguous tail buffer window
/// (`push_lines` + `buffer_line_start/end` bookkeeping), so dropping a payload
/// would punch holes into the visible lines. Blocking the worker caps memory
/// instead and cannot deadlock: the UI never waits on the worker, and a
/// dropped handle disconnects the channel so the blocked send fails.
pub const LOAD_CHANNEL_CAP: usize = 8;

/// Start a background load of `path`: opening, BOM sniffing, UTF-16
/// transcoding, content reads, and index scans all run on a worker thread
/// (issue #55). The handle drains [`LoadEvent`]s on the engine tick.
pub fn spawn_file_load(path: &str) -> FileLoadHandle {
    // Bounded (issue #238): the worker blocks on send once the UI falls
    // LOAD_CHANNEL_CAP events behind instead of buffering the whole file.
    let (tx, rx) = mpsc::sync_channel(LOAD_CHANNEL_CAP);
    let spawn_err_tx = tx.clone();
    let owned_path = path.to_string();
    let in_flight = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let worker_in_flight = in_flight.clone();
    let spawned = std::thread::Builder::new()
        .name("noviewlog-file-load".into())
        .spawn(move || {
            use std::sync::atomic::Ordering::Relaxed;
            // Gauge wrapper around the blocking send: on disconnect the
            // message is not delivered, so the pending count comes back down.
            let send = |event: LoadEvent| -> bool {
                worker_in_flight.fetch_add(1, Relaxed);
                match tx.send(event) {
                    Ok(()) => true,
                    Err(_) => {
                        worker_in_flight.fetch_sub(1, Relaxed);
                        false
                    }
                }
            };
            let mut state = match FileLoadState::open(&owned_path) {
                Ok(state) => state,
                Err(message) => {
                    let _ = send(LoadEvent::Failed(message));
                    return;
                }
            };
            let content_start_byte = state.content_start_byte;
            // Exact line number of the content window start once the index is
            // complete (interpolated line_at_offset can be off by a whole
            // checkpoint stride on uneven line lengths — issue #161).
            let exact_tail_start_line = |state: &mut FileLoadState| -> u64 {
                match state.index_file.as_mut() {
                    Some(file) => state
                        .index
                        .line_at_byte_exact(file, content_start_byte)
                        .unwrap_or_else(|_| state.index.line_at_offset(content_start_byte)),
                    None => state.index.line_at_offset(content_start_byte),
                }
            };
            loop {
                match state.tick() {
                    Ok((lines, content_done, index_done)) => {
                        let tail_start_line = if state.index_finished {
                            Some(exact_tail_start_line(&mut state))
                        } else if content_start_byte == 0 {
                            Some(0)
                        } else {
                            None
                        };
                        let event = LoadEvent::Progress {
                            lines,
                            content_done,
                            index_done,
                            content_lines_read: state.content_lines_read,
                            index_progress: state.index_progress(),
                            tail_start_line,
                        };
                        if !send(event) {
                            return;
                        }
                        if state.is_finished() {
                            let content_lines_read = state.content_lines_read;
                            let tail_start_line = exact_tail_start_line(&mut state);
                            match state.into_backed() {
                                Ok(backed) => {
                                    let _ = send(LoadEvent::Done {
                                        backed: Box::new(backed),
                                        content_lines_read,
                                        tail_start_line,
                                    });
                                }
                                Err(message) => {
                                    let _ = send(LoadEvent::Failed(message));
                                }
                            }
                            return;
                        }
                    }
                    Err(message) => {
                        let _ = send(LoadEvent::Failed(message));
                        return;
                    }
                }
            }
        });
    if let Err(err) = spawned {
        // Thread exhaustion must degrade to a Failed load, not panic the UI
        // process — every other open failure reports through the channel (#197).
        let _ = spawn_err_tx.send(LoadEvent::Failed(format!("spawn file-load worker: {err}")));
    }
    FileLoadHandle {
        path: path.to_string(),
        rx,
        in_flight,
        index_progress: 0.0,
        content_lines_read: 0,
        index_done: false,
    }
}

/// Create a temp log for tests (line_count lines, each ~20 bytes).
#[cfg(test)]
pub fn write_test_log(path: &std::path::Path, line_count: usize) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::File::create(path)?;
    for i in 0..line_count {
        writeln!(file, "log line {i:08} payload")?;
    }
    Ok(())
}

/// Test seam (issue #253): a handle whose worker already disconnected —
/// the sender was dropped without ever posting Done/Failed, exactly like a
/// worker that panicked after channel setup.
#[cfg(test)]
pub fn disconnected_file_load_handle_for_test(path: &str) -> FileLoadHandle {
    let (_, rx) = mpsc::sync_channel(LOAD_CHANNEL_CAP);
    FileLoadHandle {
        path: path.to_string(),
        rx,
        in_flight: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        index_progress: 0.0,
        content_lines_read: 0,
        index_done: false,
    }
}

/// Test seam: a live handle plus its sender, for probe/drain semantics tests.
#[cfg(test)]
pub fn channel_handle_for_test(path: &str) -> (mpsc::SyncSender<LoadEvent>, FileLoadHandle) {
    let (tx, rx) = mpsc::sync_channel(LOAD_CHANNEL_CAP);
    (
        tx,
        FileLoadHandle {
            path: path.to_string(),
            rx,
            in_flight: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            index_progress: 0.0,
            content_lines_read: 0,
            index_done: false,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_log_path(name: &str) -> std::path::PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("noviewlog-{name}-{stamp}.log"))
    }

    /// Issue #238 regression: while the UI does not drain, pending events stay at
    /// the channel cap (worker blocks) instead of growing with the file.
    #[test]
    #[ignore = "slow tier: sleeps to let the worker run ahead; run with -- --ignored"]
    fn load_channel_is_backpressured_when_ui_stalls() {
        let path = temp_log_path("backpressure");
        write_test_log(&path, 200_000).unwrap();
        let mut handle = spawn_file_load(path.to_str().unwrap());
        std::thread::sleep(std::time::Duration::from_millis(300));
        // Snapshot the gauge without draining: draining concurrently would let
        // the worker refill while we count. With the worker parked in a blocking
        // send this is the channel's pending depth (allowing one in-send slot).
        let pending = handle.in_flight();
        assert!(
            pending <= LOAD_CHANNEL_CAP + 1,
            "queue depth {pending} exceeds cap {LOAD_CHANNEL_CAP} (+1 in-send)"
        );
        assert!(pending > 0, "worker must have produced events");
        // Drain to completion so the worker exits before the temp file is removed.
        while !matches!(
            handle.drain(LOAD_CHANNEL_CAP).last(),
            Some(LoadEvent::Done { .. })
        ) {}
        let _ = std::fs::remove_file(path);
    }

    #[test]
    #[ignore = "slow tier: generates + indexes a large fixture; run with -- --ignored"]
    fn large_file_starts_at_tail_but_indexes_whole_file() {
        let path = temp_log_path("large");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            for i in 0..90_000 {
                writeln!(f, "line {i:08} {}", "x".repeat(80)).unwrap();
            }
        }
        let mut state = FileLoadState::open(path.to_str().unwrap()).unwrap();
        assert!(state.file_size > FILE_LARGE_BYTES);
        assert!(state.content_start_byte > 0);

        let mut first_line: Option<String> = None;
        while !state.content_finished {
            let (lines, _, _) = state.tick().unwrap();
            if first_line.is_none() {
                first_line = lines.first().cloned();
            }
        }
        let first = first_line.expect("tail content");
        assert!(
            !first.contains("line 000000"),
            "initial view should be tail, got {first}"
        );
        assert!(
            state.content_lines_read as usize <= FILE_INITIAL_WINDOW_LINES,
            "initial window must be capped, got {}",
            state.content_lines_read
        );

        while !state.index_finished {
            state.tick().unwrap();
        }
        assert_eq!(state.index.total_lines(), 90_000);

        let backed = state.into_backed().unwrap();
        let mut file = backed.file.lock().unwrap();
        let early = backed.index.read_lines(&mut file, 0, 2).unwrap();
        assert!(early[0].contains("line 000000"));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    #[ignore = "slow tier: generates + indexes a large fixture; run with -- --ignored"]
    fn tail_seek_mid_utf8_char_opens_large_file() {
        // Regression for issue #50: the tail seek offset (file_size - 2 MiB)
        // must land inside a multi-byte UTF-8 character. Layout (byte offsets):
        //   [0..prefix_len)              'a' prefix (no newline)
        //   [prefix_len..prefix_len+3)   "日" (E6 97 A5)
        //   [prefix_len+3..EOF)          209715 lines of "sNNNNNNNN\n"
        // With prefix_len = FILE_LARGE_BYTES - FILE_INITIAL_TAIL_BYTES + 1024,
        // file_size = prefix_len + 3 + 2_097_150 = FILE_LARGE_BYTES + 1025, so
        // the file takes the large path and seek_pos = prefix_len + 1 lands on
        // the 2nd byte of the 3-byte char.
        let path = temp_log_path("utf8-boundary");
        let tail = FILE_INITIAL_TAIL_BYTES as usize;
        let prefix_len = (FILE_LARGE_BYTES - FILE_INITIAL_TAIL_BYTES) as usize + 1024;
        {
            let mut f = std::fs::File::create(&path).unwrap();
            f.write_all(&vec![b'a'; prefix_len]).unwrap();
            f.write_all("日".as_bytes()).unwrap();
            for i in 0..((tail - 2) / 10) {
                writeln!(f, "s{i:08}").unwrap();
            }
        }
        let mut state = FileLoadState::open(path.to_str().unwrap()).unwrap();
        assert!(state.file_size > FILE_LARGE_BYTES);
        // Content starts after the discarded partial line: 2 bytes of the
        // split char + the first complete "s00000000\n" line (issue #161).
        assert_eq!(state.content_start_byte, prefix_len as u64 + 13);

        // The partial line (rest of the char + first suffix line) is discarded;
        // content starts at the first complete line after it.
        let mut first_line: Option<String> = None;
        while first_line.is_none() && !state.content_finished {
            let (lines, _, _) = state.tick().unwrap();
            first_line = lines.first().cloned();
        }
        assert_eq!(first_line.as_deref(), Some("s00000001"));

        while !state.index_finished {
            state.tick().unwrap();
        }
        assert_eq!(state.index.total_lines(), 209_715);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    #[ignore = "slow tier: generates + indexes a large fixture; run with -- --ignored"]
    fn tail_seek_on_line_boundary_keeps_the_line() {
        // Issue #197: when file_size - FILE_INITIAL_TAIL_BYTES lands exactly
        // on a line start, that intact line must open as the first content
        // line instead of being discarded. Layout: 'a' * 6_291_459 + '\n'
        // (prefix ends on a newline so the boundary check sees one), then
        // 262_144 lines of "sNNNNNN\n" (8 bytes). Size = 8_388_612 >
        // FILE_LARGE_BYTES, and seek_pos = size - 2 MiB = 6_291_460 = the
        // first line's start offset.
        let path = temp_log_path("tail-boundary");
        let prefix_len: usize = 6_291_460;
        {
            let mut f = std::fs::File::create(&path).unwrap();
            f.write_all(&vec![b'a'; prefix_len - 1]).unwrap();
            f.write_all(b"\n").unwrap();
            for i in 0..262_144usize {
                writeln!(f, "s{i:06}").unwrap();
            }
        }
        let mut state = FileLoadState::open(path.to_str().unwrap()).unwrap();
        assert!(state.file_size > FILE_LARGE_BYTES);
        assert_eq!(state.content_start_byte, prefix_len as u64);

        let mut first_line: Option<String> = None;
        while first_line.is_none() && !state.content_finished {
            let (lines, _, _) = state.tick().unwrap();
            first_line = lines.first().cloned();
        }
        assert_eq!(
            first_line.as_deref(),
            Some("s000000"),
            "the intact line at the seek boundary must be kept"
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn small_file_reads_from_start() {
        let path = temp_log_path("small");
        write_test_log(&path, 100).unwrap();
        let state = FileLoadState::open(path.to_str().unwrap()).unwrap();
        assert_eq!(state.content_start_byte, 0);
        assert!(!state.index_finished);
        let _ = std::fs::remove_file(path);
    }

    fn drain_content(state: &mut FileLoadState) -> Vec<String> {
        let mut all = Vec::new();
        while !state.content_finished {
            let (lines, _, _) = state.tick().unwrap();
            all.extend(lines);
        }
        while !state.index_finished {
            state.tick().unwrap();
        }
        all
    }

    #[test]
    fn non_utf8_byte_loads_with_replacement() {
        // Issue #51: a lone cp1251 byte must not abort load/index/match.
        let path = temp_log_path("cp1251");
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&path).unwrap();
            f.write_all(b"log \xED gotcha\nplain\n").unwrap();
        }
        let mut state = FileLoadState::open(path.to_str().unwrap()).unwrap();
        let lines = drain_content(&mut state);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains('\u{FFFD}'), "got {:?}", lines[0]);
        assert_eq!(lines[1], "plain");
        assert_eq!(state.index.total_lines(), 2);

        let backed = state.into_backed().unwrap();
        let read = backed.read_lines(0, 2).unwrap();
        assert!(read[0].contains('\u{FFFD}'));
        assert_eq!(read[1], "plain");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn utf16le_bom_file_loads() {
        // Issue #51: PowerShell `Out-File` default encoding (UTF-16LE + BOM).
        let path = temp_log_path("utf16le");
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&path).unwrap();
            let text = "alpha\r\nerror beta\r\n";
            let units: Vec<u16> = text.encode_utf16().collect();
            let bytes: Vec<u8> = units.iter().flat_map(|u| u.to_le_bytes()).collect();
            f.write_all(&[0xFF, 0xFE]).unwrap();
            f.write_all(&bytes).unwrap();
        }
        let mut state = FileLoadState::open(path.to_str().unwrap()).unwrap();
        let lines = drain_content(&mut state);
        assert_eq!(lines, vec!["alpha", "error beta"]);
        assert_eq!(state.index.total_lines(), 2);

        let backed = state.into_backed().unwrap();
        assert_eq!(
            backed.read_lines(0, 2).unwrap(),
            vec!["alpha", "error beta"]
        );
        drop(backed);
        // Temp copy is removed with the backed log.
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn utf16be_bom_file_loads() {
        let path = temp_log_path("utf16be");
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&path).unwrap();
            let text = "one\nдва\n";
            let units: Vec<u16> = text.encode_utf16().collect();
            let bytes: Vec<u8> = units.iter().flat_map(|u| u.to_be_bytes()).collect();
            f.write_all(&[0xFE, 0xFF]).unwrap();
            f.write_all(&bytes).unwrap();
        }
        let mut state = FileLoadState::open(path.to_str().unwrap()).unwrap();
        let lines = drain_content(&mut state);
        assert_eq!(lines, vec!["one", "два"]);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn utf16_unpaired_surrogate_becomes_replacement() {
        let path = temp_log_path("utf16-surrogate");
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&path).unwrap();
            f.write_all(&[0xFF, 0xFE]).unwrap();
            // Valid pair then a lone high surrogate at EOF.
            f.write_all(&0x0041u16.to_le_bytes()).unwrap();
            f.write_all(&0xD83Du16.to_le_bytes()).unwrap();
            f.write_all(&0xDE00u16.to_le_bytes()).unwrap();
            f.write_all(&0xD800u16.to_le_bytes()).unwrap();
        }
        let mut state = FileLoadState::open(path.to_str().unwrap()).unwrap();
        let lines = drain_content(&mut state);
        assert_eq!(lines, vec!["A\u{1F600}\u{FFFD}"]);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn utf8_bom_stripped_from_content_and_index() {
        // Issue #74: the BOM must not ride along line 0 into search/copy.
        let path = temp_log_path("utf8bom");
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&path).unwrap();
            f.write_all(b"\xEF\xBB\xBFalpha\nbeta\n").unwrap();
        }
        let mut state = FileLoadState::open(path.to_str().unwrap()).unwrap();
        let lines = drain_content(&mut state);
        assert_eq!(lines, vec!["alpha", "beta"]);

        let backed = state.into_backed().unwrap();
        assert_eq!(backed.read_lines(0, 2).unwrap(), vec!["alpha", "beta"]);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn big_log_opens_quickly_with_generated_fixture() {
        // Issue #72: generated fixture instead of a hardcoded dev-machine path.
        let path = crate::tests::big_log_fixture();
        let start = std::time::Instant::now();
        let mut state = FileLoadState::open(path.to_str().unwrap()).unwrap();
        assert!(state.file_size > FILE_LARGE_BYTES);

        // First content window must finish fast (not the whole file).
        while !state.content_finished {
            state.tick().unwrap();
        }
        let content_ms = start.elapsed().as_millis();
        assert!(
            state.content_lines_read as usize <= FILE_INITIAL_WINDOW_LINES,
            "content lines {}",
            state.content_lines_read
        );
        assert!(
            content_ms < 2_000,
            "initial window took {content_ms}ms (want <2s)"
        );

        // Full sparse index for ~74MB should finish in a few seconds of CPU ticks.
        let index_start = std::time::Instant::now();
        while !state.index_finished {
            state.tick().unwrap();
        }
        let index_ms = index_start.elapsed().as_millis();
        assert!(
            index_ms < 15_000,
            "sparse index took {index_ms}ms (want <15s)"
        );
        assert!(state.index.total_lines() > 100_000);
    }

    #[test]
    fn backed_log_records_open_time_watch_identity() {
        // Issue #151: change detection compares against the ORIGINAL path's
        // stat at open time.
        let path = temp_log_path("watch-identity");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "alpha").unwrap();
            writeln!(f, "beta").unwrap();
        }
        let mut state = FileLoadState::open(path.to_str().unwrap()).unwrap();
        let _ = drain_content(&mut state);
        let backed = state.into_backed().unwrap();
        assert_eq!(
            backed.watch_size,
            std::fs::metadata(&path).unwrap().len(),
            "watch size must be the original path's size at open"
        );
        assert!(backed.watch_mtime.is_some(), "mtime recorded for watching");
        assert!(!backed.changed_on_disk());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn utf16_session_watches_original_size_not_temp_copy() {
        // Issue #151: the index covers the transcoded temp UTF-8 copy, so the
        // watch baseline must be stored explicitly from the original stat —
        // deriving it from `index.file_size()` would false-positive forever.
        let path = temp_log_path("utf16-watch");
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&path).unwrap();
            let text = "alpha\r\nbeta\r\n";
            let units: Vec<u16> = text.encode_utf16().collect();
            let bytes: Vec<u8> = units.iter().flat_map(|u| u.to_le_bytes()).collect();
            f.write_all(&[0xFF, 0xFE]).unwrap();
            f.write_all(&bytes).unwrap();
        }
        let original_size = std::fs::metadata(&path).unwrap().len();
        let mut state = FileLoadState::open(path.to_str().unwrap()).unwrap();
        let _ = drain_content(&mut state);
        let backed = state.into_backed().unwrap();
        assert_eq!(backed.watch_size, original_size);
        assert_ne!(
            backed.watch_size,
            backed.index.file_size(),
            "index size is the temp UTF-8 copy, not the watched original"
        );
        assert!(!backed.changed_on_disk());
        drop(backed);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn content_start_byte_is_an_exact_line_start_on_uneven_lines() {
        // Issue #161: a huge single line before the tail seek must not skew
        // content_start_byte or the resolved tail line number.
        let path = temp_log_path("uneven-tail");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            // One oversized line (no newline) larger than the tail window,
            // then many short lines.
            let huge_len = FILE_LARGE_BYTES as usize + 4096;
            f.write_all(format!("huge {}\n", "h".repeat(huge_len)).as_bytes())
                .unwrap();
            for i in 0..3000 {
                writeln!(f, "t{i:06}").unwrap();
            }
        }
        let mut state = FileLoadState::open(path.to_str().unwrap()).unwrap();
        assert!(state.file_size > FILE_LARGE_BYTES);
        let csb = state.content_start_byte;
        while !state.index_finished {
            state.tick().unwrap();
        }
        assert!(csb > 0);

        let mut file = std::fs::File::open(&path).unwrap();
        // Ground truth: count newlines strictly before the content start.
        file.seek(std::io::SeekFrom::Start(0)).unwrap();
        let mut all = Vec::new();
        {
            use std::io::Read;
            file.read_to_end(&mut all).unwrap();
        }
        let expected_line = all[..csb as usize].iter().filter(|&&b| b == b'\n').count() as u64;
        let resolved = state.index.line_at_byte_exact(&mut file, csb).unwrap();
        assert_eq!(
            resolved, expected_line,
            "resolved tail line must equal the newline count before content_start_byte"
        );
        // Round-trip through the exact offset resolver.
        assert_eq!(
            state.index.offset_of_exact(&mut file, resolved).unwrap(),
            Some(csb),
            "the resolved line must start exactly at content_start_byte"
        );

        let _ = std::fs::remove_file(path);
    }
    // Issue found in pass 2: the old liveness probe used a bare try_recv and
    // could CONSUME a Done/Failed that landed between the caller's drain and
    // the check, so a healthy finished load was misclassified as "stalled".
    #[test]
    fn probe_returns_landed_events_instead_of_eating_them() {
        let (tx, mut handle) = channel_handle_for_test("probe-live");
        // Drain sees an empty queue, then the worker posts the final event.
        assert!(handle.drain(32).is_empty());
        tx.send(LoadEvent::Failed("done-as-failed".into())).unwrap();
        let (disconnected, events) = handle.probe();
        assert!(!disconnected, "sender still alive during probe");
        assert_eq!(events.len(), 1, "probe must return the landed event");
        // Queue is now empty but the channel is still connected.
        let (disconnected, events) = handle.probe();
        assert!(!disconnected);
        assert!(events.is_empty());
        drop(tx);
        let (disconnected, events) = handle.probe();
        assert!(disconnected);
        assert!(events.is_empty());
    }

    #[test]
    fn probe_reports_disconnected_only_when_queue_drained() {
        let (tx, mut handle) = channel_handle_for_test("probe-disc");
        tx.send(LoadEvent::Failed("late failure".into())).unwrap();
        drop(tx);
        // A disconnect with events still queued must deliver the events first.
        let (disconnected, events) = handle.probe();
        assert!(disconnected);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], LoadEvent::Failed(ref m) if m == "late failure"));
        let (disconnected, events) = handle.probe();
        assert!(disconnected);
        assert!(events.is_empty());
    }
}
