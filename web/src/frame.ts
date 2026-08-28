//! Incremental 7-byte Termland framing.
//!
//! `[Magic "TL" 2B][MsgID 1B][Payload Length 4B LE][CBOR payload]`
//!
//! Partial headers and partial payloads return `null` rather than throwing;
//! a desynchronised stream (bad magic, length > 16 MiB) is a hard error.

export const FRAME_MAGIC = new Uint8Array([0x54, 0x4c]); // "TL"
export const HEADER_SIZE = 7;
export const MAX_PAYLOAD_SIZE = 16 * 1024 * 1024;

export class FrameError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'FrameError';
  }
}

export interface Frame {
  msgId: number;
  payload: Uint8Array;
}

export function encodeFrame(msgId: number, payload: Uint8Array): Uint8Array {
  if (payload.byteLength > MAX_PAYLOAD_SIZE) {
    throw new FrameError(`payload too large: ${payload.byteLength} bytes (max ${MAX_PAYLOAD_SIZE})`);
  }
  const out = new Uint8Array(HEADER_SIZE + payload.byteLength);
  out[0] = FRAME_MAGIC[0];
  out[1] = FRAME_MAGIC[1];
  out[2] = msgId & 0xff;
  const view = new DataView(out.buffer);
  view.setUint32(3, payload.byteLength, true);
  out.set(payload, HEADER_SIZE);
  return out;
}

/** Bytes consumed, or 0 if more input is needed. Throws on a hard error. */
export function tryDecodeFrame(src: Uint8Array): { frame: Frame; consumed: number } | null {
  if (src.byteLength < HEADER_SIZE) return null;
  if (src[0] !== FRAME_MAGIC[0] || src[1] !== FRAME_MAGIC[1]) {
    throw new FrameError('invalid magic bytes');
  }
  const msgId = src[2];
  const payloadLen = new DataView(src.buffer, src.byteOffset, src.byteLength).getUint32(3, true);
  if (payloadLen > MAX_PAYLOAD_SIZE) {
    throw new FrameError(`payload too large: ${payloadLen} bytes (max ${MAX_PAYLOAD_SIZE})`);
  }
  const total = HEADER_SIZE + payloadLen;
  if (src.byteLength < total) return null;
  return {
    frame: { msgId, payload: src.slice(HEADER_SIZE, total) },
    consumed: total,
  };
}

/** Accumulates stream chunks until a complete frame is available. */
export class FrameDecoder {
  private buf: Uint8Array<ArrayBufferLike> = new Uint8Array(0);

  push(chunk: Uint8Array): Frame[] {
    this.buf = concat(this.buf, chunk);
    const frames: Frame[] = [];
    for (;;) {
      const got = tryDecodeFrame(this.buf);
      if (!got) break;
      frames.push(got.frame);
      this.buf = this.buf.slice(got.consumed);
    }
    return frames;
  }
}

function concat(a: Uint8Array, b: Uint8Array): Uint8Array {
  const out = new Uint8Array(a.byteLength + b.byteLength);
  out.set(a, 0);
  out.set(b, a.byteLength);
  return out;
}
