//! System-tray icons (StatusNotifierItem via ksni).
//!
//! Two of them:
//!  - `run`: `--tray <host>`, one icon for a single Termland host that lists
//!    its resumable sessions and lets you resume, close, or start new ones.
//!  - `spawn_manager_tray`: the icon `--manager` keeps while it runs, which
//!    reopens the manager window, starts sessions from saved profiles, and
//!    lists each profile's resumable sessions when its menu is opened.
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
                        t.error = Some(format!("{e:#}"));
                    }
                })
                .await;
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    })
}

/// The tray icon owned by `--manager`. Left click reopens the manager window;
/// the menu starts a session from any saved profile, or resumes or closes one
/// of its sessions, without opening it.
///
/// Sessions are fetched when the menu is opened (`menu_about_to_show`), not
/// polled: polling every saved host in the background, over SSH for SSH
/// profiles, for as long as the manager sits in the tray, is what kept the
/// tray from listing them before. A profile shows "Loading…" until its first
/// answer, then its last answer while a newer one is fetched.
struct ManagerTray {
    host: std::sync::mpsc::Sender<HostEvent>,
    profiles: Vec<Profile>,
    /// By profile id; absent until the menu has first been opened.
    sessions: std::collections::HashMap<String, HostSessions>,
    /// Asks the tray's loop to fetch a profile's sessions.
    fetch: tokio::sync::mpsc::UnboundedSender<Profile>,
}

/// A profile's resumable sessions, as the tray menu knows them.
#[derive(Default)]
struct HostSessions {
    /// A fetch is in flight.
    loading: bool,
    /// The last answer: the sessions, or why they could not be listed.
    last: Option<Result<Vec<SessionInfo>, String>>,
}

/// How long a profile's session list may take before the menu says it could
/// not be fetched, rather than "Loading…" for good. Covers an SSH connection
/// that never completes.
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

/// The part of a profile's submenu after "New session".
fn session_items(profile: &Profile, state: Option<&HostSessions>) -> Vec<ksni::MenuItem<ManagerTray>> {
    use ksni::menu::*;
    let note = |label: String| -> ksni::MenuItem<ManagerTray> {
        StandardItem { label, enabled: false, ..Default::default() }.into()
    };
    match state.and_then(|s| s.last.as_ref()) {
        None => vec![note("Loading…".into())],
        Some(Err(e)) => vec![note(format!("Could not list sessions: {}", first_line(e, 80)))],
        Some(Ok(sessions)) if sessions.is_empty() => vec![note("No resumable sessions".into())],
        Some(Ok(sessions)) => sessions
            .iter()
            .map(|s| {
                let (resume, close) = (profile.clone(), profile.clone());
                let (resume_id, close_id) = (s.session_id.clone(), s.session_id.clone());
                SubMenu {
                    label: format!(
                        "{} — {} ({}×{}, {}{})",
                        s.session_id,
                        s.mode,
                        s.width,
                        s.height,
                        crate::manager::format_age(s.age_secs),
                        if s.attached { ", attached" } else { "" }
                    ),
                    submenu: vec![
                        StandardItem {
                            label: "Resume".into(),
                            activate: Box::new(move |_: &mut ManagerTray| {
                                crate::manager::spawn_client_for_profile(&resume, &["--attach".into(), resume_id.clone()]);
                            }),
                            ..Default::default()
                        }
                        .into(),
                        StandardItem {
                            label: "Close session".into(),
                            activate: Box::new(move |_: &mut ManagerTray| {
                                crate::manager::spawn_client_for_profile(&close, &["--close".into(), close_id.clone()]);
                            }),
                            ..Default::default()
                        }
                        .into(),
                    ],
                    ..Default::default()
                }
                .into()
            })
            .collect(),
    }
}

