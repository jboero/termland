//! The client: handshake, session lifecycle, keepalive, and the two read
//! loops (control messages and the Q2 video plane).
//!
//! Everything the page needs is on `WebClient`, so the demo page is markup
//! plus a few lines of bootstrap rather than a second implementation.

use std::cell::RefCell;
use std::rc::Rc;

use termland_protocol::{
    AudioCodec, Hello, Message, Ping, Pong, SessionAttach, SessionClose, SessionCreate, SessionEnd,
    SessionList, SessionMode, VideoCodec, PROTOCOL_VERSION,
};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::wasm_bindgen;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::spawn_local;
use web_sys::{Document, Element, HtmlCanvasElement, WebTransport};

use crate::input::InputCapture;
use crate::transport::{self, Outbox};
use crate::video::{probe_supported_codecs, Q2Reader, VideoPipeline};

/// How often to Ping. The server answers before session selection as well as
/// during a session, so this can start at HelloAck.
const PING_INTERVAL_MS: i32 = 5_000;

struct State {
    doc: Document,
    canvas: HtmlCanvasElement,
    status: Option<Element>,
    sessions: Option<Element>,
    outbox: Option<Outbox>,
    video: Option<VideoPipeline>,
    input: Option<InputCapture>,
    probed: Vec<(VideoCodec, String)>,
    /// Framebuffer size, shared with input so pointer scaling follows a resize.
    remote: Rc<RefCell<(u32, u32)>>,
    session_id: Option<String>,
    username: String,
    password: String,
    ping_handle: Option<i32>,
    /// Kept alive for as long as the buttons they are attached to.
    session_buttons: Vec<Closure<dyn FnMut(web_sys::Event)>>,
    ping_closure: Option<Closure<dyn FnMut()>>,
    closed: bool,
}

impl State {
    fn status(&self, text: &str) {
        if let Some(el) = &self.status {
            el.set_text_content(Some(text));
        }
        web_sys::console::log_1(&JsValue::from_str(text));
    }

    fn send(&self, msg: Message) {
        if let Some(out) = &self.outbox {
            out.send(&msg);
        }
    }
}

/// Termland browser client. One per page.
#[wasm_bindgen]
pub struct WebClient {
    state: Rc<RefCell<State>>,
}

#[wasm_bindgen]
impl WebClient {
    /// `canvas_id` is required; the status and session-list elements are
    /// optional so a page can render its own chrome.
    #[wasm_bindgen(constructor)]
    pub fn new(
        canvas_id: &str,
        status_id: Option<String>,
        sessions_id: Option<String>,
    ) -> Result<WebClient, JsValue> {
        console_error_panic_hook::set_once();
        let doc = web_sys::window()
            .and_then(|w| w.document())
            .ok_or_else(|| JsValue::from_str("no document"))?;
        let canvas: HtmlCanvasElement = doc
            .get_element_by_id(canvas_id)
            .ok_or_else(|| JsValue::from_str(&format!("no element #{canvas_id}")))?
            .unchecked_into();
        let status = status_id.and_then(|id| doc.get_element_by_id(&id));
        let sessions = sessions_id.and_then(|id| doc.get_element_by_id(&id));
        let video = VideoPipeline::new(canvas.clone())?;

        Ok(WebClient {
            state: Rc::new(RefCell::new(State {
                doc,
                canvas,
                status,
                sessions,
                outbox: None,
                video: Some(video),
                input: None,
                probed: Vec::new(),
                remote: Rc::new(RefCell::new((0, 0))),
                session_id: None,
                username: String::new(),
                password: String::new(),
                ping_handle: None,
                session_buttons: Vec::new(),
                ping_closure: None,
                closed: false,
            })),
        })
    }

