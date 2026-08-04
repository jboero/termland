//! Pointer cursor capture via `ext-image-copy-capture-v1`.
//!
//! `zwlr_screencopy_manager_v1` (see screencopy.rs) captures the whole output
//! and can optionally paint the compositor's cursor into that frame, but it
//! has no way to hand back *just* the cursor bitmap on its own. When the
//! client wants to render its own cursor for latency reasons ("client-side
//! cursor" mode - see `CursorModeMsg` in termland-protocol), the video stream
//! no longer carries the cursor at all, and the client falls back to drawing
//! a generic placeholder shape unless something tells it what the remote
//! cursor actually looks like.
//!
//! `ext_image_copy_capture_manager_v1.create_pointer_cursor_session` is that
//! something: it's the correct external-observer API for exactly this - a
//! session dedicated to the pointer's cursor image, independent of the video
//! frame. It does NOT give us the semantic shape name ("text", "wait",
//! "resize", ...): that negotiation happens purely between the inner
//! application and labwc's own cursor renderer, and we have no hook into it.
//! What it gives us is the thing that actually needs to match on the client:
//! the cursor's bitmap, hotspot, and visibility, captured independently of
//! the encoded video.
//!
//! This is a *staging* (not yet stable) Wayland protocol. It requires the
//! compositor to implement `ext_image_capture_source_v1`,
//! `ext_output_image_capture_source_manager_v1`, and
//! `ext_image_copy_capture_manager_v1`. If any of those globals are missing
//! (older wlroots/labwc), [`CursorCapturer::connect`] fails cleanly and the
//! caller falls back to no cursor capture (client keeps its generic
//! placeholder) rather than crashing.

use std::os::unix::io::OwnedFd;
use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle,
    protocol::{wl_buffer, wl_output, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool},
};
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_image_capture_source_v1::ExtImageCaptureSourceV1,
    ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_cursor_session_v1::{self, ExtImageCopyCaptureCursorSessionV1},
    ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
    ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1,
    ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
};

use crate::screencopy::{CaptureError, create_shm_buffer};

#[derive(Debug, thiserror::Error)]
pub enum CursorCaptureError {
    #[error("wayland connect: {0}")]
    Connect(String),
    #[error("no wl_shm global")]
    NoShm,
    #[error("no wl_output global")]
    NoOutput,
    #[error("no wl_seat global")]
    NoSeat,
    #[error("no ext_output_image_capture_source_manager_v1 global (compositor lacks staging cursor-capture support)")]
    NoSourceManager,
    #[error("no ext_image_copy_capture_manager_v1 global (compositor lacks staging cursor-capture support)")]
    NoCaptureManager,
    #[error("compositor never sent buffer constraints for the cursor session")]
    NoConstraints,
    #[error("cursor capture session stopped by the compositor")]
    Stopped,
    #[error("capture failed: {0}")]
    Failed(String),
    #[error("shm error: {0}")]
    ShmError(String),
}

impl From<CaptureError> for CursorCaptureError {
    fn from(e: CaptureError) -> Self {
        CursorCaptureError::ShmError(e.to_string())
    }
}

/// One captured cursor image, or a "not visible" result if the pointer isn't
/// currently over the captured output at all.
#[derive(Debug, Clone, Default)]
pub struct CursorCapture {
    /// Position of the cursor hotspot in output pixel coordinates. The
    /// client does NOT use this for placement - see the note in
    /// termland-server's transport.rs on why position stays purely
    /// locally-tracked - but it's carried on the wire because the protocol
    /// message shape includes it and future consumers may want it.
    pub x: i32,
    pub y: i32,
    pub hotspot_x: i32,
    pub hotspot_y: i32,
    pub width: u32,
    pub height: u32,
    pub visible: bool,
    pub rgba: Vec<u8>,
}

/// State for our Wayland client that captures pointer cursor images.
struct CursorState {
    shm: Option<wl_shm::WlShm>,
    output: Option<wl_output::WlOutput>,
    seat: Option<wl_seat::WlSeat>,
    source_mgr: Option<ExtOutputImageCaptureSourceManagerV1>,
    capture_mgr: Option<ExtImageCopyCaptureManagerV1>,

