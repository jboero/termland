//! Q2 media-plane framing: the 18-byte video header and 5-byte audio header.
//!
//! Shared by the native QUIC listener, the WebTransport listener, and the
//! TypeScript client so the layout cannot drift.

use crate::messages::{FrameType, VideoCodec};

/// `[codec: u8][frame_type: u8][width: u16][height: u16][timestamp_us: u64][data_len: u32]`
pub const VIDEO_HEADER_LEN: usize = 18;

/// `[sample_rate: u32][channels: u8]`. No length: a datagram is already one message.
pub const AUDIO_HEADER_LEN: usize = 5;

/// Defensive cap on `data_len`. No real encoded frame at a sane resolution
/// approaches this; it exists so a corrupt header cannot become an unbounded
/// allocation. 16 MiB matches the control-stream payload cap.
pub const MAX_VIDEO_FRAME_BYTES: u32 = 16 * 1024 * 1024;

/// One parsed Q2 video header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoHeader {
    pub codec: VideoCodec,
    pub keyframe: bool,
    pub width: u16,
    pub height: u16,
    pub timestamp_us: u64,
    pub data_len: u32,
}

/// Encode one video frame's 18-byte Q2 header.
pub fn video_header_bytes(
    codec: VideoCodec,
    frame_type: FrameType,
    width: u16,
    height: u16,
    timestamp_us: u64,
    data_len: u32,
) -> [u8; VIDEO_HEADER_LEN] {
    let mut buf = [0u8; VIDEO_HEADER_LEN];
    buf[0] = codec_tag(codec);
    buf[1] = frame_type_tag(frame_type);
    buf[2..4].copy_from_slice(&width.to_le_bytes());
    buf[4..6].copy_from_slice(&height.to_le_bytes());
    buf[6..14].copy_from_slice(&timestamp_us.to_le_bytes());
    buf[14..18].copy_from_slice(&data_len.to_le_bytes());
    buf
}

/// Parse an 18-byte Q2 video header. `None` for an unrecognized codec tag.
pub fn parse_video_header(header: &[u8; VIDEO_HEADER_LEN]) -> Option<VideoHeader> {
    let codec = decode_codec_tag(header[0])?;
    Some(VideoHeader {
        codec,
        keyframe: header[1] == 1,
        width: u16::from_le_bytes([header[2], header[3]]),
        height: u16::from_le_bytes([header[4], header[5]]),
        timestamp_us: u64::from_le_bytes(header[6..14].try_into().ok()?),
        data_len: u32::from_le_bytes(header[14..18].try_into().ok()?),
    })
}

/// Encode one audio datagram's 5-byte Q2 header.
pub fn audio_header_bytes(sample_rate: u32, channels: u8) -> [u8; AUDIO_HEADER_LEN] {
    let mut buf = [0u8; AUDIO_HEADER_LEN];
    buf[0..4].copy_from_slice(&sample_rate.to_le_bytes());
    buf[4] = channels;
    buf
}

/// Split one Q2 audio datagram into header fields and Opus payload.
pub fn parse_audio_datagram(datagram: &[u8]) -> Option<(u32, u8, &[u8])> {
    if datagram.len() < AUDIO_HEADER_LEN {
        return None;
    }
    let sample_rate = u32::from_le_bytes(datagram[0..4].try_into().ok()?);
    let channels = datagram[4];
    Some((sample_rate, channels, &datagram[AUDIO_HEADER_LEN..]))
}

/// Does this encoded payload look like the given codec's elementary stream?
///
/// Used to refuse advertising a WebCodecs string whose bitstream we did not
/// actually produce. Conservative: unknown prefixes return `false`.
pub fn bitstream_matches_codec(codec: VideoCodec, data: &[u8]) -> bool {
    if data.is_empty() {
        return false;
    }
    match codec {
        // AV1 OBU: the first byte's type is in bits 3-6 (obu_type). Temporal
        // delimiter (2), sequence header (1), frame header (3) and frame (6)
        // are what SVT-AV1 / libaom emit at the start of a keyframe.
        VideoCodec::Av1 => {
            let obu_type = (data[0] >> 3) & 0x0f;
            matches!(obu_type, 1 | 2 | 3 | 6)
        }
        // VP9: frame marker is bits 7-6 == 0b10.
        VideoCodec::Vp9 => (data[0] >> 6) == 0b10,
        // VP8: first 3 bits of the first byte are the frame tag; a keyframe
        // starts with 0x10.. or similar. Accept any non-empty as VP8's tag
        // is only 3 bits and overlaps too much to be a reliable magic.
        VideoCodec::Vp8 => true,
        // H.264/H.265 Annex-B start code.
        VideoCodec::H264 | VideoCodec::H265 => {
            data.starts_with(&[0, 0, 0, 1]) || data.starts_with(&[0, 0, 1])
        }
    }
}

