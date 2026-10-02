//! CLI launch parsing for process / file / preset arguments.

use std::path::Path;

use noviewlog_core::core::types::LaunchConfig;

const LOG_EXTENSIONS: &[&str] = &["log", "txt", "out", "json", "jsonl"];

pub fn parse(args: &[String]) -> LaunchConfig {
    let mut config = LaunchConfig {
        cwd: std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().into_owned()),
        ..LaunchConfig::default()
    };

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--file" | "-f" => {
                i += 1;
                if i < args.len() {
                    config.log_file = Some(args[i].clone());
                }
            }
            "--config" | "-c" => {
                i += 1;
                if i < args.len() {
                    config.config_path = Some(args[i].clone());
                }
            }
            "--preset" | "-p" => {
                i += 1;
                if i < args.len() {
                    config.preset = Some(args[i].clone());
                }
            }
            "--" => {
                i += 1;
                if i < args.len() {
                    config.command = Some(args[i].clone());
                    config.args = args[i + 1..].to_vec();
                }
                return config;
            }
            arg if !arg.starts_with('-')
                && config.command.is_none()
                && config.log_file.is_none() =>
            {
                if is_log_file_arg(arg) {
                    config.log_file = Some(arg.to_string());
                } else {
                    config.command = Some(arg.to_string());
                    config.args = args[i + 1..].to_vec();
                    return config;
                }
            }
            _ => {}
        }
        i += 1;
    }

    config
}

fn is_log_file_arg(arg: &str) -> bool {
    if Path::new(arg).is_file() {
        return true;
    }
    let ext = Path::new(arg)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    if ext.is_empty() {
        return false;
    }
    LOG_EXTENSIONS.iter().any(|e| e.eq_ignore_ascii_case(ext))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_double_dash_command() {
        let args = vec!["--".into(), "bash".into(), "-lc".into(), "ls".into()];
        let cfg = parse(&args);
        assert_eq!(cfg.command.as_deref(), Some("bash"));
        assert_eq!(cfg.args, vec!["-lc", "ls"]);
    }

    #[test]
    fn parses_bare_command() {
        let args = vec!["bash".into()];
        let cfg = parse(&args);
        assert_eq!(cfg.command.as_deref(), Some("bash"));
    }

    #[test]
    fn parses_node_sample_app() {
        let args = vec!["--".into(), "node".into(), "sample/app.js".into()];
        let cfg = parse(&args);
        assert_eq!(cfg.command.as_deref(), Some("node"));
        assert_eq!(cfg.args, vec!["sample/app.js"]);
    }
}
