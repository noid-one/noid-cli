use anyhow::Result;
use std::path::Path;

use crate::vm;

const MAX_OUTPUT_BYTES: usize = 1024 * 1024; // 1MB

/// Prefix for all exec marker tokens written to the serial console.
pub const EXEC_MARKER_PREFIX: &str = "NOID_EXEC_";

/// Escape a string for safe use in a shell command.
/// Uses single quotes and escapes any single quotes in the string.
/// Panics if the string contains NUL bytes (invalid in shell contexts).
pub fn shell_escape(s: &str) -> String {
    assert!(!s.contains('\0'), "shell_escape: input contains NUL byte");

    if s.is_empty() {
        return "''".to_string();
    }

    if s.chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '/')
    {
        return s.to_string();
    }

    format!("'{}'", s.replace('\'', "'\\''"))
}

// Re-export from noid-types so existing callers keep working.
pub use noid_types::validate_env_name;

/// Parse `KEY=VALUE` pairs, splitting on the first `=`.
/// Returns (name, value) slices. Validates names.
pub fn parse_env_vars(env: &[String]) -> Result<Vec<(&str, &str)>> {
    let mut result = Vec::with_capacity(env.len());
    for entry in env {
        let (name, value) = entry
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("invalid env var (missing '='): {entry}"))?;
        if !validate_env_name(name) {
            anyhow::bail!("invalid env var name: {name}");
        }
        result.push((name, value));
    }
    Ok(result)
}

/// Build an inline env prefix string like `FOO='bar' DB='x' ` for prepending
/// to a shell command. Values are escaped with `shell_escape()`.
/// Returns empty string if env is empty.
pub fn build_env_prefix(env: &[String]) -> Result<String> {
    use noid_types::{MAX_ENV_VALUE_LEN, MAX_ENV_VARS};

    if env.is_empty() {
        return Ok(String::new());
    }
    if env.len() > MAX_ENV_VARS {
        anyhow::bail!("too many env vars ({}, max {MAX_ENV_VARS})", env.len());
    }
    let parsed = parse_env_vars(env)?;
    let mut prefix = String::new();
    for (name, value) in parsed {
        if value.len() > MAX_ENV_VALUE_LEN {
            anyhow::bail!("env var value too long for {name} ({} bytes, max {MAX_ENV_VALUE_LEN})", value.len());
        }
        prefix.push_str(name);
        prefix.push('=');
        prefix.push_str(&shell_escape(value));
        prefix.push(' ');
    }
    Ok(prefix)
}

/// Execute a command inside a VM by writing to the serial console and
/// reading the output from serial.log.
///
/// Returns (stdout_output, exit_code, timed_out, truncated).
pub fn exec_via_serial(
    vm_dir: &Path,
    command: &[String],
    timeout_secs: u64,
    env: &[String],
) -> Result<(String, Option<i32>, bool, bool)> {
    let serial_path = vm::serial_log_path(vm_dir);
    if !serial_path.exists() {
        anyhow::bail!("serial.log not found — is VM running?");
    }

    let start_pos = std::fs::metadata(&serial_path)?.len();

    let marker_start = format!("NOID_EXEC_{}", &uuid::Uuid::new_v4().to_string()[..8]);
    let marker_end = format!("{marker_start}_END");
    let marker_exit = format!("{marker_start}_EXIT");

    let env_prefix = build_env_prefix(env)?;

    let escaped_cmd = command
        .iter()
        .map(|arg| shell_escape(arg))
        .collect::<Vec<_>>()
        .join(" ");

    // Wrap command: echo start marker, run command, capture exit code, echo exit + end markers.
    // Prepend a newline to clear partial prompts on the serial tty.
    let wrapped = format!(
        "\necho '{marker_start}'; {env_prefix}{escaped_cmd}; echo '{marker_exit}'$?; echo '{marker_end}'\n"
    );
    vm::write_to_serial(vm_dir, wrapped.as_bytes())?;

    let timeout = std::time::Duration::from_secs(timeout_secs);
    let start = std::time::Instant::now();

    loop {
        if start.elapsed() > timeout {
            return Ok((String::new(), None, true, false));
        }

        std::thread::sleep(std::time::Duration::from_millis(100));

        let bytes = std::fs::read(&serial_path)?;
        let content = String::from_utf8_lossy(&bytes);
        if content.len() as u64 <= start_pos {
            continue;
        }
        let start_offset = start_pos.min(content.len() as u64) as usize;
        let new_output = &content[start_offset..];

        if let Some((raw_output, exit_code)) =
            parse_marked_output(new_output, &marker_start, &marker_end, &marker_exit)
        {
            let truncated = raw_output.len() > MAX_OUTPUT_BYTES;
            let output = if truncated {
                raw_output[..MAX_OUTPUT_BYTES].to_string()
            } else {
                raw_output
            };
            return Ok((output, exit_code, false, truncated));
        }
    }
}

