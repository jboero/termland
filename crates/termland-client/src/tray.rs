//! System-tray icons (StatusNotifierItem via ksni).
//!
//! Two of them:
//!  - `run`: `--tray <host>`, one icon for a single Termland host that lists
//!    its resumable sessions and lets you resume, close, or start new ones.
//!  - `spawn_manager_tray`: the icon `--manager` keeps while it runs, which
//!    reopens the manager window and starts sessions from saved profiles.
//!
//! Every action shells out to this same binary in its normal (windowed) mode,
//! so the tray stays a thin manager over the existing Rust client engine.

use anyhow::Result;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use crate::connection::{self, ConnectParams};
use crate::desktop;
use crate::manager::HostEvent;
use crate::profile::{self, Profile};
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
        desktop::tray_icon_name()
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
            icon_name: desktop::tray_icon_name(),
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
                // Unlike spawn_client, this must not append a server address
                // — the manager window manages its own multi-host profiles.
                label: "Manage profiles…".into(),
                activate: Box::new(|_: &mut Self| crate::manager::spawn_manager_window()),
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

/// The tray icon owned by `--manager`. Left click reopens the manager window;
/// the menu starts a session from any saved profile without opening it.
///
/// It deliberately does not list each host's resumable sessions the way
/// `TermlandTray` does: that would mean polling every saved host in the
/// background — over SSH, for SSH profiles — for as long as the manager sits
/// in the tray. The window polls only the selected host, and only while open.
struct ManagerTray {
    host: std::sync::mpsc::Sender<HostEvent>,
    profiles: Vec<Profile>,
}

impl ksni::Tray for ManagerTray {
    fn id(&self) -> String {
        desktop::APP_ID.into()
    }
    fn title(&self) -> String {
        "Termland".into()
    }
    fn icon_name(&self) -> String {
        desktop::tray_icon_name()
    }
    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "Termland".into(),
            description: "Session manager — click to open".into(),
            icon_name: desktop::tray_icon_name(),
            icon_pixmap: Vec::new(),
        }
    }
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.host.send(HostEvent::Show);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        let mut items: Vec<ksni::MenuItem<Self>> = vec![
            StandardItem {
                label: "Open Session Manager".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.host.send(HostEvent::Show);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
        ];

        if self.profiles.is_empty() {
            items.push(
                StandardItem {
                    label: "No saved profiles".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }
        for p in &self.profiles {
            let profile = p.clone();
            items.push(
                SubMenu {
                    label: p.display_name.clone(),
                    submenu: vec![
                        StandardItem {
                            label: "New session".into(),
                            activate: Box::new(move |_: &mut Self| {
                                crate::manager::spawn_client_for_profile(&profile, &[]);
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

        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Quit Termland".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.host.send(HostEvent::Quit);
                }),
                ..Default::default()
            }
            .into(),
        );
        items
    }
}

/// Register the manager's tray icon on a background thread. Returns a handle
/// for telling the tray the saved profiles changed, or `None` when there is no
/// StatusNotifierItem host to show it (e.g. GNOME without the AppIndicator
/// extension) — the caller then behaves as a plain window.
///
/// `wait_for_host` is for starting at login, when the panel's
/// StatusNotifierWatcher may not be up yet: registration is assumed to
/// succeed, and ksni attaches the icon whenever the watcher appears.
pub fn spawn_manager_tray(
    host: std::sync::mpsc::Sender<HostEvent>,
    wait_for_host: bool,
) -> Option<Arc<tokio::sync::Notify>> {
    let profiles_changed = Arc::new(tokio::sync::Notify::new());
    let changed = profiles_changed.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(e) => {
                tracing::warn!("tray: failed to start runtime: {e}");
                let _ = ready_tx.send(false);
                return;
            }
        };
        rt.block_on(async move {
            use ksni::TrayMethods;
            let tray = ManagerTray { host, profiles: profile::load() };
            let handle = match tray.assume_sni_available(wait_for_host).spawn().await {
                Ok(h) => h,
                Err(e) => {
                    tracing::info!("no system tray available ({e}); closing the window will quit");
                    let _ = ready_tx.send(false);
                    return;
                }
            };
            let _ = ready_tx.send(true);
            loop {
                changed.notified().await;
                let profiles = profile::load();
                handle.update(|t: &mut ManagerTray| t.profiles = profiles).await;
            }
        });
    });

    match ready_rx.recv() {
        Ok(true) => Some(profiles_changed),
        _ => None,
    }
}
