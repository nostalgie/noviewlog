//! One-line launch summary for the stopped-Terminal chrome strip.

/// Saved launch fields used to build the preview line.
#[derive(Debug, Clone, Copy, Default)]
pub struct LaunchPreview<'a> {
    pub command: &'a str,
    pub args: &'a str,
    pub cwd: &'a str,
    pub wsl: bool,
    pub wsl_distro: &'a str,
}

/// ASCII summary of what Start (or type-to-open) will do.
///
/// Segments join with `  |  `. Empty cwd and WSL-off are omitted. Shell-only
/// (no command, WSL off) uses type-to-open wording instead of `Start:`.
pub fn format_launch_preview(launch: LaunchPreview<'_>) -> String {
    let command = launch.command.trim();
    let args = launch.args.trim();
    let cwd = launch.cwd.trim();
    let distro = launch.wsl_distro.trim();

    let mut parts: Vec<String> = Vec::new();

    if command.is_empty() {
        if launch.wsl {
            parts.push("Start: interactive WSL shell".to_string());
            if !distro.is_empty() {
                parts.push(distro.to_string());
            }
        } else {
            parts.push("Type to open a shell".to_string());
        }
    } else {
        if args.is_empty() {
            parts.push(format!("Start: {command}"));
        } else {
            parts.push(format!("Start: {command} {args}"));
        }
        if launch.wsl {
            if distro.is_empty() {
                parts.push("WSL".to_string());
            } else {
                parts.push(format!("WSL {distro}"));
            }
        }
    }

    if !cwd.is_empty() {
        parts.push(cwd.to_string());
    }

    parts.join("  |  ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_command_with_wsl_cwd() {
        let text = format_launch_preview(LaunchPreview {
            command: "uname",
            args: "-a",
            cwd: "/home/tester",
            wsl: true,
            wsl_distro: "",
        });
        assert_eq!(text, "Start: uname -a  |  WSL  |  /home/tester");
    }

    #[test]
    fn saved_command_with_wsl_distro() {
        let text = format_launch_preview(LaunchPreview {
            command: "uname",
            args: "-a",
            cwd: "/home/tester",
            wsl: true,
            wsl_distro: "Ubuntu",
        });
        assert_eq!(text, "Start: uname -a  |  WSL Ubuntu  |  /home/tester");
    }

    #[test]
    fn saved_command_no_wsl_omits_wsl_and_empty_cwd() {
        let text = format_launch_preview(LaunchPreview {
            command: "npm",
            args: "run dev",
            cwd: "",
            wsl: false,
            wsl_distro: "Ubuntu",
        });
        assert_eq!(text, "Start: npm run dev");
        assert!(!text.contains("WSL"));
    }

    #[test]
    fn empty_command_wsl() {
        let text = format_launch_preview(LaunchPreview {
            command: "",
            args: "",
            cwd: "/home/tester",
            wsl: true,
            wsl_distro: "",
        });
        assert_eq!(text, "Start: interactive WSL shell  |  /home/tester");
    }

    #[test]
    fn shell_only_uses_type_to_open() {
        let text = format_launch_preview(LaunchPreview {
            command: "  ",
            args: "",
            cwd: r"C:\projects\noviewlog",
            wsl: false,
            wsl_distro: "",
        });
        assert_eq!(text, r"Type to open a shell  |  C:\projects\noviewlog");
        assert!(!text.contains("Start:"));
    }
}
