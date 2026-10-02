use std::path::PathBuf;

use super::argv::normalize_command_args;
use super::windows::{looks_like_windows_drive_path, normalize_windows_cwd};

/// Preferred `wsl.exe` path for CreateProcess (System32). Falls back to bare `wsl`
/// when SystemRoot is unset (unit tests / odd environments).
pub fn windows_wsl_exe() -> String {
    if let Ok(root) = std::env::var("SystemRoot").or_else(|_| std::env::var("SYSTEMROOT")) {
        let p = PathBuf::from(root).join("System32").join("wsl.exe");
        return p.to_string_lossy().into_owned();
    }
    // Stable default on real Windows; prepare_spawn still resolves via PATH if missing.
    r"C:\Windows\System32\wsl.exe".to_string()
}

/// Normalize a WSL-mode working directory into a Linux `--cd` path.
/// Accepts `/home/…` or `\\wsl$\Distro\…` / `\\wsl.localhost\Distro\…` (and single-slash variants).
/// Returns `(linux_path, distro_hint_from_unc)`.
pub fn normalize_wsl_linux_cwd(cwd: &str) -> Result<(String, Option<String>), String> {
    let t = cwd.trim();
    if t.is_empty() {
        return Err("WSL working directory is empty".to_string());
    }
    if looks_like_linux_abs(t) {
        // Mistaken `/wsl.localhost/Distro/home/…` (URI-style) → treat as UNC-ish.
        let lower = t.to_ascii_lowercase();
        if lower.starts_with("/wsl.localhost/") || lower.starts_with("/wsl$/") {
            let as_unc = format!(r"\\{}", t.trim_start_matches('/').replace('/', "\\"));
            if let Some((distro, linux)) = parse_wsl_unc(&as_unc) {
                return Ok((linux, Some(distro)));
            }
        }
        return Ok((t.to_string(), None));
    }
    if let Some((distro, linux)) = parse_wsl_unc(t) {
        return Ok((linux, Some(distro)));
    }
    if is_unc_path(t) {
        return Err(format!(
            "UNC working directory is not a Linux path for wsl --cd. \
             Use /home/… or \\\\wsl$\\\\Distro\\\\…. Got: {t}"
        ));
    }
    if looks_like_windows_drive_path(t) {
        return Err(format!(
            "WSL working directory must be a Linux path (e.g. /home/user/proj), \
             not a Windows drive path. Got: {t}"
        ));
    }
    Err(format!(
        "WSL working directory must be a Linux path (e.g. /home/user/proj). Got: {t}"
    ))
}

