import { describe, expect, it } from 'vitest';
import { FRAME_MAGIC, FrameDecoder, MAX_PAYLOAD_SIZE, encodeFrame, tryDecodeFrame } from '../src/frame.js';

describe('control-stream framing', () => {
  it('encodes little-endian length and TL magic', () => {
    const payload = new Uint8Array([1, 2, 3, 4]);
    const frame = encodeFrame(0x01, payload);
    expect([...frame.slice(0, 2)]).toEqual([...FRAME_MAGIC]);
    expect(frame[2]).toBe(0x01);
    expect(frame[3]).toBe(4);
    expect(frame[4]).toBe(0);
    expect(frame[5]).toBe(0);
    expect(frame[6]).toBe(0);
    expect([...frame.slice(7)]).toEqual([1, 2, 3, 4]);
  });

  it('returns null for a partial header', () => {
    expect(tryDecodeFrame(new Uint8Array([0x54, 0x4c, 0x01]))).toBeNull();
  });

  it('returns null for a partial payload', () => {
    const full = encodeFrame(0x0a, new Uint8Array(10));
    expect(tryDecodeFrame(full.slice(0, 9))).toBeNull();
  });

  it('rejects invalid magic rather than resyncing', () => {
    expect(() => tryDecodeFrame(new Uint8Array([0x00, 0x00, 0, 0, 0, 0, 0]))).toThrow(
      /invalid magic/,
    );
  });

  it('rejects a length above the 16 MiB cap before allocating', () => {
    const buf = new Uint8Array(7);
    buf[0] = 0x54;
    buf[1] = 0x4c;
    new DataView(buf.buffer).setUint32(3, MAX_PAYLOAD_SIZE + 1, true);
    expect(() => tryDecodeFrame(buf)).toThrow(/too large/);
  });

  it('assembles a frame split across two chunks', () => {
    const full = encodeFrame(0x0b, new Uint8Array([9, 8, 7]));
    const dec = new FrameDecoder();
    expect(dec.push(full.slice(0, 5))).toEqual([]);
    const frames = dec.push(full.slice(5));
    expect(frames).toHaveLength(1);
    expect(frames[0].msgId).toBe(0x0b);
    expect([...frames[0].payload]).toEqual([9, 8, 7]);
  });
});