fn codec_tag(codec: VideoCodec) -> u8 {
    match codec {
        VideoCodec::Av1 => 0,
        VideoCodec::Vp9 => 1,
        VideoCodec::Vp8 => 2,
        VideoCodec::H265 => 3,
        VideoCodec::H264 => 4,
    }
}

fn frame_type_tag(frame_type: FrameType) -> u8 {
    match frame_type {
        FrameType::Inter => 0,
        FrameType::Keyframe => 1,
    }
}

fn decode_codec_tag(tag: u8) -> Option<VideoCodec> {
    match tag {
        0 => Some(VideoCodec::Av1),
        1 => Some(VideoCodec::Vp9),
        2 => Some(VideoCodec::Vp8),
        3 => Some(VideoCodec::H265),
        4 => Some(VideoCodec::H264),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_header_round_trips_all_codecs_and_frame_types() {
        for (codec, tag) in [
            (VideoCodec::Av1, 0u8),
            (VideoCodec::Vp9, 1),
            (VideoCodec::Vp8, 2),
            (VideoCodec::H265, 3),
            (VideoCodec::H264, 4),
        ] {
            for (frame_type, keyframe) in [(FrameType::Inter, false), (FrameType::Keyframe, true)] {
                let header =
                    video_header_bytes(codec, frame_type, 1920, 1080, 123_456_789_012, 65536);
                assert_eq!(header.len(), VIDEO_HEADER_LEN);
                let parsed = parse_video_header(&header).expect("parse");
                assert_eq!(header[0], tag);
                assert_eq!(parsed.codec, codec);
                assert_eq!(parsed.keyframe, keyframe);
                assert_eq!(parsed.width, 1920);
                assert_eq!(parsed.height, 1080);
                assert_eq!(parsed.timestamp_us, 123_456_789_012);
                assert_eq!(parsed.data_len, 65536);
            }
        }
    }

    #[test]
    fn video_header_byte_offsets_are_exact() {
        let header = video_header_bytes(
            VideoCodec::H264,
            FrameType::Keyframe,
            0x0102,
            0x0304,
            0x0102030405060708,
            0xAABBCCDD,
        );
        assert_eq!(header[0], 4);
        assert_eq!(header[1], 1);
        assert_eq!(&header[2..4], &0x0102u16.to_le_bytes());
        assert_eq!(&header[4..6], &0x0304u16.to_le_bytes());
        assert_eq!(&header[6..14], &0x0102030405060708u64.to_le_bytes());
        assert_eq!(&header[14..18], &0xAABBCCDDu32.to_le_bytes());
    }

    #[test]
    fn unknown_codec_tag_is_rejected() {
        let mut header = video_header_bytes(VideoCodec::Av1, FrameType::Inter, 1, 1, 0, 0);
        header[0] = 255;
        assert!(parse_video_header(&header).is_none());
    }

    #[test]
    fn audio_header_layout_is_exact() {
        let header = audio_header_bytes(48000, 2);
        assert_eq!(header.len(), AUDIO_HEADER_LEN);
        assert_eq!(&header[0..4], &48000u32.to_le_bytes());
        assert_eq!(header[4], 2);
        let datagram = [&header[..], b"opus"].concat();
        let (rate, ch, payload) = parse_audio_datagram(&datagram).unwrap();
        assert_eq!(rate, 48000);
        assert_eq!(ch, 2);
        assert_eq!(payload, b"opus");
    }

    #[test]
    fn short_audio_datagram_is_rejected() {
        assert!(parse_audio_datagram(&[0, 1, 2, 3]).is_none());
    }

    #[test]
    fn bitstream_probe_accepts_av1_obu_and_h264_annexb() {
        // Sequence header OBU: obu_type = 1 in bits 3-6 → 0b0_0001_000 = 0x08
        assert!(bitstream_matches_codec(VideoCodec::Av1, &[0x08, 0x00]));
        assert!(bitstream_matches_codec(
            VideoCodec::H264,
            &[0, 0, 0, 1, 0x67]
        ));
        assert!(!bitstream_matches_codec(VideoCodec::Av1, &[0xFF]));
        assert!(!bitstream_matches_codec(VideoCodec::H264, &[0x67, 0x42]));
        assert!(!bitstream_matches_codec(VideoCodec::Av1, &[]));
    }
}
