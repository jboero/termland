use serde::{Deserialize, Serialize};

/// Protocol version
pub const PROTOCOL_VERSION: u32 = 1;

/// Magic bytes for frame header: "TL"
pub const FRAME_MAGIC: [u8; 2] = [0x54, 0x4C];

/// Maximum payload size: 16 MiB
pub const MAX_PAYLOAD_SIZE: u32 = 16 * 1024 * 1024;

/// Message IDs for wire format
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageId {
    // Control plane
    Hello = 0x01,
    HelloAck = 0x02,
    AuthRequest = 0x03,
    AuthResponse = 0x04,
    AuthResult = 0x05,
    SessionCreate = 0x06,
    SessionReady = 0x07,
    SessionResize = 0x08,
    SessionEnd = 0x09,
    Ping = 0x0A,
    Pong = 0x0B,
    SessionList = 0x0C,
    SessionListResult = 0x0D,
    SessionAttach = 0x0E,
    SessionClose = 0x0F,

    // Server -> Client data
    VideoFrame = 0x20,
    StillFrame = 0x21,
    AudioChunk = 0x22,
    CursorUpdate = 0x23,
    ClipboardData = 0x24,
    /// Files copied on the server side's clipboard (a `text/uri-list`), sent
    /// whole so the client can paste them as real files. See
    /// [`FileTransferPayload`] for the MVP size/chunking limitation.
    FileTransferData = 0x25,

    // Client -> Server data
    KeyEvent = 0x40,
    MouseMove = 0x41,
    MouseButton = 0x42,
    MouseScroll = 0x43,
    ClipboardSend = 0x44,
    QualityHint = 0x45,
    CursorMode = 0x46,
    TextInput = 0x47,
    /// Files copied on the client side's clipboard, sent to the server. See
    /// [`FileTransferPayload`] for the MVP size/chunking limitation.
    FileTransferSend = 0x48,
}

impl MessageId {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0x01 => Some(Self::Hello),
            0x02 => Some(Self::HelloAck),
            0x03 => Some(Self::AuthRequest),
            0x04 => Some(Self::AuthResponse),
            0x05 => Some(Self::AuthResult),
            0x06 => Some(Self::SessionCreate),
            0x07 => Some(Self::SessionReady),
            0x08 => Some(Self::SessionResize),
            0x09 => Some(Self::SessionEnd),
            0x0A => Some(Self::Ping),
            0x0B => Some(Self::Pong),
            0x0C => Some(Self::SessionList),
            0x0D => Some(Self::SessionListResult),
            0x0E => Some(Self::SessionAttach),
            0x0F => Some(Self::SessionClose),
            0x20 => Some(Self::VideoFrame),
            0x21 => Some(Self::StillFrame),
            0x22 => Some(Self::AudioChunk),
            0x23 => Some(Self::CursorUpdate),
            0x24 => Some(Self::ClipboardData),
            0x25 => Some(Self::FileTransferData),
            0x40 => Some(Self::KeyEvent),
            0x41 => Some(Self::MouseMove),
            0x42 => Some(Self::MouseButton),
            0x43 => Some(Self::MouseScroll),
            0x44 => Some(Self::ClipboardSend),
            0x45 => Some(Self::QualityHint),
            0x46 => Some(Self::CursorMode),
            0x47 => Some(Self::TextInput),
            0x48 => Some(Self::FileTransferSend),
            _ => None,
        }
    }
}

/// Top-level message envelope
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Message {
    // Control plane
    Hello(Hello),
    HelloAck(HelloAck),
    AuthRequest(AuthRequest),
    AuthResponse(AuthResponse),
    AuthResult(AuthResult),
    SessionCreate(SessionCreate),
    SessionReady(SessionReady),
    SessionResize(SessionResize),
    SessionEnd(SessionEnd),
    Ping(Ping),
    Pong(Pong),
    SessionList(SessionList),
    SessionListResult(SessionListResult),
    SessionAttach(SessionAttach),
    SessionClose(SessionClose),

    // Server -> Client data
    VideoFrame(VideoFrame),
    StillFrame(StillFrame),
    AudioChunk(AudioChunk),
    CursorUpdate(CursorUpdate),
    ClipboardData(ClipboardPayload),
    FileTransferData(FileTransferPayload),

    // Client -> Server input
    KeyEvent(super::input::KeyEvent),
    MouseMove(super::input::MouseMove),
    MouseButton(super::input::MouseButton),
    MouseScroll(super::input::MouseScroll),
    ClipboardSend(ClipboardPayload),
    QualityHint(QualityHintMsg),
    CursorMode(CursorModeMsg),
    TextInput(TextInput),
    FileTransferSend(FileTransferPayload),
}

