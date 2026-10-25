//! Line classifier and filtering pump for supervised tunnel child processes.
//!
//! - OpenSSH invoked with `-o LogLevel=DEBUG` emits the lines required to verify
//!   port forwards and track active connections, but also thousands of internal
//!   channel bookkeeping lines (`debug1: channel free`, `debug1: kex`, etc.).
//! - `gcloud compute start-iap-tunnel` emits routine per-connection notices (`INFO: [N]`)
//!   and a known teardown traceback (`AttributeError: 'NoneType' object has no attribute 'close'`)
//!   when a probe or client disconnects.
//!
//! This module classifies and filters those lines, timestamping the kept lines.

use std::io::Write;
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

/// Decide whether a log line from a child process carries useful information.
///
/// Deny-list based:
/// - Drops lines starting with `debug2:` or `debug3:`.
/// - Drops lines starting with `debug1:` unless they match explicit forwarding/connection notices.
/// - Drops routine `INFO: [N]` per-connection notices from `gcloud start-iap-tunnel`.
/// - Drops the known Google Cloud SDK WebSocket teardown lines.
/// - Keeps all other lines (errors, warnings, gcloud notices, real tracebacks).
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
    if trimmed.starts_with("INFO: [")
        && (trimmed.contains("Connecting with URL")
            || trimmed.contains("Client closed connection")
            || trimmed.contains("Received WebSocket Close")
            || trimmed.contains("Error while receiving from WebSocket"))
    {
        return false;
    }
    if trimmed.contains("iap_tunnel_lightweight_websocket")
        || trimmed.contains("iap_tunnel_websocket_helper")
        || trimmed.contains("AttributeError: 'NoneType' object has no attribute 'close'")
        || trimmed.contains("OSError: [Errno 9] Bad file descriptor")
        || trimmed.contains("self._wait_for_socket_to_ready")
        || trimmed.contains("self._websocket.run_forever")
        || trimmed.contains("self.sock.close()")
        || trimmed.contains("select.select([self.sock]")
        || trimmed.contains("During handling of the above exception, another exception occurred:")
    {
        return false;
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

/// A stateful filter that buffers Python tracebacks and discards known IAP teardown errors
/// while preserving genuine unhandled application tracebacks.
#[derive(Default)]
pub struct Filter {
    traceback_buf: Vec<String>,
    in_traceback: bool,
}

impl Filter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn process_line(&mut self, line: &str) -> Vec<String> {
        let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
        if self.in_traceback {
            if trimmed.starts_with("  File ")
                || trimmed.starts_with("    ")
                || trimmed.starts_with("Traceback ")
                || trimmed.starts_with("During handling of ")
                || trimmed.is_empty()
                || trimmed.starts_with("OSError:")
                || trimmed.starts_with("AttributeError:")
                || trimmed.starts_with("TypeError:")
                || trimmed.starts_with("ValueError:")
            {
                self.traceback_buf.push(trimmed.to_string());
                return Vec::new();
            }

            // End of traceback encountered
            self.in_traceback = false;
            let is_iap_teardown = self.traceback_buf.iter().any(|l| {
                l.contains("iap_tunnel_lightweight_websocket")
                    || l.contains("iap_tunnel_websocket_helper")
            });
            let mut out = Vec::new();
            if !is_iap_teardown {
                for tb_line in self.traceback_buf.drain(..) {
                    if keep(&tb_line) {
                        out.push(format_line(&tb_line));
                    }
                }
            } else {
                self.traceback_buf.clear();
            }
            if keep(trimmed) {
                out.push(format_line(trimmed));
            }
            return out;
        }

        if trimmed == "Traceback (most recent call last):" {
            self.in_traceback = true;
            self.traceback_buf.clear();
            self.traceback_buf.push(trimmed.to_string());
            return Vec::new();
        }

        if keep(trimmed) {
            vec![format_line(trimmed)]
        } else {
            Vec::new()
        }
    }

    pub fn flush_eof(&mut self) -> Vec<String> {
        if self.in_traceback {
            self.in_traceback = false;
            let is_iap_teardown = self.traceback_buf.iter().any(|l| {
                l.contains("iap_tunnel_lightweight_websocket")
                    || l.contains("iap_tunnel_websocket_helper")
            });
            if !is_iap_teardown {
                return self
                    .traceback_buf
                    .drain(..)
                    .filter(|l| keep(l))
                    .map(|l| format_line(&l))
                    .collect();
            }
            self.traceback_buf.clear();
        }
        Vec::new()
    }
}

