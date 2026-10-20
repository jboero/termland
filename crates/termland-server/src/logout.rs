//! `termland-server --logout-service`: makes Plasma's Log Out end a termland
//! desktop session.
//!
//! The default desktop (`detect_desktop_shell`) is plasmashell inside labwc,
//! on a private session bus from `dbus-run-session`. Plasma's Leave actions
//! do not end anything themselves: they ask ksmserver over D-Bus, through
//! `org.kde.LogoutPrompt` (when a confirmation is configured, the default)
//! or `org.kde.Shutdown`. Nothing owns those names on that bus, so the calls
//! went nowhere, and Log Out did nothing while the session stayed up.
//!
//! This owns both names on the session's bus, and every Leave action exits
//! labwc. That is the same thing that happens when the session's terminal
//! exits: the server sees the compositor gone and ends the session, reaping
//! what is left of its process group (`SessionOutcome::Gone`).
//!
//! Shut Down and Restart end the session too. They never reach the host:
//! someone logged in to a remote desktop asked to leave it, not to power off
//! the machine it runs on.

use anyhow::{Context, Result};
use std::time::Duration;
use zbus::{connection, interface};

/// labwc exports its pid to everything it launches; `labwc --exit` uses the
/// same variable.
fn labwc_pid() -> Option<u32> {
    std::env::var("LABWC_PID").ok()?.parse().ok()
}

/// End the session by exiting labwc, as `labwc --exit` does.
fn end_session(action: &str) -> zbus::fdo::Result<()> {
    let pid = labwc_pid().ok_or_else(|| zbus::fdo::Error::Failed("LABWC_PID is not set".into()))?;
    tracing::info!("{action} requested: exiting labwc (pid {pid}) to end the session");
    if unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) } != 0 {
        let e = std::io::Error::last_os_error();
        return Err(zbus::fdo::Error::Failed(format!("signal labwc (pid {pid}): {e}")));
    }
    Ok(())
}

/// ksmserver's logout interface, which `promptLogout` and friends end up
/// calling once the user confirms.
struct Shutdown;

#[interface(name = "org.kde.Shutdown")]
impl Shutdown {
    #[zbus(name = "logout")]
    fn logout(&self) -> zbus::fdo::Result<()> {
        end_session("logout")
    }

    #[zbus(name = "logoutAndShutdown")]
    fn logout_and_shutdown(&self) -> zbus::fdo::Result<()> {
        end_session("logoutAndShutdown")
    }

    #[zbus(name = "logoutAndReboot")]
    fn logout_and_reboot(&self) -> zbus::fdo::Result<()> {
        end_session("logoutAndReboot")
    }

    /// Nothing to save: a termland session is resumed by reattaching.
    #[zbus(name = "saveSession")]
    fn save_session(&self) {}
}

/// The confirmation dialog Plasma asks for first by default. The user has
/// already chosen Log Out in the remote desktop, and a second confirmation
/// there would be no more informed than the first, so it ends the session.
struct LogoutPrompt;

#[interface(name = "org.kde.LogoutPrompt")]
impl LogoutPrompt {
    #[zbus(name = "promptLogout")]
    fn prompt_logout(&self) -> zbus::fdo::Result<()> {
        end_session("promptLogout")
    }

    #[zbus(name = "promptShutDown")]
    fn prompt_shut_down(&self) -> zbus::fdo::Result<()> {
        end_session("promptShutDown")
    }

    #[zbus(name = "promptReboot")]
    fn prompt_reboot(&self) -> zbus::fdo::Result<()> {
        end_session("promptReboot")
    }

    #[zbus(name = "promptAll")]
    fn prompt_all(&self) -> zbus::fdo::Result<()> {
        end_session("promptAll")
    }
}

/// Serve both interfaces on the session bus until labwc has gone.
pub async fn run() -> Result<()> {
    let labwc = labwc_pid().context("LABWC_PID is not set: --logout-service runs inside labwc")?;
    let _conn = connection::Builder::session()?
        .name("org.kde.Shutdown")?
        .name("org.kde.LogoutPrompt")?
        .serve_at("/Shutdown", Shutdown)?
        .serve_at("/LogoutPrompt", LogoutPrompt)?
        .build()
        .await
        .context("serve org.kde.Shutdown / org.kde.LogoutPrompt on the session bus")?;
    // The desktop script stops us when the session ends; this is for when it
    // cannot (SIGKILL), so we are not left behind waiting on a dead bus.
    while crate::registry::pid_alive(labwc) {
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    Ok(())
}
