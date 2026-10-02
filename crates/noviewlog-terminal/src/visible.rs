use regex::Regex;
use std::collections::HashSet;
use std::sync::Arc;

use crate::ansi::{overlay_styles, parse_ansi_line, strip_ansi};
use crate::buffer::RecordBuffer;
use crate::filter::FilterEngine;
use crate::types::{
    compile_filter, FilterRule, FilterType, FlatLine, SearchMatch, SeverityFilter, TextSegment,
    TextStyle,
};

pub fn rebuild_flat_lines(
    buffer: &mut RecordBuffer,
    filter_engine: &FilterEngine,
    severity: SeverityFilter,
    expanded_record_ids: &HashSet<u64>,
) -> Vec<FlatLine> {
    rebuild_flat_lines_for_records(
        buffer.records(),
        filter_engine,
        severity,
        expanded_record_ids,
    )
}

pub fn rebuild_flat_lines_for_records(
    records: &[crate::types::LogRecord],
    filter_engine: &FilterEngine,
    severity: SeverityFilter,
    expanded_record_ids: &HashSet<u64>,
) -> Vec<FlatLine> {
    // Pipeline: include/exclude → severity → collapse → flat lines.
    let visible = filter_engine.filter_records(records);
    let mut flat = Vec::new();

    for record in visible {
        let level = record.effective_level();
        if !severity.allows(level) {
            continue;
        }
        let collapsible = record.lines.len() >= 2;
        let collapsed = collapsible && !expanded_record_ids.contains(&record.id);
        if collapsed {
            let colored = &record.lines[0];
            let segments = parse_ansi_line(colored);
            let plain: String = segments.iter().map(|s| s.text.as_str()).collect();
            flat.push(FlatLine {
                record_id: record.id,
                line_index: 0,
                segments,
                raw: plain,
                level,
                collapsible: true,
                collapsed: true,
                hidden_line_count: record.lines.len() - 1,
            });
            continue;
        }
        for (line_index, colored) in record.lines.iter().enumerate() {
            let segments = parse_ansi_line(colored);
            let plain: String = segments.iter().map(|s| s.text.as_str()).collect();
            flat.push(FlatLine {
                record_id: record.id,
                line_index,
                segments,
                raw: plain,
                level: if line_index == 0 { level } else { None },
                collapsible,
                collapsed: false,
                hidden_line_count: 0,
            });
        }
    }

    flat
}

/// Build flat lines from already-matched plain/ANSI lines (match-index path).
pub fn flat_lines_from_raw_lines(lines: &[String], id_base: u64) -> Vec<FlatLine> {
    let mut flat = Vec::with_capacity(lines.len());
    for (i, colored) in lines.iter().enumerate() {
        let plain = strip_ansi(colored);
        let segments = parse_ansi_line(colored);
        flat.push(FlatLine {
            record_id: id_base.saturating_add(i as u64),
            line_index: 0,
            segments,
            raw: plain,
            level: None,
            collapsible: false,
            collapsed: false,
            hidden_line_count: 0,
        });
    }
    flat
}

/// Records that need expand so a search hit on a non-preview line becomes visible.
pub fn record_ids_needing_expand_for_search(
    records: &[crate::types::LogRecord],
    filter_engine: &FilterEngine,
    severity: SeverityFilter,
    expanded_record_ids: &HashSet<u64>,
    pattern: &SearchPattern,
) -> Vec<u64> {
    let mut need = Vec::new();
    for record in filter_engine.filter_records(records) {
        if !severity.allows(record.effective_level()) {
            continue;
        }
        if record.lines.len() < 2 || expanded_record_ids.contains(&record.id) {
            continue;
        }
        for (line_index, colored) in record.lines.iter().enumerate() {
            if line_index == 0 {
                continue;
            }
            let plain = strip_ansi(colored);
            if pattern.find_iter(&plain).next().is_some() {
                need.push(record.id);
                break;
            }
        }
    }
    need
}

/// Compiled search pattern: ASCII literal scan, or regex (incl. unicode literal fallback).
#[derive(Clone, Debug)]
pub enum SearchPattern {
    /// Case-insensitive ASCII needle (bytes are ASCII).
    AsciiLiteral(String),
    Regex(Arc<Regex>),
}

impl SearchPattern {
    pub fn find_iter<'a>(&'a self, haystack: &'a str) -> SearchMatchIter<'a> {
        match self {
            SearchPattern::AsciiLiteral(needle) => SearchMatchIter::Ascii {
                haystack: haystack.as_bytes(),
                needle: needle.as_bytes(),
                pos: 0,
            },
            SearchPattern::Regex(re) => SearchMatchIter::Regex {
                iter: re.find_iter(haystack),
            },
        }
    }
}

