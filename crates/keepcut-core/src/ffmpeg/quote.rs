/// Quotes an argument so the logged command line can be pasted into a shell.
///
/// Windows: double quotes (works in cmd and PowerShell for ordinary paths).
/// Elsewhere: POSIX single quotes.
pub fn shell_quote(s: &str) -> String {
    let safe = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=,+@%\\".contains(c));
    if safe {
        return s.to_string();
    }
    if cfg!(windows) {
        format!("\"{}\"", s.replace('"', "\\\""))
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("-c:v"), "-c:v");
        assert_eq!(shell_quote(""), if cfg!(windows) { "\"\"" } else { "''" });
        let q = shell_quote("a b");
        assert!(q.starts_with(['"', '\'']));
    }
}
