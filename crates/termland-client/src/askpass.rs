//! termland-client as ssh's askpass, so an unknown host key is asked about
//! in a dialog that says what it is.
//!
//! An ssh started from the GUI has no terminal, so it asks its questions
//! through `SSH_ASKPASS`. A generic askpass (ksshaskpass, ...) shows every
//! question as a password box, including "continue connecting
//! (yes/no/[fingerprint])?", which reads as being asked for a key
//! passphrase. ssh runs us instead (see `ssh_env`); we answer the host-key
//! question ourselves and pass every other prompt on to the askpass that was
//! configured before, so passphrases still go to it (and to KWallet, or
//! whatever it remembers them in). With none configured we ask ourselves.

use std::ffi::OsString;
use std::sync::{Arc, Mutex};

/// Set on the ssh child. ssh passes its environment on to the askpass it
/// runs, which is how `main` knows it was started as one.
pub const MARK: &str = "TERMLAND_ASKPASS";
/// The askpass configured before ours, which every prompt other than a host
/// key is handed to.
const FALLBACK: &str = "TERMLAND_ASKPASS_FALLBACK";

/// Environment that makes an ssh use us as its askpass. Empty without a
/// display, where ssh would not run an askpass anyway, and when our own
/// binary is gone (replaced by a package upgrade while we were running), so
/// ssh keeps whatever askpass it had rather than one it cannot run.
pub fn ssh_env() -> Vec<(&'static str, OsString)> {
    let graphical = std::env::var_os("DISPLAY").is_some()
        || std::env::var_os("WAYLAND_DISPLAY").is_some();
    let exe = match std::env::current_exe() {
        Ok(exe) if graphical && exe.exists() => exe,
        _ => return Vec::new(),
    };
    let mut env = vec![("SSH_ASKPASS", exe.into_os_string()), (MARK, "1".into())];
    if let Some(prev) = std::env::var_os("SSH_ASKPASS") {
        env.push((FALLBACK, prev));
    }
    env
}

/// Answer one ssh prompt, the first argument, and return the exit code ssh
/// reads as accept (0) or refuse.
pub fn run() -> i32 {
    let prompt = std::env::args().nth(1).unwrap_or_default();
    if is_host_key_prompt(&prompt) {
        return match ask(Dialog::HostKey, host_key_details(&prompt)) {
            Some(_) => {
                println!("yes");
                0
            }
            None => 1,
        };
    }
    if let Some(prev) = std::env::var_os(FALLBACK) {
        match std::process::Command::new(&prev).args(std::env::args_os().skip(1)).status() {
            Ok(status) => return status.code().unwrap_or(1),
            Err(e) => eprintln!("termland askpass: cannot run {}: {e}", prev.to_string_lossy()),
        }
    }
    // SSH_ASKPASS_PROMPT is how ssh says the prompt is not after a secret:
    // "confirm" is a yes/no question, "none" a notice (touch your security
    // key) that ssh closes itself when it is done.
    let dialog = match std::env::var("SSH_ASKPASS_PROMPT").as_deref() {
        Ok("confirm") => Dialog::Confirm,
        Ok("none") => Dialog::Notice,
        _ => Dialog::Secret,
    };
    match ask(dialog, prompt.trim_end().to_string()) {
        Some(answer) => {
            if dialog == Dialog::Secret {
                println!("{answer}");
            }
            0
        }
        None => 1,
    }
}

/// ssh's question about a host key it cannot check against known_hosts:
/// an unknown host, or a known key seen at a new address.
fn is_host_key_prompt(prompt: &str) -> bool {
    prompt.contains("continue connecting (yes/no")
}

/// The prompt without its final yes/no question, which the dialog's buttons
/// take the place of.
fn host_key_details(prompt: &str) -> String {
    prompt
        .lines()
        .filter(|l| !is_host_key_prompt(l))
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_string()
}

#[derive(Clone, Copy, PartialEq)]
enum Dialog {
    HostKey,
    Confirm,
    Notice,
    Secret,
}

