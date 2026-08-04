//! Desktop session-manager window (`termland-client --manager`).
//!
//! Saved multi-host connection profiles plus a live view of each host's
//! resumable sessions. This is the window ROADMAP.md's v0.5 section C
//! deferred; it replaces having to type a server address on the CLI every
//! launch and gives the tray something to manage across more than one host.
//!
//! Two architectural rules carried over from tray.rs, for the same reasons:
//!  - Resume/New Session/Close never run the session engine in this process.
//!    They shell out to this same binary in its normal windowed mode via
//!    `std::env::current_exe()` + `Command`, exactly like `tray::spawn_client`.
//!    winit's event loop (used by `display::run`) and egui/eframe's event
//!    loop cannot safely share a process, so a subprocess is the only option.
//!  - egui's event loop is not async, but fetching each host's session list
//!    is. A background thread runs its own tokio runtime and pushes results
//!    back over a `std::sync::mpsc` channel that `update()` drains
//!    non-blockingly, calling `ctx.request_repaint()` so new data actually
//!    appears promptly instead of waiting for the next input event.

use anyhow::Result;
use std::collections::HashMap;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::connection::{self, ConnectParams};
use crate::profile::{self, Profile};
use termland_protocol::{SessionInfo, SessionMode, VideoCodec};

/// What the background poll thread is currently asked to watch. Only ever
/// one at a time — the currently-selected profile — so we don't hammer hosts
/// nobody is looking at.
#[derive(Clone)]
struct PollRequest {
    profile_id: String,
    server: String,
    ssh: bool,
    params: ConnectParams,
}

enum PollResult {
    Sessions(String, Vec<SessionInfo>),
    Error(String, String),
}

enum PollState {
    Loading,
    Ready(Vec<SessionInfo>),
    Error(String),
}

/// In-progress edits for a new or existing profile. Kept separate from
/// `Profile` because a couple of fields (ssh options, password) are edited
/// as free text / a checkbox-gated field rather than 1:1 with the struct.
struct EditState {
    profile: Profile,
    is_new: bool,
    ssh_opts_text: String,
}

impl EditState {
    fn from_profile(profile: Profile, is_new: bool) -> Self {
        let ssh_opts_text = profile.ssh_opts.join(" ");
        EditState { profile, is_new, ssh_opts_text }
    }
}

pub struct ManagerApp {
    profiles: Vec<Profile>,
    selected: Option<String>,
    edit: Option<EditState>,
    sessions: HashMap<String, PollState>,
    poll_target: Arc<Mutex<Option<PollRequest>>>,
    poll_notify: Arc<tokio::sync::Notify>,
    poll_rx: std::sync::mpsc::Receiver<PollResult>,
    delete_confirm: Option<String>,
}

impl ManagerApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let profiles = profile::load();
        let poll_target: Arc<Mutex<Option<PollRequest>>> = Arc::new(Mutex::new(None));
        let poll_notify = Arc::new(tokio::sync::Notify::new());
        let (tx, poll_rx) = std::sync::mpsc::channel();

        spawn_poll_thread(poll_target.clone(), poll_notify.clone(), tx, cc.egui_ctx.clone());

        ManagerApp {
            profiles,
            selected: None,
            edit: None,
            sessions: HashMap::new(),
            poll_target,
            poll_notify,
            poll_rx,
            delete_confirm: None,
        }
    }

    fn select(&mut self, id: &str) {
        self.selected = Some(id.to_string());
        self.sessions.insert(id.to_string(), PollState::Loading);
        if let Some(p) = self.profiles.iter().find(|p| p.id == id) {
            let req = PollRequest {
                profile_id: p.id.clone(),
                server: p.server.clone(),
                ssh: p.ssh,
                params: connect_params_for_profile(p),
            };
            *self.poll_target.lock().unwrap() = Some(req);
            self.poll_notify.notify_one();
        }
    }

    fn save_profiles(&self) {
        if let Err(e) = profile::save(&self.profiles) {
            tracing::warn!("failed to save profiles.json: {e}");
        }
    }

    fn drain_poll_results(&mut self) {
        while let Ok(result) = self.poll_rx.try_recv() {
            match result {
                PollResult::Sessions(id, sessions) => {
                    self.sessions.insert(id, PollState::Ready(sessions));
                }
                PollResult::Error(id, e) => {
                    self.sessions.insert(id, PollState::Error(e));
                }
            }
        }
    }
}

