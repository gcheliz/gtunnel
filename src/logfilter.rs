//! Line classifier and filtering pump for supervised tunnel child processes.
//!
//! OpenSSH invoked with `-o LogLevel=DEBUG` emits the lines required to verify
//! port forwards and track active connections, but also thousands of internal
//! channel bookkeeping lines (`debug1: channel free`, `debug1: kex`, etc.).
//! This module classifies and filters those lines, timestamping the kept lines.

use std::io::Write;
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

/// Decide whether a log line from a child process carries useful information.
///
/// Deny-list based:
/// - Drops lines starting with `debug2:` or `debug3:`.
/// - Drops lines starting with `debug1:` unless they match explicit forwarding/connection notices.
/// - Keeps all other lines (errors, warnings, gcloud notices, tracebacks).
pub fn keep(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with("debug2:") || trimmed.starts_with("debug3:") {
        return false;
    }
    if trimmed.starts_with("debug1:") {
        return trimmed.contains("Local forwarding listening on")
            || trimmed.contains("Connection to port ")
            || trimmed.contains("new direct-tcpip");
    }
    true
}

/// Format a kept log line by sanitizing control characters and prefixing an RFC 3339 timestamp.
pub fn format_line(line: &str) -> String {
    let sanitized: String = line
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let stamp = humantime::format_rfc3339_seconds(std::time::SystemTime::now());
    format!("{stamp} {sanitized}\n")
}

/// Read lines from an async reader, filter via [`keep`], and write formatted lines to `out`.
pub async fn pump<R: AsyncBufRead + Unpin, W: Write>(
    mut reader: R,
    out: &mut W,
) -> std::io::Result<()> {
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let bytes_read = reader.read_until(b'\n', &mut buf).await?;
        if bytes_read == 0 {
            break;
        }
        let line_str = String::from_utf8_lossy(&buf);
        let trimmed = line_str.trim_end_matches(&['\r', '\n'][..]);
        if keep(trimmed) {
            let formatted = format_line(trimmed);
            out.write_all(formatted.as_bytes())?;
            out.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn drops_noisy_debug_lines() {
        assert!(!keep(
            "debug1: channel 200: free: direct-tcpip: listening port 8443"
        ));
        assert!(!keep("debug1: kex: algorithm: curve25519-sha256"));
        assert!(!keep("debug1: Exit status 0"));
        assert!(!keep("debug2: fd 3 setting O_NONBLOCK"));
        assert!(!keep("debug3: send packet: type 1"));
    }

    #[test]
    fn keeps_connection_and_forwarding_lines() {
        assert!(keep(
            "debug1: Local forwarding listening on 127.0.0.1 port 8443."
        ));
        assert!(keep(
            "debug1: Connection to port 8443 forwarding to 10.3.0.13 port 443"
        ));
        assert!(keep("debug1: channel 2: new direct-tcpip"));
    }

    #[test]
    fn keeps_gcloud_and_general_lines() {
        assert!(keep("Listening on port [15432]."));
        assert!(keep("INFO: [4] Connecting with URL"));
        assert!(keep("WARNING: connection retry"));
        assert!(keep("ERROR: failed to connect"));
        assert!(keep("Traceback (most recent call last):"));
    }

    #[test]
    fn format_line_adds_timestamp_and_sanitizes() {
        let formatted = format_line("hello\tworld\n");
        assert!(formatted.ends_with("hello world \n"));
        // RFC3339 timestamp prefix like "2026-10-25T01:50:00Z "
        assert!(formatted.contains("Z hello"));
    }

    #[tokio::test]
    async fn pump_filters_and_formats_stream() {
        let input = "debug1: noise\ndebug1: channel 2: new direct-tcpip\ndebug2: more noise\nListening on port [15432].\n";
        let reader = Cursor::new(input.as_bytes());
        let mut out = Vec::new();

        pump(reader, &mut out).await.expect("pump");
        let output = String::from_utf8(out).expect("utf8");

        assert!(!output.contains("noise"));
        assert!(output.contains("channel 2: new direct-tcpip"));
        assert!(output.contains("Listening on port [15432]."));
    }

    #[tokio::test]
    async fn pump_handles_partial_line_at_eof() {
        let input = "partial line without newline";
        let reader = Cursor::new(input.as_bytes());
        let mut out = Vec::new();

        pump(reader, &mut out).await.expect("pump");
        let output = String::from_utf8(out).expect("utf8");

        assert!(output.contains("partial line without newline"));
        assert!(output.ends_with('\n'));
    }

    #[tokio::test]
    async fn pump_handles_lossy_utf8() {
        let input = b"invalid \xff\xfe utf8\n";
        let reader = Cursor::new(&input[..]);
        let mut out = Vec::new();

        pump(reader, &mut out).await.expect("pump");
        let output = String::from_utf8(out).expect("utf8");

        assert!(output.contains("invalid"));
        assert!(output.contains('\u{FFFD}'));
    }
}
