use termland_protocol::VideoCodec;

/// Codecs a mobile decoder can be asked to handle. Mirrors
/// `termland_protocol::VideoCodec` one-for-one; it exists separately only
/// because UniFFI has to own the type it exports.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum MobileCodec {
    Av1,
    Vp9,
    Vp8,
    H265,
    H264,
}

impl From<MobileCodec> for VideoCodec {
    fn from(c: MobileCodec) -> Self {
        match c {
            MobileCodec::Av1 => VideoCodec::Av1,
            MobileCodec::Vp9 => VideoCodec::Vp9,
            MobileCodec::Vp8 => VideoCodec::Vp8,
            MobileCodec::H265 => VideoCodec::H265,
            MobileCodec::H264 => VideoCodec::H264,
        }
    }
}

impl From<VideoCodec> for MobileCodec {
    fn from(c: VideoCodec) -> Self {
        match c {
            VideoCodec::Av1 => MobileCodec::Av1,
            VideoCodec::Vp9 => MobileCodec::Vp9,
            VideoCodec::Vp8 => MobileCodec::Vp8,
            VideoCodec::H265 => MobileCodec::H265,
            VideoCodec::H264 => MobileCodec::H264,
        }
    }
}

/// A saved server, as edited in the UI. Credentials are passed in per call
/// rather than held in the core so they can live in the Android Keystore /
/// iOS Keychain and never sit in Rust memory longer than one connect.
#[derive(uniffi::Record, Clone)]
pub struct ServerProfile {
    pub host: String,
    pub port: u16,
    pub use_tls: bool,
    /// Skip certificate validation. Termland servers default to a self-signed
    /// cert, so this is the common case on a LAN — but it is genuine MITM
    /// exposure and the UI must say so.
    pub accept_invalid_certs: bool,
    pub username: Option<String>,
    pub password: Option<String>,
    /// Route through an embedded SSH `termland` subsystem channel (see
    /// `Transport::Ssh`) instead of dialing `port` directly. Takes priority
    /// over `use_tls` when both are set.
    ///
    /// NOTE: this is a new field added after the Android app's first release
    /// against this record. UniFFI's `#[uniffi(default = false)]` gives
    /// existing generated bindings a default, so `ServerProfile(...)` call
    /// sites that predate SSH support keep compiling — but the Kotlin/Swift
    /// bindings still need regenerating from this crate to see the field at
    /// all. Flagging per the "frozen contract" rule: this is the one field
    /// added in this change.
    #[uniffi(default = false)]
    pub use_ssh: bool,
    /// Dial over QUIC/UDP (`Transport::Quic`) instead of a plain/TLS TCP
    /// socket — Q1 of the QUIC transport plan (see
    /// `docs/quic-transport.md`), the whole protocol carried unmodified over
    /// one QUIC stream. Reuses `accept_invalid_certs` for the same
    /// self-signed-cert-on-a-LAN posture as `use_tls`, since QUIC always
    /// requires TLS 1.3. Takes priority over `use_tls` (not over `use_ssh` —
    /// mobile sandboxes can't spawn `ssh`, but SSH itself stays a plain TCP
    /// tunnel underneath either way, so the two aren't a meaningful pairing).
    ///
    /// NOTE: same frozen-contract situation as `use_ssh` above — this is the
    /// one new field added in this change. `#[uniffi(default = false)]` keeps
    /// existing generated `ServerProfile(...)` call sites compiling; the
    /// Kotlin/Swift bindings still need regenerating to see it.
    #[uniffi(default = false)]
    pub use_quic: bool,
}

/// One resumable session on the server, for the session-list screen.
#[derive(uniffi::Record, Clone)]
pub struct SessionSummary {
    pub session_id: String,
    pub mode: String,
    pub width: u32,
    pub height: u32,
    pub age_secs: u64,
    pub attached: bool,
}

/// Params for creating OR attaching. `app_command: None` => Desktop mode.
#[derive(uniffi::Record, Clone)]
pub struct SessionParams {
    pub width: u32,
    pub height: u32,
    pub quality: u8,
    pub audio: bool,
    pub desktop_shell: Option<String>,
    pub app_command: Option<String>,
    /// What the device's MediaCodec/VideoToolbox can actually decode, in
    /// preference order. The server picks the best encoder in this set, so an
    /// honest list here is what keeps us off software decode paths.
    pub supported_codecs: Vec<MobileCodec>,
}

impl SessionParams {
    /// Codecs to advertise in SessionCreate/SessionAttach. An empty list would
    /// leave the server nothing to pick, so fall back to the full preference
    /// order (which is also what an older client that omits the field means).
    pub(crate) fn advertised_codecs(&self) -> Vec<VideoCodec> {
        if self.supported_codecs.is_empty() {
            tracing::warn!("no supported_codecs given; advertising all");
            VideoCodec::all_preferred()
        } else {
            self.supported_codecs.iter().copied().map(VideoCodec::from).collect()
        }
    }

    pub(crate) fn session_mode(&self) -> termland_protocol::SessionMode {
        match self.app_command.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(cmd) => {
                let mut parts = cmd.split_whitespace().map(str::to_string);
                match parts.next() {
                    Some(command) => termland_protocol::SessionMode::App {
                        command,
                        args: parts.collect(),
                    },
                    None => termland_protocol::SessionMode::Desktop,
                }
            }
            None => termland_protocol::SessionMode::Desktop,
        }
    }
}

/// Emitted once the server has accepted the session; the platform decoder must
/// be configured from this before any packet is fed to it.
#[derive(uniffi::Record, Clone)]
pub struct SessionReadyInfo {
    pub width: u32,
    pub height: u32,
    pub codec: MobileCodec,
    /// Stable id to pass back to `attach()` after the link drops.
    pub session_id: String,
}

/// One encoded video access unit, straight off the wire. The core never
/// inspects or transcodes `data` — that is MediaCodec's job.
#[derive(uniffi::Record, Clone)]
pub struct VideoPacket {
    pub data: Vec<u8>,
    pub timestamp_us: u64,
    pub keyframe: bool,
    /// Codec of *this* packet. Usually the negotiated one, but the server may
    /// switch encoders mid-stream (e.g. after a resize lands on a different
    /// backend), so the decoder must be prepared to reconfigure.
    pub codec: MobileCodec,
    pub width: u32,
    pub height: u32,
}

/// Implemented in Kotlin/Swift. Called from a Rust worker thread — the foreign
/// side must not block: hand packets straight to MediaCodec and return.
/// Blocking here stalls the socket read and the stream backs up.
#[uniffi::export(with_foreign)]
pub trait SessionObserver: Send + Sync {
    fn on_session_ready(&self, info: SessionReadyInfo);
    fn on_video_packet(&self, packet: VideoPacket);
    /// Raw Opus packets. The core does not decode audio; feed these to an
    /// AudioTrack/AVAudioEngine Opus decoder on the platform side.
    fn on_audio_packet(&self, data: Vec<u8>);
    fn on_clipboard(&self, mime_type: String, data: Vec<u8>);
    fn on_data_rate(&self, bytes_per_sec: u64);
    fn on_disconnected(&self, reason: String);
    fn on_error(&self, message: String);
}