impl Message {
    pub fn message_id(&self) -> MessageId {
        match self {
            Self::Hello(_) => MessageId::Hello,
            Self::HelloAck(_) => MessageId::HelloAck,
            Self::AuthRequest(_) => MessageId::AuthRequest,
            Self::AuthResponse(_) => MessageId::AuthResponse,
            Self::AuthResult(_) => MessageId::AuthResult,
            Self::SessionCreate(_) => MessageId::SessionCreate,
            Self::SessionReady(_) => MessageId::SessionReady,
            Self::SessionResize(_) => MessageId::SessionResize,
            Self::SessionEnd(_) => MessageId::SessionEnd,
            Self::Ping(_) => MessageId::Ping,
            Self::Pong(_) => MessageId::Pong,
            Self::SessionList(_) => MessageId::SessionList,
            Self::SessionListResult(_) => MessageId::SessionListResult,
            Self::SessionAttach(_) => MessageId::SessionAttach,
            Self::SessionClose(_) => MessageId::SessionClose,
            Self::VideoFrame(_) => MessageId::VideoFrame,
            Self::StillFrame(_) => MessageId::StillFrame,
            Self::AudioChunk(_) => MessageId::AudioChunk,
            Self::CursorUpdate(_) => MessageId::CursorUpdate,
            Self::ClipboardData(_) => MessageId::ClipboardData,
            Self::FileTransferData(_) => MessageId::FileTransferData,
            Self::KeyEvent(_) => MessageId::KeyEvent,
            Self::MouseMove(_) => MessageId::MouseMove,
            Self::MouseButton(_) => MessageId::MouseButton,
            Self::MouseScroll(_) => MessageId::MouseScroll,
            Self::ClipboardSend(_) => MessageId::ClipboardSend,
            Self::QualityHint(_) => MessageId::QualityHint,
            Self::CursorMode(_) => MessageId::CursorMode,
            Self::TextInput(_) => MessageId::TextInput,
            Self::FileTransferSend(_) => MessageId::FileTransferSend,
        }
    }

    /// Serialize this message to CBOR bytes.
    pub fn encode(&self) -> Result<Vec<u8>, EncodeError> {
        let mut buf = Vec::new();
        ciborium::into_writer(self, &mut buf).map_err(|e| EncodeError(e.to_string()))?;
        Ok(buf)
    }

    /// Deserialize a message from CBOR bytes.
    pub fn decode(data: &[u8]) -> Result<Self, DecodeError> {
        ciborium::from_reader(data).map_err(|e| DecodeError(e.to_string()))
    }
}

#[derive(Debug, thiserror::Error)]
#[error("encode error: {0}")]
pub struct EncodeError(pub String);

#[derive(Debug, thiserror::Error)]
#[error("decode error: {0}")]
pub struct DecodeError(pub String);

