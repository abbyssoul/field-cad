//! `McpConnections::count` has to reflect a *real* MCP session, not just be
//! plumbing that happens to type-check — this drives an actual client
//! handshake over a real TCP connection against `serve_http` and checks the
//! count on the other side, the way the desktop panel's indicator does.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use fieldcad_mcp::{McpConnections, McpServer, bind_http, serve_http};
use fieldcad_server::HeadlessServer;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn connections_reflects_a_real_client_session() {
    let connections = McpConnections::new();
    assert_eq!(connections.count(), Some(0), "nothing has connected yet");

    let listener = bind_http("127.0.0.1:0".parse().unwrap())
        .await
        .expect("binding an ephemeral loopback port succeeds");
    let addr = listener.local_addr().unwrap();

    let source = fieldcad_server::default_session().expect("default session builds");
    let server = McpServer::new(
        Arc::new(Mutex::new(HeadlessServer::new(source))),
        Arc::new(fieldcad_mcp::mcp_plugin_catalog),
    );

    let ct = CancellationToken::new();
    let serve_ct = ct.clone();
    let serve_connections = connections.clone();
    let serve_handle = tokio::spawn(async move {
        serve_http(listener, server, None, serve_connections, serve_ct).await
    });

    let mut stream = tokio::time::timeout(Duration::from_secs(2), connect_with_retry(addr))
        .await
        .expect("the listener accepts within 2s");

    let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"connections-test","version":"0"}}}"#;
    let request = format!(
        "POST /mcp HTTP/1.1\r\n\
         Host: localhost\r\n\
         Content-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\n\
         Content-Length: {}\r\n\
         \r\n\
         {body}",
        body.len(),
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("writing the initialize request succeeds");

    // The response either closes the connection once complete or keeps it
    // open as an SSE stream — read what's available for a bounded window
    // rather than waiting for EOF, so this can't hang either way.
    let mut response = Vec::new();
    let mut buffer = [0_u8; 4096];
    let _ = tokio::time::timeout(Duration::from_millis(500), async {
        loop {
            match stream.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(n) => response.extend_from_slice(&buffer[..n]),
            }
        }
    })
    .await;
    let response = String::from_utf8_lossy(&response);
    assert!(
        response.contains("200") && response.contains("protocolVersion"),
        "expected a successful initialize response, got: {response}"
    );

    assert_eq!(
        connections.count(),
        Some(1),
        "the session `initialize` just created should be visible"
    );

    ct.cancel();
    let _ = serve_handle.await;
}

/// The server task's `bind_http` already succeeded before this is called,
/// but the accept loop inside `serve_http` needs a moment to actually start
/// — a couple of retries avoids a flaky "connection refused" on a loaded
/// machine without an arbitrary fixed sleep.
async fn connect_with_retry(addr: std::net::SocketAddr) -> tokio::net::TcpStream {
    loop {
        match tokio::net::TcpStream::connect(addr).await {
            Ok(stream) => return stream,
            Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    }
}
