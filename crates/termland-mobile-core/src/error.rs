/// Every failure the mobile core can report across the FFI boundary.
///
/// Deliberately coarse: the Kotlin/Swift UI only ever needs to decide between
/// "retry", "ask for credentials", and "show a message", so the variants map to
/// those decisions rather than to internal error types. Nothing panics on an
/// I/O or network path — everything funnels through here.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum TermlandError {
    #[error("connect failed: {msg}")]
    Connect { msg: String },
    #[error("TLS error: {msg}")]
    Tls { msg: String },
    #[error("authentication failed: {msg}")]
    Auth { msg: String },
    #[error("protocol error: {msg}")]
    Protocol { msg: String },
    #[error("I/O error: {msg}")]
    Io { msg: String },
}

impl TermlandError {
    pub(crate) fn connect(msg: impl std::fmt::Display) -> Self {
        Self::Connect { msg: msg.to_string() }
    }
    pub(crate) fn tls(msg: impl std::fmt::Display) -> Self {
        Self::Tls { msg: msg.to_string() }
    }
    pub(crate) fn auth(msg: impl std::fmt::Display) -> Self {
        Self::Auth { msg: msg.to_string() }
    }
    pub(crate) fn protocol(msg: impl std::fmt::Display) -> Self {
        Self::Protocol { msg: msg.to_string() }
    }
    pub(crate) fn io(msg: impl std::fmt::Display) -> Self {
        Self::Io { msg: msg.to_string() }
    }
}

impl From<termland_protocol::frame::CodecError> for TermlandError {
    fn from(e: termland_protocol::frame::CodecError) -> Self {
        match e {
            termland_protocol::frame::CodecError::Io(io) => Self::io(io),
            other => Self::protocol(other),
        }
    }
}

pub type Result<T> = std::result::Result<T, TermlandError>;
