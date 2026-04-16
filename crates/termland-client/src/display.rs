use anyhow::{Context, Result};
use std::num::NonZeroU32;
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton as WinitMouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};

use crate::Args;
use crate::connection::{ClientCommand, ConnectParams, ServerEvent, connect};
use crate::overlay::{self, BarItem, BarLayout, MenuState, MENUBAR_HEIGHT};
use termland_protocol::input;
use winit::window::Fullscreen;

/// Inhibit compositor keyboard shortcuts on the given window so ALL keys
/// (including Ctrl, Alt, Super, Alt-F4, etc.) are forwarded to us.
/// Uses the zwp_keyboard_shortcuts_inhibit_manager_v1 Wayland protocol.
fn inhibit_shortcuts(window: &Window) -> Result<()> {
    use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
    use wayland_client::protocol::{wl_registry, wl_seat, wl_surface};
    use wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::client::{
        zwp_keyboard_shortcuts_inhibit_manager_v1,
        zwp_keyboard_shortcuts_inhibitor_v1,
    };

    struct State {
        seat: Option<wl_seat::WlSeat>,
        inhibit_mgr: Option<zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1>,
    }

    impl Dispatch<wl_registry::WlRegistry, ()> for State {
        fn event(state: &mut Self, registry: &wl_registry::WlRegistry, event: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
            if let wl_registry::Event::Global { name, interface, version } = event {
                match interface.as_str() {
                    "wl_seat" => {
                        if state.seat.is_none() {
                            state.seat = Some(registry.bind(name, version.min(1), qh, ()));
                        }
                    }
                    "zwp_keyboard_shortcuts_inhibit_manager_v1" => {
                        state.inhibit_mgr = Some(registry.bind(name, version.min(1), qh, ()));
                    }
                    _ => {}
                }
            }
        }
    }
    impl Dispatch<wl_seat::WlSeat, ()> for State {
        fn event(_: &mut Self, _: &wl_seat::WlSeat, _: wl_seat::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    }
    impl Dispatch<zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1, ()> for State {
        fn event(_: &mut Self, _: &zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1, _: zwp_keyboard_shortcuts_inhibit_manager_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    }
    impl Dispatch<zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1, ()> for State {
        fn event(_: &mut Self, _: &zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1, _: zwp_keyboard_shortcuts_inhibitor_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    }
    impl Dispatch<wl_surface::WlSurface, ()> for State {
        fn event(_: &mut Self, _: &wl_surface::WlSurface, _: wl_surface::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    }

    // Get raw Wayland display and surface from winit
    let display_handle = window.display_handle()
        .map_err(|e| anyhow::anyhow!("display handle: {e}"))?;
    let window_handle = window.window_handle()
        .map_err(|e| anyhow::anyhow!("window handle: {e}"))?;

    let (wl_display_ptr, wl_surface_ptr) = match (display_handle.as_raw(), window_handle.as_raw()) {
        (RawDisplayHandle::Wayland(d), RawWindowHandle::Wayland(w)) => {
            (d.display.as_ptr(), w.surface.as_ptr())
        }
        _ => anyhow::bail!("not running on Wayland"),
    };

    // Connect to the existing Wayland display
    let backend = unsafe {
        wayland_backend::client::Backend::from_foreign_display(wl_display_ptr as *mut _)
    };
    let conn = Connection::from_backend(backend);
    let display = conn.display();
    let mut event_queue = conn.new_event_queue();
    let qh = event_queue.handle();
    let mut state = State { seat: None, inhibit_mgr: None };

    let _registry = display.get_registry(&qh, ());
    event_queue.roundtrip(&mut state)
        .map_err(|e| anyhow::anyhow!("roundtrip: {e}"))?;

    let seat = state.seat.as_ref()
        .ok_or_else(|| anyhow::anyhow!("no wl_seat found"))?;
    let mgr = state.inhibit_mgr.as_ref()
        .ok_or_else(|| anyhow::anyhow!("compositor does not support keyboard shortcuts inhibitor protocol"))?;

    // Wrap the raw surface pointer as a wl_surface proxy
    let surface_id = unsafe {
        wayland_backend::client::ObjectId::from_ptr(
            wl_surface::WlSurface::interface(),
            wl_surface_ptr as *mut _,
        ).map_err(|e| anyhow::anyhow!("surface id: {e}"))?
    };
    let surface = wl_surface::WlSurface::from_id(&conn, surface_id)
        .map_err(|e| anyhow::anyhow!("surface from id: {e}"))?;

    // Inhibit shortcuts on our surface
    let _inhibitor = mgr.inhibit_shortcuts(&surface, seat, &qh, ());
    event_queue.roundtrip(&mut state)
        .map_err(|e| anyhow::anyhow!("roundtrip after inhibit: {e}"))?;

    // Leak the inhibitor and event queue so they stay alive for the window's lifetime
    std::mem::forget(_inhibitor);
    std::mem::forget(event_queue);

    Ok(())
}


pub fn run(args: Args) -> Result<()> {
    let event_loop = EventLoop::new().context("failed to create event loop")?;
    let mut app = App::new(args);
    event_loop.run_app(&mut app).context("event loop error")?;
    Ok(())
}

struct App {
    args: Args,
    window: Option<Arc<Window>>,
    surface: Option<softbuffer::Surface<Arc<Window>, Arc<Window>>>,
    server_rx: Option<tokio::sync::mpsc::UnboundedReceiver<ServerEvent>>,
    client_tx: Option<tokio::sync::mpsc::UnboundedSender<ClientCommand>>,
    runtime: tokio::runtime::Runtime,
    frame_width: u32,
    frame_height: u32,
    frame_buffer: Vec<u32>,
    should_exit: bool,
    menu: MenuState,
    /// Current cursor position in window pixels (what winit gives us).
    /// We store this and scale on demand when sending to the server.
    cursor_win_x: f64,
    cursor_win_y: f64,
    /// Whether the cursor is currently inside the window.
    cursor_in_window: bool,
    /// Latest reported network data rate in bytes/sec.
    data_rate: u64,
    /// Track last-set title so we don't spam set_title on every redraw.
    last_title: String,
    /// Cached menubar hit-test layout from the last render.
    bar_layout: Option<BarLayout>,
    /// Menubar item currently under the mouse (for hover highlight).
    bar_hovered: Option<BarItem>,
    /// Is the menubar currently visible? Off by default — toggle with F10.
    bar_visible: bool,
    /// Is the window currently fullscreen? (also hides the menubar)
    fullscreen: bool,
    /// Pending window-size change that hasn't been sent to the server yet.
    /// We debounce resize events so we don't reconfigure the remote
    /// compositor + encoder on every drag pixel.
    pending_resize: Option<(u32, u32, std::time::Instant)>,
    /// Last size we actually sent to the server.
    last_sent_size: Option<(u32, u32)>,
}

impl App {
    fn new(args: Args) -> Self {
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        Self {
            args, window: None, surface: None, server_rx: None, client_tx: None,
            runtime, frame_width: 0, frame_height: 0, frame_buffer: Vec::new(),
            should_exit: false,
            menu: MenuState::new(),
            cursor_win_x: 0.0, cursor_win_y: 0.0,
            cursor_in_window: false,
            data_rate: 0,
            last_title: String::new(),
            bar_layout: None,
            bar_hovered: None,
            bar_visible: false,
            fullscreen: false,
            pending_resize: None,
            last_sent_size: None,
        }
    }

    fn toggle_fullscreen(&mut self) {
        self.fullscreen = !self.fullscreen;
        if let Some(w) = &self.window {
            if self.fullscreen {
                w.set_fullscreen(Some(Fullscreen::Borderless(None)));
            } else {
                w.set_fullscreen(None);
            }
        }
    }

    /// Handle activation of a menubar item (click or key).
    fn activate_bar_item(&mut self, item: BarItem, event_loop: &ActiveEventLoop) {
        match item {
            BarItem::DataRate => {
                self.menu.show_data_rate = !self.menu.show_data_rate;
                // Force title to refresh immediately
                self.last_title.clear();
            }
            BarItem::ClientCursor => {
                self.menu.client_cursor = !self.menu.client_cursor;
                self.send_cmd(ClientCommand::SetCursorInFrame(!self.menu.client_cursor));
            }
            BarItem::Fullscreen => {
                self.toggle_fullscreen();
            }
            BarItem::Quit => {
                self.send_cmd(ClientCommand::Disconnect);
                event_loop.exit();
            }
        }
    }

    fn send_cmd(&self, cmd: ClientCommand) {
        if let Some(tx) = &self.client_tx { let _ = tx.send(cmd); }
    }

    fn start_connection(&mut self) {
        let server = self.args.server.clone();
        let ssh = self.args.ssh;

        // Use the window's actual physical inner_size for the initial session.
        // On a HiDPI display, `LogicalSize::new(1280, 720)` becomes a physical
        // 1600x900 window (1.25x) or 2560x1440 (2x), and we want the remote
        // compositor to render at those physical pixels so the stream is crisp
        // when blitted 1:1 into the window. If we used the logical args size
        // we'd immediately churn: create compositor at 1280x720, then the first
        // Resized event would arrive at physical size and we'd reinit the
        // encoder. Doing it this way means the first frame is already right.
        let (phys_w, phys_h) = if let Some(win) = &self.window {
            let sz = win.inner_size();
            // Menubar covers the top MENUBAR_HEIGHT rows of the framebuffer,
            // but the remote output doesn't know about it. We currently render
            // the full remote frame and overlay the menubar on top. Keep the
            // remote size matching the window size for 1:1 pixel mapping.
            let w = sz.width.max(320);
            let h = sz.height.max(240);
            self.last_sent_size = Some((w, h));
            (w, h)
        } else {
            (self.args.width, self.args.height)
        };

        tracing::info!("Initial remote size: {phys_w}x{phys_h} (physical pixels)");

        let params = ConnectParams {
            mode: self.args.session_mode(),
            width: phys_w,
            height: phys_h,
            quality: self.args.quality,
            desktop_shell: self.args.desktop_shell.clone(),
            encoder_preset: self.args.preset.clone(),
            encoder_crf: self.args.crf,
            encoder_extra_params: self.args.svt_params.clone(),
        };
        match self.runtime.block_on(connect(&server, ssh, params)) {
            Ok((rx, tx)) => { self.server_rx = Some(rx); self.client_tx = Some(tx); }
            Err(e) => tracing::error!("Connect failed: {e:#}"),
        }
    }

    fn process_events(&mut self) {
        let Some(rx) = &mut self.server_rx else { return; };
        while let Ok(event) = rx.try_recv() {
            match event {
                ServerEvent::SessionReady(sr) => {
                    self.frame_width = sr.width;
                    self.frame_height = sr.height;
                    self.frame_buffer = vec![0; (sr.width * sr.height) as usize];
                    tracing::info!("Session ready: {}x{}", sr.width, sr.height);
                }
                ServerEvent::Frame { width, height, pixels } => {
                    self.frame_width = width;
                    self.frame_height = height;
                    self.frame_buffer = pixels;
                }
                ServerEvent::DataRate { bytes_per_sec } => {
                    self.data_rate = bytes_per_sec;
                }
                ServerEvent::Pong(_) => {}
                ServerEvent::Disconnected => {
                    tracing::info!("Session ended");
                    self.should_exit = true;
                }
            }
        }
    }

    /// Send a `Resize` command to the server if the user has stopped dragging
    /// for long enough that the size is stable. Prevents flooding the server
    /// with mid-drag sizes.
    fn flush_pending_resize(&mut self) {
        const QUIESCE: std::time::Duration = std::time::Duration::from_millis(150);
        if let Some((w, h, set_at)) = self.pending_resize {
            if set_at.elapsed() >= QUIESCE && self.last_sent_size != Some((w, h)) {
                self.send_cmd(ClientCommand::Resize(w, h));
                self.last_sent_size = Some((w, h));
                self.pending_resize = None;
                tracing::info!("Requested remote resize to {w}x{h}");
            }
        }
    }

    /// Update the window title based on current flags.
    fn update_title(&mut self) {
        let Some(win) = &self.window else { return; };
        let title = if self.menu.show_data_rate {
            format!("Termland  [{}]", overlay::format_rate(self.data_rate))
        } else {
            "Termland".to_string()
        };
        if title != self.last_title {
            win.set_title(&title);
            self.last_title = title;
        }
    }

    fn render(&mut self) {
        let Some(surface) = &mut self.surface else { return; };
        let Some(window) = &self.window else { return; };
        if self.frame_buffer.is_empty() || self.frame_width == 0 { return; }

        // Size softbuffer to the *window*, not the frame. This is the key
        // fix for the "diagonal" / row-wrap artifact: attaching a
        // differently-sized buffer to a surface during resize causes the
        // compositor to apply its own scaling with the wrong stride.
        let win_size = window.inner_size();
        let Some(ww_nz) = NonZeroU32::new(win_size.width) else { return; };
        let Some(wh_nz) = NonZeroU32::new(win_size.height) else { return; };
        if surface.resize(ww_nz, wh_nz).is_err() { return; }

        let win_w = ww_nz.get() as usize;
        let win_h = wh_nz.get() as usize;
        let fw = self.frame_width as usize;
        let fh = self.frame_height as usize;

        if let Ok(mut buffer) = surface.buffer_mut() {
            if fw == win_w && fh == win_h {
                // Happy path: dimensions match, direct 1:1 copy.
                let len = buffer.len().min(self.frame_buffer.len());
                buffer[..len].copy_from_slice(&self.frame_buffer[..len]);
            } else if fw > 0 && fh > 0 {
                // Transition frame: the window was resized but the server
                // hasn't caught up yet. Nearest-neighbor scale the old frame
                // into the new window buffer so the user sees *something*
                // reasonable for the few hundred ms it takes the remote
                // compositor + encoder to reconfigure.
                //
                // Fixed-point arithmetic so we don't call float ops in a
                // hot per-pixel loop.
                let x_ratio = ((fw << 16) / win_w.max(1)) as u32;
                let y_ratio = ((fh << 16) / win_h.max(1)) as u32;
                for y in 0..win_h {
                    let src_y = ((y as u32 * y_ratio) >> 16) as usize;
                    let src_y = src_y.min(fh - 1);
                    let src_row = src_y * fw;
                    let dst_row = y * win_w;
                    for x in 0..win_w {
                        let src_x = ((x as u32 * x_ratio) >> 16) as usize;
                        let src_x = src_x.min(fw - 1);
                        buffer[dst_row + x] = self.frame_buffer[src_row + src_x];
                    }
                }
            } else {
                // No frame yet - paint black.
                buffer.fill(0);
            }

            // Local cursor overlay in window-space.
            // Skip the menubar area so the cursor doesn't pass over it.
            let over_bar = self.bar_visible && !self.fullscreen
                && self.cursor_win_y < MENUBAR_HEIGHT as f64;
            if self.menu.client_cursor && self.cursor_in_window && !over_bar {
                overlay::draw_local_cursor(&mut buffer, ww_nz.get(), wh_nz.get(),
                    self.cursor_win_x, self.cursor_win_y);
            }

            // Menubar (toggle with F10, also hidden in fullscreen)
            if self.bar_visible && !self.fullscreen {
                let layout = overlay::draw_menubar(
                    &mut buffer, ww_nz.get(), wh_nz.get(),
                    self.menu.show_data_rate,
                    self.menu.client_cursor,
                    self.fullscreen,
                    self.data_rate,
                    self.bar_hovered,
                );
                self.bar_layout = Some(layout);
            } else {
                self.bar_layout = None;
                self.bar_hovered = None;
            }

            let _ = buffer.present();
        }
    }
}

fn keycode_to_evdev(key: KeyCode) -> Option<u32> {
    Some(match key {
        KeyCode::Escape => 1,
        KeyCode::Digit1 => 2, KeyCode::Digit2 => 3, KeyCode::Digit3 => 4,
        KeyCode::Digit4 => 5, KeyCode::Digit5 => 6, KeyCode::Digit6 => 7,
        KeyCode::Digit7 => 8, KeyCode::Digit8 => 9, KeyCode::Digit9 => 10,
        KeyCode::Digit0 => 11, KeyCode::Minus => 12, KeyCode::Equal => 13,
        KeyCode::Backspace => 14, KeyCode::Tab => 15,
        KeyCode::KeyQ => 16, KeyCode::KeyW => 17, KeyCode::KeyE => 18,
        KeyCode::KeyR => 19, KeyCode::KeyT => 20, KeyCode::KeyY => 21,
        KeyCode::KeyU => 22, KeyCode::KeyI => 23, KeyCode::KeyO => 24,
        KeyCode::KeyP => 25, KeyCode::BracketLeft => 26, KeyCode::BracketRight => 27,
        KeyCode::Enter => 28, KeyCode::ControlLeft => 29,
        KeyCode::KeyA => 30, KeyCode::KeyS => 31, KeyCode::KeyD => 32,
        KeyCode::KeyF => 33, KeyCode::KeyG => 34, KeyCode::KeyH => 35,
        KeyCode::KeyJ => 36, KeyCode::KeyK => 37, KeyCode::KeyL => 38,
        KeyCode::Semicolon => 39, KeyCode::Quote => 40, KeyCode::Backquote => 41,
        KeyCode::ShiftLeft => 42, KeyCode::Backslash => 43,
        KeyCode::KeyZ => 44, KeyCode::KeyX => 45, KeyCode::KeyC => 46,
        KeyCode::KeyV => 47, KeyCode::KeyB => 48, KeyCode::KeyN => 49,
        KeyCode::KeyM => 50, KeyCode::Comma => 51, KeyCode::Period => 52,
        KeyCode::Slash => 53, KeyCode::ShiftRight => 54,
        KeyCode::AltLeft => 56, KeyCode::Space => 57, KeyCode::CapsLock => 58,
        KeyCode::F1 => 59, KeyCode::F2 => 60, KeyCode::F3 => 61,
        KeyCode::F4 => 62, KeyCode::F5 => 63, KeyCode::F6 => 64,
        KeyCode::F7 => 65, KeyCode::F8 => 66, KeyCode::F9 => 67,
        KeyCode::F10 => 68, KeyCode::F11 => 87, KeyCode::F12 => 88,
        KeyCode::Home => 102, KeyCode::ArrowUp => 103, KeyCode::PageUp => 104,
        KeyCode::ArrowLeft => 105, KeyCode::ArrowRight => 106,
        KeyCode::End => 107, KeyCode::ArrowDown => 108, KeyCode::PageDown => 109,
        KeyCode::Insert => 110, KeyCode::Delete => 111,
        KeyCode::AltRight => 100, KeyCode::ControlRight => 97,
        KeyCode::SuperLeft => 125, KeyCode::SuperRight => 126,
        _ => return None,
    })
}

fn mouse_button_to_linux(button: WinitMouseButton) -> u32 {
    match button {
        WinitMouseButton::Left => 0x110,
        WinitMouseButton::Right => 0x111,
        WinitMouseButton::Middle => 0x112,
        WinitMouseButton::Back => 0x113,
        WinitMouseButton::Forward => 0x114,
        WinitMouseButton::Other(n) => 0x110 + n as u32,
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() { return; }

        let attrs = Window::default_attributes()
            .with_title("Termland")
            .with_inner_size(LogicalSize::new(self.args.width, self.args.height));

        match event_loop.create_window(attrs) {
            Ok(window) => {
                let window = Arc::new(window);
                window.set_cursor_visible(false);

                // Inhibit compositor keyboard shortcuts so we get ALL keys
                if let Err(e) = inhibit_shortcuts(&window) {
                    tracing::warn!("Could not inhibit keyboard shortcuts: {e}");
                    tracing::warn!("Modifier keys (Ctrl, Alt, Super) may be intercepted by your compositor");
                } else {
                    tracing::info!("Keyboard shortcuts inhibited - all keys forwarded to remote session");
                }

                let ctx = softbuffer::Context::new(window.clone()).expect("softbuffer context");
                let surface = softbuffer::Surface::new(&ctx, window.clone()).expect("softbuffer surface");
                self.window = Some(window);
                self.surface = Some(surface);
                self.start_connection();
            }
            Err(e) => { tracing::error!("Window: {e}"); event_loop.exit(); }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.send_cmd(ClientCommand::Disconnect);
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                // Don't send a Resize command on every pixel while the user is
                // dragging the edge — we just remember the latest target and flush
                // it once the resize quiesces (see flush_pending_resize()).
                if size.width > 0 && size.height > 0 {
                    self.pending_resize = Some((size.width, size.height, std::time::Instant::now()));
                }
            }
            WindowEvent::RedrawRequested => {
                self.process_events();
                if self.should_exit { event_loop.exit(); return; }
                self.flush_pending_resize();
                self.update_title();
                self.render();
                if let Some(w) = &self.window { w.request_redraw(); }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                // Debug: log all physical keys so we can see whether F10/F11 reach us.
                if event.state == ElementState::Pressed && !event.repeat {
                    tracing::trace!("key press: {:?}", event.physical_key);
                }

                // F10 toggles the menubar (hidden by default).
                if event.state == ElementState::Pressed
                    && matches!(event.physical_key, PhysicalKey::Code(KeyCode::F10))
                    && !event.repeat
                {
                    self.bar_visible = !self.bar_visible;
                    return;
                }

                // F11 toggles fullscreen mode.
                if event.state == ElementState::Pressed
                    && matches!(event.physical_key, PhysicalKey::Code(KeyCode::F11))
                    && !event.repeat
                {
                    self.toggle_fullscreen();
                    return;
                }

                // Forward the key to the remote session as before.
                if event.repeat { return; }
                if let PhysicalKey::Code(keycode) = event.physical_key {
                    if let Some(scancode) = keycode_to_evdev(keycode) {
                        let state = match event.state {
                            ElementState::Pressed => input::KeyState::Pressed,
                            ElementState::Released => input::KeyState::Released,
                        };
                        self.send_cmd(ClientCommand::KeyEvent(input::KeyEvent {
                            scancode, keysym: 0, state, modifiers: 0,
                        }));
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                // Store cursor in WINDOW pixels (what winit gives us).
                self.cursor_win_x = position.x;
                self.cursor_win_y = position.y;

                // Hit-test the menubar (window-space) for hover highlight.
                self.bar_hovered = if self.bar_visible && !self.fullscreen {
                    self.bar_layout.as_ref().and_then(|l|
                        overlay::hit_test_menubar(l, position.x, position.y))
                } else {
                    None
                };

                // Don't forward pointer motion while the cursor is over the
                // visible menubar strip.
                let over_bar = self.bar_visible && !self.fullscreen
                    && position.y < MENUBAR_HEIGHT as f64;
                if !over_bar {
                    // Scale window coords to compositor (frame) coords for the server.
                    let (sx, sy) = if let Some(win) = &self.window {
                        let ws = win.inner_size();
                        if ws.width > 0 && ws.height > 0 && self.frame_width > 0 {
                            (position.x * self.frame_width as f64 / ws.width as f64,
                             position.y * self.frame_height as f64 / ws.height as f64)
                        } else { (position.x, position.y) }
                    } else { (position.x, position.y) };
                    self.send_cmd(ClientCommand::MouseMove(input::MouseMove {
                        x: sx, y: sy, absolute: true,
                    }));
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                // Check if the click is on a menubar item first.
                let on_bar = self.bar_visible && !self.fullscreen
                    && self.cursor_win_y < MENUBAR_HEIGHT as f64;
                if on_bar {
                    if state == ElementState::Pressed && button == WinitMouseButton::Left {
                        if let Some(item) = self.bar_layout.as_ref().and_then(|l|
                            overlay::hit_test_menubar(l, self.cursor_win_x, self.cursor_win_y))
                        {
                            self.activate_bar_item(item, event_loop);
                        }
                    }
                    // Swallow all clicks on the menubar strip.
                    return;
                }

                let s = match state {
                    ElementState::Pressed => input::ButtonState::Pressed,
                    ElementState::Released => input::ButtonState::Released,
                };
                self.send_cmd(ClientCommand::MouseButton(input::MouseButton {
                    button: mouse_button_to_linux(button), state: s,
                }));
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Don't scroll the remote when the cursor is over the menubar.
                let over_bar = self.bar_visible && !self.fullscreen
                    && self.cursor_win_y < MENUBAR_HEIGHT as f64;
                if over_bar { return; }
                let (dx, dy) = match delta {
                    winit::event::MouseScrollDelta::LineDelta(x, y) => (x as f64 * 15.0, y as f64 * 15.0),
                    winit::event::MouseScrollDelta::PixelDelta(p) => (p.x, p.y),
                };
                self.send_cmd(ClientCommand::MouseScroll(input::MouseScroll { dx, dy }));
            }
            WindowEvent::CursorEntered { .. } => {
                self.cursor_in_window = true;
                if let Some(w) = &self.window { w.set_cursor_visible(false); }
            }
            WindowEvent::CursorLeft { .. } => {
                self.cursor_in_window = false;
                if let Some(w) = &self.window { w.set_cursor_visible(true); }
            }
            WindowEvent::ModifiersChanged(_) => {
                // Modifiers come through KeyboardInput now that shortcuts are inhibited
            }
            _ => {}
        }
    }
}
