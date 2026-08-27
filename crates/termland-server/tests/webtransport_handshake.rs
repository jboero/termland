//! Live check of the browser WebTransport listener.
//!
//! A browser is not required to exercise this: the parts that could break are
//! the HTTP/3 session establishment, the origin check, and whether the control
//! stream really reaches `handle_session`. A Rust WebTransport client
//! exercises all three, and unlike a browser it can be told exactly which
//! `Origin` to send — including none, which a browser can never do.
//!
//! What this deliberately does not prove is that a *browser* interoperates.
//! That needs a browser, and is covered separately.

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use anyhow::Result;
use futures::{SinkExt, StreamExt};
use tokio_util::codec::Framed;
use wtransport::{ClientConfig, Endpoint};

use termland_protocol::{Hello, Message, TermlandCodec, PROTOCOL_VERSION};

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Start a server with the WebTransport listener on `wt_port`.
///
/// An explicit --port matters here for the same reason as the QUIC tests: the
/// default collides with any termland-server the developer already has
/// running, and the bind failure kills the child before the listener starts.
fn spawn_server(tcp_port: u16, wt_port: u16, origins: &[&str]) -> ChildGuard {
    let bin = env!("CARGO_BIN_EXE_termland-server");
    let mut cmd = Command::new(bin);
    cmd.args(["--bind", "127.0.0.1", "--port", &tcp_port.to_string()])
        .args(["--webtransport", "--webtransport-port", &wt_port.to_string()]);
    for o in origins {
        cmd.args(["--webtransport-origin", o]);
    }
    let child = cmd
        .env("RUST_LOG", "info")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn termland-server");
    ChildGuard(child)
}

fn client_endpoint() -> Endpoint<wtransport::endpoint::endpoint_side::Client> {
    // The server generates a short-lived self-signed certificate when none is
    // configured. A browser would pin it via serverCertificateHashes; a test
    // client has no such ceremony to perform.
    let config = ClientConfig::builder()
        .with_bind_default()
        .with_no_cert_validation()
        .build();
    Endpoint::client(config).expect("client endpoint")
}

/// The whole point: a WebTransport session reaches the real control path and
/// speaks the existing protocol, with no second implementation involved.
#[tokio::test]
#[ignore = "spawns a real server; run with --ignored"]
async fn allowed_origin_reaches_the_real_control_stream() -> Result<()> {
    let _server = spawn_server(28801, 28802, &["https://app.example"]);
    tokio::time::sleep(Duration::from_millis(1200)).await;

    let endpoint = client_endpoint();
    let connection = endpoint
        .connect(
            wtransport::endpoint::ConnectOptions::builder("https://127.0.0.1:28802/termland")
                .add_header("origin", "https://app.example")
                .build(),
        )
        .await?;

    // Same contract as raw QUIC: the client opens one bidi stream and it
    // carries the control plane.
    let (send, recv) = connection.open_bi().await?.await?;
    let mut framed = Framed::new(tokio::io::join(recv, send), TermlandCodec);

    framed
        .send(Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            client_name: "webtransport-test".into(),
        }))
        .await?;

    let reply = tokio::time::timeout(Duration::from_secs(10), framed.next())
        .await
        .expect("timed out waiting for HelloAck")
        .expect("stream closed before HelloAck")?;

    match reply {
        Message::HelloAck(ack) => {
            assert_eq!(
                ack.protocol_version, PROTOCOL_VERSION,
                "the browser transport must speak the same protocol version as every other one",
            );
            assert!(!ack.server_name.is_empty());
        }
        other => panic!("expected HelloAck, got {:?}", other.message_id()),
    }
    Ok(())
}

/// An origin that is not on the allowlist must be refused before any session
/// exists. This is the check that stops an arbitrary web page reaching a
/// Termland server on the user's network.
#[tokio::test]
#[ignore = "spawns a real server; run with --ignored"]
async fn disallowed_origin_is_rejected() -> Result<()> {
    let _server = spawn_server(28803, 28804, &["https://app.example"]);
    tokio::time::sleep(Duration::from_millis(1200)).await;

    let endpoint = client_endpoint();
    let result = endpoint
        .connect(
            wtransport::endpoint::ConnectOptions::builder("https://127.0.0.1:28804/termland")
                .add_header("origin", "https://evil.example")
                .build(),
        )
        .await;

    assert!(
        result.is_err(),
        "a disallowed origin established a session — any web page could then \
         reach this server",
    );
    Ok(())
}

/// With no origins configured, every browser request is refused. The default
/// has to be closed: a server started with just --webtransport must not be
/// reachable from any page on the internet.
#[tokio::test]
#[ignore = "spawns a real server; run with --ignored"]
async fn empty_allowlist_refuses_browser_origins() -> Result<()> {
    let _server = spawn_server(28805, 28806, &[]);
    tokio::time::sleep(Duration::from_millis(1200)).await;

    let endpoint = client_endpoint();
    let result = endpoint
        .connect(
            wtransport::endpoint::ConnectOptions::builder("https://127.0.0.1:28806/termland")
                .add_header("origin", "https://anything.example")
                .build(),
        )
        .await;

    assert!(
        result.is_err(),
        "a browser origin was accepted with an empty allowlist",
    );
    Ok(())
}