    /// Connect and run until the transport closes. The returned promise
    /// settles when the session ends.
    pub fn connect(
        &self,
        url: String,
        cert_hash: Option<String>,
        username: Option<String>,
        password: Option<String>,
    ) -> js_sys::Promise {
        let state = self.state.clone();
        {
            let mut s = state.borrow_mut();
            s.username = username.unwrap_or_default();
            s.password = password.unwrap_or_default();
            s.closed = false;
        }
        wasm_bindgen_futures::future_to_promise(async move {
            match run(state.clone(), url, cert_hash).await {
                Ok(()) => {
                    state.borrow().status("disconnected");
                    Ok(JsValue::NULL)
                }
                Err(e) => {
                    let msg = describe(&e);
                    state.borrow().status(&format!("error: {msg}"));
                    Err(e)
                }
            }
        })
    }

    /// Ask for a new desktop session at this size.
    pub fn create_session(&self, width: u32, height: u32) {
        let s = self.state.borrow();
        let codecs: Vec<VideoCodec> = s.probed.iter().map(|(c, _)| *c).collect();
        s.send(Message::SessionCreate(SessionCreate {
            mode: SessionMode::Desktop,
            width,
            height,
            audio: false,
            quality: 75,
            desktop_shell: None,
            encoder_preset: None,
            encoder_crf: None,
            encoder_extra_params: None,
            supported_codecs: codecs,
            // Q2's audio header has no timestamp field, which WebCodecs
            // requires, so the browser path does not carry audio yet.
            supported_audio_codecs: Vec::<AudioCodec>::new(),
        }));
    }

    /// Re-attach to an existing session.
    pub fn attach_session(&self, id: String) {
        let mut s = self.state.borrow_mut();
        s.session_id = Some(id.clone());
        let codecs: Vec<VideoCodec> = s.probed.iter().map(|(c, _)| *c).collect();
        s.send(Message::SessionAttach(SessionAttach {
            session_id: id,
            audio: false,
            quality: 75,
            encoder_preset: None,
            encoder_crf: None,
            encoder_extra_params: None,
            supported_codecs: codecs,
            supported_audio_codecs: Vec::<AudioCodec>::new(),
        }));
    }

    pub fn list_sessions(&self) {
        self.state.borrow().send(Message::SessionList(SessionList {}));
    }

    pub fn close_session(&self, id: String) {
        self.state
            .borrow()
            .send(Message::SessionClose(SessionClose { session_id: id }));
    }

    /// Release held input and tell the server we are going.
    pub fn disconnect(&self) {
        let mut s = self.state.borrow_mut();
        s.closed = true;
        if let Some(input) = &s.input {
            input.release_all();
        }
        s.send(Message::SessionEnd(SessionEnd {
            reason: "client quit".into(),
        }));
        stop_ping(&mut s);
    }

    /// WebCodecs strings this browser accepted, for display.
    pub fn codecs(&self) -> Vec<String> {
        self.state
            .borrow()
            .probed
            .iter()
            .map(|(c, s)| format!("{c} ({s})"))
            .collect()
    }
}

async fn run(
    state: Rc<RefCell<State>>,
    url: String,
    cert_hash: Option<String>,
) -> Result<(), JsValue> {
    state.borrow().status("probing codecs…");
    let probed = probe_supported_codecs().await;
    if probed.is_empty() {
        return Err(JsValue::from_str(
            "this browser cannot decode any codec Termland encodes",
        ));
    }
    state.borrow_mut().probed = probed;

    state.borrow().status("opening WebTransport…");
    let transport = transport::open(&url, cert_hash.as_deref()).await?;
    let (outbox, mut inbox) = transport::control_stream(&transport).await?;
    state.borrow_mut().outbox = Some(outbox.clone());
    state.borrow().status("transport ready");

    spawn_local(video_task(transport.clone(), state.clone()));

    outbox.send(&Message::Hello(Hello {
        protocol_version: PROTOCOL_VERSION,
        client_name: "termland-wasm".into(),
    }));

    while let Some(msg) = inbox.next().await? {
        if state.borrow().closed {
            break;
        }
        on_message(&state, msg)?;
    }
    let mut s = state.borrow_mut();
    stop_ping(&mut s);
    Ok(())
}

