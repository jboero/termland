//! Control-plane types, plus the reshape between `{ type: 'Hello', ... }` and
//! serde's `{ Hello: { ... } }` that the wasm codec speaks. CBOR is wasm.

export const PROTOCOL_VERSION = 1;

export type VideoCodec = 'Av1' | 'Vp9' | 'Vp8' | 'H265' | 'H264';
export type AudioCodec = 'Opus';
export type FrameType = 'Keyframe' | 'Inter';
export type KeyState = 'Pressed' | 'Released' | 'Repeat';
export type ButtonState = 'Pressed' | 'Released';

export type Message =
  | { type: 'Hello'; protocol_version: number; client_name: string }
  | {
      type: 'HelloAck';
      protocol_version: number;
      server_name: string;
      session_id: string;
      auth_required: boolean;
    }
  | { type: 'AuthRequest'; methods: string[] }
  | { type: 'AuthResponse'; username: string; credential: string }
  | { type: 'AuthResult'; success: boolean; message: string }
  | {
      type: 'SessionCreate';
      mode: SessionMode;
      width: number;
      height: number;
      audio: boolean;
      quality: number;
      desktop_shell: string | null;
      encoder_preset: string | null;
      encoder_crf: number | null;
      encoder_extra_params: string | null;
      supported_codecs: VideoCodec[];
      supported_audio_codecs: AudioCodec[];
    }
  | {
      type: 'SessionReady';
      width: number;
      height: number;
      xkb_keymap: string | null;
      codec: VideoCodec | null;
      audio_codec: AudioCodec | null;
      session_id: string;
    }
  | { type: 'SessionResize'; width: number; height: number }
  | { type: 'SessionEnd'; reason: string }
  | { type: 'Ping'; timestamp_us: number }
  | { type: 'Pong'; timestamp_us: number }
  | { type: 'SessionList' }
  | { type: 'SessionListResult'; sessions: SessionInfo[] }
  | {
      type: 'SessionAttach';
      session_id: string;
      audio: boolean;
      quality: number;
      encoder_preset: string | null;
      encoder_crf: number | null;
      encoder_extra_params: string | null;
      supported_codecs: VideoCodec[];
      supported_audio_codecs: AudioCodec[];
    }
  | { type: 'SessionClose'; session_id: string }
  | { type: 'KeyEvent'; scancode: number; keysym: number; state: KeyState; modifiers: number }
  | { type: 'TextInput'; text: string }
  | { type: 'MouseMove'; x: number; y: number; absolute: boolean }
  | { type: 'MouseButton'; button: number; state: ButtonState }
  | { type: 'MouseScroll'; dx: number; dy: number }
  | { type: 'Unknown'; tag: string };

export type SessionMode = { kind: 'Desktop' } | { kind: 'App'; command: string; args: string[] };

export interface SessionInfo {
  session_id: string;
  mode: string;
  width: number;
  height: number;
  age_secs: number;
  attached: boolean;
}

/** Serde externally-tagged object the wasm `Message` deserializer expects. */
export function toWire(msg: Message): object {
  if (msg.type === 'Unknown') {
    throw new Error(`cannot encode unknown message ${msg.tag}`);
  }
  if (msg.type === 'SessionList') {
    return { SessionList: {} };
  }
  if (msg.type === 'SessionCreate') {
    const { type, mode, ...rest } = msg;
    return { [type]: { ...rest, mode: modeToWire(mode) } };
  }
  const { type, ...rest } = msg;
  return { [type]: rest };
}

/** Inverse of `toWire`. Also accepts `{ type: 'Unknown', tag }` from wasm. */
export function fromWire(value: unknown): Message {
  if (!value || typeof value !== 'object') {
    throw new Error('decodeMessage: expected an object');
  }
  const rec = value as Record<string, unknown>;
  if (rec.type === 'Unknown') {
    return { type: 'Unknown', tag: String(rec.tag ?? 'undecodable') };
  }
  const keys = Object.keys(rec);
  if (keys.length !== 1) {
    throw new Error(`decodeMessage: expected one tag, got ${keys.join(',')}`);
  }
  const type = keys[0];
  const body = rec[type];
  if (type === 'SessionList') {
    return { type: 'SessionList' };
  }
  if (body === null || typeof body !== 'object' || Array.isArray(body)) {
    throw new Error(`decodeMessage: ${type} body is not a map`);
  }
  const fields = nulls(body as Record<string, unknown>);
  if (type === 'SessionCreate') {
    return {
      type: 'SessionCreate',
      ...fields,
      mode: modeFromWire(fields.mode),
    } as Message;
  }
  return { type, ...fields } as Message;
}

function modeToWire(mode: SessionMode): unknown {
  if (mode.kind === 'Desktop') return 'Desktop';
  return { App: { command: mode.command, args: mode.args } };
}

function modeFromWire(v: unknown): SessionMode {
  if (v === 'Desktop') return { kind: 'Desktop' };
  if (v && typeof v === 'object' && 'App' in v) {
    const app = (v as { App: { command: string; args: string[] } }).App;
    return { kind: 'App', command: app.command, args: app.args };
  }
  throw new Error(`unknown SessionMode ${JSON.stringify(v)}`);
}

function nulls(obj: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(obj)) {
    out[k] = v === undefined ? null : v;
  }
  return out;
}