/// A CONNECT to the wrong path must not become a session. The listener logs
/// `request.path()`; logging is not a check.
#[tokio::test]
#[ignore = "spawns a real server; run with --ignored"]
async fn unknown_path_is_rejected() -> Result<()> {
    let _server = spawn_server(28807, 28808, &["https://app.example"]);
    tokio::time::sleep(Duration::from_millis(1200)).await;

    let endpoint = client_endpoint();
    let result = endpoint
        .connect(
            wtransport::endpoint::ConnectOptions::builder("https://127.0.0.1:28808/not-termland")
                .add_header("origin", "https://app.example")
                .build(),
        )
        .await;

    assert!(
        result.is_err(),
        "a session was established on a path other than /termland",
    );
    Ok(())
}

/// SessionList after HelloAck must work on WebTransport the same as on TCP:
/// no compositor, no video stream, just the control plane.
#[tokio::test]
#[ignore = "spawns a real server; run with --ignored"]
async fn hello_then_session_list_on_webtransport() -> Result<()> {
    let _server = spawn_server(28809, 28810, &["https://app.example"]);
    tokio::time::sleep(Duration::from_millis(1200)).await;

    let endpoint = client_endpoint();
    let connection = endpoint
        .connect(
            wtransport::endpoint::ConnectOptions::builder("https://127.0.0.1:28810/termland")
                .add_header("origin", "https://app.example")
                .build(),
        )
        .await?;

    let (send, recv) = connection.open_bi().await?.await?;
    let mut framed = Framed::new(tokio::io::join(recv, send), TermlandCodec);

    framed
        .send(Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            client_name: "webtransport-list".into(),
        }))
        .await?;

    let hello_ack = tokio::time::timeout(Duration::from_secs(10), framed.next())
        .await
        .expect("timed out waiting for HelloAck")
        .expect("stream closed before HelloAck")?;
    assert!(matches!(hello_ack, Message::HelloAck(_)));

    framed
        .send(Message::SessionList(termland_protocol::SessionList {}))
        .await?;

    let list = tokio::time::timeout(Duration::from_secs(10), framed.next())
        .await
        .expect("timed out waiting for SessionListResult")
        .expect("stream closed before SessionListResult")?;
    match list {
        Message::SessionListResult(_) => {}
        other => panic!("expected SessionListResult, got {:?}", other.message_id()),
    }
    Ok(())
}

/// The browser client starts Ping after HelloAck, before SessionCreate. That
/// keepalive must not tear the control stream down — a regression here is
/// exactly "Connect works for five seconds, then STOP_SENDING".
#[tokio::test]
#[ignore = "spawns a real server; run with --ignored"]
async fn ping_before_session_create_gets_pong() -> Result<()> {
    let _server = spawn_server(28811, 28812, &["https://app.example"]);
    tokio::time::sleep(Duration::from_millis(1200)).await;

    let endpoint = client_endpoint();
    let connection = endpoint
        .connect(
            wtransport::endpoint::ConnectOptions::builder("https://127.0.0.1:28812/termland")
                .add_header("origin", "https://app.example")
                .build(),
        )
        .await?;

    let (send, recv) = connection.open_bi().await?.await?;
    let mut framed = Framed::new(tokio::io::join(recv, send), TermlandCodec);

    framed
        .send(Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            client_name: "webtransport-ping".into(),
        }))
        .await?;

    let hello_ack = tokio::time::timeout(Duration::from_secs(10), framed.next())
        .await
        .expect("timed out waiting for HelloAck")
        .expect("stream closed before HelloAck")?;
    assert!(matches!(hello_ack, Message::HelloAck(_)));

    framed
        .send(Message::Ping(termland_protocol::Ping {
            timestamp_us: 42,
        }))
        .await?;

    let pong = tokio::time::timeout(Duration::from_secs(10), framed.next())
        .await
        .expect("timed out waiting for Pong — Ping before SessionCreate was likely rejected")
        .expect("stream closed before Pong — Ping must be accepted in the session-control loop")?;
    match pong {
        Message::Pong(p) => assert_eq!(p.timestamp_us, 42),
        other => panic!("expected Pong, got {:?}", other.message_id()),
    }

    // And the stream must still accept SessionList afterwards.
    framed
        .send(Message::SessionList(termland_protocol::SessionList {}))
        .await?;
    let list = tokio::time::timeout(Duration::from_secs(10), framed.next())
        .await
        .expect("timed out waiting for SessionListResult after Ping")
        .expect("stream closed after Ping")?;
    assert!(
        matches!(list, Message::SessionListResult(_)),
        "expected SessionListResult after Ping, got {:?}",
        list.message_id()
    );
    Ok(())
}
