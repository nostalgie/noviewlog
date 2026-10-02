//! Resolve spawn argv before handing off to portable-pty.
//!
//! On Windows, CreateProcessW (used by portable-pty) does **not** apply PATHEXT when
//! `lpApplicationName` is a bare name like `node`. If PATH lookup also misses, the OS
//! error is ERROR_FILE_NOT_FOUND (localized OS message, e.g. "file not found"). Node/npm installers
//! often leave `node.exe` and `npm.cmd` on PATH — we must resolve `.exe` ourselves and
//! wrap `.cmd`/`.bat` via `cmd.exe /d /c`.
//!
//! Also: Microsoft Store "App execution aliases" put 0-byte `node.exe` stubs under
//! `%LOCALAPPDATA%\Microsoft\WindowsApps`. Spawning those under ConPTY exits with
//! `STATUS_DLL_INIT_FAILED` (0xC0000142 / -1073741502). Prefer real installs.
//!
//! UNC working directories (`\\server\share`, `\\wsl$\…`) break CreateProcess/ConPTY and
//! `cmd.exe` ("CMD does not support UNC paths as current directories"). For `wsl.exe` we
//! convert `\\wsl$\Distro\path` into `-d Distro --cd /path` and always use a local
//! Windows cwd for the `wsl` process itself.
//!
//! WSL mode must never CreateProcess Windows `pnpm`/`npm`/`node` against a UNC mount of
//! the distro. A bare `wsl -- pnpm` without a login shell often misses Linux nvm/fnm PATH
//! and falls through to Windows pnpm via WSL interop — which then sees
//! `\\wsl.localhost\…` and writes to Windows directories. We pass `--shell-type login`
//! so Linux tools win, keep Linux cwd only via `--cd`, and pin `wsl.exe` under System32.

mod argv;
mod shell;
mod windows;
mod wsl;

pub use argv::normalize_command_args;
pub use shell::prewarm_shell_probe;
pub use windows::{
    candidate_score, finalize_windows_cwd_and_args, format_spawn_cmdline, normalize_windows_cwd,
    pick_best_candidate, safe_windows_cwd,
};
pub use wsl::{
    build_wsl_argv, inject_wsl_cd_args, is_unc_path, is_wsl_executable, normalize_wsl_linux_cwd,
    parse_wsl_unc, windows_wsl_exe,
};

use shell::default_interactive_shell;
#[cfg(windows)]
use windows::resolve_windows;

use crate::core::types::{LaunchConfig, ShellPreference};

/// Resolved spawn plan ready for portable-pty `CommandBuilder`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedSpawn {
    pub command: String,
    pub args: Vec<String>,
    /// Working directory for CreateProcess / posix_spawn — never a UNC path on Windows.
    pub cwd: String,
}

/// Expand a saved [`LaunchConfig`] into `(command, args, cwd)` ready for [`prepare_spawn`].
///
/// WSL mode builds `wsl.exe … -- <command> <args>` and forces a **local Windows** cwd
/// (never the Linux path, never UNC). On non-Windows hosts WSL mode returns an error.
pub fn resolve_process_launch(
    launch: &LaunchConfig,
) -> Result<(String, Vec<String>, Option<String>), String> {
    if launch.wsl {
        if !cfg!(windows) {
            return Err("WSL launch mode is Windows-only".to_string());
        }
        let cmd = launch
            .command
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                "WSL mode requires a Command (Linux executable, e.g. npm) — not `wsl` itself"
                    .to_string()
            })?;
        let (exe, args) = build_wsl_argv(
            cmd,
            &launch.args,
            launch.cwd.as_deref(),
            launch.wsl_distro.as_deref(),
        )?;
        // CreateProcess cwd must be local; Linux workdir is already in `--cd`.
        return Ok((exe, args, Some(safe_windows_cwd())));
    }

    // Process mode: never silently run Windows tools against a WSL UNC mount.
    if let Some(cwd) = launch
        .cwd
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if parse_wsl_unc(cwd).is_some() || is_unc_path(cwd) {
            return Err(format!(
                "Working directory looks like a WSL/UNC path ({cwd}). \
                 Use launch mode **WSL** with a Linux path (e.g. /home/…), \
                 not Process mode with \\\\wsl.localhost\\\\… — that runs Windows \
                 node/pnpm against the UNC mount."
            ));
        }
    }

    let command = launch
        .command
        .clone()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "no command configured".to_string())?;
    Ok((command, launch.args.clone(), launch.cwd.clone()))
}