    // Cursor-session-level state (from ext_image_copy_capture_cursor_session_v1
    // events). Persists across frames; only changes when the compositor
    // reports the cursor moved, entered, or left the captured area.
    cursor_present: bool,
    pos_x: i32,
    pos_y: i32,
    hotspot_x: i32,
    hotspot_y: i32,

    // Buffer constraints for the cursor's own capture session (from
    // ext_image_copy_capture_session_v1 events). The compositor re-sends
    // these whenever the cursor bitmap's size/format changes - e.g. swapping
    // from a small arrow to a larger resize-corner glyph - so we re-check
    // them before every frame rather than caching them once at connect time.
    buf_width: u32,
    buf_height: u32,
    shm_format: Option<u32>,
    constraints_done: bool,
    session_stopped: bool,

    // Per-frame capture state, reset before each create_frame/capture cycle.
    frame_ready: bool,
    frame_failed: bool,
}

impl CursorState {
    fn new() -> Self {
        Self {
            shm: None,
            output: None,
            seat: None,
            source_mgr: None,
            capture_mgr: None,
            cursor_present: false,
            pos_x: 0,
            pos_y: 0,
            hotspot_x: 0,
            hotspot_y: 0,
            buf_width: 0,
            buf_height: 0,
            shm_format: None,
            constraints_done: false,
            session_stopped: false,
            frame_ready: false,
            frame_failed: false,
        }
    }
}

// --- Wayland dispatch implementations ---

