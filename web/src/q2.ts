//! Q2 video uni-stream header: 18 bytes, little-endian.
//!
//! `[codec: u8][frame_type: u8][width: u16][height: u16][timestamp_us: u64][data_len: u32]`
//! Must match `termland_protocol::q2` byte-for-byte.

import type { FrameType, VideoCodec } from './messages.js';

export const VIDEO_HEADER_LEN = 18;
/** Same cap as `termland_protocol::q2::MAX_VIDEO_FRAME_BYTES`. */
export const MAX_FRAME_BYTES = 16 * 1024 * 1024;

export interface Q2Frame {
  codec: VideoCodec;
  keyframe: boolean;
  width: number;
  height: number;
  timestamp_us: number;
  data: Uint8Array;
}

const CODEC_BY_TAG: Record<number, VideoCodec> = {
  0: 'Av1',
  1: 'Vp9',
  2: 'Vp8',
  3: 'H265',
  4: 'H264',
};

export function parseVideoHeader(buf: Uint8Array): Omit<Q2Frame, 'data'> & { data_len: number } {
  if (buf.byteLength < VIDEO_HEADER_LEN) {
    throw new Error(`video header too short (${buf.byteLength})`);
  }
  const codec = CODEC_BY_TAG[buf[0]];
  if (!codec) throw new Error(`unknown codec tag ${buf[0]}`);
  const view = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
  const data_len = view.getUint32(14, true);
  if (data_len > MAX_FRAME_BYTES) {
    throw new Error(`implausible frame size ${data_len}`);
  }
  return {
    codec,
    keyframe: buf[1] === 1,
    width: view.getUint16(2, true),
    height: view.getUint16(4, true),
    timestamp_us: Number(view.getBigUint64(6, true)),
    data_len,
  };
}

export function encodeVideoHeader(
  codec: VideoCodec,
  frameType: FrameType,
  width: number,
  height: number,
  timestamp_us: number | bigint,
  data_len: number,
): Uint8Array {
  const tag =
    codec === 'Av1' ? 0 : codec === 'Vp9' ? 1 : codec === 'Vp8' ? 2 : codec === 'H265' ? 3 : 4;
  const out = new Uint8Array(VIDEO_HEADER_LEN);
  out[0] = tag;
  out[1] = frameType === 'Keyframe' ? 1 : 0;
  const view = new DataView(out.buffer);
  view.setUint16(2, width, true);
  view.setUint16(4, height, true);
  view.setBigUint64(6, BigInt(timestamp_us), true);
  view.setUint32(14, data_len, true);
  return out;
}

/** Pull complete Q2 frames off a WebTransport uni stream. */
export async function* readQ2Frames(
  stream: ReadableStream<Uint8Array>,
): AsyncGenerator<Q2Frame> {
  const reader = new ByteReader(stream.getReader());
  for (;;) {
    const header = await reader.readExact(VIDEO_HEADER_LEN);
    if (!header) return;
    const parsed = parseVideoHeader(header);
    const data = await reader.readExact(parsed.data_len);
    if (!data) return;
    yield {
      codec: parsed.codec,
      keyframe: parsed.keyframe,
      width: parsed.width,
      height: parsed.height,
      timestamp_us: parsed.timestamp_us,
      data,
    };
  }
}

class ByteReader {
  private buf = new Uint8Array(0);
  constructor(private readonly reader: ReadableStreamDefaultReader<Uint8Array>) {}

  async readExact(n: number): Promise<Uint8Array | null> {
    while (this.buf.byteLength < n) {
      const { value, done } = await this.reader.read();
      if (done) return null;
      const next = new Uint8Array(this.buf.byteLength + value.byteLength);
      next.set(this.buf);
      next.set(value, this.buf.byteLength);
      this.buf = next;
    }
    const out = this.buf.slice(0, n);
    this.buf = this.buf.slice(n);
    return out;
  }
}
