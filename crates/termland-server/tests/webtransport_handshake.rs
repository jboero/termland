//! Live check of the WebTransport listener: HTTP/3 session, origin/path
//! checks, and that the control stream reaches `handle_session`.
//!
//! A Rust WebTransport client is not a browser; `web/test-browser.sh` covers
//! that. These tests can send a chosen `Origin`, including none.

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use anyhow::Result;
use futures::{SinkExt, StreamExt};
use tokio_util::codec::Framed;
use wtransport::{ClientConfig, Endpoint};

use termland_protocol::{Hello, Message, PROTOCOL_VERSION, TermlandCodec};

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_server(tcp_port: u16, wt_port: u16, origins: &[&str]) -> ChildGuard {
    let bin = env!("CARGO_BIN_EXE_termland-server");
    let mut cmd = Command::new(bin);
    cmd.args(["--bind", "127.0.0.1", "--port", &tcp_port.to_string()])
        .args([
            "--webtransport",
            "--webtransport-port",
            &wt_port.to_string(),
        ]);
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
    let config = ClientConfig::builder()
        .with_bind_default()
        .with_no_cert_validation()
        .build();
    Endpoint::client(config).expect("client endpoint")
}

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

    let (send, recv) = connection.open_bi().await?.await?;
    let mut framed = Framed::new(tokio::io::join(recv, send), TermlandCodec);

    framed
        .send(Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            client_name: "webtransport-test".into(),
        }))
        .await?;

    let hello_ack = tokio::time::timeout(Duration::from_secs(10), framed.next())
        .await
        .expect("timed out waiting for HelloAck")
        .expect("stream closed before HelloAck")?;
    match hello_ack {
        Message::HelloAck(ack) => {
            assert_eq!(ack.protocol_version, PROTOCOL_VERSION);
            assert!(!ack.server_name.is_empty());
        }
        other => panic!("expected HelloAck, got {:?}", other.message_id()),
    }

    // The browser client starts Ping after HelloAck, before SessionCreate.
    framed
        .send(Message::Ping(termland_protocol::Ping { timestamp_us: 42 }))
        .await?;
    let pong = tokio::time::timeout(Duration::from_secs(10), framed.next())
        .await
        .expect("timed out waiting for Pong")
        .expect("stream closed before Pong")?;
    match pong {
        Message::Pong(p) => assert_eq!(p.timestamp_us, 42),
        other => panic!("expected Pong, got {:?}", other.message_id()),
    }

    framed
        .send(Message::SessionList(termland_protocol::SessionList {}))
        .await?;
    let list = tokio::time::timeout(Duration::from_secs(10), framed.next())
        .await
        .expect("timed out waiting for SessionListResult")
        .expect("stream closed before SessionListResult")?;
    assert!(
        matches!(list, Message::SessionListResult(_)),
        "expected SessionListResult after Ping, got {:?}",
        list.message_id()
    );
    Ok(())
}

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
    assert!(result.is_err(), "a disallowed origin established a session");
    Ok(())
}

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
        "a session was established on a path other than /termland"
    );
    Ok(())
}
