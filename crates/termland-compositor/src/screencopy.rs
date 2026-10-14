//! wlr-screencopy frame capture client.
//!
//! Connects to a Wayland compositor as a client and uses the
//! zwlr_screencopy_manager_v1 protocol to capture screen frames.

use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::io::OwnedFd;
use std::time::{Duration, Instant};
use wayland_client::{
    backend::ObjectId,
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
    protocol::{wl_buffer, wl_output, wl_registry, wl_shm, wl_shm_pool},
};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1, zwlr_screencopy_manager_v1,
};

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("wayland connect: {0}")]
    Connect(String),
    #[error("no wl_shm global")]
    NoShm,
    #[error("no wl_output global")]
    NoOutput,
    #[error("no screencopy manager global")]
    NoScreencopy,
    #[error("capture failed: {0}")]
    Failed(String),
    #[error("shm error: {0}")]
    ShmError(String),
}

/// State for our Wayland client that captures frames.
struct CaptureState {
    shm: Option<wl_shm::WlShm>,
    output: Option<wl_output::WlOutput>,
    screencopy_mgr: Option<zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1>,

    // Frame capture state
    frame_width: u32,
    frame_height: u32,
    frame_stride: u32,
    frame_format: u32,
    frame_ready: bool,
    frame_failed: bool,
    buffer_ready: bool,
    /// The frame request the events below belong to. Events for any other
    /// (an abandoned request whose events were already in flight) are ignored.
    current_frame: Option<ObjectId>,
    /// Damage reported for the current frame (copy_with_damage only).
    damage: Vec<DamageRect>,
}

impl CaptureState {
    fn new() -> Self {
        Self {
            shm: None,
            output: None,
            screencopy_mgr: None,
            frame_width: 0,
            frame_height: 0,
            frame_stride: 0,
            frame_format: 0,
            frame_ready: false,
            frame_failed: false,
            buffer_ready: false,
            current_frame: None,
            damage: Vec::new(),
        }
    }
}

// --- Wayland dispatch implementations ---

