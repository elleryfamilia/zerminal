//! Recording shim: capture what `copilot` CLI sends to a fake IDE.
//!
//! Throwaway. Run via:
//!
//!     cargo run -p copilot_cli_ide --example recording_shim
//!
//! Then in another terminal, `cd` into a directory you wrote into the
//! shim's lockfile (printed at startup) and run `copilot`. Type `/ide`.
//! The shim logs every byte the CLI sends to stdout and to a log file.
//!
//! Replies with minimal-valid responses so the CLI stays alive long
//! enough to send tools/list, GET /mcp, etc. Auth is permissive: if the
//! Authorization header doesn't match our nonce we still log + reply 401.
//! Wait 30 seconds of idle and the shim exits.

use std::collections::HashMap;
use std::fs;
use std::io::{Read as _, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use copilot_cli_ide::lockfile::{Lockfile, copilot_state_dir, write_atomic};

const IDLE_SHUTDOWN: Duration = Duration::from_secs(120);
const READ_TIMEOUT: Duration = Duration::from_secs(2);

fn main() -> anyhow::Result<()> {
    let workspace = std::env::current_dir()?;
    let socket_dir = tempfile::tempdir_in(std::env::temp_dir())?;
    let socket_path = socket_dir.path().join("sock");
    let listener = UnixListener::bind(&socket_path)?;

    let nonce = uuid::Uuid::new_v4().to_string();
    let state_dir = copilot_state_dir()?;
    let lockfile = Lockfile::new(
        socket_path.to_string_lossy().into_owned(),
        &nonce,
        vec![workspace.clone()],
    );
    let _guard = write_atomic(&state_dir, &lockfile)?;

    let log_path = std::env::temp_dir().join("copilot-recording-shim.log");
    let log = Arc::new(Mutex::new(fs::File::create(&log_path)?));

    eprintln!("Recording shim ready.");
    eprintln!("  Lockfile : {}", _guard.path().display());
    eprintln!("  Socket   : {}", socket_path.display());
    eprintln!("  Log file : {}", log_path.display());
    eprintln!("  Workspace: {}", workspace.display());
    eprintln!("  Auth     : Nonce {nonce}");
    eprintln!();
    eprintln!("In another terminal:");
    eprintln!("    cd {} && copilot", workspace.display());
    eprintln!();
    eprintln!("Idle exit after {IDLE_SHUTDOWN:?}.");
    eprintln!();

    let last_activity = Arc::new(Mutex::new(Instant::now()));

    listener.set_nonblocking(true)?;
    loop {
        match listener.accept() {
            Ok((stream, _addr)) => {
                *last_activity.lock().unwrap() = Instant::now();
                let log = log.clone();
                let nonce = nonce.clone();
                let last_activity = last_activity.clone();
                thread::spawn(move || {
                    if let Err(error) = handle_connection(stream, &nonce, log, last_activity) {
                        eprintln!("connection error: {error:#}");
                    }
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if last_activity.lock().unwrap().elapsed() > IDLE_SHUTDOWN {
                    eprintln!("Idle. Exiting.");
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(error.into()),
        }
    }

    Ok(())
}

fn handle_connection(
    mut stream: UnixStream,
    nonce: &str,
    log: Arc<Mutex<fs::File>>,
    last_activity: Arc<Mutex<Instant>>,
) -> anyhow::Result<()> {
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    let mut request_count = 0usize;

    loop {
        let mut buf = Vec::with_capacity(4096);
        let mut tmp = [0u8; 4096];
        let mut headers_end = None;

        // Read until we see \r\n\r\n or EOF or timeout.
        loop {
            match stream.read(&mut tmp) {
                Ok(0) => {
                    if buf.is_empty() {
                        return Ok(());
                    }
                    break;
                }
                Ok(n) => {
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(idx) = find_subslice(&buf, b"\r\n\r\n") {
                        headers_end = Some(idx + 4);
                        break;
                    }
                    if buf.len() > 65536 {
                        anyhow::bail!("oversized headers, bailing");
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) => return Err(error.into()),
            }
        }

        let headers_end = match headers_end {
            Some(idx) => idx,
            None => return Ok(()),
        };

        // Parse Content-Length / Transfer-Encoding from headers.
        let header_bytes = &buf[..headers_end];
        let header_str = String::from_utf8_lossy(header_bytes).into_owned();
        let content_length = parse_content_length(&header_str);
        let chunked = extract_header(&header_str, "transfer-encoding")
            .map(|v| v.to_ascii_lowercase().contains("chunked"))
            .unwrap_or(false);

        // Read body. Decoded body replaces the raw chunked envelope so the
        // logged "Body" section shows actual JSON.
        let mut decoded_body: Vec<u8> = Vec::new();
        if chunked {
            // Read chunks: <hex>\r\n<bytes>\r\n... ending with 0\r\n\r\n.
            let mut buffered = buf[headers_end..].to_vec();
            loop {
                while find_subslice(&buffered, b"\r\n").is_none() {
                    let mut chunk = [0u8; 4096];
                    let n = stream.read(&mut chunk)?;
                    if n == 0 {
                        anyhow::bail!("EOF mid-chunk-size");
                    }
                    buffered.extend_from_slice(&chunk[..n]);
                }
                let crlf = find_subslice(&buffered, b"\r\n").unwrap();
                let size_line = std::str::from_utf8(&buffered[..crlf])?;
                let size_hex = size_line.split(';').next().unwrap_or("").trim();
                let size = usize::from_str_radix(size_hex, 16)?;
                buffered.drain(..crlf + 2);
                if size == 0 {
                    // Drain trailers until \r\n\r\n.
                    while find_subslice(&buffered, b"\r\n").is_none()
                        || (buffered.starts_with(b"\r\n") == false
                            && find_subslice(&buffered, b"\r\n").unwrap() != 0)
                    {
                        let mut chunk = [0u8; 4096];
                        let n = stream.read(&mut chunk)?;
                        if n == 0 {
                            break;
                        }
                        buffered.extend_from_slice(&chunk[..n]);
                        if buffered.starts_with(b"\r\n") {
                            break;
                        }
                    }
                    break;
                }
                while buffered.len() < size + 2 {
                    let mut chunk = [0u8; 4096];
                    let n = stream.read(&mut chunk)?;
                    if n == 0 {
                        anyhow::bail!("EOF mid-chunk-data");
                    }
                    buffered.extend_from_slice(&chunk[..n]);
                }
                decoded_body.extend_from_slice(&buffered[..size]);
                buffered.drain(..size + 2);
            }
        } else if let Some(len) = content_length {
            let already_have = buf.len() - headers_end;
            decoded_body.extend_from_slice(&buf[headers_end..]);
            if already_have < len {
                let remaining = len - already_have;
                let mut body_buf = vec![0u8; remaining];
                stream.read_exact(&mut body_buf)?;
                decoded_body.extend_from_slice(&body_buf);
            } else {
                decoded_body.truncate(len);
            }
        }
        // Replace the request buffer's body section with the decoded form so
        // logging is clean.
        buf.truncate(headers_end);
        buf.extend_from_slice(&decoded_body);

        request_count += 1;
        *last_activity.lock().unwrap() = Instant::now();

        // Log request: full bytes + parsed pretty form.
        {
            let mut log = log.lock().unwrap();
            let separator = "=".repeat(80);
            writeln!(log, "{separator}\n>>> REQUEST #{request_count} ({} bytes)", buf.len())?;
            writeln!(log, "{}", pretty_dump(&buf, headers_end, content_length))?;
            log.flush()?;
            eprintln!("{separator}");
            eprintln!(">>> REQUEST #{request_count} ({} bytes)", buf.len());
            eprintln!("{}", pretty_dump(&buf, headers_end, content_length));
        }

        // Decide response based on method + path + body.
        let request_line = header_str.lines().next().unwrap_or("");
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("");
        let path = parts.next().unwrap_or("");

        let auth = extract_header(&header_str, "authorization");
        let auth_ok = auth.as_deref() == Some(&format!("Nonce {nonce}"));

        let session_id_header = extract_header(&header_str, "x-copilot-session-id")
            .or_else(|| extract_header(&header_str, "mcp-session-id"))
            .unwrap_or_else(|| "shim-default".to_string());

        let response = if !auth_ok {
            build_response(401, "Unauthorized", "text/plain", b"unauthorized")
        } else if method == "POST" && path == "/mcp" {
            let body_str = std::str::from_utf8(&buf[headers_end..]).unwrap_or("");
            mock_post_response(body_str, &session_id_header)
        } else if method == "GET" && path == "/mcp" {
            // Open SSE stream, send one keepalive, then close so the CLI moves on.
            build_sse_initial()
        } else if method == "DELETE" && path == "/mcp" {
            build_response(200, "OK", "text/plain", b"")
        } else {
            build_response(404, "Not Found", "text/plain", b"not found")
        };

        // Log response.
        {
            let mut log = log.lock().unwrap();
            writeln!(log, "<<< RESPONSE #{request_count} ({} bytes)", response.len())?;
            writeln!(log, "{}", String::from_utf8_lossy(&response))?;
            log.flush()?;
            eprintln!("<<< RESPONSE #{request_count} ({} bytes)", response.len());
            eprintln!("{}", String::from_utf8_lossy(&response));
        }

        stream.write_all(&response)?;
        stream.flush()?;

        // For GET (SSE) we close after the initial response — easier to log.
        if method == "GET" {
            return Ok(());
        }

        // For DELETE the CLI usually closes the connection immediately.
        if method == "DELETE" {
            return Ok(());
        }

        // Otherwise loop for keep-alive and pick up the next request on the
        // same connection (which is itself useful data — does the CLI pipeline?).
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn parse_content_length(headers: &str) -> Option<usize> {
    for line in headers.lines() {
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                return value.trim().parse().ok();
            }
        }
    }
    None
}

fn extract_header(headers: &str, name: &str) -> Option<String> {
    for line in headers.lines() {
        if let Some((header_name, value)) = line.split_once(':') {
            if header_name.trim().eq_ignore_ascii_case(name) {
                return Some(value.trim().to_string());
            }
        }
    }
    None
}

fn pretty_dump(buf: &[u8], headers_end: usize, content_length: Option<usize>) -> String {
    let header_str = String::from_utf8_lossy(&buf[..headers_end]);
    let body = &buf[headers_end..];
    let mut out = String::new();
    out.push_str("--- Headers ---\n");
    out.push_str(&header_str);
    if !body.is_empty() {
        out.push_str("--- Body (");
        out.push_str(&body.len().to_string());
        out.push_str(" bytes");
        if let Some(cl) = content_length {
            if cl != body.len() {
                out.push_str(&format!(", Content-Length={cl}"));
            }
        }
        out.push_str(") ---\n");
        match std::str::from_utf8(body) {
            Ok(s) => {
                // Try to pretty-print JSON.
                match serde_json::from_str::<serde_json::Value>(s) {
                    Ok(v) => out.push_str(&serde_json::to_string_pretty(&v).unwrap_or_default()),
                    Err(_) => out.push_str(s),
                }
            }
            Err(_) => out.push_str(&format!("<{} non-utf8 bytes>", body.len())),
        }
        out.push('\n');
    }
    out
}

fn build_response(status: u16, reason: &str, content_type: &str, body: &[u8]) -> Vec<u8> {
    let mut response = Vec::new();
    response.extend_from_slice(format!("HTTP/1.1 {status} {reason}\r\n").as_bytes());
    response.extend_from_slice(format!("Content-Type: {content_type}\r\n").as_bytes());
    response.extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
    response.extend_from_slice(b"Connection: keep-alive\r\n");
    response.extend_from_slice(b"\r\n");
    response.extend_from_slice(body);
    response
}

fn shim_tool_descriptors() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "name": "get_vscode_info",
            "description": "Get IDE info.",
            "inputSchema": { "type": "object", "properties": {} },
        }),
        serde_json::json!({
            "name": "get_selection",
            "description": "Get the current text selection from the active editor.",
            "inputSchema": { "type": "object", "properties": {} },
        }),
        serde_json::json!({
            "name": "get_diagnostics",
            "description": "Get diagnostics.",
            "inputSchema": {
                "type": "object",
                "properties": { "uri": { "type": "string" } },
            },
        }),
        serde_json::json!({
            "name": "open_diff",
            "description": "Open diff.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "original_file_path": { "type": "string" },
                    "new_file_contents": { "type": "string" },
                    "tab_name": { "type": "string" },
                },
                "required": ["original_file_path", "new_file_contents", "tab_name"],
            },
        }),
        serde_json::json!({
            "name": "close_diff",
            "description": "Close diff.",
            "inputSchema": {
                "type": "object",
                "properties": { "tab_name": { "type": "string" } },
                "required": ["tab_name"],
            },
        }),
        serde_json::json!({
            "name": "update_session_name",
            "description": "Update session name.",
            "inputSchema": {
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"],
            },
        }),
    ]
}