impl Dispatch<wl_registry::WlRegistry, ()> for CursorState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            match interface.as_str() {
                "wl_shm" => {
                    state.shm = Some(registry.bind::<wl_shm::WlShm, _, _>(name, version.min(1), qh, ()));
                }
                "wl_output" => {
                    if state.output.is_none() {
                        state.output =
                            Some(registry.bind::<wl_output::WlOutput, _, _>(name, version.min(4), qh, ()));
                    }
                }
                "wl_seat" => {
                    if state.seat.is_none() {
                        state.seat =
                            Some(registry.bind::<wl_seat::WlSeat, _, _>(name, version.min(8), qh, ()));
                    }
                }
                "ext_output_image_capture_source_manager_v1" => {
                    state.source_mgr = Some(registry.bind::<ExtOutputImageCaptureSourceManagerV1, _, _>(
                        name,
                        version.min(1),
                        qh,
                        (),
                    ));
                }
                "ext_image_copy_capture_manager_v1" => {
                    state.capture_mgr = Some(registry.bind::<ExtImageCopyCaptureManagerV1, _, _>(
                        name,
                        version.min(1),
                        qh,
                        (),
                    ));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_shm::WlShm, ()> for CursorState {
    fn event(_: &mut Self, _: &wl_shm::WlShm, _: wl_shm::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for CursorState {
    fn event(_: &mut Self, _: &wl_shm_pool::WlShmPool, _: wl_shm_pool::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_buffer::WlBuffer, ()> for CursorState {
    fn event(_: &mut Self, _: &wl_buffer::WlBuffer, _: wl_buffer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_output::WlOutput, ()> for CursorState {
    fn event(_: &mut Self, _: &wl_output::WlOutput, _: wl_output::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_seat::WlSeat, ()> for CursorState {
    fn event(_: &mut Self, _: &wl_seat::WlSeat, _: wl_seat::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_pointer::WlPointer, ()> for CursorState {
    // We only need the wl_pointer object as a handle to identify which seat's
    // pointer to capture the cursor of; we have no surface for it to send
    // enter/motion/button events about, so there's nothing to act on here.
    fn event(_: &mut Self, _: &wl_pointer::WlPointer, _: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ExtOutputImageCaptureSourceManagerV1, ()> for CursorState {
    fn event(
        _: &mut Self,
        _: &ExtOutputImageCaptureSourceManagerV1,
        _: <ExtOutputImageCaptureSourceManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtImageCaptureSourceV1, ()> for CursorState {
    fn event(
        _: &mut Self,
        _: &ExtImageCaptureSourceV1,
        _: <ExtImageCaptureSourceV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtImageCopyCaptureManagerV1, ()> for CursorState {
    fn event(
        _: &mut Self,
        _: &ExtImageCopyCaptureManagerV1,
        _: <ExtImageCopyCaptureManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtImageCopyCaptureCursorSessionV1, ()> for CursorState {
    fn event(
        state: &mut Self,
        _session: &ExtImageCopyCaptureCursorSessionV1,
        event: ext_image_copy_capture_cursor_session_v1::Event,
        _: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_cursor_session_v1::Event;
        match event {
            Event::Enter => {
                state.cursor_present = true;
            }
            Event::Leave => {
                state.cursor_present = false;
            }
            Event::Position { x, y } => {
                state.pos_x = x;
                state.pos_y = y;
            }
            Event::Hotspot { x, y } => {
                state.hotspot_x = x;
                state.hotspot_y = y;
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, ()> for CursorState {
    fn event(
        state: &mut Self,
        _session: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        _: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_session_v1::Event;
        match event {
            Event::BufferSize { width, height } => {
                state.buf_width = width;
                state.buf_height = height;
            }
            Event::ShmFormat { format } => {
                // Prefer a format screencopy.rs's decoder actually handles
                // (4bpp beats 3bpp); keep whatever we already picked otherwise.
                let fmt: u32 = format.into();
                let is_4bpp = matches!(fmt, 0 | 1 | 875713089 | 875713112 | 875708993 | 875709016);
                if is_4bpp || state.shm_format.is_none() {
                    state.shm_format = Some(fmt);
                }
            }
            Event::Done => {
                state.constraints_done = true;
            }
            Event::Stopped => {
                state.session_stopped = true;
                tracing::warn!("Cursor capture session stopped by compositor");
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, ()> for CursorState {
    fn event(
        state: &mut Self,
        _frame: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        _: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_frame_v1::Event;
        match event {
            Event::Ready => {
                state.frame_ready = true;
            }
            Event::Failed { reason } => {
                state.frame_failed = true;
                tracing::debug!("Cursor frame capture failed: {reason:?}");
            }
            // Transform / Damage / PresentationTime: not needed, the cursor
            // bitmap is small enough that we just re-read it whole each time.
            _ => {}
        }
    }
}

/// High-level cursor capturer: connects to the compositor as a second
/// Wayland client (independent from screencopy's video capture connection)
/// and pulls the pointer cursor's bitmap on demand.
pub struct CursorCapturer {
    _conn: Connection,
    event_queue: EventQueue<CursorState>,
    state: CursorState,
    capture_session: ExtImageCopyCaptureSessionV1,
    /// Kept alive only so the compositor doesn't tear down objects derived
    /// from them (`capture_session` is a child of `_cursor_session`); never
    /// used directly after setup.
    _cursor_session: ExtImageCopyCaptureCursorSessionV1,
    _source: ExtImageCaptureSourceV1,
    _pointer: wl_pointer::WlPointer,
}

impl CursorCapturer {
    /// Connect to `display_name` and set up a pointer cursor capture session
    /// for its (sole) output. Fails cleanly - rather than panicking or
    /// hanging - if the compositor doesn't advertise the staging protocols
    /// this needs, so callers can treat cursor-shape sync as optional.
    ///
    /// `runtime_dir`: see `screencopy::ScreenCapturer::connect`'s doc
    /// comment - must be the compositor's actual runtime dir, not
    /// necessarily this process's own.
    pub fn connect(display_name: &str, runtime_dir: &std::path::Path) -> Result<Self, CursorCaptureError> {
        let socket_path = runtime_dir.join(display_name);

        let stream = std::os::unix::net::UnixStream::connect(&socket_path)
            .map_err(|e| CursorCaptureError::Connect(format!("{}: {e}", socket_path.display())))?;

        let conn = Connection::from_socket(stream)
            .map_err(|e| CursorCaptureError::Connect(e.to_string()))?;

        let display = conn.display();
        let mut event_queue = conn.new_event_queue();
        let qh = event_queue.handle();

        let mut state = CursorState::new();

        let _registry = display.get_registry(&qh, ());
        event_queue
            .roundtrip(&mut state)
            .map_err(|e| CursorCaptureError::Connect(format!("roundtrip: {e}")))?;

        let shm = state.shm.clone().ok_or(CursorCaptureError::NoShm)?;
        let output = state.output.clone().ok_or(CursorCaptureError::NoOutput)?;
        let seat = state.seat.clone().ok_or(CursorCaptureError::NoSeat)?;
        let source_mgr = state.source_mgr.clone().ok_or(CursorCaptureError::NoSourceManager)?;
        let capture_mgr = state.capture_mgr.clone().ok_or(CursorCaptureError::NoCaptureManager)?;
        let _ = shm; // kept on `state`, just validating presence here

        let pointer = seat.get_pointer(&qh, ());
        let source = source_mgr.create_source(&output, &qh, ());
        let cursor_session = capture_mgr.create_pointer_cursor_session(&source, &pointer, &qh, ());
        let capture_session = cursor_session.get_capture_session(&qh, ());

        // One roundtrip is enough for the compositor to have sent the initial
        // buffer_size/shm_format/done batch (and any immediate enter, if the
        // pointer already happens to be over this output) - these were all
        // queued before the registry.get_registry sync point that roundtrip
        // waits for, so the server processes them in order first.
        event_queue
            .roundtrip(&mut state)
            .map_err(|e| CursorCaptureError::Connect(format!("roundtrip for cursor session: {e}")))?;

        if state.session_stopped {
            return Err(CursorCaptureError::Stopped);
        }
        if !state.constraints_done {
            return Err(CursorCaptureError::NoConstraints);
        }

        tracing::info!("Cursor capturer ready on display {display_name}");

        Ok(Self {
            _conn: conn,
            event_queue,
            state,
            capture_session,
            _cursor_session: cursor_session,
            _source: source,
            _pointer: pointer,
        })
    }

    /// Capture the current cursor image. Returns `visible: false` (with no
    /// pixel data) when the pointer isn't over the captured output at all,
    /// rather than erroring - that's a normal, common state, not a failure.
    pub fn capture_cursor(&mut self) -> Result<CursorCapture, CursorCaptureError> {
        // Pump any pending enter/leave/position/hotspot/buffer-constraint
        // events before deciding whether (and at what size) to capture.
        self.event_queue
            .roundtrip(&mut self.state)
            .map_err(|e| CursorCaptureError::Failed(format!("roundtrip for cursor state: {e}")))?;

        if self.state.session_stopped {
            return Err(CursorCaptureError::Stopped);
        }

        if !self.state.cursor_present {
            return Ok(CursorCapture {
                visible: false,
                ..Default::default()
            });
        }

        let width = self.state.buf_width;
        let height = self.state.buf_height;
        if width == 0 || height == 0 {
            // Cursor is present but the compositor hasn't told us a buffer
            // size yet; nothing sensible to capture into this round.
            return Ok(CursorCapture {
                visible: false,
                ..Default::default()
            });
        }
        let format = self.state.shm_format.ok_or(CursorCaptureError::NoConstraints)?;

        let qh = self.event_queue.handle();
        let shm = self.state.shm.as_ref().ok_or(CursorCaptureError::NoShm)?.clone();

        // wl_shm requires a byte-aligned stride; 4 bytes/pixel for every
        // format screencopy's decoder accepts (see read_shm_to_rgba).
        let bpp = if matches!(format, 875710274 | 875710290) { 3 } else { 4 };
        let stride = width * bpp as u32;
        let size = (stride * height) as usize;

        let (pool, buffer, fd) = create_shm_buffer(&shm, width, height, stride, format, &qh)?;

        self.state.frame_ready = false;
        self.state.frame_failed = false;

        let frame = self.capture_session.create_frame(&qh, ());
        frame.attach_buffer(&buffer);
        frame.damage_buffer(0, 0, width as i32, height as i32);
        frame.capture();

        // Bounded wait: unlike screencopy's full-frame capture (driven by a
        // fixed ~30fps loop where an indefinite wait is fine), this runs in a
        // tight polling loop and the cursor can leave the captured area mid
        // capture, so we cap retries instead of blocking forever.
        const MAX_ROUNDTRIPS: usize = 50;
        let mut tries = 0;
        while !self.state.frame_ready && !self.state.frame_failed {
            self.event_queue
                .roundtrip(&mut self.state)
                .map_err(|e| CursorCaptureError::Failed(format!("roundtrip for cursor frame: {e}")))?;
            tries += 1;
            if tries > MAX_ROUNDTRIPS {
                frame.destroy();
                buffer.destroy();
                pool.destroy();
                return Err(CursorCaptureError::Failed("timed out waiting for cursor frame".into()));
            }
        }

        if self.state.frame_failed {
            buffer.destroy();
            pool.destroy();
            return Err(CursorCaptureError::Failed("compositor reported failure".into()));
        }

        let rgba = read_shm_to_rgba_preserving_alpha(&fd, size, width, height, stride, format)?;

        buffer.destroy();
        pool.destroy();

        Ok(CursorCapture {
            x: self.state.pos_x,
            y: self.state.pos_y,
            hotspot_x: self.state.hotspot_x,
            hotspot_y: self.state.hotspot_y,
            width,
            height,
            visible: true,
            rgba,
        })
    }
}

/// Read a captured cursor buffer from shm and convert to RGBA, preserving the
/// REAL per-pixel alpha channel.
///
/// This deliberately does NOT reuse screencopy.rs's `read_shm_to_rgba`: that
/// one hardcodes alpha=255 for every pixel, which is correct for a full
/// screen capture (the output is opaque) but wrong here - a cursor bitmap's
/// whole point is a transparent surround around an opaque glyph, and
/// dropping that would make every cursor render as an opaque square.
///
/// Format/byte-layout table (see screencopy.rs's longer comment for the
/// derivation): only ARGB8888 and ABGR8888 carry a real alpha byte; XRGB8888/
/// XBGR8888's 4th byte is unspecified padding (treat as opaque), and the
/// 3bpp formats have no 4th byte at all.
fn read_shm_to_rgba_preserving_alpha(
    fd: &OwnedFd,
    size: usize,
    width: u32,
    height: u32,
    stride: u32,
    format: u32,
) -> Result<Vec<u8>, CursorCaptureError> {
    use std::os::fd::AsRawFd;

    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ,
            libc::MAP_SHARED,
            fd.as_raw_fd(),
            0,
        )
    };
    if ptr == libc::MAP_FAILED {
        return Err(CursorCaptureError::ShmError("mmap failed".into()));
    }

    let data = unsafe { std::slice::from_raw_parts(ptr as *const u8, size) };

    // (bytes-per-pixel, bgr_order, has_real_alpha)
    let (bpp, bgr_order, has_alpha): (usize, bool, bool) = match format {
        0 => (4, true, true),          // wl_shm ARGB8888
        1 => (4, true, false),         // wl_shm XRGB8888
        875713089 => (4, true, true),  // AR24 ARGB8888
        875713112 => (4, true, false), // XR24 XRGB8888
        875708993 => (4, false, true), // AB24 ABGR8888
        875709016 => (4, false, false),// XB24 XBGR8888
        875710274 => (3, false, false),// BG24 BGR888
        875710290 => (3, true, false), // RG24 RGB888
        _ => {
            tracing::warn!("Unknown cursor shm format {format:#x} ({format}), guessing BGRA");
            (4, true, true)
        }
    };

    let pixel_count = (width * height) as usize;
    let mut rgba = Vec::with_capacity(pixel_count * 4);

    for y in 0..height as usize {
        let row_offset = y * stride as usize;
        for x in 0..width as usize {
            let px_offset = row_offset + x * bpp;
            let (r, g, b) = if bgr_order {
                (data[px_offset + 2], data[px_offset + 1], data[px_offset])
            } else {
                (data[px_offset], data[px_offset + 1], data[px_offset + 2])
            };
            let a = if has_alpha { data[px_offset + 3] } else { 255 };
            rgba.extend_from_slice(&[r, g, b, a]);
        }
    }

    unsafe {
        libc::munmap(ptr, size);
    }

    Ok(rgba)
}
