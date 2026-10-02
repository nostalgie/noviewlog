use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
}

/// Per-view severity reading mode (orthogonal to include/exclude FilterRules).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeverityFilter {
    #[default]
    All,
    Error,
    Warn,
    Info,
    Debug,
    Unleveled,
}

impl SeverityFilter {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Unleveled => "unleveled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "all" => Some(Self::All),
            "error" | "errors" => Some(Self::Error),
            "warn" | "warning" | "warnings" => Some(Self::Warn),
            "info" => Some(Self::Info),
            "debug" => Some(Self::Debug),
            "unleveled" | "none" | "normal" => Some(Self::Unleveled),
            _ => None,
        }
    }

    /// Whether a Record with this detected level passes the mode.
    pub fn allows(self, level: Option<LogLevel>) -> bool {
        match self {
            Self::All => true,
            Self::Error => level == Some(LogLevel::Error),
            Self::Warn => level == Some(LogLevel::Warn),
            Self::Info => level == Some(LogLevel::Info),
            Self::Debug => level == Some(LogLevel::Debug),
            Self::Unleveled => level.is_none(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct LogRecord {
    pub id: u64,
    pub lines: Vec<String>,
    pub text: String,
    pub received_at: DateTime<Utc>,
    pub level: Option<LogLevel>,
    /// Live spinner / `\r` overwrite — safe to replace in place; cleared when a real line arrives.
    pub overwrite: bool,
}

impl LogRecord {
    /// Level stored at ingest, or classified from `text` (PTY firehose leaves `level` unset).
    pub fn effective_level(&self) -> Option<LogLevel> {
        self.level.or_else(|| detect_level(&self.text))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FilterType {
    Include,
    Exclude,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FilterRule {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub filter_type: FilterType,
    pub pattern: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// When true, `pattern` is a regex; when false, literal (search-equivalent CI).
    /// Omitted in legacy config → true (historical regex-first filters).
    #[serde(default = "default_true")]
    pub use_regex: bool,
    #[serde(skip)]
    pub regex: Option<Arc<Regex>>,
}

#[derive(Clone, Debug)]
pub struct LogFormat {
    pub id: String,
    pub name: String,
    pub start: String,
    pub continuation: Vec<String>,
    pub start_regex: Option<Arc<Regex>>,
    pub continuation_regexes: Vec<Arc<Regex>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FormatPreset {
    pub start: String,
    #[serde(default)]
    pub continuation: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct PresetConfig {
    #[serde(default)]
    pub filters: Vec<FilterRule>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TabConfig {
    pub name: String,
    #[serde(default)]
    pub filters: Vec<FilterRule>,
    #[serde(default)]
    pub search_query: String,
    #[serde(default)]
    pub search_regex: bool,
    #[serde(default)]
    pub search_case_sensitive: bool,
    #[serde(default)]
    pub search_whole_word: bool,
    #[serde(default = "default_true")]
    pub auto_follow: bool,
    #[serde(default = "default_true")]
    pub wrap_lines: bool,
    /// Severity reading mode, persisted with the workspace (issue #109).
    #[serde(default)]
    pub severity: SeverityFilter,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct WorkspaceConfig {
    #[serde(default)]
    pub tabs: Vec<TabConfig>,
    #[serde(default)]
    pub active_tab: usize,
}

/// A launchable command within a project (e.g. "backend", "redis", "wsl-worker").
/// Each program stores a full, self-contained launch definition — command, args,
/// and cwd are independent per program (paths stored as-is for cross-platform).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProgramConfig {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub launch: LaunchConfig,
    #[serde(default)]
    pub workspace: WorkspaceConfig,
}

/// Groups related programs — abstract container; does **not** imply a shared cwd.
/// Optional metadata (`default_cwd`, `path_hint`) is for UI/organization only;
/// each `ProgramConfig.launch` is the authoritative per-program launch spec.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectConfig {
    pub id: String,
    pub name: String,
    /// Optional hint for new programs / display (not applied automatically).
    #[serde(default)]
    pub default_cwd: Option<String>,
    /// Optional free-form path or notes (e.g. repo root); not used at runtime.
    #[serde(default)]
    pub path_hint: Option<String>,
    #[serde(default)]
    pub programs: Vec<ProgramConfig>,
    #[serde(default)]
    pub active_program: usize,
}

/// Top-level store persisted as `~/.config/noviewlog/projects.yaml`
/// (Windows: `%USERPROFILE%\.config\noviewlog\projects.yaml`).
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ProjectsStore {
    #[serde(default)]
    pub projects: Vec<ProjectConfig>,
    #[serde(default)]
    pub active_project: usize,
}

/// Unique id suffix: millisecond timestamp plus a monotonic in-process counter
/// — ids created in the same millisecond stay distinct (issue #76).
pub fn unique_time_suffix() -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| format!("{}-{n}", d.as_millis()))
        .unwrap_or_else(|_| n.to_string())
}

pub fn next_project_id(_projects: &[ProjectConfig]) -> String {
    format!("project-{}", unique_time_suffix())
}

pub fn next_program_id(_programs: &[ProgramConfig]) -> String {
    format!("program-{}", unique_time_suffix())
}

pub fn program_display_name(launch: &LaunchConfig) -> String {
    if let Some(file) = &launch.log_file {
        return std::path::Path::new(file)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(file)
            .to_string();
    }
    if let Some(cmd) = &launch.command {
        let prefix = if launch.wsl { "wsl: " } else { "" };
        if launch.args.is_empty() {
            return format!("{prefix}{cmd}");
        }
        let joined = launch
            .args
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        let suffix = if launch.args.len() > 3 { " …" } else { "" };
        return format!("{prefix}{cmd} {joined}{suffix}");
    }
    "program".to_string()
}

/// Preferred shell for empty-terminal spawns on Windows (issue #68).
/// `auto` prefers PowerShell 7 (`pwsh`) when found on PATH, else Windows
/// PowerShell 5.1. Ignored on non-Windows hosts (`$SHELL` is used there).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShellPreference {
    #[default]
    Auto,
    Pwsh,
    Powershell,
    Cmd,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default = "default_format")]
    pub default_format: String,
    #[serde(default = "default_preset")]
    pub default_preset: String,
    /// Max retained log records (≈ lines) in the live scrollback buffer.
    #[serde(default = "default_max_scrollback_lines")]
    pub max_scrollback_lines: usize,
    /// Bitmap viewport fontdue size in points (not UI chrome scale).
    #[serde(default = "default_viewport_font_size")]
    pub viewport_font_size: f32,
    /// Sidebar TERMINALS section expanded.
    #[serde(default = "default_true")]
    pub terminals_section_expanded: bool,
    /// Sidebar FILES section expanded.
    #[serde(default = "default_true")]
    pub files_section_expanded: bool,
    /// Interactive shell for empty terminals (Windows; see [`ShellPreference`]).
    #[serde(default)]
    pub shell: ShellPreference,
    #[serde(default)]
    pub formats: std::collections::HashMap<String, FormatPreset>,
    #[serde(default)]
    pub presets: std::collections::HashMap<String, PresetConfig>,
    #[serde(default)]
    pub workspaces: std::collections::HashMap<String, WorkspaceConfig>,
    /// SSH connection profiles for the TUI host (system `ssh` client in the
    /// engine PTY). Targets only — no secrets; auth stays with ssh/agent.
    /// Ignored by the GUI host.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tui_ssh_profiles: Vec<SshProfile>,
}

/// One named SSH connection of the TUI host (`terminals/tui-ssh`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshProfile {
    pub name: String,
    /// Forwarded to `ssh` verbatim as a single argv element
    /// (`user@host`, bare host, or an `ssh_config` alias).
    pub target: String,
    /// `0` = omit `-p` (ssh defaults / ssh_config decide).
    #[serde(default)]
    pub port: u16,
    /// Extra ssh arguments, whitespace-split at use (e.g. `-J bastion`).
    #[serde(default)]
    pub extra_args: String,
}

pub const DEFAULT_MAX_SCROLLBACK_LINES: usize = 10_000;
pub const MIN_MAX_SCROLLBACK_LINES: usize = 100;
pub const MAX_MAX_SCROLLBACK_LINES: usize = 30_000;

pub const DEFAULT_VIEWPORT_FONT_SIZE: f32 = 13.0;
pub const MIN_VIEWPORT_FONT_SIZE: f32 = 8.0;
pub const MAX_VIEWPORT_FONT_SIZE: f32 = 32.0;

pub fn clamp_max_scrollback_lines(value: usize) -> usize {
    value.clamp(MIN_MAX_SCROLLBACK_LINES, MAX_MAX_SCROLLBACK_LINES)
}

pub fn clamp_viewport_font_size(value: f32) -> f32 {
    if !value.is_finite() {
        return DEFAULT_VIEWPORT_FONT_SIZE;
    }
    value.clamp(MIN_VIEWPORT_FONT_SIZE, MAX_VIEWPORT_FONT_SIZE)
}

#[derive(Clone, Debug, Default)]
pub struct TextSegment {
    pub text: String,
    pub style: Option<TextStyle>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextStyle {
    pub fg: Option<(u8, u8, u8)>,
    pub bg: Option<(u8, u8, u8)>,
    pub bold: bool,
    pub dim: bool,
    pub underline: bool,
    pub search: bool,
    pub search_current: bool,
    pub selected: bool,
    /// OSC 8 hyperlink target for this run (`None` = plain text).
    pub link: Option<Arc<str>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchMatch {
    pub line_index: usize,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug)]
pub struct FlatLine {
    pub record_id: u64,
    pub line_index: usize,
    pub segments: Vec<TextSegment>,
    pub raw: String,
    /// Detected level for Viewport severity cue; set only on the Record's first physical line.
    pub level: Option<LogLevel>,
    /// Record has ≥2 physical lines (show disclosure cue on the first painted row).
    pub collapsible: bool,
    /// This flat row is a collapsed preview (only the first physical line is shown).
    pub collapsed: bool,
    /// When `collapsed`, how many additional physical lines are hidden.
    pub hidden_line_count: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LaunchConfig {
    pub command: Option<String>,
    pub args: Vec<String>,
    pub preset: Option<String>,
    pub config_path: Option<String>,
    /// Open an existing log file instead of wrapping a process.
    pub log_file: Option<String>,
    /// Working directory for this program's process (optional, per-program).
    /// Stored verbatim — Windows paths (e.g. `c:\bin`) are preserved for cross-platform configs.
    /// In WSL mode (`wsl: true`) this is a **Linux** path passed to `wsl --cd`.
    pub cwd: Option<String>,
    /// When true, spawn via `wsl.exe` (Windows-only). Command/args run inside the distro;
    /// `cwd` is the Linux working directory (`--cd`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub wsl: bool,
    /// Optional WSL distribution (`wsl -d`). Empty / omitted = default distro.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wsl_distro: Option<String>,
}

impl LaunchConfig {
    pub fn has_process_launch(&self) -> bool {
        self.command.is_some() || self.log_file.is_some()
    }
}

pub fn default_true() -> bool {
    true
}

fn is_false(v: &bool) -> bool {
    !*v
}

fn default_format() -> String {
    "node-default".to_string()
}

fn default_preset() -> String {
    "node-dev".to_string()
}

fn default_max_scrollback_lines() -> usize {
    DEFAULT_MAX_SCROLLBACK_LINES
}

fn default_viewport_font_size() -> f32 {
    DEFAULT_VIEWPORT_FONT_SIZE
}

pub fn compile_regex(pattern: &str) -> Arc<Regex> {
    match Regex::new(pattern) {
        Ok(re) => Arc::new(re),
        Err(_) => {
            let escaped = regex::escape(pattern);
            Arc::new(Regex::new(&escaped).expect("escaped pattern is valid"))
        }
    }
}

/// Compile a regex-mode pattern, surfacing invalid patterns instead of
/// silently degrading to a literal match (issue #75). The rule still works
/// (escaped-literal fallback) but the user gets a visible notice.
pub fn compile_regex_checked(pattern: &str) -> (Arc<Regex>, Option<String>) {
    match Regex::new(pattern) {
        Ok(re) => (Arc::new(re), None),
        Err(err) => (
            compile_regex(pattern),
            Some(format!(
                "Invalid regex ({err}); filter treats the pattern as literal text"
            )),
        ),
    }
}

/// [`compile_filter`] with invalid-regex feedback: the compiled rule plus a
/// status notice when the regex fell back to a literal.
pub fn compile_filter_checked(mut rule: FilterRule) -> (FilterRule, Option<String>) {
    if !rule.use_regex {
        return (compile_filter(rule), None);
    }
    let (re, notice) = compile_regex_checked(&rule.pattern);
    rule.regex = Some(re);
    (rule, notice)
}

pub fn compile_filter(mut rule: FilterRule) -> FilterRule {
    // Literal mode matches search's case-insensitive semantics; regex mode uses
    // Regex::new with the historical escape fallback on invalid patterns.
    rule.regex = Some(if rule.use_regex {
        compile_regex(&rule.pattern)
    } else {
        let escaped = regex::escape(&rule.pattern);
        Arc::new(Regex::new(&format!("(?i){escaped}")).expect("escaped literal pattern is valid"))
    });
    rule
}

pub fn detect_level(text: &str) -> Option<LogLevel> {
    use std::sync::OnceLock;
    // Compile once; this runs per committed + per live line, so re-compiling
    // on every call is a severe hot-path cost.
    static LEVEL_RES: OnceLock<[Regex; 4]> = OnceLock::new();
    let [error, warn, debug, info] = LEVEL_RES.get_or_init(|| {
        [
            Regex::new(r"(?i)\b(error|exception|fatal)\b").unwrap(),
            Regex::new(r"(?i)\b(warn|warning)\b").unwrap(),
            Regex::new(r"(?i)\b(debug|trace)\b").unwrap(),
            Regex::new(r"(?i)\binfo\b").unwrap(),
        ]
    });

    if error.is_match(text) {
        Some(LogLevel::Error)
    } else if warn.is_match(text) {
        Some(LogLevel::Warn)
    } else if debug.is_match(text) {
        Some(LogLevel::Debug)
    } else if info.is_match(text) {
        Some(LogLevel::Info)
    } else {
        None
    }
}

pub fn next_filter_id(_filters: &[FilterRule], kind: &str) -> String {
    // Same-millisecond re-adds after a remove collided on len()+millis;
    // unique_time_suffix keeps ids distinct (issue #113, like #76).
    format!("{kind}-{}", unique_time_suffix())
}

pub fn style_from_name(style: &str) -> TextStyle {
    let mut out = TextStyle::default();
    for part in style.split('.') {
        match part.trim() {
            "red" => out.fg = Some((248, 81, 73)),
            "green" => out.fg = Some((63, 185, 80)),
            "yellow" => out.fg = Some((210, 153, 34)),
            "blue" => out.fg = Some((88, 166, 255)),
            "magenta" => out.fg = Some((210, 96, 230)),
            "cyan" => out.fg = Some((57, 197, 207)),
            "white" => out.fg = Some((230, 237, 243)),
            "gray" => out.fg = Some((139, 148, 158)),
            "bold" => out.bold = true,
            "dim" => {
                out.dim = true;
                if out.fg.is_none() {
                    out.fg = Some((139, 148, 158));
                }
            }
            "underline" => out.underline = true,
            _ => {}
        }
    }
    out
}
