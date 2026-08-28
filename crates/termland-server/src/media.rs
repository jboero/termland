//! Native-QUIC / WebTransport media planes.
//!
//! Both transports open the same Q2 video uni stream. Audio datagrams stay
//! native-QUIC-only: Q2's 5-byte audio header omits `AudioChunk.timestamp_us`,
//! which WebCodecs `EncodedAudioChunk` requires.

use anyhow::{Context, Result};
use termland_protocol::{audio_header_bytes, video_header_bytes, AudioChunk, FrameType, VideoCodec};

/// How (if at all) this session can open Q2 media planes.
pub(crate) enum MediaConnection {
    /// TCP/TLS/SSH: video stays as CBOR on the control stream.
    None,
    Quic(quinn::Connection),
    WebTransport(wtransport::Connection),
}

impl MediaConnection {
    /// Open the video uni stream once the encoder has a first frame.
    /// `None` keeps video on the control stream (TCP/TLS/SSH).
    pub(crate) async fn open_planes(self) -> Result<Option<MediaPlanes>> {
        match self {
            Self::None => Ok(None),
            Self::Quic(connection) => {
                let video = connection
                    .open_uni()
                    .await
                    .context("failed to open QUIC video stream")?;
                Ok(Some(MediaPlanes {
                    video: VideoSink::Quic(video),
                    audio: AudioSink::Quic(connection),
                }))
            }
            Self::WebTransport(connection) => {
                let video = connection
                    .open_uni()
                    .await
                    .context("failed to open WebTransport video stream")?
                    .await
                    .context("failed to finish opening WebTransport video stream")?;
                Ok(Some(MediaPlanes {
                    video: VideoSink::WebTransport(video),
                    audio: AudioSink::WebTransportHeld(connection),
                }))
            }
        }
    }
}

/// Opened Q2 video (and, for native QUIC, audio) planes.
pub(crate) struct MediaPlanes {
    video: VideoSink,
    audio: AudioSink,
}

enum VideoSink {
    Quic(quinn::SendStream),
    WebTransport(wtransport::SendStream),
}

enum AudioSink {
    Quic(quinn::Connection),
    /// Held so dropping `MediaPlanes` does not close the WebTransport session.
    /// Datagrams are not sent — see the module doc.
    #[allow(dead_code)]
    WebTransportHeld(wtransport::Connection),
}

impl MediaPlanes {
    pub(crate) fn transport_name(&self) -> &'static str {
        match self.video {
            VideoSink::Quic(_) => "QUIC (Q2: split video uni stream + audio datagrams)",
            VideoSink::WebTransport(_) => {
                "WebTransport (Q2: split video uni stream; audio deferred)"
            }
        }
    }

    pub(crate) async fn send_video(
        &mut self,
        codec: VideoCodec,
        frame_type: FrameType,
        width: u16,
        height: u16,
        timestamp_us: u64,
        data: &[u8],
    ) -> Result<()> {
        let header = video_header_bytes(
            codec,
            frame_type,
            width,
            height,
            timestamp_us,
            data.len() as u32,
        );
        self.video.write_all(&header).await?;
        self.video.write_all(data).await?;
        Ok(())
    }

    pub(crate) fn send_audio(&self, chunk: &AudioChunk) {
        match &self.audio {
            AudioSink::WebTransportHeld(_) => {}
            AudioSink::Quic(connection) => send_audio_datagram_quic(connection, chunk),
        }
    }
}

impl VideoSink {
    async fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        match self {
            Self::Quic(s) => {
                s.write_all(buf)
                    .await
                    .context("failed to write QUIC video bytes")?;
            }
            Self::WebTransport(s) => {
                s.write_all(buf)
                    .await
                    .context("failed to write WebTransport video bytes")?;
            }
        }
        Ok(())
    }
}

fn send_audio_datagram_quic(connection: &quinn::Connection, chunk: &AudioChunk) {
    let header = audio_header_bytes(chunk.sample_rate, chunk.channels);
    let total_len = header.len() + chunk.data.len();

    match connection.max_datagram_size() {
        Some(max) if total_len > max => {
            tracing::warn!(
                "dropping audio datagram: {total_len} bytes exceeds negotiated max datagram size {max}"
            );
            return;
        }
        Some(_) => {}
        None => {
            tracing::warn!("dropping audio datagram: peer does not support QUIC datagrams");
            return;
        }
    }

    let mut buf = Vec::with_capacity(total_len);
    buf.extend_from_slice(&header);
    buf.extend_from_slice(&chunk.data);

    if let Err(e) = connection.send_datagram(buf.into()) {
        tracing::warn!("failed to send audio datagram: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn none_opens_no_planes() {
        let planes = MediaConnection::None.open_planes().await.unwrap();
        assert!(planes.is_none());
    }
}
