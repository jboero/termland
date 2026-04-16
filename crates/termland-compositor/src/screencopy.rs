//! wlr-screencopy frame capture client.
//!
//! Connects to a Wayland compositor as a client and uses the
//! zwlr_screencopy_manager_v1 protocol to capture screen frames.

use std::os::fd::AsFd;
use std::os::unix::io::OwnedFd;
use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle,
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
    _frame_data: Vec<u8>,
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
            _frame_data: Vec::new(),
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
        _frame: &zwlr_screencopy_frame_v1::ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        _: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
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

/// High-level screen capturer that manages the Wayland connection and screencopy.
pub struct ScreenCapturer {
    _conn: Connection,
    event_queue: EventQueue<CaptureState>,
    state: CaptureState,
    /// Set after we log the first selected format, so we only log it once.
    format_logged: bool,
}

impl ScreenCapturer {
    /// Connect to the given Wayland display and bind screencopy globals.
    pub fn connect(display_name: &str) -> Result<Self, CaptureError> {
        let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .unwrap_or_else(|_| format!("/run/user/{}", nix::unistd::getuid()));
        let socket_path = std::path::Path::new(&runtime_dir).join(display_name);

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
        })
    }

    /// Capture a single frame. Returns (width, height, rgba_data).
    /// `overlay_cursor`: include the compositor's cursor in the captured frame.
    pub fn capture_frame(&mut self, overlay_cursor: bool) -> Result<(u32, u32, Vec<u8>), CaptureError> {
        let qh = self.event_queue.handle();

        let output = self
            .state
            .output
            .as_ref()
            .ok_or(CaptureError::NoOutput)?
            .clone();
        let mgr = self
            .state
            .screencopy_mgr
            .as_ref()
            .ok_or(CaptureError::NoScreencopy)?
            .clone();

        // Reset frame state
        self.state.buffer_ready = false;
        self.state.frame_ready = false;
        self.state.frame_failed = false;

        // overlay_cursor=1 → include the compositor's software cursor in the captured
        // frame. wlroots renders cursors in a separate plane by default, so we have to
        // ask for it explicitly. The client can toggle this off to render its own local
        // cursor for lower latency.
        let frame = mgr.capture_output(overlay_cursor as i32, &output, &qh, ());

        // Roundtrip to get the Buffer event with dimensions
        self.event_queue
            .roundtrip(&mut self.state)
            .map_err(|e| CaptureError::Failed(format!("roundtrip for buffer info: {e}")))?;

        if !self.state.buffer_ready {
            return Err(CaptureError::Failed("no buffer info received".into()));
        }

        let width = self.state.frame_width;
        let height = self.state.frame_height;
        let stride = self.state.frame_stride;
        let size = (stride * height) as usize;

        // Log the selected format once, at INFO level, so the user can diagnose
        // colorspace issues without enabling debug logging.
        if !self.format_logged {
            let fmt = self.state.frame_format;
            let fmt_name = match fmt {
                0 => "ARGB8888(wl_shm)", 1 => "XRGB8888(wl_shm)",
                875713089 => "ARGB8888(AR24)", 875713112 => "XRGB8888(XR24)",
                875708993 => "ABGR8888(AB24)", 875709016 => "XBGR8888(XB24)",
                875710274 => "BGR888(BG24)", 875710290 => "RGB888(RG24)",
                _ => "unknown",
            };
            tracing::info!("Screencopy format: {fmt_name} ({fmt}), {width}x{height} stride={stride}");
            self.format_logged = true;
        }

        // Create a shared memory buffer
        let shm = self.state.shm.as_ref().ok_or(CaptureError::NoShm)?.clone();
        let (pool, buffer, fd) =
            create_shm_buffer(&shm, width, height, stride, self.state.frame_format, &qh)?;

        // Tell screencopy to copy into our buffer
        frame.copy(&buffer);

        // Wait for Ready or Failed
        while !self.state.frame_ready && !self.state.frame_failed {
            self.event_queue
                .roundtrip(&mut self.state)
                .map_err(|e| CaptureError::Failed(format!("roundtrip for frame: {e}")))?;
        }

        if self.state.frame_failed {
            buffer.destroy();
            pool.destroy();
            return Err(CaptureError::Failed("compositor reported failure".into()));
        }

        // Read pixels from the shared memory fd
        let rgba = read_shm_to_rgba(&fd, size, width, height, stride, self.state.frame_format)?;

        // Clean up
        buffer.destroy();
        pool.destroy();

        Ok((width, height, rgba))
    }
}

/// Create a wl_shm pool and buffer for frame capture.
fn create_shm_buffer(
    shm: &wl_shm::WlShm,
    width: u32,
    height: u32,
    stride: u32,
    format: u32,
    qh: &QueueHandle<CaptureState>,
) -> Result<(wl_shm_pool::WlShmPool, wl_buffer::WlBuffer, OwnedFd), CaptureError> {
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
fn create_memfd(size: usize) -> Result<OwnedFd, CaptureError> {
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

/// Read from the shm fd and convert to RGBA.
fn read_shm_to_rgba(
    fd: &OwnedFd,
    size: usize,
    width: u32,
    height: u32,
    stride: u32,
    format: u32,
) -> Result<Vec<u8>, CaptureError> {
    use std::os::fd::AsRawFd;

    // mmap the fd
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
        return Err(CaptureError::ShmError("mmap failed".into()));
    }

    let data = unsafe { std::slice::from_raw_parts(ptr as *const u8, size) };

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

    unsafe {
        libc::munmap(ptr, size);
    }

    Ok(rgba)
}