impl Dispatch<wl_registry::WlRegistry, ()> for CaptureState {
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
                    let shm = registry.bind::<wl_shm::WlShm, _, _>(name, version.min(1), qh, ());
                    state.shm = Some(shm);
                }
                "wl_output" => {
                    if state.output.is_none() {
                        let output = registry
                            .bind::<wl_output::WlOutput, _, _>(name, version.min(4), qh, ());
                        state.output = Some(output);
                    }
                }
                "zwlr_screencopy_manager_v1" => {
                    let mgr = registry
                        .bind::<zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1, _, _>(
                            name,
                            version.min(3),
                            qh,
                            (),
                        );
                    state.screencopy_mgr = Some(mgr);
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_shm::WlShm, ()> for CaptureState {
    fn event(
        _state: &mut Self,
        _shm: &wl_shm::WlShm,
        _event: wl_shm::Event,
        _: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // wl_shm::Event::Format - we don't need to track these
    }
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for CaptureState {
    fn event(
        _state: &mut Self,
        _pool: &wl_shm_pool::WlShmPool,
        _event: wl_shm_pool::Event,
        _: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for CaptureState {
    fn event(
        _state: &mut Self,
        _buffer: &wl_buffer::WlBuffer,
        _event: wl_buffer::Event,
        _: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_output::WlOutput, ()> for CaptureState {
    fn event(
        _state: &mut Self,
        _output: &wl_output::WlOutput,
        _event: wl_output::Event,
        _: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1, ()> for CaptureState {
    fn event(
        _state: &mut Self,
        _mgr: &zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
        _event: zwlr_screencopy_manager_v1::Event,
        _: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwlr_screencopy_frame_v1::ZwlrScreencopyFrameV1, ()> for CaptureState {
    fn event(
        state: &mut Self,
        frame: &zwlr_screencopy_frame_v1::ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        _: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if state.current_frame.as_ref() != Some(&frame.id()) {
            return;
        }
        match event {
            zwlr_screencopy_frame_v1::Event::Buffer {
                format,
                width,
                height,
                stride,
            } => {
                let fmt: u32 = format.into();
                // Accept 4bpp formats preferentially, 3bpp as fallback.
                // fourcc(a,b,c,d) = a | b<<8 | c<<16 | d<<24
                //   ARGB8888 'AR24' = 875713089
                //   XRGB8888 'XR24' = 875713112
                //   ABGR8888 'AB24' = 875708993
                //   XBGR8888 'XB24' = 875709016
                //   BGR888   'BG24' = 875710274 (3bpp)
                //   RGB888   'RG24' = 875710290 (3bpp)
                let is_4bpp = matches!(fmt,
                    0 | 1 |                                  // wl_shm ARGB/XRGB
                    875713089 | 875713112 |                  // ARGB8888 / XRGB8888
                    875708993 | 875709016                    // ABGR8888 / XBGR8888
                );
                let is_3bpp = matches!(fmt, 875710274 | 875710290);
                let fmt_name = match fmt {
                    0 => "ARGB8888", 1 => "XRGB8888",
                    875713089 => "ARGB8888", 875713112 => "XRGB8888",
                    875708993 => "ABGR8888", 875709016 => "XBGR8888",
                    875710274 => "BGR888", 875710290 => "RGB888",
                    _ => "unknown",
                };
                tracing::debug!(
                    "Screencopy buffer offer: {}x{} stride={} format={} ({}) {}",
                    width, height, stride, fmt, fmt_name,
                    if is_4bpp { "[4bpp]" } else if is_3bpp { "[3bpp]" } else { "[unsupported]" }
                );
                // Accept 4bpp always (overwrites any 3bpp); accept 3bpp only if nothing yet
                let currently_3bpp = state.buffer_ready && matches!(state.frame_format, 875710274 | 875710290);
                if is_4bpp || (is_3bpp && (!state.buffer_ready || currently_3bpp)) {
                    state.frame_format = fmt;
                    state.frame_width = width;
                    state.frame_height = height;
                    state.frame_stride = stride;
                    state.buffer_ready = true;
                }
            }
            zwlr_screencopy_frame_v1::Event::Damage { x, y, width, height } => {
                state.damage.push(DamageRect { x, y, width, height });
            }
            zwlr_screencopy_frame_v1::Event::Ready { .. } => {
                state.frame_ready = true;
            }
            zwlr_screencopy_frame_v1::Event::Failed => {
                state.frame_failed = true;
                tracing::error!("Screencopy frame capture failed");
            }
            _ => {}
        }
    }
}

/// A region of the output that changed since the previous capture, in
/// buffer pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DamageRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// A captured output frame.
#[derive(Debug)]
pub struct ScreenFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// What changed since the previous capture. Empty for a capture that did
    /// not ask for damage ([`ScreenCapturer::capture_frame`]).
    pub damage: Vec<DamageRect>,
}

/// The wl_shm buffer frames are copied into. Kept, and kept mapped, for as
/// long as the output's size and format stay the same, instead of creating a
/// memfd, pool and mapping for every frame.
struct ShmBuffer {
    pool: wl_shm_pool::WlShmPool,
    buffer: wl_buffer::WlBuffer,
    _fd: OwnedFd,
    map: *mut libc::c_void,
    size: usize,
    width: u32,
    height: u32,
    stride: u32,
    format: u32,
}

// The mapping is private to this buffer and only read through `&self`.
unsafe impl Send for ShmBuffer {}

impl ShmBuffer {
    fn new(
        shm: &wl_shm::WlShm,
        width: u32,
        height: u32,
        stride: u32,
        format: u32,
        qh: &QueueHandle<CaptureState>,
    ) -> Result<Self, CaptureError> {
        let size = (stride * height) as usize;
        let (pool, buffer, fd) = create_shm_buffer(shm, width, height, stride, format, qh)?;
        let map = unsafe {
            libc::mmap(std::ptr::null_mut(), size, libc::PROT_READ, libc::MAP_SHARED, fd.as_raw_fd(), 0)
        };
        if map == libc::MAP_FAILED {
            buffer.destroy();
            pool.destroy();
            return Err(CaptureError::ShmError("mmap failed".into()));
        }
        Ok(Self { pool, buffer, _fd: fd, map, size, width, height, stride, format })
    }

    fn fits(&self, width: u32, height: u32, stride: u32, format: u32) -> bool {
        (self.width, self.height, self.stride, self.format) == (width, height, stride, format)
    }

    fn data(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.map as *const u8, self.size) }
    }
}

impl Drop for ShmBuffer {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.map, self.size) };
        self.buffer.destroy();
        self.pool.destroy();
    }
}

/// High-level screen capturer that manages the Wayland connection and screencopy.
pub struct ScreenCapturer {
    _conn: Connection,
    event_queue: EventQueue<CaptureState>,
    state: CaptureState,
    /// Set after we log the first selected format, so we only log it once.
    format_logged: bool,
    shm_buffer: Option<ShmBuffer>,
    /// A frame request the compositor has not completed yet. A
    /// copy_with_damage request stays pending until something changes, and is
    /// kept across calls so that a wait which times out does not lose it.
    pending: Option<zwlr_screencopy_frame_v1::ZwlrScreencopyFrameV1>,
}

impl ScreenCapturer {
    /// Connect to the given Wayland display and bind screencopy globals.
    ///
    /// `runtime_dir` must be the compositor's ACTUAL `XDG_RUNTIME_DIR` (see
    /// `backend::DetachedBackend`'s doc comment) - under session isolation
    /// this is the target user's `/run/user/<uid>`, which is not necessarily
    /// this (server) process's own. Reading `XDG_RUNTIME_DIR` from our own
    /// environment here would silently connect to (or fail to find) the
    /// wrong socket for an isolated session.
    pub fn connect(display_name: &str, runtime_dir: &std::path::Path) -> Result<Self, CaptureError> {
        let socket_path = runtime_dir.join(display_name);

        let stream = std::os::unix::net::UnixStream::connect(&socket_path)
            .map_err(|e| CaptureError::Connect(format!("{}: {e}", socket_path.display())))?;

        let conn = Connection::from_socket(stream)
            .map_err(|e| CaptureError::Connect(e.to_string()))?;

        let display = conn.display();
        let mut event_queue = conn.new_event_queue();
        let qh = event_queue.handle();

        let mut state = CaptureState::new();

        // Get the registry and do initial roundtrip to discover globals
        let _registry = display.get_registry(&qh, ());
        event_queue
            .roundtrip(&mut state)
            .map_err(|e| CaptureError::Connect(format!("roundtrip: {e}")))?;

        if state.shm.is_none() {
            return Err(CaptureError::NoShm);
        }
        if state.output.is_none() {
            return Err(CaptureError::NoOutput);
        }
        if state.screencopy_mgr.is_none() {
            return Err(CaptureError::NoScreencopy);
        }

        tracing::info!("Screencopy capturer ready on display {display_name}");

        Ok(Self {
            _conn: conn,
            event_queue,
            state,
            format_logged: false,
            shm_buffer: None,
            pending: None,
        })
    }

    /// Capture a frame now, whether or not anything changed. Returns
    /// (width, height, rgba_data).
    /// `overlay_cursor`: include the compositor's cursor in the captured frame.
    pub fn capture_frame(&mut self, overlay_cursor: bool) -> Result<(u32, u32, Vec<u8>), CaptureError> {
        self.abandon_pending();
        self.request(overlay_cursor, false)?;
        self.wait(None)?;
        let frame = self.finish()?;
        Ok((frame.width, frame.height, frame.rgba))
    }

    /// Capture the next frame that differs from the previous capture, waiting
    /// at most `timeout` for something to change. `Ok(None)` means nothing
    /// did; the request stays pending, so the next call carries on waiting
    /// for the same change rather than starting over.
    pub fn capture_damaged(
        &mut self,
        overlay_cursor: bool,
        timeout: Duration,
    ) -> Result<Option<ScreenFrame>, CaptureError> {
        if self.pending.is_none() {
            self.request(overlay_cursor, true)?;
        }
        if !self.wait(Some(Instant::now() + timeout))? {
            return Ok(None);
        }
        self.finish().map(Some)
    }

    /// Ask the compositor for the next frame, into our shm buffer.
    fn request(&mut self, overlay_cursor: bool, with_damage: bool) -> Result<(), CaptureError> {
        let qh = self.event_queue.handle();
        let output = self.state.output.as_ref().ok_or(CaptureError::NoOutput)?.clone();
        let mgr = self.state.screencopy_mgr.as_ref().ok_or(CaptureError::NoScreencopy)?.clone();

        self.state.buffer_ready = false;
        self.state.frame_ready = false;
        self.state.frame_failed = false;
        self.state.damage.clear();

        // overlay_cursor=1 → include the compositor's software cursor in the captured
        // frame. wlroots renders cursors in a separate plane by default, so we have to
        // ask for it explicitly. The client can toggle this off to render its own local
        // cursor for lower latency.
        let frame = mgr.capture_output(overlay_cursor as i32, &output, &qh, ());
        self.state.current_frame = Some(frame.id());

        // Roundtrip to get the Buffer event with dimensions
        if let Err(e) = self.event_queue.roundtrip(&mut self.state) {
            frame.destroy();
            return Err(CaptureError::Failed(format!("roundtrip for buffer info: {e}")));
        }
        if !self.state.buffer_ready {
            frame.destroy();
            return Err(CaptureError::Failed("no buffer info received".into()));
        }

        let (width, height) = (self.state.frame_width, self.state.frame_height);
        let (stride, format) = (self.state.frame_stride, self.state.frame_format);

        // Log the selected format once, at INFO level, so the user can diagnose
        // colorspace issues without enabling debug logging.
        if !self.format_logged {
            let fmt_name = match format {
                0 => "ARGB8888(wl_shm)", 1 => "XRGB8888(wl_shm)",
                875713089 => "ARGB8888(AR24)", 875713112 => "XRGB8888(XR24)",
                875708993 => "ABGR8888(AB24)", 875709016 => "XBGR8888(XB24)",
                875710274 => "BGR888(BG24)", 875710290 => "RGB888(RG24)",
                _ => "unknown",
            };
            tracing::info!("Screencopy format: {fmt_name} ({format}), {width}x{height} stride={stride}");
            self.format_logged = true;
        }

        if !self.shm_buffer.as_ref().is_some_and(|b| b.fits(width, height, stride, format)) {
            // Drop the old buffer before creating its replacement.
            self.shm_buffer = None;
            let shm = self.state.shm.as_ref().ok_or(CaptureError::NoShm)?.clone();
            match ShmBuffer::new(&shm, width, height, stride, format, &qh) {
                Ok(b) => self.shm_buffer = Some(b),
                Err(e) => {
                    frame.destroy();
                    return Err(e);
                }
            }
        }
        let buffer = &self.shm_buffer.as_ref().expect("created above").buffer;
        if with_damage {
            frame.copy_with_damage(buffer);
        } else {
            frame.copy(buffer);
        }
        self.pending = Some(frame);
        Ok(())
    }

    /// Wait for the pending frame to complete or fail, until `deadline`
    /// (forever if `None`). Returns false if the deadline passed first.
    fn wait(&mut self, deadline: Option<Instant>) -> Result<bool, CaptureError> {
        let failed = |e: &dyn std::fmt::Display| CaptureError::Failed(format!("waiting for frame: {e}"));
        loop {
            self.event_queue.dispatch_pending(&mut self.state).map_err(|e| failed(&e))?;
            if self.state.frame_ready || self.state.frame_failed {
                return Ok(true);
            }
            self.event_queue.flush().map_err(|e| failed(&e))?;
            // None means events are already queued: dispatch them first.
            let Some(guard) = self.event_queue.prepare_read() else { continue };
            let timeout_ms = match deadline {
                None => -1,
                Some(d) => {
                    let left = d.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Ok(false);
                    }
                    left.as_millis().clamp(1, i32::MAX as u128) as i32
                }
            };
            let mut pfd = libc::pollfd {
                fd: guard.connection_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let n = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
            if n < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(failed(&err));
            }
            if n == 0 {
                // Timed out; dropping the guard cancels the read.
                return Ok(false);
            }
            match guard.read() {
                Ok(_) => {}
                Err(wayland_client::backend::WaylandError::Io(e))
                    if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(failed(&e)),
            }
        }
    }

    /// Collect the completed pending frame.
    fn finish(&mut self) -> Result<ScreenFrame, CaptureError> {
        if let Some(frame) = self.pending.take() {
            frame.destroy();
        }
        self.state.current_frame = None;
        if self.state.frame_failed {
            return Err(CaptureError::Failed("compositor reported failure".into()));
        }
        let buf = self.shm_buffer.as_ref().ok_or_else(|| CaptureError::Failed("no buffer".into()))?;
        let rgba = to_rgba(buf.data(), buf.width, buf.height, buf.stride, buf.format);
        Ok(ScreenFrame {
            width: buf.width,
            height: buf.height,
            rgba,
            damage: std::mem::take(&mut self.state.damage),
        })
    }

    /// Withdraw a pending request, so a new one can be made.
    fn abandon_pending(&mut self) {
        if let Some(frame) = self.pending.take() {
            frame.destroy();
        }
        self.state.current_frame = None;
    }
}

/// Create a wl_shm pool and buffer for frame capture.
///
/// Generic over the Dispatch state `D` so [`crate::cursor_capture`] (a
/// separate Wayland client with its own state type) can reuse this instead of
/// duplicating the memfd/pool/buffer dance.
pub(crate) fn create_shm_buffer<D>(
    shm: &wl_shm::WlShm,
    width: u32,
    height: u32,
    stride: u32,
    format: u32,
    qh: &QueueHandle<D>,
) -> Result<(wl_shm_pool::WlShmPool, wl_buffer::WlBuffer, OwnedFd), CaptureError>
where
    D: Dispatch<wl_shm_pool::WlShmPool, ()> + Dispatch<wl_buffer::WlBuffer, ()> + 'static,
{
    let size = (stride * height) as i32;

    // Create a memfd for the shared memory
    let fd = create_memfd(size as usize)?;

    let pool = shm.create_pool(fd.as_fd(), size, qh, ());

    let wl_format = wl_shm::Format::try_from(format)
        .map_err(|_| CaptureError::ShmError(format!("unknown format {format}")))?;

    let buffer = pool.create_buffer(0, width as i32, height as i32, stride as i32, wl_format, qh, ());

    Ok((pool, buffer, fd))
}

/// Create an anonymous shared memory fd.
pub(crate) fn create_memfd(size: usize) -> Result<OwnedFd, CaptureError> {
    use nix::sys::memfd;
    use nix::unistd;

    let fd = memfd::memfd_create(
        c"termland-shm",
        memfd::MemFdCreateFlag::MFD_CLOEXEC,
    )
    .map_err(|e| CaptureError::ShmError(format!("memfd_create: {e}")))?;

    unistd::ftruncate(&fd, size as nix::libc::off_t)
        .map_err(|e| CaptureError::ShmError(format!("ftruncate: {e}")))?;

    Ok(fd)
}

/// Convert a captured shm buffer to RGBA.
fn to_rgba(data: &[u8], width: u32, height: u32, stride: u32, format: u32) -> Vec<u8> {
    // Convert from compositor format to RGBA.
    //
    // DRM/wl_shm format names describe the BIT LAYOUT as a little-endian 24/32-bit
    // value like `[23:0] R:G:B`. The MEMORY byte order is the little-endian
    // serialization of that, so it's REVERSED from the name's letter order:
    //
    //   Name      wl_shm  fourcc              bits              memory bytes
    //   ARGB8888  0       AR24 = 875713089    [A:R:G:B]         B G R A
    //   XRGB8888  1       XR24 = 875713112    [X:R:G:B]         B G R X
    //   ABGR8888          AB24 = 875708993    [A:B:G:R]         R G B A
    //   XBGR8888          XB24 = 875709016    [X:B:G:R]         R G B X
    //   BGR888            BG24 = 875710274    [B:G:R]  (3bpp)   R G B
    //   RGB888            RG24 = 875710290    [R:G:B]  (3bpp)   B G R
    //
    // "bgr_order" = first byte is B, third byte is R  (ARGB/XRGB 4bpp, RGB888 3bpp)
    // "rgb_order" = first byte is R, third byte is B  (ABGR/XBGR 4bpp, BGR888 3bpp)
    let (bpp, bgr_order) = match format {
        0 => (4, true),          // wl_shm ARGB8888 (per spec, always this format)
        1 => (4, true),          // wl_shm XRGB8888 (per spec, always this format)
        875713089 => (4, true),  // AR24 ARGB8888
        875713112 => (4, true),  // XR24 XRGB8888
        875708993 => (4, false), // AB24 ABGR8888
        875709016 => (4, false), // XB24 XBGR8888
        875710274 => (3, false), // BG24 BGR888
        875710290 => (3, true),  // RG24 RGB888
        _ => {
            tracing::warn!("Unknown screencopy format {format:#x} ({format}), guessing BGR 4bpp");
            (4usize, true)
        }
    };
    let bpp: usize = bpp;

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
            rgba.extend_from_slice(&[r, g, b, 255]);
        }
    }

    rgba
}