fn on_message(state: &Rc<RefCell<State>>, msg: Message) -> Result<(), JsValue> {
    match msg {
        Message::HelloAck(ack) => {
            {
                let s = state.borrow();
                s.status(&format!(
                    "connected to {} (auth {})",
                    ack.server_name,
                    if ack.auth_required { "required" } else { "off" }
                ));
            }
            start_ping(state);
            if !ack.auth_required {
                state.borrow().send(Message::SessionList(SessionList {}));
            }
        }
        Message::AuthRequest(_) => {
            let s = state.borrow();
            s.status("authenticating…");
            s.send(Message::AuthResponse(termland_protocol::AuthResponse {
                username: s.username.clone(),
                credential: s.password.clone(),
            }));
        }
        Message::AuthResult(result) => {
            let s = state.borrow();
            if !result.success {
                s.status(&format!("auth failed: {}", result.message));
                return Err(JsValue::from_str("authentication failed"));
            }
            s.status("authenticated");
            s.send(Message::SessionList(SessionList {}));
        }
        Message::SessionListResult(list) => render_sessions(state, &list.sessions)?,
        Message::SessionReady(ready) => {
            let mut s = state.borrow_mut();
            s.session_id = Some(ready.session_id.clone());
            *s.remote.borrow_mut() = (ready.width, ready.height);
            s.canvas.set_width(ready.width);
            s.canvas.set_height(ready.height);
            let codec = ready.codec.unwrap_or(VideoCodec::Av1);
            let probed = s.probed.clone();
            if let Some(video) = s.video.as_mut() {
                video.configure(codec, ready.width as u16, ready.height as u16, &probed)?;
            }
            if s.input.is_none() {
                let outbox = s
                    .outbox
                    .clone()
                    .ok_or_else(|| JsValue::from_str("no control stream"))?;
                let remote = s.remote.clone();
                s.input = Some(InputCapture::attach(&s.canvas, outbox, remote)?);
            }
            let _ = s.canvas.focus();
            s.status(&format!(
                "session {} — {}x{} {codec}",
                ready.session_id, ready.width, ready.height
            ));
            if let Some(el) = &s.sessions {
                el.set_text_content(None);
            }
        }
        Message::SessionEnd(end) => {
            let mut s = state.borrow_mut();
            s.status(&format!("session ended: {}", end.reason));
            s.closed = true;
            if let Some(input) = &s.input {
                input.release_all();
            }
        }
        Message::Ping(p) => {
            state.borrow().send(Message::Pong(Pong {
                timestamp_us: p.timestamp_us,
            }));
        }
        // Pong, WindowList, CursorUpdate and anything a newer server adds are
        // not errors here: an unhandled control message is ignored rather than
        // tearing down a working session.
        _ => {}
    }
    Ok(())
}

/// Draw the session list as buttons. Rust owns this so the page does not need
/// its own rendering code.
fn render_sessions(
    state: &Rc<RefCell<State>>,
    sessions: &[termland_protocol::SessionInfo],
) -> Result<(), JsValue> {
    let mut s = state.borrow_mut();
    let Some(container) = s.sessions.clone() else {
        return Ok(());
    };
    container.set_text_content(None);
    s.session_buttons.clear();

    if sessions.is_empty() {
        let p = s.doc.create_element("p")?;
        p.set_text_content(Some("No sessions yet."));
        container.append_child(&p)?;
        return Ok(());
    }

    for info in sessions {
        let button = s.doc.create_element("button")?;
        button.set_text_content(Some(&format!(
            "{} — {}x{} {} ({})",
            info.session_id,
            info.width,
            info.height,
            info.mode,
            if info.attached { "attached" } else { "free" },
        )));
        let outbox = s.outbox.clone();
        let id = info.session_id.clone();
        let codecs: Vec<VideoCodec> = s.probed.iter().map(|(c, _)| *c).collect();
        let cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |_e: web_sys::Event| {
            if let Some(out) = &outbox {
                out.send(&Message::SessionAttach(SessionAttach {
                    session_id: id.clone(),
                    audio: false,
                    quality: 75,
                    encoder_preset: None,
                    encoder_crf: None,
                    encoder_extra_params: None,
                    supported_codecs: codecs.clone(),
                    supported_audio_codecs: Vec::<AudioCodec>::new(),
                }));
            }
        });
        button.add_event_listener_with_callback("click", cb.as_ref().unchecked_ref())?;
        container.append_child(&button)?;
        s.session_buttons.push(cb);
    }
    Ok(())
}

