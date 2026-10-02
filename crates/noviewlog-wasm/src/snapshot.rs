//! Snapshot DTOs crossing to JavaScript via serde-wasm-bindgen.
//!
//! Shapes follow the desktop wire conventions: `tab_*` naming for Tab/View
//! fields, `#[serde(default)]`-friendly optional fields, and search state
//! rendered exactly as the desktop status shows it. Segments carry the
//! Line-SGR styles the webview paints (truecolor RGB passthrough); the
//! live-VT grid never crosses this boundary.

use serde::Serialize;

use noviewlog_terminal::types::{FilterRule, FlatLine, TextSegment};
use noviewlog_terminal::visible::highlight_search_in_segments;

use crate::session::{level_name, Session, SessionView};

#[derive(Serialize)]
pub struct SessionSnapshot {
    pub session_id: u32,
    pub name: String,
    pub source: &'static str,
    pub finished: bool,
    pub active_tab: usize,
    pub tabs: Vec<TabInfo>,
    pub view: ViewSnapshot,
    /// Hosted app wants mouse events (webview forwards them to the PTY).
    pub mouse_tracking: bool,
    /// Hosted app wants bracketed paste.
    pub bracketed_paste: bool,
    pub dropped_records: usize,
    pub buffer_records: usize,
    pub buffer_max: usize,
}

#[derive(Serialize)]
pub struct TabInfo {
    pub index: usize,
    pub name: String,
    pub active: bool,
    /// Index 0 is the Terminal tab: read-only filter semantics.
    pub terminal: bool,
}

#[derive(Serialize)]
pub struct ViewSnapshot {
    pub name: String,
    pub severity: &'static str,
    pub follow: bool,
    pub wrap: bool,
    /// Total visible flat lines in this view (`lines` is 0..total).
    pub total_lines: usize,
    /// Session content epoch: stable across pure appends, bumped otherwise.
    pub epoch: u32,
    pub lines: Vec<LineDto>,
    pub search: SearchDto,
    pub filters_locked: bool,
    pub filters: Vec<FilterRule>,
    pub notice: Option<String>,
}

#[derive(Serialize)]
pub struct LineDto {
    pub record_id: u32,
    pub line_index: usize,
    /// Plain text (styles live in `segments`) — the webview wraps on this.
    pub raw: String,
    pub segments: Vec<SegDto>,
    pub level: Option<&'static str>,
    pub collapsible: bool,
    pub collapsed: bool,
    pub hidden_line_count: usize,
}