/// Build `wsl` argv for dedicated WSL launch mode (pure; tested on all platforms).
///
/// Result:
/// `("<wsl.exe>", [--shell-type, login, -d Distro?, --cd /linux/path?, --, command, ...args])`.
/// Callers must use a local Windows cwd (never UNC / never the Linux path).
pub fn build_wsl_argv(
    linux_command: &str,
    linux_args: &[String],
    linux_cwd: Option<&str>,
    distro: Option<&str>,
) -> Result<(String, Vec<String>), String> {
    let (exe, mut rest) = normalize_command_args(linux_command, linux_args.to_vec());
    if exe.is_empty() {
        return Err("WSL command is empty".to_string());
    }
    if is_wsl_executable(&exe) {
        return Err(
            "WSL mode Command should be the Linux executable (e.g. npm), not wsl itself"
                .to_string(),
        );
    }

    let mut distro = distro
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let mut cd: Option<String> = None;
    if let Some(raw) = linux_cwd.map(str::trim).filter(|s| !s.is_empty()) {
        let (linux, from_unc) = normalize_wsl_linux_cwd(raw)?;
        if distro.is_none() {
            distro = from_unc;
        }
        cd = Some(linux);
    }

    let mut args = Vec::new();
    // Login shell so Linux nvm/fnm/pnpm PATH wins over Windows interop appendWindowsPath.
    args.push("--shell-type".to_string());
    args.push("login".to_string());
    if let Some(d) = distro {
        args.push("-d".to_string());
        args.push(d);
    }
    if let Some(cd) = cd {
        args.push("--cd".to_string());
        args.push(cd);
    }
    args.push("--".to_string());
    args.push(exe);
    args.append(&mut rest);
    Ok((windows_wsl_exe(), args))
}
/// True for UNC paths: `\\server\share`, `//server/share`, `\\?\UNC\…`, `\\wsl$\…`.
pub fn is_unc_path(path: &str) -> bool {
    let n = normalize_windows_cwd(path);
    if n.len() >= 2 && n.as_bytes()[0] == b'\\' && n.as_bytes()[1] == b'\\' {
        return true;
    }
    // Defensive: if normalize somehow left `UNC\server\…` without leading `\\`.
    let lower = n.to_ascii_lowercase();
    lower.starts_with(r"unc\") || lower.starts_with("unc/")
}

/// Parse `\\wsl$\Distro\rest` / `\\wsl.localhost\Distro\rest` into `(distro, /linux/path)`.
/// Also accepts a single leading `\wsl.localhost\…` (sometimes shown in UI / copied paths).
pub fn parse_wsl_unc(path: &str) -> Option<(String, String)> {
    let mut n = normalize_windows_cwd(path);
    // `\wsl.localhost\Distro\…` (one slash) → `\\wsl.localhost\Distro\…`
    if n.len() >= 2 && n.as_bytes()[0] == b'\\' && n.as_bytes()[1] != b'\\' {
        let lower = n.to_ascii_lowercase();
        if lower.starts_with(r"\wsl$") || lower.starts_with(r"\wsl.localhost") {
            n = format!(r"\{n}");
        }
    }
    let rest = n.strip_prefix(r"\\")?;
    let (host, after_host) = rest.split_once('\\')?;
    let host_l = host.to_ascii_lowercase();
    if host_l != "wsl$" && host_l != "wsl.localhost" {
        return None;
    }
    let (distro, linux_rel) = match after_host.split_once('\\') {
        // Trim the distro segment: a trailing space in a copied UNC path
        // would otherwise be sent to WSL as part of `-d` and rejected (#255).
        Some((d, r)) => (d.trim(), r),
        None => (after_host.trim(), ""),
    };
    if distro.is_empty() {
        return None;
    }
    let linux = if linux_rel.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", linux_rel.replace('\\', "/"))
    };
    Some((distro.to_string(), linux))
}
/// True when the executable basename is `wsl` / `wsl.exe`.
pub fn is_wsl_executable(program: &str) -> bool {
    // Split on both separators so `C:\…\wsl.exe` works when tests/host are Unix.
    let name = program
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program);
    name.eq_ignore_ascii_case("wsl") || name.eq_ignore_ascii_case("wsl.exe")
}
pub(super) fn looks_like_linux_abs(path: &str) -> bool {
    let t = path.trim();
    t.starts_with('/') && !t.starts_with("//")
}

fn wsl_args_have_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

fn wsl_args_have_distro(args: &[String]) -> bool {
    args.iter()
        .any(|a| a == "-d" || a == "-D" || a == "--distribution")
}

/// Prepend `-d` / `--cd` when missing so we do not clobber explicit user args.
pub fn inject_wsl_cd_args(
    args: Vec<String>,
    distro: Option<&str>,
    linux_cd: Option<&str>,
) -> Vec<String> {
    let mut prefix = Vec::new();
    if let Some(d) = distro {
        if !d.is_empty() && !wsl_args_have_distro(&args) {
            prefix.push("-d".to_string());
            prefix.push(d.to_string());
        }
    }
    if let Some(cd) = linux_cd {
        if !cd.is_empty() && !wsl_args_have_flag(&args, "--cd") {
            prefix.push("--cd".to_string());
            prefix.push(cd.to_string());
        }
    }
    prefix.extend(args);
    prefix
}