fn shim_tool_call_result(body: &serde_json::Value) -> serde_json::Value {
    let name = body
        .pointer("/params/name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    eprintln!("\n*** SHIM: tools/call invoked: {name} ***\n");
    let payload = match name {
        "get_vscode_info" => serde_json::json!({
            "version": "shim-0.0.1",
            "appName": "ZerminalShim",
            "appRoot": "/tmp/shim",
            "language": "en",
            "machineId": "shim",
            "sessionId": "shim",
            "uriScheme": "zerminal-shim",
            "shell": "/bin/zsh",
        }),
        "get_selection" => serde_json::json!({
            // Match Copilot's get_selection shape exactly. Returns an active
            // editor with no text selected (the "I'm in this file but
            // haven't selected anything" scenario we're investigating).
            "text": "",
            "filePath": "/tmp/shim-experiment/install.sh",
            "fileUrl": "file:///tmp/shim-experiment/install.sh",
            "selection": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 0, "character": 0 },
                "isEmpty": true,
            },
            "current": true,
        }),
        "get_diagnostics" => serde_json::json!([]),
        "update_session_name" => serde_json::json!({ "success": true }),
        _ => serde_json::json!({ "ack": true }),
    };
    serde_json::json!({
        "content": [{ "type": "text", "text": serde_json::to_string_pretty(&payload).unwrap_or_default() }]
    })
}

