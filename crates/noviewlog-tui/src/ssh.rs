//! SSH session support (`terminals/tui-ssh`): the system `ssh` client runs
//! as the wrapped child in the engine PTY — no embedded SSH stack, so keys,
//! agent, known_hosts, and 2FA all stay with the user's ssh.

use noviewlog_core::core::types::SshProfile;

/// A chosen session to start: the local interactive shell or one ssh argv.
#[derive(Clone, Debug)]
pub enum SessionChoice {
    Local,
    Ssh { label: String, argv: Vec<String> },
}

/// Assemble the ssh argv for a target (design D3): `ssh -t [extra] target`,
/// `-p <port>` only when `port > 0`. The target is a single argv element —
/// no shell interpolation anywhere (the engine spawns directly).
pub fn build_ssh_argv(target: &str, port: u16, extra_args: &str) -> Vec<String> {
    let mut argv = vec!["ssh".to_string(), "-t".to_string()];
    if port > 0 {
        argv.push("-p".to_string());
        argv.push(port.to_string());
    }
    argv.extend(extra_args.split_whitespace().map(str::to_string));
    argv.push(target.to_string());
    argv
}

/// Same for a saved profile.
pub fn argv_for_profile(profile: &SshProfile) -> Vec<String> {
    build_ssh_argv(&profile.target, profile.port, &profile.extra_args)
}

/// Same for the `--ssh` CLI flags: `--port <n>` and repeatable
/// `--ssh-arg <arg>` (each one argv element). Empty defaults keep the
/// plain `ssh -t <target>` argv.
pub fn argv_from_cli_flags(target: &str, port: u16, extra_args: &[String]) -> Vec<String> {
    build_ssh_argv(target, port, &extra_args.join(" "))
}

/// Verify the system ssh client exists before `Command::Start` (a missing
/// client must be a clear error, esp. the Windows optional feature).
pub fn probe_ssh_client() -> Result<(), String> {
    match std::process::Command::new("ssh").arg("-V").output() {
        Ok(_) => Ok(()),
        Err(_) => Err("the `ssh` client was not found on PATH. \
             Install OpenSSH (Windows: Settings → Apps → Optional Features → OpenSSH Client) \
             or fix your PATH, then retry."
            .to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_target_gets_t_flag_only() {
        assert_eq!(
            build_ssh_argv("deploy@example.com", 0, ""),
            vec!["ssh", "-t", "deploy@example.com"]
        );
    }

    #[test]
    fn port_is_passed_only_when_nonzero() {
        assert_eq!(
            build_ssh_argv("web01", 2222, ""),
            vec!["ssh", "-t", "-p", "2222", "web01"]
        );
        assert!(!build_ssh_argv("web01", 0, "").contains(&"-p".to_string()));
    }

    #[test]
    fn extra_args_are_whitespace_split() {
        assert_eq!(
            build_ssh_argv("web01", 0, "-J bastion -4"),
            vec!["ssh", "-t", "-J", "bastion", "-4", "web01"]
        );
    }

    #[test]
    fn target_stays_one_argv_element() {
        // IPv6 literal / odd targets must never be split.
        let argv = build_ssh_argv("user@[2001:db8::1]", 0, "");
        assert_eq!(argv.last().unwrap(), "user@[2001:db8::1]");
    }

    #[test]
    fn cli_flags_assemble_port_and_repeatable_args() {
        // `--ssh web01 --port 2222 --ssh-arg -J --ssh-arg bastion -4`
        let extra: Vec<String> = ["-J", "bastion", "-4"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            argv_from_cli_flags("web01", 2222, &extra),
            vec!["ssh", "-t", "-p", "2222", "-J", "bastion", "-4", "web01"]
        );
    }

    #[test]
    fn cli_flag_defaults_match_plain_target() {
        assert_eq!(
            argv_from_cli_flags("deploy@example.com", 0, &[]),
            vec!["ssh", "-t", "deploy@example.com"]
        );
    }

    #[test]
    fn profile_argv_uses_profile_fields() {
        let p = SshProfile {
            name: "prod".into(),
            target: "deploy@example.com".into(),
            port: 2022,
            extra_args: "-A".into(),
        };
        assert_eq!(
            argv_for_profile(&p),
            vec!["ssh", "-t", "-p", "2022", "-A", "deploy@example.com"]
        );
    }
}
