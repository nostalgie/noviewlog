/// If `command` looks like `node app.js` (no path separators), treat the first token as
/// the executable and prepend the rest to `args`. Quoted paths and real path strings
/// are left alone.
pub fn normalize_command_args(command: &str, mut args: Vec<String>) -> (String, Vec<String>) {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return (String::new(), args);
    }

    let unquoted = strip_outer_quotes(trimmed);
    if looks_like_path(unquoted) {
        return (unquoted.to_string(), args);
    }

    let tokens = split_whitespace_tokens(unquoted);
    if tokens.len() <= 1 {
        return (unquoted.to_string(), args);
    }

    let mut iter = tokens.into_iter();
    let exe = iter.next().unwrap_or_default();
    let mut rest: Vec<String> = iter.collect();
    rest.append(&mut args);
    (exe, rest)
}

fn strip_outer_quotes(s: &str) -> &str {
    if s.len() >= 2 {
        let bytes = s.as_bytes();
        if (bytes[0] == b'"' && bytes[s.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[s.len() - 1] == b'\'')
        {
            return &s[1..s.len() - 1];
        }
    }
    s
}

pub(super) fn looks_like_path(s: &str) -> bool {
    if s.contains('/') || s.contains('\\') {
        return true;
    }
    // Windows drive-relative: `C:foo` or `C:`
    let b = s.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

fn split_whitespace_tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes: Option<char> = None;
    for ch in s.chars() {
        match in_quotes {
            Some(q) if ch == q => {
                in_quotes = None;
            }
            Some(_) => cur.push(ch),
            None if ch == '"' || ch == '\'' => {
                in_quotes = Some(ch);
            }
            None if ch.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            None => cur.push(ch),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}
