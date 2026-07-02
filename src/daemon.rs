use crate::context::json_string;
use crate::store::Store;
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub(crate) const DEFAULT_DAEMON_ADDR: &str = "127.0.0.1:5874";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DaemonConfig {
    pub addr: String,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            addr: DEFAULT_DAEMON_ADDR.to_string(),
        }
    }
}

pub(crate) async fn serve(config: DaemonConfig) -> Result<(), String> {
    let listener = TcpListener::bind(&config.addr)
        .await
        .map_err(|error| format!("failed to bind daemon to {}: {error}", config.addr))?;
    let local_addr = listener.local_addr().map_err(|error| error.to_string())?;
    println!("Hugr daemon listening on http://{local_addr}");

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, peer_addr) = accepted.map_err(|error| error.to_string())?;
                tokio::spawn(async move {
                    if let Err(error) = handle_client(stream, peer_addr).await {
                        eprintln!("daemon request failed: {error}");
                    }
                });
            }
            signal = tokio::signal::ctrl_c() => {
                signal.map_err(|error| error.to_string())?;
                println!("Hugr daemon shutting down");
                return Ok(());
            }
        }
    }
}

async fn handle_client(mut stream: TcpStream, peer_addr: SocketAddr) -> Result<(), String> {
    let mut buffer = [0_u8; 8192];
    let bytes_read = stream
        .read(&mut buffer)
        .await
        .map_err(|error| error.to_string())?;
    if bytes_read == 0 {
        return Ok(());
    }

    let request = String::from_utf8_lossy(&buffer[..bytes_read]);
    let response = response_for_request(&request, peer_addr);
    stream
        .write_all(response.as_bytes())
        .await
        .map_err(|error| error.to_string())?;
    stream.shutdown().await.map_err(|error| error.to_string())
}

fn response_for_request(request: &str, peer_addr: SocketAddr) -> String {
    let Some((method, path)) = request_line_parts(request) else {
        return http_response(400, "application/json", r#"{"error":"bad_request"}"#);
    };

    if method != "GET" {
        return http_response(405, "application/json", r#"{"error":"method_not_allowed"}"#);
    }

    match path {
        "/health" => http_response(200, "application/json", &render_health_json()),
        "/status" => http_response(200, "application/json", &render_status_json(peer_addr)),
        _ => http_response(404, "application/json", r#"{"error":"not_found"}"#),
    }
}

fn request_line_parts(request: &str) -> Option<(&str, &str)> {
    let mut parts = request.lines().next()?.split_whitespace();
    let method = parts.next()?;
    let path = parts.next()?;
    Some((method, path))
}

fn render_health_json() -> String {
    r#"{"status":"ok","service":"hugr-daemon"}"#.to_string()
}

fn render_status_json(peer_addr: SocketAddr) -> String {
    let store = Store::open_current();
    let current_dir = std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    format!(
        "{{\"status\":\"running\",\"service\":\"hugr-daemon\",\"peer_addr\":{},\"current_dir\":{},\"store_exists\":{},\"store_root\":{},\"storage\":{}}}",
        json_string(&peer_addr.to_string()),
        json_string(&current_dir),
        store.exists(),
        json_string(&store.root().display().to_string()),
        json_string(&store.storage_summary())
    )
}

fn http_response(status_code: u16, content_type: &str, body: &str) -> String {
    let reason = match status_code {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Internal Server Error",
    };

    format!(
        "HTTP/1.1 {status_code} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[cfg(test)]
mod tests {
    use super::{request_line_parts, response_for_request};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    #[test]
    fn parses_http_request_line() {
        let request = "GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n";

        assert_eq!(request_line_parts(request), Some(("GET", "/health")));
    }

    #[test]
    fn health_response_is_json() {
        let response = response_for_request(
            "GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n",
            local_peer_addr(),
        );

        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("Content-Type: application/json"));
        assert!(response.ends_with(r#"{"status":"ok","service":"hugr-daemon"}"#));
    }

    #[test]
    fn unknown_route_returns_not_found() {
        let response = response_for_request(
            "GET /missing HTTP/1.1\r\nHost: localhost\r\n\r\n",
            local_peer_addr(),
        );

        assert!(response.starts_with("HTTP/1.1 404 Not Found"));
        assert!(response.ends_with(r#"{"error":"not_found"}"#));
    }

    fn local_peer_addr() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49152)
    }
}