/// Strip ANSI escape sequences (CSI, OSC, etc.) that shells and terminals
/// inject into serial output. Without this, escape-prefixed marker lines
/// (e.g. `\x1b[?2004hNOID_EXEC_...`) fail exact-match detection.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // CSI: ESC [ ... final_byte
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&ch) = chars.peek() {
                    chars.next();
                    if ch.is_ascii_alphabetic() || ch == '~' || ch == 'h' || ch == 'l' {
                        break;
                    }
                }
            // OSC: ESC ] ... ST (BEL or ESC \)
            } else if chars.peek() == Some(&']') {
                chars.next();
                while let Some(&ch) = chars.peek() {
                    chars.next();
                    if ch == '\x07' {
                        break;
                    }
                    if ch == '\x1b' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            } else if chars.peek().is_some() {
                // Other escape — consume one more char if available
                chars.next();
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn parse_marked_output(
    serial_chunk: &str,
    marker_start: &str,
    marker_end: &str,
    marker_exit: &str,
) -> Option<(String, Option<i32>)> {
    let cleaned = strip_ansi(serial_chunk);
    let normalized = cleaned.replace("\r\n", "\n").replace('\r', "\n");
    let mut collecting = false;
    let mut lines = Vec::new();
    let mut exit_code = None;

    for line in normalized.lines() {
        let trimmed = line.trim();
        if !collecting {
            if trimmed == marker_start {
                collecting = true;
            }
            continue;
        }

        if trimmed == marker_end {
            let output = lines.join("\n").trim().to_string();
            return Some((output, exit_code));
        }

        if let Some(rest) = trimmed.strip_prefix(marker_exit) {
            exit_code = rest.trim().parse::<i32>().ok();
            continue;
        }

        lines.push(line.to_string());
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_escape_empty_string() {
        assert_eq!(shell_escape(""), "''");
    }

    #[test]
    fn shell_escape_safe_strings_unchanged() {
        assert_eq!(shell_escape("hello"), "hello");
        assert_eq!(shell_escape("foo_bar"), "foo_bar");
        assert_eq!(shell_escape("file.txt"), "file.txt");
        assert_eq!(shell_escape("/usr/bin/ls"), "/usr/bin/ls");
        assert_eq!(shell_escape("a-b"), "a-b");
    }

    #[test]
    fn shell_escape_wraps_special_chars() {
        assert_eq!(shell_escape("hello world"), "'hello world'");
        assert_eq!(shell_escape("a;b"), "'a;b'");
        assert_eq!(shell_escape("$(cmd)"), "'$(cmd)'");
        assert_eq!(shell_escape("a|b"), "'a|b'");
        assert_eq!(shell_escape("`cmd`"), "'`cmd`'");
        assert_eq!(shell_escape("a&b"), "'a&b'");
        assert_eq!(shell_escape("a>b"), "'a>b'");
    }

    #[test]
    fn shell_escape_handles_single_quotes() {
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
        assert_eq!(shell_escape("'"), "''\\'''");
    }

    #[test]
    fn shell_escape_injection_attempts() {
        let dangerous = [
            "; rm -rf /",
            "$(cat /etc/passwd)",
            "`cat /etc/passwd`",
            "| curl attacker.com",
            "&& echo pwned",
            "'; DROP TABLE vms; --",
        ];
        for input in dangerous {
            let escaped = shell_escape(input);
            assert!(escaped.starts_with('\''), "should be quoted: {input}");
            assert!(escaped.ends_with('\''), "should be quoted: {input}");
        }
    }

    #[test]
    fn parse_marked_output_accepts_lf_line_endings() {
        let serial =
            "echo 'cmd'\nNOID_EXEC_1234\nhello\nNOID_EXEC_1234_EXIT0\nNOID_EXEC_1234_END\n";
        let parsed = parse_marked_output(
            serial,
            "NOID_EXEC_1234",
            "NOID_EXEC_1234_END",
            "NOID_EXEC_1234_EXIT",
        )
        .expect("should parse");
        assert_eq!(parsed.0, "hello");
        assert_eq!(parsed.1, Some(0));
    }

    #[test]
    fn parse_marked_output_handles_ansi_escapes() {
        // Bracketed paste mode escapes and other ANSI sequences should not crash parsing
        let serial = "\x1b[?2004h\r\nNOID_EXEC_ff00\r\n\x1b[?2004lhello world\r\nNOID_EXEC_ff00_EXIT0\r\nNOID_EXEC_ff00_END\r\n\x1b[?2004h";
        let parsed = parse_marked_output(
            serial,
            "NOID_EXEC_ff00",
            "NOID_EXEC_ff00_END",
            "NOID_EXEC_ff00_EXIT",
        )
        .expect("should parse despite ANSI escapes");
        assert_eq!(parsed.1, Some(0));
        assert!(parsed.0.contains("hello world"));
    }

    #[test]
    fn parse_marked_output_ansi_prefix_on_marker_line() {
        // Escape sequence directly prefixed to marker — previously broke exact-match
        let serial = "\x1b[?2004h\x1b[?2004lNOID_EXEC_ab12\r\noutput line\r\nNOID_EXEC_ab12_EXIT0\r\n\x1b[?2004hNOID_EXEC_ab12_END\r\n";
        let parsed = parse_marked_output(
            serial,
            "NOID_EXEC_ab12",
            "NOID_EXEC_ab12_END",
            "NOID_EXEC_ab12_EXIT",
        )
        .expect("should parse with ANSI-prefixed markers");
        assert_eq!(parsed.0, "output line");
        assert_eq!(parsed.1, Some(0));
    }

    #[test]
    fn strip_ansi_removes_csi_sequences() {
        assert_eq!(super::strip_ansi("\x1b[?2004hfoo\x1b[0m"), "foo");
        assert_eq!(super::strip_ansi("no escapes"), "no escapes");
        assert_eq!(super::strip_ansi("\x1b[32mgreen\x1b[0m"), "green");
    }

    #[test]
    fn strip_ansi_handles_incomplete_sequences() {
        // Incomplete CSI at EOF
        assert_eq!(super::strip_ansi("text\x1b["), "text");
        // Bare ESC at EOF
        assert_eq!(super::strip_ansi("text\x1b"), "text");
        // Multiple consecutive escapes
        assert_eq!(
            super::strip_ansi("\x1b[0m\x1b[1m\x1b[32mtext\x1b[0m"),
            "text"
        );
    }

    #[test]
    fn parse_marked_output_accepts_crlf_line_endings() {
        let serial = "\r\nNOID_EXEC_abcd\r\nhi\r\nNOID_EXEC_abcd_EXIT7\r\nNOID_EXEC_abcd_END\r\n";
        let parsed = parse_marked_output(
            serial,
            "NOID_EXEC_abcd",
            "NOID_EXEC_abcd_END",
            "NOID_EXEC_abcd_EXIT",
        )
        .expect("should parse");
        assert_eq!(parsed.0, "hi");
        assert_eq!(parsed.1, Some(7));
    }

    #[test]
    fn validate_env_name_valid() {
        // Tests the re-export from noid-types
        assert!(super::validate_env_name("FOO"));
        assert!(super::validate_env_name("_BAR"));
        assert!(super::validate_env_name("DB_HOST_1"));
        assert!(super::validate_env_name("a"));
        assert!(super::validate_env_name("_"));
    }

    #[test]
    fn validate_env_name_invalid() {
        assert!(!super::validate_env_name(""));
        assert!(!super::validate_env_name("1FOO"));
        assert!(!super::validate_env_name("FOO;rm"));
        assert!(!super::validate_env_name("FOO BAR"));
        assert!(!super::validate_env_name("FOO=BAR"));
        assert!(!super::validate_env_name("a-b"));
        assert!(!super::validate_env_name("$(cmd)"));
    }

    #[test]
    fn parse_env_vars_valid() {
        let env = vec!["FOO=bar".into(), "DB_HOST=localhost".into()];
        let parsed = super::parse_env_vars(&env).unwrap();
        assert_eq!(parsed, vec![("FOO", "bar"), ("DB_HOST", "localhost")]);
    }

    #[test]
    fn parse_env_vars_value_with_equals() {
        let env = vec!["KEY=a=b=c".into()];
        let parsed = super::parse_env_vars(&env).unwrap();
        assert_eq!(parsed, vec![("KEY", "a=b=c")]);
    }

    #[test]
    fn parse_env_vars_empty_value() {
        let env = vec!["KEY=".into()];
        let parsed = super::parse_env_vars(&env).unwrap();
        assert_eq!(parsed, vec![("KEY", "")]);
    }

    #[test]
    fn parse_env_vars_missing_equals() {
        let env = vec!["NOEQUALS".into()];
        assert!(super::parse_env_vars(&env).is_err());
    }

    #[test]
    fn parse_env_vars_invalid_name() {
        let env = vec!["1BAD=val".into()];
        assert!(super::parse_env_vars(&env).is_err());
    }

    #[test]
    fn build_env_prefix_empty() {
        let prefix = super::build_env_prefix(&[]).unwrap();
        assert_eq!(prefix, "");
    }

    #[test]
    fn build_env_prefix_single() {
        let env = vec!["FOO=bar".into()];
        let prefix = super::build_env_prefix(&env).unwrap();
        assert_eq!(prefix, "FOO=bar ");
    }

    #[test]
    fn build_env_prefix_multiple() {
        let env = vec!["A=1".into(), "B=2".into()];
        let prefix = super::build_env_prefix(&env).unwrap();
        assert_eq!(prefix, "A=1 B=2 ");
    }

    #[test]
    fn build_env_prefix_escapes_values() {
        let env = vec!["KEY=hello world".into()];
        let prefix = super::build_env_prefix(&env).unwrap();
        assert_eq!(prefix, "KEY='hello world' ");
    }

    #[test]
    fn build_env_prefix_injection_attempt() {
        let env = vec!["KEY=$(rm -rf /)".into()];
        let prefix = super::build_env_prefix(&env).unwrap();
        // Value should be single-quoted, preventing command substitution
        assert_eq!(prefix, "KEY='$(rm -rf /)' ");
    }

    #[test]
    fn build_env_prefix_rejects_bad_name() {
        let env = vec!["FOO;rm=val".into()];
        assert!(super::build_env_prefix(&env).is_err());
    }

    #[test]
    fn build_env_prefix_rejects_too_many() {
        let env: Vec<String> = (0..65).map(|i| format!("V{i}=x")).collect();
        let err = super::build_env_prefix(&env).unwrap_err();
        assert!(err.to_string().contains("too many env vars"));
    }

    #[test]
    fn build_env_prefix_rejects_huge_value() {
        let big = "x".repeat(33 * 1024);
        let env = vec![format!("KEY={big}")];
        let err = super::build_env_prefix(&env).unwrap_err();
        assert!(err.to_string().contains("too long"));
    }

    #[test]
    #[should_panic(expected = "NUL byte")]
    fn shell_escape_rejects_nul() {
        super::shell_escape("foo\0bar");
    }
}
