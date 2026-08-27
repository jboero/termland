import { describe, expect, it } from 'vitest';
import { encodeVideoHeader, parseVideoHeader, VIDEO_HEADER_LEN } from '../src/q2.js';

describe('Q2 video header', () => {
  it('round-trips every codec and frame type', () => {
    for (const codec of ['Av1', 'Vp9', 'Vp8', 'H265', 'H264'] as const) {
      for (const frameType of ['Keyframe', 'Inter'] as const) {
        const header = encodeVideoHeader(codec, frameType, 1920, 1080, 123_456_789_012, 65536);
        expect(header.byteLength).toBe(VIDEO_HEADER_LEN);
        const parsed = parseVideoHeader(header);
        expect(parsed.codec).toBe(codec);
        expect(parsed.keyframe).toBe(frameType === 'Keyframe');
        expect(parsed.width).toBe(1920);
        expect(parsed.height).toBe(1080);
        expect(parsed.timestamp_us).toBe(123_456_789_012);
        expect(parsed.data_len).toBe(65536);
      }
    }
  });

  it('matches the exact byte layout pinned on the Rust side', () => {
    // crates/termland-server/src/quic.rs::video_header_byte_offsets_are_exact
    const header = encodeVideoHeader('H264', 'Keyframe', 0x0102, 0x0304, 0x0102030405060708n, 0xaabbccdd);
    expect(header[0]).toBe(4);
    expect(header[1]).toBe(1);
    expect(header[2]).toBe(0x02);
    expect(header[3]).toBe(0x01);
    expect(header[4]).toBe(0x04);
    expect(header[5]).toBe(0x03);
    const ts = new DataView(header.buffer, header.byteOffset, header.byteLength).getBigUint64(6, true);
    expect(ts).toBe(0x0102030405060708n);
    expect([...header.slice(14)]).toEqual([0xdd, 0xcc, 0xbb, 0xaa]);
  });

  it('rejects an unknown codec tag', () => {
    const buf = new Uint8Array(VIDEO_HEADER_LEN);
    buf[0] = 99;
    expect(() => parseVideoHeader(buf)).toThrow(/unknown codec/);
  });

  it('rejects a data_len above the 16 MiB cap before allocating', () => {
    const header = encodeVideoHeader('Av1', 'Keyframe', 64, 64, 0, 16 * 1024 * 1024 + 1);
    expect(() => parseVideoHeader(header)).toThrow(/implausible frame size/);
  });
});