pub enum SearchMatchIter<'a> {
    Ascii {
        haystack: &'a [u8],
        needle: &'a [u8],
        pos: usize,
    },
    Regex {
        iter: regex::Matches<'a, 'a>,
    },
}

impl Iterator for SearchMatchIter<'_> {
    type Item = (usize, usize);

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            SearchMatchIter::Ascii {
                haystack,
                needle,
                pos,
            } => {
                if needle.is_empty() || *pos >= haystack.len() {
                    return None;
                }
                let nlen = needle.len();
                while *pos + nlen <= haystack.len() {
                    let slice = &haystack[*pos..*pos + nlen];
                    if eq_ignore_ascii_case(slice, needle) {
                        let start = *pos;
                        *pos += nlen;
                        return Some((start, start + nlen));
                    }
                    *pos += 1;
                }
                *pos = haystack.len();
                None
            }
            SearchMatchIter::Regex { iter } => iter.next().map(|m| (m.start(), m.end())),
        }
    }
}

fn eq_ignore_ascii_case(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|(x, y)| x.eq_ignore_ascii_case(y))
}

/// Compile a FILTERS draft preview pattern with the same rules as [`compile_filter`].
/// Empty pattern → `None` (no highlight). Invalid regex falls back to escaped literal.
pub fn compile_filter_draft_pattern(pattern: &str, use_regex: bool) -> Option<SearchPattern> {
    if pattern.is_empty() {
        return None;
    }
    let rule = compile_filter(FilterRule {
        id: String::new(),
        name: None,
        filter_type: FilterType::Include,
        pattern: pattern.to_string(),
        enabled: true,
        use_regex,
        regex: None,
    });
    rule.regex.map(SearchPattern::Regex)
}

pub fn compile_search_pattern(
    query: &str,
    regex_mode: bool,
    case_sensitive: bool,
    whole_word: bool,
) -> Result<SearchPattern, String> {
    if query.is_empty() {
        return Err("empty query".to_string());
    }
    // Fast path: CI ASCII literal without whole-word (historical default).
    if !regex_mode && query.is_ascii() && !case_sensitive && !whole_word {
        return Ok(SearchPattern::AsciiLiteral(query.to_string()));
    }

    let body = if regex_mode {
        query.to_string()
    } else {
        regex::escape(query)
    };
    let pattern = if whole_word {
        format!(r"\b(?:{body})\b")
    } else {
        body
    };

    let mut builder = regex::RegexBuilder::new(&pattern);
    builder.case_insensitive(!case_sensitive);
    builder
        .build()
        .map(|re| SearchPattern::Regex(Arc::new(re)))
        .map_err(|e| e.to_string())
}

pub fn collect_search_matches(
    flat_lines: &[FlatLine],
    pattern: &SearchPattern,
) -> Vec<SearchMatch> {
    let mut out = Vec::new();
    append_search_matches(&mut out, flat_lines, 0, pattern);
    out
}

/// Scan `flat_lines[line_offset..]` and append matches with absolute `line_index`.
pub fn append_search_matches(
    out: &mut Vec<SearchMatch>,
    flat_lines: &[FlatLine],
    line_offset: usize,
    pattern: &SearchPattern,
) {
    for (i, line) in flat_lines.iter().enumerate() {
        let line_index = line_offset + i;
        for (start, end) in pattern.find_iter(&line.raw) {
            out.push(SearchMatch {
                line_index,
                start,
                end,
            });
        }
    }
}

pub fn highlight_search_in_segments(
    segments: &[TextSegment],
    pattern: &SearchPattern,
    active_range: Option<(usize, usize)>,
) -> Vec<TextSegment> {
    let raw: String = segments.iter().map(|s| s.text.as_str()).collect();
    if raw.is_empty() {
        return segments.to_vec();
    }

    let mut overlay = Vec::new();
    let mut last = 0usize;
    for (start, end) in pattern.find_iter(&raw) {
        if start > last {
            overlay.push(TextSegment {
                text: raw[last..start].to_string(),
                style: None,
            });
        }
        let is_active = active_range == Some((start, end));
        let mut style = TextStyle::default();
        if is_active {
            style.search_current = true;
        } else {
            style.search = true;
        }
        overlay.push(TextSegment {
            text: raw[start..end].to_string(),
            style: Some(style),
        });
        last = end;
    }
    if last < raw.len() {
        overlay.push(TextSegment {
            text: raw[last..].to_string(),
            style: None,
        });
    }
    if overlay.is_empty() {
        segments.to_vec()
    } else {
        overlay_styles(segments, &overlay)
    }
}
