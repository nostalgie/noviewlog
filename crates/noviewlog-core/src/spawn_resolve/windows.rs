use std::path::{Path, PathBuf};

#[cfg(windows)]
use super::argv::looks_like_path;
#[cfg(windows)]
use super::wsl::is_wsl_executable;
use super::wsl::{inject_wsl_cd_args, is_unc_path, looks_like_linux_abs, parse_wsl_unc};
#[cfg(windows)]
use super::PreparedSpawn;

/// Normalize a Windows working directory for ConPTY CreateProcess.
/// Strips the `\\?\` extended prefix (breaks some ConPTY/cwd combos), rewrites
/// `\\?\UNC\server\…` back to `\\server\…`, and unifies separators — does **not**
/// canonicalize (that would re-introduce `\\?\` and can fail on mapped drives).
pub fn normalize_windows_cwd(cwd: &str) -> String {
    let trimmed = cwd.trim();
    let without_ext = if let Some(rest) = trimmed
        .strip_prefix(r"\\?\")
        .or_else(|| trimmed.strip_prefix("//?/"))
    {
        let rest_norm = rest.replace('/', "\\");
        // Byte-wise compare: a string slice at a fixed offset panics when the
        // boundary lands inside a multi-byte char (issue #233).
        if rest_norm.len() >= 4 && rest_norm.as_bytes()[..4].eq_ignore_ascii_case(b"UNC\\") {
            format!(r"\\{}", &rest_norm[4..])
        } else {
            rest_norm
        }
    } else {
        trimmed.replace('/', "\\")
    };
    without_ext
}
/// Local drive/folder safe for CreateProcess when UNC or WSL is involved.
pub fn safe_windows_cwd() -> String {
    if let Ok(root) = std::env::var("SystemRoot").or_else(|_| std::env::var("SYSTEMROOT")) {
        let p = PathBuf::from(root).join("System32");
        if p.is_dir() {
            return p.to_string_lossy().into_owned();
        }
    }
    if let Ok(profile) = std::env::var("USERPROFILE") {
        let p = PathBuf::from(profile);
        if p.is_dir() {
            return p.to_string_lossy().into_owned();
        }
    }
    std::env::temp_dir().to_string_lossy().into_owned()
}
#[cfg(windows)]
pub(super) fn resolve_windows(
    program: &str,
    args: Vec<String>,
    cwd: &str,
) -> Result<PreparedSpawn, String> {
    let cwd_norm = normalize_windows_cwd(cwd);
    // For PATH probing, prefer a usable local dir; UNC cwd is not a valid probe base.
    let probe_cwd = if is_unc_path(&cwd_norm) {
        None
    } else {
        Some(cwd_norm.as_str())
    };

    let resolved = find_windows_executable(program, probe_cwd).ok_or_else(|| {
        format!(
            "executable '{program}' not found on PATH (with PATHEXT). \
             Set Command to the full path of the .exe (e.g. C:\\\\Program Files\\\\nodejs\\\\node.exe), \
             or ensure the tool is on the system/user PATH. \
             If you only have a Microsoft Store 'App execution alias' under WindowsApps, \
             install Node from https://nodejs.org instead. \
             Working directory: {cwd_norm}"
        )
    })?;

    if is_windows_apps_stub(&resolved) {
        return Err(format!(
            "refusing to spawn Microsoft Store App execution alias '{resolved}' \
             (exits with STATUS_DLL_INIT_FAILED / 0xC0000142 under ConPTY). \
             Install a real Node.js (nodejs.org / fnm / nvm-windows) or set Command to \
             the full path of node.exe.",
            resolved = resolved.display()
        ));
    }

    let is_wsl = is_wsl_executable(program) || is_wsl_executable(&resolved.to_string_lossy());
    let (args, final_cwd) = finalize_windows_cwd_and_args(is_wsl, args, &cwd_norm)?;

    let (command, args) = if is_batch_file(&resolved) {
        wrap_batch_via_cmd(resolved, args)
    } else {
        (resolved.to_string_lossy().into_owned(), args)
    };

    Ok(PreparedSpawn {
        command,
        args,
        cwd: final_cwd,
    })
}

/// Sanitize Windows cwd for CreateProcess/cmd/WSL; may inject `wsl --cd` / `-d`.
pub fn finalize_windows_cwd_and_args(
    is_wsl: bool,
    args: Vec<String>,
    cwd: &str,
) -> Result<(Vec<String>, String), String> {
    // Linux absolute paths are only for `wsl --cd` — do not Windows-normalize
    // (`/` → `\`) or CreateProcess will get a bogus path.
    if is_wsl && looks_like_linux_abs(cwd) {
        let linux = cwd.trim().to_string();
        let args = inject_wsl_cd_args(args, None, Some(&linux));
        return Ok((args, safe_windows_cwd()));
    }

    let cwd = normalize_windows_cwd(cwd);
    let unc = is_unc_path(&cwd);

    if is_wsl {
        if let Some((distro, linux)) = parse_wsl_unc(&cwd) {
            let args = inject_wsl_cd_args(args, Some(&distro), Some(&linux));
            return Ok((args, safe_windows_cwd()));
        }
        if unc {
            return Err(format!(
                "UNC working directory is not usable as a Windows process cwd \
                 (CreateProcess/cmd.exe reject UNC). For WSL, either:\n\
                 • Browse to \\\\wsl$\\Distro\\… (auto-converted to wsl -d Distro --cd /linux/path), or\n\
                 • Leave Working directory empty and put `--cd /linux/path` in Args, or\n\
                 • Use a local drive path (e.g. C:\\Users\\…). \
                 Got: {cwd}"
            ));
        }
        // Local Windows path (or `.`): never inherit a UNC parent — use a known-local dir.
        // Project dir for Linux is expected via Args `--cd` when cwd is empty/local-only.
        if cwd == "." || cwd.is_empty() {
            return Ok((args, safe_windows_cwd()));
        }
        // Keep a real local drive cwd (harmless for wsl.exe); still avoid relative `.`.
        if looks_like_windows_drive_path(&cwd) {
            return Ok((args, cwd));
        }
        return Ok((args, safe_windows_cwd()));
    }

    if unc {
        return Err(format!(
            "UNC working directory is not supported for CreateProcess/cmd.exe \
             (\"CMD does not support UNC paths as current directories\"). \
             Use a local drive path (C:\\…, Z:\\…), or for Linux projects set Command to \
             wsl.exe and either Browse \\\\wsl$\\Distro\\… or leave cwd empty and use \
             `--cd /linux/path` in Args. Got: {cwd}"
        ));
    }

    // Relative `.` still resolves against the parent process cwd — which may be UNC
    // if NoViewLog itself was started from \\\\wsl$\\…. Prefer a known-local folder.
    if cwd.is_empty() || cwd == "." {
        return Ok((args, safe_windows_cwd()));
    }

    Ok((args, cwd))
}

pub(super) fn looks_like_windows_drive_path(path: &str) -> bool {
    let b = path.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

/// Human-readable spawn line for status / errors (`exe arg1 arg2` + optional cwd).
pub fn format_spawn_cmdline(command: &str, args: &[String], cwd: Option<&str>) -> String {
    let mut line = command.to_string();
    for a in args {
        line.push(' ');
        if a.is_empty() || a.contains(char::is_whitespace) {
            line.push('"');
            line.push_str(a);
            line.push('"');
        } else {
            line.push_str(a);
        }
    }
    if let Some(c) = cwd.map(str::trim).filter(|s| !s.is_empty()) {
        line.push_str("  [cwd: ");
        line.push_str(c);
        line.push(']');
    }
    line
}

#[cfg(windows)]
fn is_batch_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let e = e.to_ascii_lowercase();
            e == "cmd" || e == "bat"
        })
        .unwrap_or(false)
}

