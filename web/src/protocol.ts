//! wasm-bindgen glue for `crates/termland-web`.
//!
//! Glue lives in `web/pkg/` (outside `src/`) and is loaded relative to the
//! compiled file. `TermlandClient.start` calls `initProtocol()`; tests use
//! `tests/setup-wasm.ts`.

import { fromWire, toWire, type FrameType, type Message, type VideoCodec } from './messages.js';

type WasmApi = {
  default: (module?: unknown) => Promise<unknown>;
  FrameDecoder: new () => { push: (chunk: Uint8Array) => Array<unknown> };
  decodeMessage: (payload: Uint8Array) => unknown;
  encodeFrame: (msgId: number, payload: Uint8Array) => Uint8Array;
  encodePayload: (msg: object) => Uint8Array;
  encodeVideoHeader: (
    codec: string,
    keyframe: boolean,
    width: number,
    height: number,
    timestamp_us: bigint,
    data_len: number,
  ) => Uint8Array;
  encodeWire: (msg: object) => Uint8Array;
  frameMagic: () => Uint8Array;
  maxPayloadSize: () => number;
  parseVideoHeader: (header: Uint8Array) => unknown;
  videoHeaderLen: () => number;
};

let api: WasmApi | null = null;
let ready: Promise<void> | null = null;

function wasm(): WasmApi {
  if (!api) {
    throw new Error('protocol wasm is not initialised; call initProtocol() first');
  }
  return api;
}

/** Load the wasm module. Safe to call more than once.
 *
 * In the browser, omit `wasmBytes` so the glue fetch()es `termland_web_bg.wasm`
 * next to itself. Node tests pass the file bytes (fetch of file: is missing).
 */
export function initProtocol(wasmBytes?: BufferSource): Promise<void> {
  if (!ready) {
    ready = (async () => {
      const jsUrl = new URL('../pkg/termland_web.js', import.meta.url);
      const mod = (await import(jsUrl.href)) as WasmApi;
      await mod.default(wasmBytes ? { module_or_path: wasmBytes } : undefined);
      api = mod;
    })();
  }
  return ready;
}

export function encodeWire(msg: Message): Uint8Array {
  return wasm().encodeWire(toWire(msg));
}

export function encodePayload(msg: Message): Uint8Array {
  return wasm().encodePayload(toWire(msg));
}

export function decodeMessage(payload: Uint8Array): Message {
  return fromWire(wasm().decodeMessage(payload));
}

export function encodeFrame(msgId: number, payload: Uint8Array): Uint8Array {
  return wasm().encodeFrame(msgId, payload);
}

export function FRAME_MAGIC(): Uint8Array {
  return wasm().frameMagic();
}

export function MAX_PAYLOAD_SIZE(): number {
  return wasm().maxPayloadSize();
}

export function VIDEO_HEADER_LEN(): number {
  return wasm().videoHeaderLen();
}

export class FrameDecoder {
  private inner: { push: (chunk: Uint8Array) => Array<unknown> };

  constructor() {
    this.inner = new (wasm().FrameDecoder)();
  }

  /** Complete CBOR payloads (not `{ msgId, payload }` frames). */
  push(chunk: Uint8Array): Uint8Array[] {
    const arr = this.inner.push(chunk);
    const out: Uint8Array[] = [];
    for (let i = 0; i < arr.length; i++) {
      out.push(arr[i] as Uint8Array);
    }
    return out;
  }
}

export function parseVideoHeader(buf: Uint8Array): {
  codec: VideoCodec;
  keyframe: boolean;
  width: number;
  height: number;
  timestamp_us: number;
  data_len: number;
} {
  const parsed = wasm().parseVideoHeader(buf) as {
    codec: VideoCodec;
    keyframe: boolean;
    width: number;
    height: number;
    timestamp_us: number | bigint;
    data_len: number;
  };
  return {
    codec: parsed.codec,
    keyframe: parsed.keyframe,
    width: parsed.width,
    height: parsed.height,
    timestamp_us: Number(parsed.timestamp_us),
    data_len: parsed.data_len,
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
  return wasm().encodeVideoHeader(
    codec,
    frameType === 'Keyframe',
    width,
    height,
    BigInt(timestamp_us),
    data_len,
  );
}
