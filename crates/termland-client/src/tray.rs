//! Global system-tray session manager (StatusNotifierItem via ksni).
//!
//! One icon for a Termland host: lists resumable sessions and lets you resume,
//! close, or start new ones. Each action shells out to this same binary in its
//! normal (windowed) mode, so the tray stays a thin manager over the existing
//! Rust client engine.

use anyhow::Result;
use std::process::Command;
use std::time::Duration;

use crate::connection::{self, ConnectParams};
use termland_protocol::SessionInfo;

struct TermlandTray {
    server: String,
    ssh: bool,
    sessions: Vec<SessionInfo>,
    error: Option<String>,
}

impl TermlandTray {
    /// Launch this same executable in windowed mode with extra args, targeting
    /// the configured host.
    fn spawn_client(&self, extra: &[&str]) {
        let exe = std::env::current_exe().unwrap_or_else(|_| "termland-client".into());
        let mut cmd = Command::new(exe);
        if self.ssh {
            cmd.arg("--ssh");
        }
        for a in extra {
            cmd.arg(a);
        }
        cmd.arg(&self.server);
        if let Err(e) = cmd.spawn() {
            tracing::error!("failed to launch client: {e}");
        }
    }
}

impl ksni::Tray for TermlandTray {
    fn icon_name(&self) -> String {
        "video-display".into()
    }
    fn title(&self) -> String {
        "Termland".into()
    }
    fn id(&self) -> String {
        "termland".into()
    }
    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: format!("Termland — {}", self.server),
            description: match &self.error {
                Some(e) => format!("offline: {e}"),
                None => format!("{} session(s)", self.sessions.len()),
            },
            icon_name: "video-display".into(),
            icon_pixmap: Vec::new(),
        }
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        let mut items: Vec<ksni::MenuItem<Self>> = Vec::new();

        items.push(
            StandardItem {
                label: format!("Termland — {}", self.server),
                enabled: false,
                ..Default::default()
            }
            .into(),
        );
        items.push(MenuItem::Separator);

        if let Some(err) = &self.error {
            items.push(
                StandardItem {
                    label: format!("⚠ offline: {err}"),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        } else if self.sessions.is_empty() {
            items.push(
                StandardItem {
                    label: "No resumable sessions".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        } else {
            for s in &self.sessions {
                let resume_id = s.session_id.clone();
                let close_id = s.session_id.clone();
                items.push(
                    SubMenu {
                        label: format!("{} — {} ({}×{})", s.session_id, s.mode, s.width, s.height),
                        submenu: vec![
                            StandardItem {
                                label: "Resume".into(),
                                activate: Box::new(move |t: &mut Self| {
                                    t.spawn_client(&["--attach", &resume_id]);
                                }),
                                ..Default::default()
                            }
                            .into(),
                            StandardItem {
                                label: "Close session".into(),
                                activate: Box::new(move |t: &mut Self| {
                                    // Shell out (like resume) so we don't nest a
                                    // runtime inside the tray's async event loop.
                                    t.spawn_client(&["--close", &close_id]);
                                }),
                                ..Default::default()
                            }
                            .into(),
                        ],
                        ..Default::default()
                    }
                    .into(),
                );
            }
        }

        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "New desktop session".into(),
                activate: Box::new(|t: &mut Self| t.spawn_client(&[])),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: "Quit tray".into(),
                activate: Box::new(|_| std::process::exit(0)),
                ..Default::default()
            }
            .into(),
        );
        items
    }
}

/// Run the tray until quit. Blocks; polls the host for sessions every few
/// seconds and updates the menu.
pub fn run(server: String, ssh: bool, params: ConnectParams) -> Result<()> {
    tracing::info!("Starting Termland tray for {server}");
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        use ksni::TrayMethods;
        let tray = TermlandTray {
            server: server.clone(),
            ssh,
            sessions: Vec::new(),
            error: None,
        };
        let handle = tray
            .spawn()
            .await
            .map_err(|e| anyhow::anyhow!("failed to register tray icon: {e}"))?;

        loop {
            let result = connection::fetch_sessions(&server, ssh, &params).await;
            handle
                .update(|t: &mut TermlandTray| match &result {
                    Ok(s) => {
                        t.sessions = s.clone();
                        t.error = None;
                    }
                    Err(e) => {
                        t.error = Some(e.to_string());
                    }
                })
                .await;
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    })
}