/// Read the server-opened uni streams carrying Q2 video.
async fn video_task(transport: WebTransport, state: Rc<RefCell<State>>) {
    let streams = transport.incoming_unidirectional_streams();
    let reader: web_sys::ReadableStreamDefaultReader = streams.get_reader().unchecked_into();
    loop {
        let Ok(result) = wasm_bindgen_futures::JsFuture::from(reader.read()).await else {
            return;
        };
        let result: js_sys::Object = result.unchecked_into();
        let done = js_sys::Reflect::get(&result, &JsValue::from_str("done"))
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        if done {
            return;
        }
        let Ok(stream) = js_sys::Reflect::get(&result, &JsValue::from_str("value")) else {
            return;
        };
        let mut q2 = Q2Reader::new(stream.unchecked_into());
        loop {
            match q2.next().await {
                Ok(Some(frame)) => {
                    let mut s = state.borrow_mut();
                    if s.closed {
                        return;
                    }
                    // A tab freeze can close the decoder underneath us; the
                    // next keyframe rebuilds it rather than dropping video
                    // silently until reconnect.
                    let needs = s.video.as_ref().is_some_and(|v| v.needs_reconfigure());
                    if needs && frame.keyframe {
                        let probed = s.probed.clone();
                        let (w, h) = (frame.width, frame.height);
                        let codec = frame.codec;
                        if let Some(v) = s.video.as_mut() {
                            if let Err(e) = v.configure(codec, w, h, &probed) {
                                web_sys::console::warn_2(
                                    &JsValue::from_str("decoder rebuild failed:"),
                                    &e,
                                );
                            }
                        }
                    }
                    if let Some(v) = s.video.as_mut() {
                        if let Err(e) = v.push(&frame) {
                            web_sys::console::warn_2(&JsValue::from_str("decode failed:"), &e);
                        }
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    web_sys::console::warn_2(&JsValue::from_str("video stream error:"), &e);
                    return;
                }
            }
        }
    }
}

fn start_ping(state: &Rc<RefCell<State>>) {
    let mut s = state.borrow_mut();
    if s.ping_handle.is_some() {
        return;
    }
    let inner = state.clone();
    let cb = Closure::<dyn FnMut()>::new(move || {
        let s = inner.borrow();
        if s.closed {
            return;
        }
        s.send(Message::Ping(Ping {
            timestamp_us: (js_sys::Date::now() * 1000.0) as u64,
        }));
    });
    if let Some(win) = web_sys::window() {
        if let Ok(handle) = win.set_interval_with_callback_and_timeout_and_arguments_0(
            cb.as_ref().unchecked_ref(),
            PING_INTERVAL_MS,
        ) {
            s.ping_handle = Some(handle);
        }
    }
    s.ping_closure = Some(cb);
}

fn stop_ping(s: &mut State) {
    if let (Some(handle), Some(win)) = (s.ping_handle.take(), web_sys::window()) {
        win.clear_interval_with_handle(handle);
    }
    s.ping_closure = None;
}

/// A `JsValue` error is often an Error object, sometimes a string.
fn describe(e: &JsValue) -> String {
    e.as_string()
        .or_else(|| {
            js_sys::Reflect::get(e, &JsValue::from_str("message"))
                .ok()
                .and_then(|m| m.as_string())
        })
        .unwrap_or_else(|| format!("{e:?}"))
}