/// The first line of `text`, cut to `max` characters.
fn first_line(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or_default();
    match line.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_string(),
    }
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

    /// Fetch every profile's sessions, unless a fetch is already under way:
    /// hosts may call this more than once for one opening.
    fn menu_about_to_show(&mut self) {
        for p in &self.profiles {
            let state = self.sessions.entry(p.id.clone()).or_default();
            if !state.loading {
                state.loading = true;
                let _ = self.fetch.send(p.clone());
            }
        }
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
            let mut submenu: Vec<ksni::MenuItem<Self>> = vec![
                StandardItem {
                    label: "New session".into(),
                    activate: Box::new(move |_: &mut Self| {
                        crate::manager::spawn_client_for_profile(&profile, &[]);
                    }),
                    ..Default::default()
                }
                .into(),
                MenuItem::Separator,
            ];
            submenu.extend(session_items(p, self.sessions.get(&p.id)));
            items.push(
                SubMenu {
                    label: p.display_name.clone(),
                    submenu,
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
            let (fetch, mut to_fetch) = tokio::sync::mpsc::unbounded_channel::<Profile>();
            let tray = ManagerTray {
                host,
                profiles: profile::load(),
                sessions: Default::default(),
                fetch,
            };
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
                tokio::select! {
                    _ = changed.notified() => {
                        let profiles = profile::load();
                        handle.update(|t: &mut ManagerTray| t.profiles = profiles).await;
                    }
                    Some(p) = to_fetch.recv() => {
                        let handle = handle.clone();
                        tokio::spawn(async move {
                            // fetch_sessions never prompts (BatchMode for
                            // SSH): opening a menu must not pop up a
                            // passphrase or host-key dialog.
                            let params = crate::manager::connect_params_for_profile(&p);
                            let result = match tokio::time::timeout(
                                FETCH_TIMEOUT,
                                connection::fetch_sessions(&p.server, p.ssh, &params),
                            ).await {
                                Ok(Ok(sessions)) => Ok(sessions),
                                Ok(Err(e)) => Err(format!("{e:#}")),
                                Err(_) => Err(format!("no answer in {}s", FETCH_TIMEOUT.as_secs())),
                            };
                            handle.update(move |t: &mut ManagerTray| {
                                let state = t.sessions.entry(p.id.clone()).or_default();
                                state.loading = false;
                                state.last = Some(result);
                            }).await;
                        });
                    }
                }
            }
        });
    });

    match ready_rx.recv() {
        Ok(true) => Some(profiles_changed),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a menu shows: each item's label, submenus as "label > children".
    fn labels(items: &[ksni::MenuItem<ManagerTray>]) -> Vec<String> {
        items
            .iter()
            .map(|item| match item {
                ksni::MenuItem::Standard(i) => i.label.clone(),
                ksni::MenuItem::SubMenu(m) => format!("{} > {}", m.label, labels(&m.submenu).join(" | ")),
                ksni::MenuItem::Separator => "---".into(),
                _ => "?".into(),
            })
            .collect()
    }

    fn session(id: &str, attached: bool) -> SessionInfo {
        SessionInfo { session_id: id.into(), mode: "desktop".into(), width: 1280, height: 720, age_secs: 90, attached }
    }

    fn shown(state: Option<HostSessions>) -> Vec<String> {
        labels(&session_items(&Profile::new_default(), state.as_ref()))
    }

    #[test]
    fn a_profile_shows_loading_until_its_first_answer() {
        assert_eq!(shown(None), ["Loading…"]);
        assert_eq!(shown(Some(HostSessions { loading: true, last: None })), ["Loading…"]);
    }

    #[test]
    fn the_last_answer_stays_while_a_newer_one_is_fetched() {
        let state = HostSessions { loading: true, last: Some(Ok(vec![session("s1", false)])) };
        assert_eq!(shown(Some(state)), ["s1 — desktop (1280×720, 1m) > Resume | Close session"]);
    }

    #[test]
    fn sessions_can_be_resumed_or_closed() {
        let state = HostSessions { loading: false, last: Some(Ok(vec![session("s1", false), session("s2", true)])) };
        assert_eq!(
            shown(Some(state)),
            [
                "s1 — desktop (1280×720, 1m) > Resume | Close session",
                "s2 — desktop (1280×720, 1m, attached) > Resume | Close session",
            ]
        );
    }

    #[test]
    fn no_sessions_and_failures_say_so() {
        assert_eq!(shown(Some(HostSessions { loading: false, last: Some(Ok(vec![])) })), ["No resumable sessions"]);
        let failed = HostSessions { loading: false, last: Some(Err("ssh: connect to host home port 9022: No route\nmore".into())) };
        assert_eq!(shown(Some(failed)), ["Could not list sessions: ssh: connect to host home port 9022: No route"]);
    }

    #[test]
    fn long_errors_are_cut() {
        assert_eq!(first_line(&"x".repeat(100), 80), format!("{}…", "x".repeat(80)));
        assert_eq!(first_line("short", 80), "short");
    }
}
