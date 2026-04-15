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

    // Server -> Client data
    VideoFrame = 0x20,
    StillFrame = 0x21,
    AudioChunk = 0x22,
    CursorUpdate = 0x23,
    ClipboardData = 0x24,

    // Client -> Server data
    KeyEvent = 0x40,
    MouseMove = 0x41,
    MouseButton = 0x42,
    MouseScroll = 0x43,
    ClipboardSend = 0x44,
    QualityHint = 0x45,
    CursorMode = 0x46,
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
            0x20 => Some(Self::VideoFrame),
            0x21 => Some(Self::StillFrame),
            0x22 => Some(Self::AudioChunk),
            0x23 => Some(Self::CursorUpdate),
            0x24 => Some(Self::ClipboardData),
            0x40 => Some(Self::KeyEvent),
            0x41 => Some(Self::MouseMove),
            0x42 => Some(Self::MouseButton),
            0x43 => Some(Self::MouseScroll),
            0x44 => Some(Self::ClipboardSend),
            0x45 => Some(Self::QualityHint),
            0x46 => Some(Self::CursorMode),
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

    // Server -> Client data
    VideoFrame(VideoFrame),
    StillFrame(StillFrame),
    AudioChunk(AudioChunk),
    CursorUpdate(CursorUpdate),
    ClipboardData(ClipboardPayload),

    // Client -> Server input
    KeyEvent(super::input::KeyEvent),
    MouseMove(super::input::MouseMove),
    MouseButton(super::input::MouseButton),
    MouseScroll(super::input::MouseScroll),
    ClipboardSend(ClipboardPayload),
    QualityHint(QualityHintMsg),
    CursorMode(CursorModeMsg),
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
            Self::VideoFrame(_) => MessageId::VideoFrame,
            Self::StillFrame(_) => MessageId::StillFrame,
            Self::AudioChunk(_) => MessageId::AudioChunk,
            Self::CursorUpdate(_) => MessageId::CursorUpdate,
            Self::ClipboardData(_) => MessageId::ClipboardData,
            Self::KeyEvent(_) => MessageId::KeyEvent,
            Self::MouseMove(_) => MessageId::MouseMove,
            Self::MouseButton(_) => MessageId::MouseButton,
            Self::MouseScroll(_) => MessageId::MouseScroll,
            Self::ClipboardSend(_) => MessageId::ClipboardSend,
            Self::QualityHint(_) => MessageId::QualityHint,
            Self::CursorMode(_) => MessageId::CursorMode,
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
}

fn default_quality() -> u8 { 75 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionReady {
    pub width: u32,
    pub height: u32,
    pub xkb_keymap: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoFrame {
    pub timestamp_us: u64,
    pub frame_type: FrameType,
    pub width: u16,
    pub height: u16,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityHintMsg {
    pub max_fps: u8,
    pub max_bitrate_kbps: u32,
    pub prefer_lossless: bool,
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