#[cfg(windows)]
fn wrap_batch_via_cmd(script: PathBuf, args: Vec<String>) -> (String, Vec<String>) {
    let comspec = std::env::var_os("COMSPEC")
        .or_else(|| std::env::var_os("ComSpec"))
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows\System32\cmd.exe"));

    // `call` is required so cmd returns the script's exit code; /D skips AutoRun hooks
    // that sometimes break headless ConPTY sessions.
    let mut line = format!("call {}", quote_win_arg(&script.to_string_lossy()));
    for arg in &args {
        line.push(' ');
        line.push_str(&quote_win_arg(arg));
    }

    (
        comspec.to_string_lossy().into_owned(),
        vec!["/D".to_string(), "/C".to_string(), line],
    )
}

/// Quote one argument for cmd.exe parsing (double internal quotes).
#[cfg(windows)]
fn quote_win_arg(arg: &str) -> String {
    let mut out = String::from("\"");
    for ch in arg.chars() {
        if ch == '"' {
            out.push('"');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

#[cfg(windows)]
pub(super) fn find_windows_executable(program: &str, cwd: Option<&str>) -> Option<PathBuf> {
    let program_path = Path::new(program);
    let extensions = pathext_list();

    if program_path.components().count() > 1 || looks_like_path(program) {
        return resolve_existing_candidate(program_path, cwd, &extensions)
            .filter(|p| !is_unusable_stub(p));
    }

    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Some(dir) = cwd.map(Path::new).filter(|p| p.is_dir()) {
        if let Some(found) = probe_dir(dir, program, &extensions) {
            push_unique(&mut candidates, found);
        }
    }

    for dir in std::env::split_paths(&windows_path_env()) {
        if let Some(found) = probe_dir(&dir, program, &extensions) {
            push_unique(&mut candidates, found);
        }
    }

    for dir in well_known_bin_dirs() {
        if let Some(found) = probe_dir(&dir, program, &extensions) {
            push_unique(&mut candidates, found);
        }
    }

    pick_best_candidate(candidates)
}

#[cfg(windows)]
fn push_unique(candidates: &mut Vec<PathBuf>, path: PathBuf) {
    if !candidates.iter().any(|c| c == &path) {
        candidates.push(path);
    }
}

/// Prefer real installs over Microsoft Store App Execution Alias stubs in WindowsApps.
pub fn pick_best_candidate(candidates: Vec<PathBuf>) -> Option<PathBuf> {
    let mut best: Option<(PathBuf, i32)> = None;
    for candidate in candidates {
        if is_unusable_stub(&candidate) {
            continue;
        }
        let score = candidate_score(&candidate);
        match &best {
            Some((_, best_score)) if score <= *best_score => {}
            _ => best = Some((candidate, score)),
        }
    }
    best.map(|(p, _)| p)
}

/// Score a resolved executable path. Higher is better.
pub fn candidate_score(path: &Path) -> i32 {
    let s = path.to_string_lossy().to_ascii_lowercase();
    let mut score = 0;
    if is_windows_apps_path(&s) {
        score -= 1000;
    }
    if s.contains(r"\nodejs\")
        || s.contains(r"\fnm\")
        || s.contains(r"\volta\")
        || s.contains(r"\nvm\")
        || s.contains(r"\nvs\")
    {
        score += 50;
    }
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("exe") => score += 10,
        Some(ext) if ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat") => {
            score += 5;
        }
        _ => {}
    }
    score
}

fn is_windows_apps_path(path_lower: &str) -> bool {
    path_lower.contains(r"\windowsapps\") || path_lower.contains("/windowsapps/")
}

fn is_windows_apps_stub(path: &Path) -> bool {
    is_windows_apps_path(&path.to_string_lossy().to_ascii_lowercase())
}

fn is_unusable_stub(path: &Path) -> bool {
    if is_windows_apps_stub(path) {
        return true;
    }
    // App-execution alias stubs are often 0 bytes even outside WindowsApps naming.
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.is_file() && meta.len() == 0 {
            return true;
        }
    }
    false
}

/// Merge process PATH with Machine + User PATH from the registry (same idea as
/// portable-pty's CommandBuilder base env).
#[cfg(windows)]
fn windows_path_env() -> std::ffi::OsString {
    // Registry + PATH merge is the expensive part of every spawn (issue #59):
    // cache it for the process lifetime. PATH changes mid-session are picked
    // up by tools launched from shells; a stale cache only affects our own
    // executable resolution, which is what the shell inherits anyway.
    use std::sync::OnceLock;
    static CACHE: OnceLock<std::ffi::OsString> = OnceLock::new();
    CACHE.get_or_init(windows_path_env_uncached).clone()
}

#[cfg(windows)]
fn windows_path_env_uncached() -> std::ffi::OsString {
    use std::ffi::OsString;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    use winreg::RegKey;

    fn reg_path(root: winreg::HKEY, subkey: &str) -> Option<OsString> {
        let key = RegKey::predef(root).open_subkey(subkey).ok()?;
        // Prefer expanded values when Windows stored REG_EXPAND_SZ.
        key.get_value::<String, _>("Path")
            .or_else(|_| key.get_value::<String, _>("PATH"))
            .ok()
            .map(OsString::from)
    }

    let mut parts: Vec<OsString> = Vec::new();
    if let Some(p) = std::env::var_os("PATH") {
        if !p.is_empty() {
            parts.push(p);
        }
    }
    if let Some(p) = reg_path(
        HKEY_LOCAL_MACHINE,
        r"System\CurrentControlSet\Control\Session Manager\Environment",
    ) {
        parts.push(p);
    }
    if let Some(p) = reg_path(HKEY_CURRENT_USER, "Environment") {
        parts.push(p);
    }

    let mut merged = OsString::new();
    for (i, part) in parts.into_iter().enumerate() {
        if i > 0 {
            merged.push(";");
        }
        merged.push(part);
    }
    merged
}

#[cfg(windows)]
fn well_known_bin_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(pf) = std::env::var_os("ProgramFiles") {
        dirs.push(PathBuf::from(pf).join("nodejs"));
    }
    if let Some(pf86) = std::env::var_os("ProgramFiles(x86)") {
        dirs.push(PathBuf::from(pf86).join("nodejs"));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        dirs.push(PathBuf::from(&local).join(r"Programs\nodejs"));
        // nvm-windows symlink location
        dirs.push(PathBuf::from(&local).join("nvm"));
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        dirs.push(PathBuf::from(appdata).join(r"npm"));
    }
    dirs
}

#[cfg(windows)]
fn resolve_existing_candidate(
    program_path: &Path,
    cwd: Option<&str>,
    extensions: &[String],
) -> Option<PathBuf> {
    let candidates: Vec<PathBuf> = if program_path.is_absolute() {
        vec![program_path.to_path_buf()]
    } else if let Some(dir) = cwd.map(Path::new) {
        vec![dir.join(program_path)]
    } else {
        vec![program_path.to_path_buf()]
    };

    for base in candidates {
        if base.is_file() {
            return Some(base);
        }
        // CreateProcess appends PATHEXT extensions to the name as given, so a
        // dotted name (`my.tool`) resolves via `my.tool.exe` — probe that
        // first (#255). Also keep the legacy with_extension probe so the name
        // still resolves via an extension-replaced variant (`my.exe`).
        for ext in extensions {
            let ext_body = ext.trim_start_matches('.');
            let mut os = base.clone().into_os_string();
            os.push(".");
            os.push(ext_body);
            let appended = PathBuf::from(os);
            if appended.is_file() {
                return Some(appended);
            }
            let with_ext = base.with_extension(ext_body);
            if with_ext.is_file() {
                return Some(with_ext);
            }
        }
    }
    None
}

#[cfg(windows)]
fn probe_dir(dir: &Path, program: &str, extensions: &[String]) -> Option<PathBuf> {
    let exact = dir.join(program);
    if exact.is_file() {
        // Prefer PE images over batch shims when both exist as the bare name.
        if !is_batch_file(&exact) {
            return Some(exact);
        }
        // Fall through to try .exe first via extensions, then accept batch.
    }

    let mut batch: Option<PathBuf> = if exact.is_file() && is_batch_file(&exact) {
        Some(exact)
    } else {
        None
    };

    for ext in extensions {
        let ext_body = ext.trim_start_matches('.');
        // Append, not with_extension: that replaces everything after the last
        // dot, so a dotted program name ("my.tool") probed as "my.exe"
        // instead of "my.tool.exe".
        let candidate = dir.join(format!("{program}.{ext_body}"));
        if !candidate.is_file() {
            continue;
        }
        if is_batch_file(&candidate) {
            if batch.is_none() {
                batch = Some(candidate);
            }
            continue;
        }
        return Some(candidate);
    }
    batch
}

#[cfg(windows)]
fn pathext_list() -> Vec<String> {
    let raw = std::env::var_os("PATHEXT")
        .unwrap_or_else(|| std::ffi::OsString::from(".COM;.EXE;.BAT;.CMD"));
    std::env::split_paths(&raw)
        .filter_map(|p| p.into_os_string().into_string().ok())
        .map(|s| s.to_ascii_lowercase())
        .collect()
}

#[cfg(all(windows, test))]
mod tests {
    use super::*;

    #[test]
    fn dotted_full_path_resolves_appended_ext() {
        // Issue #255: `C:\...\my.tool` must probe `my.tool.exe` (which
        // CreateProcess would find), not only `my.exe`.
        let dir = std::env::temp_dir().join(format!("nvl-resolve-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let base = dir.join("my.tool");
        let appended = dir.join("my.tool.exe");
        let replaced = dir.join("my.exe");
        std::fs::write(&appended, b"MZ").unwrap();

        let resolved = resolve_existing_candidate(&base, None, &["exe".to_string()])
            .expect("must resolve via appended extension");
        assert_eq!(resolved, appended, "appended form must win");

        // Both orderings: with only `my.exe` present, the with_extension
        // probe still resolves.
        std::fs::remove_file(&appended).unwrap();
        std::fs::write(&replaced, b"MZ").unwrap();
        let resolved = resolve_existing_candidate(&base, None, &["exe".to_string()])
            .expect("must resolve via with_extension");
        assert_eq!(resolved, replaced);

        std::fs::remove_file(&replaced).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }
}
