//! Q2 video uni-stream: 18-byte header plus payload pump.
//!
//! Header encode/decode is the wasm protocol crate so the layout cannot drift
//! from `termland_protocol::q2`. `readQ2Frames` stays here: it is a
//! `ReadableStream` pump, which is browser API surface.

import type { VideoCodec } from './messages.js';
import { parseVideoHeader } from './protocol.js';

export { parseVideoHeader, encodeVideoHeader } from './protocol.js';

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

/**
 * Accumulate stream chunks in an array and concat once per `readExact`,
 * rather than reallocating the whole buffer on every QUIC chunk.
 */
class ByteReader {
  private leftover = new Uint8Array(0);
  constructor(private readonly reader: ReadableStreamDefaultReader<Uint8Array>) {}

  async readExact(n: number): Promise<Uint8Array | null> {
    const parts: Uint8Array[] = [];
    let have = this.leftover.byteLength;
    if (have > 0) parts.push(this.leftover);
    while (have < n) {
      const { value, done } = await this.reader.read();
      if (done) return null;
      parts.push(value);
      have += value.byteLength;
    }
    const joined = concatOnce(parts, have);
    const out = joined.slice(0, n);
    this.leftover = joined.byteLength > n ? joined.slice(n) : new Uint8Array(0);
    return out;
  }
}

function concatOnce(parts: Uint8Array[], total: number): Uint8Array {
  if (parts.length === 1) return parts[0];
  const out = new Uint8Array(total);
  let offset = 0;
  for (const p of parts) {
    out.set(p, offset);
    offset += p.byteLength;
  }
  return out;
}