/// Resolve an interactive shell for the Terminal tab (no configured Start command needed).
///
/// Uses the program's cwd / WSL settings when present. On Unix: `$SHELL` (fallback
/// `/bin/bash`). On Windows: the configured [`ShellPreference`] (`auto` prefers
/// PowerShell 7 when installed, then Windows PowerShell 5.1), or a login `bash`
/// inside WSL when `wsl: true`.
pub fn resolve_interactive_shell(
    launch: &LaunchConfig,
    shell: ShellPreference,
) -> Result<(String, Vec<String>, Option<String>), String> {
    if launch.wsl {
        if !cfg!(windows) {
            return Err("WSL launch mode is Windows-only".to_string());
        }
        let (exe, args) = build_wsl_argv(
            "bash",
            &["-l".to_string()],
            launch.cwd.as_deref(),
            launch.wsl_distro.as_deref(),
        )?;
        return Ok((exe, args, Some(safe_windows_cwd())));
    }

    if let Some(cwd) = launch
        .cwd
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if parse_wsl_unc(cwd).is_some() || is_unc_path(cwd) {
            return Err(format!(
                "Working directory looks like a WSL/UNC path ({cwd}). \
                 Use launch mode **WSL** with a Linux path (e.g. /home/…)."
            ));
        }
    }

    Ok((
        default_interactive_shell(shell),
        Vec::new(),
        launch.cwd.clone(),
    ))
}
/// Split a mistaken "cmd + args in Command" field, then platform-resolve the executable
/// and (on Windows) sanitize cwd for ConPTY / WSL / cmd.exe.
pub fn prepare_spawn(
    command: &str,
    args: Vec<String>,
    cwd: Option<&str>,
) -> Result<PreparedSpawn, String> {
    let (exe, args) = normalize_command_args(command, args);
    if exe.is_empty() {
        return Err("command is empty".to_string());
    }

    let cwd = cwd.unwrap_or(".").to_string();

    #[cfg(windows)]
    {
        resolve_windows(&exe, args, &cwd)
    }
    #[cfg(not(windows))]
    {
        Ok(PreparedSpawn {
            command: exe,
            args,
            cwd,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn normalize_splits_node_app_js() {
        let (exe, args) = normalize_command_args("node app.js", vec![]);
        assert_eq!(exe, "node");
        assert_eq!(args, vec!["app.js"]);
    }

    #[test]
    fn normalize_prepends_split_tokens_to_existing_args() {
        let (exe, args) = normalize_command_args("node app.js", vec!["--flag".into()]);
        assert_eq!(exe, "node");
        assert_eq!(args, vec!["app.js", "--flag"]);
    }

    #[test]
    fn normalize_keeps_path_with_spaces_when_quoted() {
        let (exe, args) =
            normalize_command_args(r#""C:\Program Files\nodejs\node.exe""#, vec!["a.js".into()]);
        assert_eq!(exe, r"C:\Program Files\nodejs\node.exe");
        assert_eq!(args, vec!["a.js"]);
    }

    #[test]
    fn normalize_keeps_unix_path() {
        let (exe, args) = normalize_command_args("/usr/bin/node", vec!["a.js".into()]);
        assert_eq!(exe, "/usr/bin/node");
        assert_eq!(args, vec!["a.js"]);
    }

    #[test]
    fn normalize_bare_node_unchanged() {
        let (exe, args) = normalize_command_args("node", vec!["app.js".into()]);
        assert_eq!(exe, "node");
        assert_eq!(args, vec!["app.js"]);
    }

    #[test]
    fn normalize_npm_run_develop_in_command() {
        let (exe, args) = normalize_command_args("npm run develop", vec![]);
        assert_eq!(exe, "npm");
        assert_eq!(args, vec!["run", "develop"]);
    }

    #[test]
    fn normalize_windows_cwd_strips_extended_prefix() {
        assert_eq!(normalize_windows_cwd(r"\\?\Z:\sample"), r"Z:\sample");
        assert_eq!(normalize_windows_cwd(r"Z:/sample/app"), r"Z:\sample\app");
        assert_eq!(normalize_windows_cwd(r"C:\Prog"), r"C:\Prog");
    }

    #[test]
    fn normalize_windows_cwd_rewrites_extended_unc() {
        assert_eq!(
            normalize_windows_cwd(r"\\?\UNC\wsl$\Ubuntu\home\x"),
            r"\\wsl$\Ubuntu\home\x"
        );
    }

    // Issue #233: a fixed-offset string slice panicked when byte 4 fell inside
    // a multi-byte char (len() >= 4 counts bytes, not chars).
    #[test]
    fn normalize_windows_cwd_survives_multibyte_after_prefix() {
        // "UN€x": bytes 55 4E E2 82 AC 78 — index 4 splits the €.
        assert_eq!(normalize_windows_cwd(r"\\?\UN€x"), "UN€x");
        assert_eq!(normalize_windows_cwd(r"\\?\ÜNC\x"), "ÜNC\\x");
        // Regression must not break the real UNC rewrite.
        assert_eq!(normalize_windows_cwd(r"\\?\UNC\srv\share"), r"\\srv\share");
    }

    #[test]
    fn is_unc_detects_wsl_and_share() {
        assert!(is_unc_path(r"\\wsl$\Ubuntu\home\x"));
        assert!(is_unc_path(r"//wsl$/Ubuntu/home/x"));
        assert!(is_unc_path(r"\\?\UNC\server\share\dir"));
        assert!(is_unc_path(r"\\nas\projects"));
        assert!(!is_unc_path(r"C:\Users\x"));
        assert!(!is_unc_path(r"Z:\sample"));
        assert!(!is_unc_path(r"\\?\C:\Windows"));
    }

    #[test]
    fn parse_wsl_unc_distro_and_linux_path() {
        assert_eq!(
            parse_wsl_unc(r"\\wsl$\Ubuntu\home\user\proj"),
            Some(("Ubuntu".into(), "/home/user/proj".into()))
        );
        assert_eq!(
            parse_wsl_unc(r"\\wsl.localhost\Debian\var\log"),
            Some(("Debian".into(), "/var/log".into()))
        );
        assert_eq!(
            parse_wsl_unc(r"\\?\UNC\wsl$\Ubuntu\home\x"),
            Some(("Ubuntu".into(), "/home/x".into()))
        );
        assert_eq!(
            parse_wsl_unc(r"\\wsl$\Ubuntu"),
            Some(("Ubuntu".into(), "/".into()))
        );
        assert_eq!(parse_wsl_unc(r"\\nas\share\dir"), None);
        assert_eq!(parse_wsl_unc(r"C:\Users"), None);
    }

    #[test]
    fn parse_wsl_unc_trims_distro_segment() {
        // Issue #255: a trailing space in a copied UNC path must not become
        // part of the distro name (`wsl -d "Ubuntu "` is rejected).
        assert_eq!(
            parse_wsl_unc(r"\\wsl$\Ubuntu \home\user"),
            Some(("Ubuntu".into(), "/home/user".into()))
        );
        assert_eq!(
            parse_wsl_unc(r"\\wsl$\ Ubuntu\home"),
            Some(("Ubuntu".into(), "/home".into()))
        );
    }

    #[test]
    fn is_wsl_executable_basename() {
        assert!(is_wsl_executable("wsl"));
        assert!(is_wsl_executable("WSL.EXE"));
        assert!(is_wsl_executable(r"C:\Windows\System32\wsl.exe"));
        assert!(!is_wsl_executable("wsl-helper"));
        assert!(!is_wsl_executable("node"));
    }

    #[test]
    fn inject_wsl_cd_preserves_existing_cd_adds_distro() {
        let out = inject_wsl_cd_args(
            vec!["--cd".into(), "/tmp".into(), "bash".into()],
            Some("Ubuntu"),
            Some("/home/x"),
        );
        assert_eq!(out, vec!["-d", "Ubuntu", "--cd", "/tmp", "bash"]);
    }

    #[test]
    fn inject_wsl_cd_adds_distro_and_cd() {
        let out = inject_wsl_cd_args(
            vec!["bash".into(), "-lc".into(), "npm run develop".into()],
            Some("Ubuntu"),
            Some("/home/x/proj"),
        );
        assert_eq!(
            out,
            vec![
                "-d",
                "Ubuntu",
                "--cd",
                "/home/x/proj",
                "bash",
                "-lc",
                "npm run develop"
            ]
        );
    }

    #[test]
    fn finalize_wsl_converts_wsl_unc() {
        let (args, cwd) = finalize_windows_cwd_and_args(
            true,
            vec!["bash".into(), "-lc".into(), "echo hi".into()],
            r"\\wsl$\Ubuntu\home\user\app",
        )
        .unwrap();
        assert!(!is_unc_path(&cwd), "cwd must be local: {cwd}");
        assert_eq!(
            args,
            vec![
                "-d",
                "Ubuntu",
                "--cd",
                "/home/user/app",
                "bash",
                "-lc",
                "echo hi"
            ]
        );
    }

    #[test]
    fn finalize_wsl_linux_abs_cwd_injects_cd() {
        let (args, cwd) =
            finalize_windows_cwd_and_args(true, vec!["uname".into(), "-a".into()], "/home/user")
                .unwrap();
        assert!(!is_unc_path(&cwd));
        assert_eq!(args, vec!["--cd", "/home/user", "uname", "-a"]);
    }

    #[test]
    fn finalize_wsl_keeps_explicit_cd() {
        let (args, cwd) = finalize_windows_cwd_and_args(
            true,
            vec!["--cd".into(), "/opt/app".into(), "bash".into()],
            r"\\wsl$\Ubuntu\home\other",
        )
        .unwrap();
        assert!(!is_unc_path(&cwd));
        // distro injected; user's --cd preserved
        assert_eq!(args, vec!["-d", "Ubuntu", "--cd", "/opt/app", "bash"]);
    }

    #[test]
    fn finalize_non_wsl_rejects_unc() {
        let err = finalize_windows_cwd_and_args(false, vec![], r"\\nas\share\proj").unwrap_err();
        assert!(err.contains("UNC"), "{err}");
    }

    #[test]
    fn finalize_wsl_rejects_non_wsl_unc() {
        let err = finalize_windows_cwd_and_args(true, vec![], r"\\nas\share\proj").unwrap_err();
        assert!(err.contains("UNC"), "{err}");
    }

    #[test]
    fn finalize_non_wsl_keeps_local_drive() {
        let (args, cwd) =
            finalize_windows_cwd_and_args(false, vec!["run".into()], r"Z:\sample").unwrap();
        assert_eq!(args, vec!["run"]);
        assert_eq!(cwd, r"Z:\sample");
    }

    #[test]
    fn candidate_score_prefers_nodejs_over_windowsapps() {
        let stub = PathBuf::from(r"C:\Users\x\AppData\Local\Microsoft\WindowsApps\node.exe");
        let real = PathBuf::from(r"C:\Program Files\nodejs\node.exe");
        assert!(candidate_score(&real) > candidate_score(&stub));
    }

    #[test]
    fn candidate_score_prefers_exe_over_cmd() {
        let exe = PathBuf::from(r"C:\Program Files\nodejs\node.exe");
        let cmd = PathBuf::from(r"C:\Program Files\nodejs\npm.cmd");
        assert!(candidate_score(&exe) > candidate_score(&cmd));
    }

    #[test]
    fn pick_best_skips_windowsapps_when_real_exists() {
        let stub = PathBuf::from(r"C:\Users\x\AppData\Local\Microsoft\WindowsApps\node.exe");
        let real = PathBuf::from(r"C:\Program Files\nodejs\node.exe");
        // is_unusable_stub checks filesystem for 0-byte — WindowsApps path is always skipped.
        let picked = pick_best_candidate(vec![stub, real.clone()]);
        assert_eq!(picked, Some(real));
    }

    #[test]
    fn pick_best_returns_none_when_only_windowsapps() {
        let stub = PathBuf::from(r"C:\Users\x\AppData\Local\Microsoft\WindowsApps\node.exe");
        assert_eq!(pick_best_candidate(vec![stub]), None);
    }

    #[test]
    fn status_dll_init_failed_exit_code_maps() {
        // Confirm the signed mapping the UI shows: 0xC0000142 → -1073741502
        let code = 0xC0000142u32 as i32;
        assert_eq!(code, -1073741502);
    }

    #[test]
    fn build_wsl_argv_full() {
        let (exe, args) = build_wsl_argv(
            "npm",
            &["run".into(), "dev".into()],
            Some("/home/user/projects/my-app"),
            Some("Ubuntu"),
        )
        .unwrap();
        assert!(
            exe.eq_ignore_ascii_case("wsl")
                || exe.to_ascii_lowercase().ends_with(r"\wsl.exe")
                || exe.to_ascii_lowercase().ends_with("/wsl.exe"),
            "exe={exe}"
        );
        assert_eq!(
            args,
            vec![
                "--shell-type",
                "login",
                "-d",
                "Ubuntu",
                "--cd",
                "/home/user/projects/my-app",
                "--",
                "npm",
                "run",
                "dev"
            ]
        );
    }

    #[test]
    fn build_wsl_argv_default_distro_no_cwd() {
        let (exe, args) = build_wsl_argv("uname", &["-a".into()], None, Some("  ")).unwrap();
        assert!(exe.to_ascii_lowercase().contains("wsl"), "exe={exe}");
        assert_eq!(args, vec!["--shell-type", "login", "--", "uname", "-a"]);
    }

    #[test]
    fn build_wsl_argv_splits_command_line() {
        let (exe, args) = build_wsl_argv("npm run develop", &[], Some("/opt/app"), None).unwrap();
        assert!(exe.to_ascii_lowercase().contains("wsl"), "exe={exe}");
        assert_eq!(
            args,
            vec![
                "--shell-type",
                "login",
                "--cd",
                "/opt/app",
                "--",
                "npm",
                "run",
                "develop"
            ]
        );
    }

    #[test]
    fn build_wsl_argv_rejects_nested_wsl() {
        let err = build_wsl_argv("wsl", &["-e".into(), "bash".into()], None, None).unwrap_err();
        assert!(err.contains("not wsl"), "{err}");
    }

    #[test]
    fn build_wsl_argv_converts_unc_cwd_to_linux_cd() {
        let (exe, args) = build_wsl_argv(
            "pnpm",
            &["run".into(), "dev".into()],
            Some(r"\\wsl.localhost\Ubuntu\home\user\projects\my-app"),
            None,
        )
        .unwrap();
        assert!(exe.to_ascii_lowercase().ends_with("wsl.exe") || exe.eq_ignore_ascii_case("wsl"));
        assert!(args.iter().any(|a| a == "--cd"));
        assert!(args.iter().any(|a| a == "/home/user/projects/my-app"));
        assert!(args.iter().any(|a| a == "-d"));
        assert!(args.iter().any(|a| a == "Ubuntu"));
        assert!(args.iter().any(|a| a == "--"));
        assert!(args.iter().any(|a| a == "pnpm"));
        assert!(!args.iter().any(|a| a.contains("wsl.localhost")));
    }

    #[test]
    fn build_wsl_argv_converts_single_slash_wsl_localhost() {
        let (_, args) = build_wsl_argv(
            "pnpm",
            &["run".into(), "dev".into()],
            Some(r"\wsl.localhost\Ubuntu\home\x\app"),
            Some("Ubuntu"),
        )
        .unwrap();
        assert!(args.iter().any(|a| a == "/home/x/app"));
        assert!(!args.iter().any(|a| a.contains("wsl.localhost")));
    }

    #[test]
    fn resolve_process_launch_wsl_full_plan() {
        use crate::core::types::LaunchConfig;
        let launch = LaunchConfig {
            wsl: true,
            command: Some("pnpm".into()),
            args: vec!["run".into(), "dev".into()],
            cwd: Some("/home/user/projects/my-app".into()),
            wsl_distro: Some("Ubuntu".into()),
            ..Default::default()
        };
        let result = resolve_process_launch(&launch);
        if cfg!(windows) {
            let (exe, args, cwd) = result.unwrap();
            assert!(
                exe.to_ascii_lowercase().ends_with("wsl.exe"),
                "exe must be wsl.exe, got {exe}"
            );
            assert!(args.iter().any(|a| a == "--cd"));
            assert!(args.iter().any(|a| a == "/home/user/projects/my-app"));
            assert!(args.iter().any(|a| a == "--"));
            assert!(args.iter().any(|a| a == "pnpm"));
            assert!(args.iter().any(|a| a == "--shell-type"));
            let windows_cwd = cwd.expect("windows cwd");
            assert!(
                !is_unc_path(&windows_cwd),
                "cwd must be local: {windows_cwd}"
            );
            assert!(!windows_cwd.contains("wsl.localhost"));
            assert!(!windows_cwd.contains("wsl$"));
        } else {
            let err = result.unwrap_err();
            assert!(err.contains("Windows-only"), "{err}");
        }
    }

    #[test]
    fn resolve_process_launch_wsl_unc_cwd_still_linux_cd() {
        use crate::core::types::LaunchConfig;
        let launch = LaunchConfig {
            wsl: true,
            command: Some("pnpm".into()),
            args: vec!["run".into(), "dev".into()],
            cwd: Some(r"\\wsl.localhost\Ubuntu\home\user\projects\my-app".into()),
            wsl_distro: Some("Ubuntu".into()),
            ..Default::default()
        };
        // build_wsl_argv is pure — always test argv shape; resolve gates on cfg(windows).
        let (exe, args) = build_wsl_argv(
            launch.command.as_deref().unwrap(),
            &launch.args,
            launch.cwd.as_deref(),
            launch.wsl_distro.as_deref(),
        )
        .unwrap();
        assert!(exe.to_ascii_lowercase().contains("wsl"));
        assert!(args.iter().any(|a| a == "--cd"));
        assert!(args.iter().any(|a| a == "/home/user/projects/my-app"));
        assert!(args.iter().any(|a| a == "pnpm"));
        assert!(!args
            .iter()
            .any(|a| a.contains(r"\\") || a.contains("wsl.localhost")));

        let result = resolve_process_launch(&launch);
        if cfg!(windows) {
            let (exe, args, cwd) = result.unwrap();
            assert!(exe.to_ascii_lowercase().ends_with("wsl.exe"));
            assert!(args.iter().any(|a| a == "/home/user/projects/my-app"));
            assert!(!is_unc_path(cwd.as_deref().unwrap_or("")));
        } else {
            assert!(result.unwrap_err().contains("Windows-only"));
        }
    }

    #[test]
    fn resolve_process_launch_wsl_windows_only() {
        use crate::core::types::LaunchConfig;
        let launch = LaunchConfig {
            wsl: true,
            command: Some("npm".into()),
            args: vec!["run".into(), "dev".into()],
            cwd: Some("/home/x/app".into()),
            wsl_distro: Some("Ubuntu".into()),
            ..Default::default()
        };
        let result = resolve_process_launch(&launch);
        if cfg!(windows) {
            let (exe, args, cwd) = result.unwrap();
            assert!(exe.to_ascii_lowercase().contains("wsl"));
            assert!(args.iter().any(|a| a == "--cd"));
            assert!(cwd.is_some());
            assert!(!is_unc_path(cwd.as_deref().unwrap_or("")));
        } else {
            let err = result.unwrap_err();
            assert!(err.contains("Windows-only"), "{err}");
        }
    }

    #[test]
    fn resolve_process_launch_rejects_process_mode_wsl_unc() {
        use crate::core::types::LaunchConfig;
        let launch = LaunchConfig {
            wsl: false,
            command: Some("pnpm".into()),
            args: vec!["run".into(), "dev".into()],
            cwd: Some(r"\\wsl.localhost\Ubuntu\home\x".into()),
            ..Default::default()
        };
        let err = resolve_process_launch(&launch).unwrap_err();
        assert!(err.contains("WSL") || err.contains("UNC"), "{err}");
    }

    #[test]
    fn resolve_interactive_shell_unix_uses_shell_env_or_bash() {
        use crate::core::types::LaunchConfig;
        let launch = LaunchConfig {
            cwd: Some("/tmp".to_string()),
            ..Default::default()
        };
        let (exe, args, cwd) =
            resolve_interactive_shell(&launch, ShellPreference::Powershell).unwrap();
        assert!(args.is_empty());
        assert_eq!(cwd.as_deref(), Some("/tmp"));
        #[cfg(not(windows))]
        {
            let expected = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
            assert_eq!(exe, expected);
        }
        #[cfg(windows)]
        {
            assert_eq!(exe, "powershell.exe");
        }
    }

    #[test]
    fn resolve_interactive_shell_honors_windows_preference() {
        // Issue #68: explicit preferences must win; only `auto` probes PATH.
        let launch = crate::core::types::LaunchConfig::default();
        #[cfg(windows)]
        {
            let (exe, _, _) = resolve_interactive_shell(&launch, ShellPreference::Cmd).unwrap();
            assert_eq!(exe, "cmd.exe");
            let (exe, _, _) =
                resolve_interactive_shell(&launch, ShellPreference::Powershell).unwrap();
            assert_eq!(exe, "powershell.exe");
            let (exe, _, _) = resolve_interactive_shell(&launch, ShellPreference::Pwsh).unwrap();
            assert_eq!(exe, "pwsh.exe");
            let (auto, _, _) = resolve_interactive_shell(&launch, ShellPreference::Auto).unwrap();
            assert!(
                auto == "pwsh.exe" || auto == "powershell.exe",
                "auto={auto}"
            );
        }
        #[cfg(not(windows))]
        {
            let (exe, _, _) = resolve_interactive_shell(&launch, ShellPreference::Cmd).unwrap();
            let expected = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
            assert_eq!(exe, expected, "Unix ignores the Windows shell preference");
        }
    }

    #[test]
    fn resolve_process_launch_plain_passthrough() {
        use crate::core::types::LaunchConfig;
        let launch = LaunchConfig {
            command: Some("node".into()),
            args: vec!["app.js".into()],
            cwd: Some(r"C:\proj".into()),
            ..Default::default()
        };
        let (exe, args, cwd) = resolve_process_launch(&launch).unwrap();
        assert_eq!(exe, "node");
        assert_eq!(args, vec!["app.js"]);
        assert_eq!(cwd.as_deref(), Some(r"C:\proj"));
    }

    #[test]
    fn normalize_wsl_linux_cwd_plain_and_unc() {
        assert_eq!(
            normalize_wsl_linux_cwd("/home/x/app").unwrap(),
            ("/home/x/app".into(), None)
        );
        assert_eq!(
            normalize_wsl_linux_cwd(r"\\wsl$\Ubuntu\home\x\app").unwrap(),
            ("/home/x/app".into(), Some("Ubuntu".into()))
        );
    }

    #[test]
    fn parse_wsl_unc_accepts_single_leading_slash() {
        assert_eq!(
            parse_wsl_unc(r"\wsl.localhost\Ubuntu\home\x"),
            Some(("Ubuntu".into(), "/home/x".into()))
        );
    }

    #[test]
    fn wsl_launch_yaml_roundtrip_preserves_wsl_flag() {
        use crate::core::types::{LaunchConfig, ProgramConfig, ProjectsStore};
        let store = ProjectsStore {
            active_project: 0,
            projects: vec![crate::core::types::ProjectConfig {
                id: "p1".into(),
                name: "p1".into(),
                default_cwd: None,
                path_hint: None,
                active_program: 0,
                programs: vec![ProgramConfig {
                    id: "prog1".into(),
                    name: "api".into(),
                    launch: LaunchConfig {
                        wsl: true,
                        wsl_distro: Some("Ubuntu".into()),
                        command: Some("pnpm".into()),
                        args: vec!["run".into(), "dev".into()],
                        cwd: Some("/home/user/projects/my-app".into()),
                        ..Default::default()
                    },
                    workspace: Default::default(),
                }],
            }],
        };
        let yaml = serde_yaml::to_string(&store).unwrap();
        assert!(
            yaml.contains("wsl: true") || yaml.contains("wsl:true"),
            "{yaml}"
        );
        let parsed: ProjectsStore = serde_yaml::from_str(&yaml).unwrap();
        let launch = &parsed.projects[0].programs[0].launch;
        assert!(launch.wsl);
        assert_eq!(launch.wsl_distro.as_deref(), Some("Ubuntu"));
        assert_eq!(launch.command.as_deref(), Some("pnpm"));
        assert_eq!(launch.cwd.as_deref(), Some("/home/user/projects/my-app"));
    }
}
