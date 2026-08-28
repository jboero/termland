import { describe, expect, it } from 'vitest';
import {
  FRAME_MAGIC,
  FrameDecoder,
  MAX_PAYLOAD_SIZE,
  encodeFrame,
} from '../src/protocol.ts';

describe('control-stream framing (wasm)', () => {
  it('encodes little-endian length and TL magic', () => {
    const payload = new Uint8Array([1, 2, 3, 4]);
    const frame = encodeFrame(0x01, payload);
    const magic = FRAME_MAGIC();
    expect([...frame.slice(0, 2)]).toEqual([...magic]);
    expect(frame[2]).toBe(0x01);
    expect(frame[3]).toBe(4);
    expect(frame[4]).toBe(0);
    expect(frame[5]).toBe(0);
    expect(frame[6]).toBe(0);
    expect([...frame.slice(7)]).toEqual([1, 2, 3, 4]);
  });

  it('returns no frame for a partial header', () => {
    const dec = new FrameDecoder();
    expect(dec.push(new Uint8Array([0x54, 0x4c, 0x01]))).toEqual([]);
  });

  it('returns no frame for a partial payload', () => {
    const full = encodeFrame(0x0a, new Uint8Array(10));
    const dec = new FrameDecoder();
    expect(dec.push(full.slice(0, 9))).toEqual([]);
  });

  it('rejects invalid magic rather than resyncing', () => {
    const dec = new FrameDecoder();
    expect(() => dec.push(new Uint8Array([0x00, 0x00, 0, 0, 0, 0, 0]))).toThrow();
  });

  it('rejects a length above the 16 MiB cap before allocating', () => {
    const buf = new Uint8Array(7);
    buf[0] = 0x54;
    buf[1] = 0x4c;
    new DataView(buf.buffer).setUint32(3, MAX_PAYLOAD_SIZE() + 1, true);
    const dec = new FrameDecoder();
    expect(() => dec.push(buf)).toThrow();
  });

  it('assembles a frame split across two chunks', () => {
    const full = encodeFrame(0x0b, new Uint8Array([9, 8, 7]));
    const dec = new FrameDecoder();
    expect(dec.push(full.slice(0, 5))).toEqual([]);
    const payloads = dec.push(full.slice(5));
    expect(payloads).toHaveLength(1);
    expect([...payloads[0]]).toEqual([9, 8, 7]);
  });
});