impl eframe::App for ManagerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_poll_results();

        egui::SidePanel::left("profiles_panel").resizable(true).default_width(240.0).show(ctx, |ui| {
            ui.heading("Profiles");
            if ui.button("+ New Profile").clicked() {
                self.edit = Some(EditState::from_profile(Profile::new_default(), true));
            }
            ui.separator();

            if self.profiles.is_empty() {
                ui.weak("No saved profiles yet.\nClick \"+ New Profile\" to add a host.");
            }

            let mut to_select = None;
            let mut to_edit = None;
            let mut to_delete = None;
            for p in &self.profiles {
                let is_selected = self.selected.as_deref() == Some(p.id.as_str());
                let response = ui.selectable_label(is_selected, format!("{}\n{}", p.display_name, p.server));
                if response.clicked() {
                    to_select = Some(p.id.clone());
                }
                response.context_menu(|ui| {
                    if ui.button("Edit").clicked() {
                        to_edit = Some(p.id.clone());
                        ui.close_menu();
                    }
                    if ui.button("Delete").clicked() {
                        to_delete = Some(p.id.clone());
                        ui.close_menu();
                    }
                });
            }
            if let Some(id) = to_select {
                self.select(&id);
            }
            if let Some(id) = to_edit {
                if let Some(p) = self.profiles.iter().find(|p| p.id == id) {
                    self.edit = Some(EditState::from_profile(p.clone(), false));
                }
            }
            if let Some(id) = to_delete {
                self.delete_confirm = Some(id);
            }
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            match self.selected.clone() {
                None => {
                    ui.vertical_centered(|ui| {
                        ui.add_space(40.0);
                        ui.heading("No profile selected");
                        ui.label("Select a saved profile on the left, or create one to get started.");
                    });
                }
                Some(id) => {
                    let profile = self.profiles.iter().find(|p| p.id == id).cloned();
                    let Some(profile) = profile else {
                        self.selected = None;
                        return;
                    };
                    ui.heading(&profile.display_name);
                    ui.label(format!("{} {}", if profile.ssh { "ssh" } else if profile.tls { "tls" } else { "tcp" }, profile.server));
                    ui.add_space(8.0);

                    if ui.button("New Session").clicked() {
                        spawn_client_for_profile(&profile, &[]);
                    }
                    ui.separator();

                    match self.sessions.get(&id) {
                        None | Some(PollState::Loading) => {
                            ui.spinner();
                            ui.label("Loading sessions…");
                        }
                        Some(PollState::Error(e)) => {
                            ui.colored_label(egui::Color32::from_rgb(220, 80, 80), format!("offline: {e}"));
                        }
                        Some(PollState::Ready(sessions)) => {
                            if sessions.is_empty() {
                                ui.weak("No resumable sessions on this host.");
                            } else {
                                let mut to_resume = None;
                                let mut to_close = None;
                                egui::Grid::new("sessions_grid").num_columns(6).striped(true).show(ui, |ui| {
                                    ui.strong("Session");
                                    ui.strong("Mode");
                                    ui.strong("Size");
                                    ui.strong("Age");
                                    ui.strong("Attached");
                                    ui.strong("");
                                    ui.end_row();
                                    for s in sessions {
                                        ui.label(&s.session_id);
                                        ui.label(&s.mode);
                                        ui.label(format!("{}x{}", s.width, s.height));
                                        ui.label(format_age(s.age_secs));
                                        ui.label(if s.attached { "yes" } else { "no" });
                                        ui.horizontal(|ui| {
                                            if ui.button("Resume").clicked() {
                                                to_resume = Some(s.session_id.clone());
                                            }
                                            if ui.button("Close").clicked() {
                                                to_close = Some(s.session_id.clone());
                                            }
                                        });
                                        ui.end_row();
                                    }
                                });
                                if let Some(id) = to_resume {
                                    spawn_client_for_profile(&profile, &["--attach".into(), id]);
                                }
                                if let Some(id) = to_close {
                                    spawn_client_for_profile(&profile, &["--close".into(), id]);
                                }
                            }
                        }
                    }
                }
            }
        });

        // Edit/create form.
        let mut close_edit = false;
        let mut save_edit = false;
        if let Some(edit) = &mut self.edit {
            let title = if edit.is_new { "New Profile" } else { "Edit Profile" };
            egui::Window::new(title).collapsible(false).resizable(false).show(ctx, |ui| {
                egui::Grid::new("edit_grid").num_columns(2).show(ui, |ui| {
                    ui.label("Name");
                    ui.text_edit_singleline(&mut edit.profile.display_name);
                    ui.end_row();

                    ui.label("Server (host:port)");
                    ui.text_edit_singleline(&mut edit.profile.server);
                    ui.end_row();

                    ui.label("SSH");
                    ui.checkbox(&mut edit.profile.ssh, "connect via ssh -s <server> termland");
                    ui.end_row();

                    ui.label("SSH options");
                    ui.text_edit_singleline(&mut edit.ssh_opts_text);
                    ui.end_row();

                    ui.label("TLS");
                    ui.checkbox(&mut edit.profile.tls, "encrypt with TLS");
                    ui.end_row();

                    ui.label("Accept invalid certs");
                    ui.checkbox(&mut edit.profile.accept_invalid_certs, "for self-signed servers");
                    ui.end_row();

                    ui.label("Username");
                    let mut username = edit.profile.username.clone().unwrap_or_default();
                    if ui.text_edit_singleline(&mut username).changed() {
                        edit.profile.username = if username.is_empty() { None } else { Some(username) };
                    }
                    ui.end_row();

                    ui.label("Remember password");
                    ui.checkbox(&mut edit.profile.remember_password, "");
                    ui.end_row();

                    if edit.profile.remember_password {
                        ui.colored_label(
                            egui::Color32::from_rgb(220, 150, 50),
                            "Warning: stored in plaintext in profiles.json.",
                        );
                        let mut password = edit.profile.password.clone().unwrap_or_default();
                        if ui.add(egui::TextEdit::singleline(&mut password).password(true)).changed() {
                            edit.profile.password = if password.is_empty() { None } else { Some(password) };
                        }
                        ui.end_row();
                    }

                    ui.label("Width");
                    ui.add(egui::DragValue::new(&mut edit.profile.width));
                    ui.end_row();

                    ui.label("Height");
                    ui.add(egui::DragValue::new(&mut edit.profile.height));
                    ui.end_row();

                    ui.label("Quality (1-100)");
                    ui.add(egui::Slider::new(&mut edit.profile.quality, 1..=100));
                    ui.end_row();

                    ui.label("Mode");
                    ui.text_edit_singleline(&mut edit.profile.mode);
                    ui.end_row();

                    ui.label("Desktop shell");
                    let mut shell = edit.profile.desktop_shell.clone().unwrap_or_default();
                    if ui.text_edit_singleline(&mut shell).changed() {
                        edit.profile.desktop_shell = if shell.is_empty() { None } else { Some(shell) };
                    }
                    ui.end_row();

                    ui.label("Codec");
                    egui::ComboBox::from_id_salt("codec_combo")
                        .selected_text(edit.profile.codec.clone().unwrap_or_else(|| "auto".into()))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut edit.profile.codec, None, "auto");
                            for c in ["av1", "vp9", "vp8", "h265", "h264"] {
                                ui.selectable_value(&mut edit.profile.codec, Some(c.to_string()), c);
                            }
                        });
                    ui.end_row();
                });

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        save_edit = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close_edit = true;
                    }
                });
            });
        }
        if save_edit {
            if let Some(mut edit) = self.edit.take() {
                edit.profile.ssh_opts = edit.ssh_opts_text.split_whitespace().map(String::from).collect();
                if let Some(existing) = self.profiles.iter_mut().find(|p| p.id == edit.profile.id) {
                    *existing = edit.profile;
                } else {
                    self.profiles.push(edit.profile);
                }
                self.save_profiles();
            }
        }
        if close_edit {
            self.edit = None;
        }

        // Delete confirmation.
        let mut confirmed = false;
        let mut cancelled = false;
        if let Some(id) = self.delete_confirm.clone() {
            let name = self.profiles.iter().find(|p| p.id == id).map(|p| p.display_name.clone()).unwrap_or_default();
            egui::Window::new("Delete profile?").collapsible(false).resizable(false).show(ctx, |ui| {
                ui.label(format!("Delete \"{name}\"? This only removes the saved profile; it does not close any sessions."));
                ui.horizontal(|ui| {
                    if ui.button("Delete").clicked() {
                        confirmed = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancelled = true;
                    }
                });
            });
        }
        if confirmed {
            if let Some(id) = self.delete_confirm.take() {
                self.profiles.retain(|p| p.id != id);
                self.sessions.remove(&id);
                if self.selected.as_deref() == Some(id.as_str()) {
                    self.selected = None;
                    *self.poll_target.lock().unwrap() = None;
                }
                self.save_profiles();
            }
        }
        if cancelled {
            self.delete_confirm = None;
        }
    }
}