fn build_sse_initial() -> Vec<u8> {
    let mut response = Vec::new();
    response.extend_from_slice(b"HTTP/1.1 200 OK\r\n");
    response.extend_from_slice(b"Content-Type: text/event-stream\r\n");
    response.extend_from_slice(b"Cache-Control: no-cache\r\n");
    response.extend_from_slice(b"Connection: keep-alive\r\n");
    response.extend_from_slice(b"\r\n");
    response.extend_from_slice(b": shim-keepalive\r\n\r\n");
    response
}

fn mock_post_response(body: &str, session_id: &str) -> Vec<u8> {
    // Try to extract id and method from JSON-RPC body.
    let parsed: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return build_response(400, "Bad Request", "text/plain", b"bad json"),
    };
    let method = parsed.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let id = parsed.get("id").cloned();

    // Notification (no id): 202 empty.
    if id.is_none() || id.as_ref() == Some(&serde_json::Value::Null) {
        return build_response(202, "Accepted", "text/plain", b"");
    }

    // Construct a minimal-valid JSON-RPC response.
    // Echo back the protocolVersion the client requested so we don't trigger
    // a downgrade-rejection in the CLI's MCP SDK.
    let requested_version = parsed
        .pointer("/params/protocolVersion")
        .and_then(|v| v.as_str())
        .unwrap_or("2025-06-18")
        .to_string();
    let result = match method {
        "initialize" => serde_json::json!({
            "protocolVersion": requested_version,
            "serverInfo": { "name": "zerminal-recording-shim", "version": "0.0.1" },
            "capabilities": { "tools": { "listChanged": false } }
        }),
        "tools/list" => serde_json::json!({ "tools": shim_tool_descriptors() }),
        "tools/call" => shim_tool_call_result(&parsed),
        _ => serde_json::json!({}),
    };
    let response_json = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    });
    let mut headers = HashMap::new();
    // Echo back both naming conventions the CLI / vscode-copilot-chat use.
    headers.insert("mcp-session-id".to_string(), session_id.to_string());
    headers.insert("x-copilot-session-id".to_string(), session_id.to_string());
    let body_bytes = serde_json::to_vec(&response_json).unwrap_or_default();

    let mut response = Vec::new();
    response.extend_from_slice(b"HTTP/1.1 200 OK\r\n");
    response.extend_from_slice(b"Content-Type: application/json\r\n");
    response.extend_from_slice(format!("Content-Length: {}\r\n", body_bytes.len()).as_bytes());
    for (name, value) in &headers {
        response.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    response.extend_from_slice(b"Connection: keep-alive\r\n");
    response.extend_from_slice(b"\r\n");
    response.extend_from_slice(&body_bytes);
    response
}