/// Show `dialog` and wait for it: `Some` with what was typed (empty unless
/// `Secret`) if accepted, `None` if refused or closed.
fn ask(dialog: Dialog, text: String) -> Option<String> {
    let answer = Arc::new(Mutex::new(None));
    let app = PromptApp { dialog, text, secret: String::new(), focused: false, answer: answer.clone() };
    // An ordinary window: no always-on-top or grab. It only opens because
    // the user just asked to connect, and must not take over other windows.
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_app_id(crate::desktop::APP_ID)
            .with_inner_size([600.0, 280.0]),
        ..Default::default()
    };
    if let Err(e) = eframe::run_native("Termland - SSH", options, Box::new(|_| Ok(Box::new(app)))) {
        eprintln!("termland askpass: {e}");
        return None;
    }
    answer.lock().unwrap().take()
}

struct PromptApp {
    dialog: Dialog,
    text: String,
    secret: String,
    focused: bool,
    answer: Arc<Mutex<Option<String>>>,
}

impl eframe::App for PromptApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // None: still open. Some(None): refused. Some(Some(_)): accepted.
        let mut done: Option<Option<String>> = None;
        egui::CentralPanel::default().show(ctx, |ui| {
            match self.dialog {
                Dialog::HostKey => {
                    ui.heading("Verify host key");
                    ui.label(
                        "ssh cannot check this host's key against ~/.ssh/known_hosts. \
                         Compare the fingerprint with the one the host itself reports \
                         before trusting it; trusting adds it to known_hosts.",
                    );
                    ui.add_space(6.0);
                    egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
                        ui.label(egui::RichText::new(&self.text).monospace());
                    });
                }
                _ => {
                    ui.label(&self.text);
                }
            }
            if self.dialog == Dialog::Secret {
                let field = ui.add(
                    egui::TextEdit::singleline(&mut self.secret)
                        .password(true)
                        .desired_width(f32::INFINITY),
                );
                if !self.focused {
                    field.request_focus();
                    self.focused = true;
                }
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let (yes, no) = match self.dialog {
                    Dialog::HostKey => ("Trust and connect", Some("Cancel")),
                    Dialog::Confirm => ("Yes", Some("No")),
                    Dialog::Notice => ("OK", None),
                    Dialog::Secret => ("OK", Some("Cancel")),
                };
                if ui.button(yes).clicked() {
                    done = Some(Some(self.secret.clone()));
                }
                if no.is_some_and(|no| ui.button(no).clicked()) {
                    done = Some(None);
                }
            });
        });
        // Enter only submits a typed secret; trusting a host key takes a click.
        if self.dialog == Dialog::Secret && ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
            done = Some(Some(self.secret.clone()));
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            done = Some(None);
        }
        if let Some(answer) = done {
            *self.answer.lock().unwrap() = answer;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As OpenSSH 10.2 asks it for a host with no known_hosts entry.
    const UNKNOWN_HOST: &str = "The authenticity of host '[home]:9022 ([203.0.113.7]:9022)' can't be established.\n\
        ED25519 key fingerprint is: SHA256:AAAAexamplefingerprintAAAAAAAAAAAAAAAAAAAAAA.\n\
        This host key is known by the following other names/addresses:\n    \
        ~/.ssh/known_hosts:150: z840\n\
        Are you sure you want to continue connecting (yes/no/[fingerprint])? ";

    #[test]
    fn a_host_key_question_is_recognised() {
        assert!(is_host_key_prompt(UNKNOWN_HOST));
        assert!(is_host_key_prompt("Are you sure you want to continue connecting (yes/no)? "));
    }

    #[test]
    fn passphrase_prompts_are_not_host_key_questions() {
        assert!(!is_host_key_prompt("Enter passphrase for key '/home/u/.ssh/id_ed25519': "));
        assert!(!is_host_key_prompt("u@home's password: "));
    }

    #[test]
    fn host_key_details_drop_the_yes_no_question() {
        let details = host_key_details(UNKNOWN_HOST);
        assert!(details.starts_with("The authenticity of host '[home]:9022"));
        assert!(details.contains("SHA256:AAAAexample"));
        assert!(details.ends_with("~/.ssh/known_hosts:150: z840"));
        assert!(!details.contains("yes/no"));
    }
}