/// Background bridge between egui's synchronous event loop and the async
/// `connection::fetch_sessions`. Polls whatever `target` currently holds
/// every ~5s (matching tray.rs's interval), waking early via `notify` when
/// the UI thread changes the selected profile so switching profiles doesn't
/// sit on a stale wait.
fn spawn_poll_thread(
    target: Arc<Mutex<Option<PollRequest>>>,
    notify: Arc<tokio::sync::Notify>,
    tx: std::sync::mpsc::Sender<PollResult>,
    ctx: egui::Context,
) {
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                tracing::error!("manager poll thread: failed to start tokio runtime: {e}");
                return;
            }
        };
        rt.block_on(async move {
            loop {
                let req = target.lock().unwrap().clone();
                if let Some(req) = req {
                    let result = connection::fetch_sessions(&req.server, req.ssh, &req.params).await;
                    let msg = match result {
                        Ok(sessions) => PollResult::Sessions(req.profile_id, sessions),
                        Err(e) => PollResult::Error(req.profile_id, e.to_string()),
                    };
                    let _ = tx.send(msg);
                    ctx.request_repaint();
                }
                tokio::select! {
                    _ = notify.notified() => {}
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                }
            }
        });
    });
}

/// Same age formatting as `connection.rs`'s local `format_age` — a tiny
/// duplicate rather than a shared crate export, matching the precedent in
/// `termland-server/src/main.rs`.
fn format_age(secs: u64) -> String {
    if secs < 60 { format!("{secs}s") }
    else if secs < 3600 { format!("{}m", secs / 60) }
    else if secs < 86400 { format!("{}h", secs / 3600) }
    else { format!("{}d", secs / 86400) }
}