// --- Control plane messages ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub protocol_version: u32,
    pub client_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloAck {
    pub protocol_version: u32,
    pub server_name: String,
    pub session_id: String,
    #[serde(default)]
    pub auth_required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthRequest {
    pub methods: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthResponse {
    pub username: String,
    pub credential: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthResult {
    pub success: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SessionMode {
    Desktop,
    App { command: String, args: Vec<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCreate {
    pub mode: SessionMode,
    pub width: u32,
    pub height: u32,
    pub audio: bool,
    /// Video quality 1-100 (default 75). Maps to encoder bitrate/CRF.
    #[serde(default = "default_quality")]
    pub quality: u8,
    /// For Desktop mode: startup command to run inside labwc.
    /// Examples: "konsole", "startplasma-wayland", "dbus-run-session sway".
    /// If None/empty, server auto-detects a terminal emulator.
    #[serde(default)]
    pub desktop_shell: Option<String>,
    /// Override encoder preset (SVT-AV1: 0-13, QSV: veryfast/faster/etc).
    /// None = use the backend-specific default.
    #[serde(default)]
    pub encoder_preset: Option<String>,
    /// Override CRF (0-63 for SVT-AV1, ignored by HW encoders).
    /// None = use default (35 for SVT-AV1).
    #[serde(default)]
    pub encoder_crf: Option<u8>,
    /// Extra key:value params passed verbatim as `svtav1-params` (SVT-AV1 only).
    /// Merged with our mandatory low-delay params. Example:
    /// "fast-decode=1:tile-columns=2:scm=2"
    #[serde(default)]
    pub encoder_extra_params: Option<String>,
    /// Codecs this client can decode, in preference order. The server picks
    /// the best encoder whose codec appears in this set. An omitted field
    /// (older client) means "all codecs supported".
    #[serde(default = "VideoCodec::all_preferred")]
    pub supported_codecs: Vec<VideoCodec>,
}

fn default_quality() -> u8 { 75 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionReady {
    pub width: u32,
    pub height: u32,
    pub xkb_keymap: Option<String>,
    /// Codec the server chose to encode this session with, so the client can
    /// construct the matching decoder up front. `None` (older server) means
    /// the client should fall back to auto-detecting from the frame stream.
    #[serde(default)]
    pub codec: Option<VideoCodec>,
    /// Stable id of the (persistent) session, so the client can reattach to it
    /// later after disconnecting. Empty for older servers with no persistence.
    #[serde(default)]
    pub session_id: String,
}

/// Client -> server: request the list of resumable sessions on this host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionList {}

/// Server -> client: the sessions currently running and available to resume.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionListResult {
    pub sessions: Vec<SessionInfo>,
}

/// Summary of one persistent session, for the resume list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: String,
    /// Human-readable mode description, e.g. "desktop" or "app: firefox".
    pub mode: String,
    pub width: u32,
    pub height: u32,
    /// Seconds since the session was created.
    pub age_secs: u64,
    /// Whether a client is currently attached and streaming.
    pub attached: bool,
}

/// Client -> server: attach to an existing persistent session and resume its
/// stream. The compositor keeps running; the server spins up a fresh encoder
/// (re-negotiating the codec from `supported_codecs`) and sends a keyframe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionAttach {
    pub session_id: String,
    pub audio: bool,
    #[serde(default = "default_quality")]
    pub quality: u8,
    #[serde(default)]
    pub encoder_preset: Option<String>,
    #[serde(default)]
    pub encoder_crf: Option<u8>,
    #[serde(default)]
    pub encoder_extra_params: Option<String>,
    #[serde(default = "VideoCodec::all_preferred")]
    pub supported_codecs: Vec<VideoCodec>,
}

/// Client -> server: permanently close (terminate) a persistent session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionClose {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionResize {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEnd {
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ping {
    pub timestamp_us: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pong {
    pub timestamp_us: u64,
}

// --- Data plane messages ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FrameType {
    Keyframe,
    Inter,
}

/// Video codec carried by a `VideoFrame`. Used for codec negotiation:
/// the client advertises the codecs it can decode in `SessionCreate`,
/// the server reports the one it chose in `SessionReady`, and every
/// `VideoFrame` is tagged so the client can pick the matching decoder
/// deterministically instead of guessing by trial-and-error.
///
/// Listed in preference order (best compression / most open first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoCodec {
    Av1,
    Vp9,
    Vp8,
    H265,
    H264,
}

impl VideoCodec {
    /// All codecs in preference order. Used as the default advertised set
    /// (a client that omits `supported_codecs` is treated as supporting all)
    /// and as the client's own advertised capabilities.
    pub fn all_preferred() -> Vec<VideoCodec> {
        vec![
            VideoCodec::Av1,
            VideoCodec::Vp9,
            VideoCodec::Vp8,
            VideoCodec::H265,
            VideoCodec::H264,
        ]
    }
}

impl std::fmt::Display for VideoCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            VideoCodec::Av1 => "AV1",
            VideoCodec::Vp9 => "VP9",
            VideoCodec::Vp8 => "VP8",
            VideoCodec::H265 => "H.265",
            VideoCodec::H264 => "H.264",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoFrame {
    pub timestamp_us: u64,
    pub frame_type: FrameType,
    pub width: u16,
    pub height: u16,
    /// Codec this frame is encoded with. `None` (older server) means the
    /// client should decode with whatever it negotiated at SessionReady.
    #[serde(default)]
    pub codec: Option<VideoCodec>,
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StillFrame {
    pub timestamp_us: u64,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub lossless: bool,
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioChunk {
    pub timestamp_us: u64,
    pub sample_rate: u32,
    pub channels: u8,
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CursorUpdate {
    pub x: i32,
    pub y: i32,
    pub hotspot_x: i32,
    pub hotspot_y: i32,
    pub width: u32,
    pub height: u32,
    pub visible: bool,
    #[serde(with = "serde_bytes")]
    pub image_rgba: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardPayload {
    pub mime_type: String,
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
}

/// Maximum total size (sum of all file contents) for one clipboard file-paste
/// transfer ([`FileTransferPayload`]).
///
/// This is a deliberate MVP limitation, not an oversight. The wire protocol
/// caps a single message at `MAX_PAYLOAD_SIZE` (16 MiB - see `frame.rs`), and
/// this first pass sends an entire file-list clipboard as **one** message
/// with no chunking across multiple messages. 12 MiB leaves headroom under
/// that 16 MiB frame cap for CBOR/framing overhead (struct tags, per-file
/// name strings, byte-array length prefixes) so a transfer that is under
/// *this* cap doesn't still blow the wire limit once serialized.
///
/// This comfortably covers the realistic use case it closes - clipboard-paste
/// of a few documents/images - not multi-gigabyte transfers, which would need
/// real chunking across multiple messages (future work, not attempted here).
pub const MAX_FILE_TRANSFER_BYTES: u64 = 12 * 1024 * 1024;

/// One file being transferred as part of a clipboard file-paste.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    /// Basename only (no directory components). The receiving side must
    /// sanitize this before using it in a filesystem write - it arrives
    /// as attacker-controllable data from the other end of the connection.
    /// See `termland_protocol::clipboard_files::sanitize_filename`.
    pub name: String,
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
}

/// Clipboard file-paste payload: the files a desktop "Copy" action put on one
/// side's clipboard (as a `text/uri-list`), read into memory and sent whole
/// to the other side so a `Ctrl+V` there can paste them as real files.
///
/// MVP limitation: total size across all files is capped at
/// [`MAX_FILE_TRANSFER_BYTES`] and sent as a single wire message with no
/// chunking - see that constant's doc comment for the reasoning. A clipboard
/// file-list whose total size exceeds the cap is rejected (logged, not sent)
/// by the sending side rather than truncated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileTransferPayload {
    pub files: Vec<FileEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityHintMsg {
    pub max_fps: u8,
    pub max_bitrate_kbps: u32,
    pub prefer_lossless: bool,
}

/// Client -> server: already-composed Unicode text, as finalized by a soft
/// keyboard or IME (autocorrect result, emoji, CJK commit).
///
/// This is a separate channel from `KeyEvent` on purpose: `KeyEvent` carries an
/// evdev scancode that only has meaning against the server's fixed xkb keymap,
/// so it cannot express codepoints outside that layout. Text arriving here has
/// no scancode at all and is injected by synthesizing a temporary keymap
/// server-side. Editing and shortcut keys (Backspace, Enter, arrows, Ctrl+C)
/// are *not* text and must keep using `KeyEvent`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextInput {
    pub text: String,
}

/// Tell the server whether to include the compositor cursor in the video stream.
/// When `include_cursor_in_frame = false`, the client renders its own local cursor
/// for lower latency (no round-trip through the encoder for cursor motion).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CursorModeMsg {
    pub include_cursor_in_frame: bool,
}

mod serde_bytes {
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(data: &[u8], ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_bytes(data)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Vec<u8>, D::Error> {
        struct BytesVisitor;

        impl<'de> serde::de::Visitor<'de> for BytesVisitor {
            type Value = Vec<u8>;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("bytes or byte array")
            }

            fn visit_bytes<E: serde::de::Error>(self, v: &[u8]) -> Result<Vec<u8>, E> {
                Ok(v.to_vec())
            }

            fn visit_byte_buf<E: serde::de::Error>(self, v: Vec<u8>) -> Result<Vec<u8>, E> {
                Ok(v)
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Vec<u8>, A::Error> {
                let mut buf = Vec::with_capacity(seq.size_hint().unwrap_or(0));
                while let Some(b) = seq.next_element()? {
                    buf.push(b);
                }
                Ok(buf)
            }
        }

        de.deserialize_any(BytesVisitor)
    }
}