#[derive(Serialize)]
pub struct SegDto {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fg: Option<[u8; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bg: Option<[u8; 3]>,
    #[serde(skip_serializing_if = "is_default")]
    pub bold: bool,
    #[serde(skip_serializing_if = "is_default")]
    pub dim: bool,
    #[serde(skip_serializing_if = "is_default")]
    pub underline: bool,
    #[serde(skip_serializing_if = "is_default")]
    pub search: bool,
    #[serde(skip_serializing_if = "is_default")]
    pub search_current: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

#[derive(Serialize)]
pub struct SearchDto {
    pub query: String,
    pub regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub error: Option<String>,
    /// Desktop counter label (`"3/40"`), empty when no search is active.
    pub label: String,
    pub match_count: usize,
    pub active_line: Option<usize>,
    /// Monotonic counter; the webview scrolls to `active_line` when it changes.
    pub scroll_request: u32,
}

/// Incremental counterpart to [`SessionSnapshot`]: only flat lines after
/// `base` plus the view metadata. `ok=false` means the host must fall back
/// to a full snapshot.
#[derive(Serialize)]
pub struct AppendSnapshot {
    pub ok: bool,
    pub epoch: u32,
    pub base: usize,
    pub total_lines: usize,
    pub lines: Vec<LineDto>,
    pub search: SearchDto,
    pub follow: bool,
    pub mouse_tracking: bool,
    pub bracketed_paste: bool,
    pub dropped_records: usize,
    pub buffer_records: usize,
}

fn is_default(v: &bool) -> bool {
    !*v
}

fn seg_dto(segment: &TextSegment) -> SegDto {
    let (fg, bg, bold, dim, underline, search, search_current, link) = match &segment.style {
        None => (None, None, false, false, false, false, false, None),
        Some(style) => (
            style.fg.map(|(r, g, b)| [r, g, b]),
            style.bg.map(|(r, g, b)| [r, g, b]),
            style.bold,
            style.dim,
            style.underline,
            style.search,
            style.search_current,
            style.link.as_ref().map(|l| l.to_string()),
        ),
    };
    SegDto {
        text: segment.text.clone(),
        fg,
        bg,
        bold,
        dim,
        underline,
        search,
        search_current,
        link,
    }
}

/// `view_line` is the line's flat index within the view — search match
/// positions live in that coordinate space (the FlatLine `line_index` field
/// is the physical line inside its Record).
fn line_dto(view_line: usize, line: &FlatLine, view: &SessionView) -> LineDto {
    // Search highlighting is baked into the segments so the webview stays a
    // dumb painter (the desktop applies it at paint time the same way).
    let segments = match view.search_highlight_for(view_line) {
        Some((pattern, active)) => highlight_search_in_segments(&line.segments, pattern, active),
        None => line.segments.clone(),
    };
    LineDto {
        record_id: u32::try_from(line.record_id).unwrap_or(u32::MAX),
        line_index: line.line_index,
        raw: line.raw.clone(),
        segments: segments.iter().map(seg_dto).collect(),
        level: level_name(line.level),
        collapsible: line.collapsible,
        collapsed: line.collapsed,
        hidden_line_count: line.hidden_line_count,
    }
}

fn search_dto(view: &SessionView) -> SearchDto {
    SearchDto {
        query: view.search_query.clone(),
        regex: view.search_regex,
        case_sensitive: view.search_case_sensitive,
        whole_word: view.search_whole_word,
        error: view.search_error.clone(),
        label: view.search_counter_label(),
        match_count: view.search_matches_len(),
        active_line: view.search_active_line(),
        scroll_request: view.scroll_request,
    }
}

fn tab_infos(session: &Session) -> Vec<TabInfo> {
    session
        .views
        .iter()
        .enumerate()
        .map(|(index, view)| TabInfo {
            index,
            name: view.name.clone(),
            active: index == session.active_view,
            terminal: index == 0,
        })
        .collect()
}

/// Full snapshot of the session's active Tab/View. The invalid-regex
/// notice of the last filter edit travels in `view.notice` (cleared on
/// tab changes); the panel marks the offending rule inline from it.
pub fn session_snapshot(session: &mut Session) -> SessionSnapshot {
    session.refresh_active();
    let view = session.active();
    let lines: Vec<LineDto> = view
        .flat_lines
        .iter()
        .enumerate()
        .map(|(i, line)| line_dto(i, line, view))
        .collect();
    let view_snapshot = ViewSnapshot {
        name: view.name.clone(),
        severity: view.severity.as_str(),
        follow: view.auto_follow,
        wrap: view.wrap_lines,
        total_lines: view.flat_lines.len(),
        epoch: session.epoch,
        lines,
        search: search_dto(view),
        filters_locked: session.active_view == 0,
        filters: view.filters().to_vec(),
        notice: session.filter_notice.clone(),
    };
    SessionSnapshot {
        session_id: session.id,
        name: session.name.clone(),
        source: session.source.as_str(),
        finished: session.finished,
        active_tab: session.active_view,
        tabs: tab_infos(session),
        view: view_snapshot,
        mouse_tracking: session.ingest.mouse_tracking(),
        bracketed_paste: session.ingest.bracketed_paste(),
        dropped_records: session.buffer.dropped_count(),
        buffer_records: session.buffer.records_len(),
        buffer_max: session.buffer.max_records(),
    }
}

/// Incremental snapshot: flat lines after `base`, valid only when the epoch
/// still matches (any non-append change invalidates it).
pub fn session_append_since(session: &mut Session, epoch: u32, base: usize) -> AppendSnapshot {
    session.refresh_active();
    let view = session.active();
    let total = view.flat_lines.len();
    let ok = session.epoch == epoch && base <= total;
    let lines = if ok {
        view.flat_lines[base..]
            .iter()
            .enumerate()
            .map(|(i, line)| line_dto(base + i, line, view))
            .collect()
    } else {
        Vec::new()
    };
    AppendSnapshot {
        ok,
        epoch: session.epoch,
        base,
        total_lines: total,
        lines,
        search: search_dto(view),
        follow: view.auto_follow,
        mouse_tracking: session.ingest.mouse_tracking(),
        bracketed_paste: session.ingest.bracketed_paste(),
        dropped_records: session.buffer.dropped_count(),
        buffer_records: session.buffer.records_len(),
    }
}