fn session_mode_for(mode: &str) -> SessionMode {
    if mode == "desktop" {
        SessionMode::Desktop
    } else if let Some(cmd) = mode.strip_prefix("app:") {
        let parts: Vec<&str> = cmd.split_whitespace().collect();
        if parts.is_empty() {
            SessionMode::Desktop
        } else {
            SessionMode::App { command: parts[0].to_string(), args: parts[1..].iter().map(|s| s.to_string()).collect() }
        }
    } else {
        SessionMode::Desktop
    }
}

fn codec_from_name(name: &str) -> Option<VideoCodec> {
    match name {
        "av1" => Some(VideoCodec::Av1),
        "vp9" => Some(VideoCodec::Vp9),
        "vp8" => Some(VideoCodec::Vp8),
        "h265" | "hevc" => Some(VideoCodec::H265),
        "h264" | "avc" => Some(VideoCodec::H264),
        _ => None,
    }
}

fn connect_params_for_profile(p: &Profile) -> ConnectParams {
    ConnectParams {
        mode: session_mode_for(&p.mode),
        width: p.width,
        height: p.height,
        quality: p.quality,
        audio: false,
        ssh_opts: p.ssh_opts.clone(),
        tls: p.tls || p.accept_invalid_certs,
        accept_invalid_certs: p.accept_invalid_certs,
        username: p.username.clone(),
        password: if p.remember_password { p.password.clone() } else { None },
        desktop_shell: p.desktop_shell.clone(),
        encoder_preset: None,
        encoder_crf: None,
        encoder_extra_params: None,
        codec: p.codec.as_deref().and_then(codec_from_name),
        attach: None,
    }
}