/// Read lines from an async reader, filter via [`Filter`], and write formatted lines to `out`.
pub async fn pump<R: AsyncBufRead + Unpin, W: Write>(
    mut reader: R,
    out: &mut W,
) -> std::io::Result<()> {
    let mut filter = Filter::new();
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let bytes_read = reader.read_until(b'\n', &mut buf).await?;
        if bytes_read == 0 {
            break;
        }
        let line_str = String::from_utf8_lossy(&buf);
        for formatted in filter.process_line(&line_str) {
            out.write_all(formatted.as_bytes())?;
            out.flush()?;
        }
    }
    for formatted in filter.flush_eof() {
        out.write_all(formatted.as_bytes())?;
        out.flush()?;
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
        assert!(keep("WARNING: connection retry"));
        assert!(keep("ERROR: failed to connect"));
        assert!(keep("Traceback (most recent call last):"));
    }

    #[test]
    fn drops_routine_iap_info_lines() {
        assert!(!keep(
            "INFO: [0] Connecting with URL ['wss://tunnel.cloudproxy.app/v4/connect...']"
        ));
        assert!(!keep(
            "INFO: [0] Client closed connection from [('127.0.0.1', 59102)]."
        ));
        assert!(!keep("INFO: [0] Received WebSocket Close message [9: 9]."));
        assert!(!keep("INFO: [0] Error while receiving from WebSocket."));
    }

    #[test]
    fn drops_known_iap_websocket_teardown_lines() {
        assert!(!keep("  File \"/opt/homebrew/share/google-cloud-sdk/lib/googlecloudsdk/api_lib/compute/iap_tunnel_lightweight_websocket.py\", line 240, in run_forever"));
        assert!(!keep(
            "AttributeError: 'NoneType' object has no attribute 'close'"
        ));
        assert!(!keep("OSError: [Errno 9] Bad file descriptor"));
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

    #[tokio::test]
    async fn pump_drops_iap_websocket_teardown_tracebacks_and_info_lines() {
        let input = r#"Testing if tunnel connection works.
INFO: [-1] Connecting with URL ['wss://tunnel.cloudproxy.app/v4/connect?project=example-dev-1a2b&port=22&newWebsocket=True&zone=europe-west1-b&instance=bastion&interface=nic0']
Listening on port [2222].
INFO: [-1] Error while receiving from WebSocket.
Traceback (most recent call last):
  File "/opt/homebrew/share/google-cloud-sdk/lib/googlecloudsdk/api_lib/compute/iap_tunnel_lightweight_websocket.py", line 240, in run_forever
    self._wait_for_socket_to_ready(timeout=WEBSOCKET_RETRY_TIMEOUT_SECS)
OSError: [Errno 9] Bad file descriptor

During handling of the above exception, another exception occurred:

Traceback (most recent call last):
  File "/opt/homebrew/share/google-cloud-sdk/lib/googlecloudsdk/api_lib/compute/iap_tunnel_websocket_helper.py", line 269, in _ReceiveFromWebSocket
    self._websocket.run_forever(origin=TUNNEL_CLOUDPROXY_ORIGIN, sslopt=self._sslopt)
  File "/opt/homebrew/share/google-cloud-sdk/lib/googlecloudsdk/api_lib/compute/iap_tunnel_lightweight_websocket.py", line 253, in run_forever
    self.close()
  File "/opt/homebrew/share/google-cloud-sdk/lib/googlecloudsdk/api_lib/compute/iap_tunnel_lightweight_websocket.py", line 194, in close
    self.sock.close()
AttributeError: 'NoneType' object has no attribute 'close'
INFO: [0] Connecting with URL ['wss://tunnel.cloudproxy.app/v4/connect?project=example-dev-1a2b&port=22&newWebsocket=True&zone=europe-west1-b&instance=bastion&interface=nic0']
INFO: [0] Client closed connection from [('127.0.0.1', 59102)].
INFO: [0] Received WebSocket Close message [9: 9].
"#;
        let reader = Cursor::new(input.as_bytes());
        let mut out = Vec::new();
        pump(reader, &mut out).await.expect("pump");
        let output = String::from_utf8(out).expect("utf8");

        assert!(output.contains("Listening on port [2222]."));
        assert!(!output.contains("Traceback"));
        assert!(!output.contains("AttributeError"));
        assert!(!output.contains("INFO: ["));
    }

    #[tokio::test]
    async fn pump_preserves_real_exceptions() {
        let input = "Traceback (most recent call last):\n  File \"script.py\", line 10, in <module>\nValueError: invalid literal for int()\n";
        let reader = Cursor::new(input.as_bytes());
        let mut out = Vec::new();
        pump(reader, &mut out).await.expect("pump");
        let output = String::from_utf8(out).expect("utf8");

        assert!(output.contains("Traceback"));
        assert!(output.contains("ValueError: invalid literal for int()"));
    }
}