/// Build the CLI args that reproduce `profile`'s settings, plus whatever
/// mode-specific extras the caller wants (`--attach <id>`, `--close <id>`, or
/// none for a new session). Split out from `spawn_client_for_profile` so the
/// argument-construction logic itself is unit-testable without spawning a
/// real process.
fn client_args(profile: &Profile, extra: &[String]) -> Vec<String> {
    let mut args = Vec::new();
    if profile.ssh {
        args.push("--ssh".into());
    }
    for opt in &profile.ssh_opts {
        args.push("--ssh-opt".into());
        args.push(opt.clone());
    }
    if profile.tls {
        args.push("--tls".into());
    }
    if profile.accept_invalid_certs {
        args.push("--accept-invalid-certs".into());
    }
    if let Some(u) = &profile.username {
        args.push("--user".into());
        args.push(u.clone());
    }
    if profile.remember_password {
        if let Some(pw) = &profile.password {
            args.push("--password".into());
            args.push(pw.clone());
        }
    }
    args.push("--width".into());
    args.push(profile.width.to_string());
    args.push("--height".into());
    args.push(profile.height.to_string());
    args.push("--mode".into());
    args.push(profile.mode.clone());
    args.push("--quality".into());
    args.push(profile.quality.to_string());
    if let Some(c) = &profile.codec {
        args.push("--codec".into());
        args.push(c.clone());
    }
    if let Some(shell) = &profile.desktop_shell {
        args.push("--desktop-shell".into());
        args.push(shell.clone());
    }
    args.extend(extra.iter().cloned());
    args.push(profile.server.clone());
    args
}

/// Launch this same executable in windowed mode against `profile`, exactly
/// like `tray::spawn_client` — never run the session engine in this process
/// (see module doc comment for why).
fn spawn_client_for_profile(profile: &Profile, extra: &[String]) {
    let exe = std::env::current_exe().unwrap_or_else(|_| "termland-client".into());
    let args = client_args(profile, extra);
    let mut cmd = Command::new(exe);
    cmd.args(&args);
    if let Err(e) = cmd.spawn() {
        tracing::error!("failed to launch client: {e}");
    }
}

/// Spawn `termland-client --manager` as a detached subprocess. Used by the
/// tray's "Manage profiles…" menu item, which — unlike `tray::spawn_client`
/// — must not append a server address.
pub fn spawn_manager_window() {
    let exe = std::env::current_exe().unwrap_or_else(|_| "termland-client".into());
    if let Err(e) = Command::new(exe).arg("--manager").spawn() {
        tracing::error!("failed to launch manager window: {e}");
    }
}

pub fn run() -> Result<()> {
    tracing::info!("Starting Termland session manager");
    let native_options = eframe::NativeOptions::default();
    eframe::run_native(
        "Termland Session Manager",
        native_options,
        Box::new(|cc| Ok(Box::new(ManagerApp::new(cc)))),
    )
    .map_err(|e| anyhow::anyhow!("eframe failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_args_new_session_matches_expected_flags() {
        let mut p = Profile::new_default();
        p.server = "example.com:7100".into();
        p.tls = true;
        p.ssh_opts = vec!["-oPort=9022".into()];
        p.username = Some("alice".into());
        p.remember_password = false;
        p.password = Some("should-not-appear".into());
        p.codec = Some("av1".into());

        let args = client_args(&p, &[]);
        assert_eq!(
            args,
            vec![
                "--ssh-opt", "-oPort=9022",
                "--tls",
                "--user", "alice",
                "--width", "1280",
                "--height", "720",
                "--mode", "desktop",
                "--quality", "75",
                "--codec", "av1",
                "example.com:7100",
            ]
        );
    }

    #[test]
    fn client_args_resume_appends_attach_before_server() {
        let mut p = Profile::new_default();
        p.server = "host:7100".into();
        let args = client_args(&p, &["--attach".into(), "sess-1".into()]);
        assert_eq!(args.last().unwrap(), "host:7100");
        assert!(args.windows(2).any(|w| w == ["--attach", "sess-1"]));
    }

    #[test]
    fn client_args_omits_password_unless_remembered() {
        let mut p = Profile::new_default();
        p.server = "host:7100".into();
        p.password = Some("secret".into());
        p.remember_password = false;
        let args = client_args(&p, &[]);
        assert!(!args.contains(&"--password".to_string()));

        p.remember_password = true;
        let args = client_args(&p, &[]);
        assert!(args.contains(&"--password".to_string()));
        assert!(args.contains(&"secret".to_string()));
    }
}
