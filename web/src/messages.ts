//! Termland control-plane messages: serde externally-tagged CBOR.
//!
//! Field names and enum variant names must match `termland-protocol` exactly.
//! Adding a field here that the Rust side doesn't know is fine (unknown keys
//! are skipped); renaming one is a protocol break.

import {
  CborValue,
  array,
  asArray,
  asBool,
  asFloat,
  asMap,
  asText,
  asUint,
  bool,
  decode as cborDecode,
  encode as cborEncode,
  field,
  fieldOr,
  float,
  map,
  nil,
  opt,
  singleKey,
  tagged,
  text,
  uint,
} from './cbor.js';
import { encodeFrame } from './frame.js';

export const PROTOCOL_VERSION = 1;

export const MessageId = {
  Hello: 0x01,
  HelloAck: 0x02,
  AuthRequest: 0x03,
  AuthResponse: 0x04,
  AuthResult: 0x05,
  SessionCreate: 0x06,
  SessionReady: 0x07,
  SessionResize: 0x08,
  SessionEnd: 0x09,
  Ping: 0x0a,
  Pong: 0x0b,
  SessionList: 0x0c,
  SessionListResult: 0x0d,
  SessionAttach: 0x0e,
  SessionClose: 0x0f,
  VideoFrame: 0x20,
  KeyEvent: 0x40,
  MouseMove: 0x41,
  MouseButton: 0x42,
  MouseScroll: 0x43,
  TextInput: 0x47,
} as const;

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

export function messageId(msg: Message): number {
  switch (msg.type) {
    case 'Hello':
      return MessageId.Hello;
    case 'HelloAck':
      return MessageId.HelloAck;
    case 'AuthRequest':
      return MessageId.AuthRequest;
    case 'AuthResponse':
      return MessageId.AuthResponse;
    case 'AuthResult':
      return MessageId.AuthResult;
    case 'SessionCreate':
      return MessageId.SessionCreate;
    case 'SessionReady':
      return MessageId.SessionReady;
    case 'SessionResize':
      return MessageId.SessionResize;
    case 'SessionEnd':
      return MessageId.SessionEnd;
    case 'Ping':
      return MessageId.Ping;
    case 'Pong':
      return MessageId.Pong;
    case 'SessionList':
      return MessageId.SessionList;
    case 'SessionListResult':
      return MessageId.SessionListResult;
    case 'SessionAttach':
      return MessageId.SessionAttach;
    case 'SessionClose':
      return MessageId.SessionClose;
    case 'KeyEvent':
      return MessageId.KeyEvent;
    case 'TextInput':
      return MessageId.TextInput;
    case 'MouseMove':
      return MessageId.MouseMove;
    case 'MouseButton':
      return MessageId.MouseButton;
    case 'MouseScroll':
      return MessageId.MouseScroll;
    case 'Unknown':
      return 0;
  }
}

function optText(v: string | null): CborValue {
  return v === null ? nil() : text(v);
}
function optUint(v: number | null): CborValue {
  return v === null ? nil() : uint(v);
}

function encodeMode(mode: SessionMode): CborValue {
  if (mode.kind === 'Desktop') return text('Desktop');
  return tagged('App', map([
    ['command', text(mode.command)],
    ['args', array(mode.args.map(text))],
  ]));
}

function encodeCodecs(cs: string[]): CborValue {
  return array(cs.map(text));
}

function encodeBody(msg: Message): CborValue {
  switch (msg.type) {
    case 'Hello':
      return map([
        ['protocol_version', uint(msg.protocol_version)],
        ['client_name', text(msg.client_name)],
      ]);
    case 'HelloAck':
      return map([
        ['protocol_version', uint(msg.protocol_version)],
        ['server_name', text(msg.server_name)],
        ['session_id', text(msg.session_id)],
        ['auth_required', bool(msg.auth_required)],
      ]);
    case 'AuthRequest':
      return map([['methods', array(msg.methods.map(text))]]);
    case 'AuthResponse':
      return map([
        ['username', text(msg.username)],
        ['credential', text(msg.credential)],
      ]);
    case 'AuthResult':
      return map([
        ['success', bool(msg.success)],
        ['message', text(msg.message)],
      ]);
    case 'SessionCreate':
      return map([
        ['mode', encodeMode(msg.mode)],
        ['width', uint(msg.width)],
        ['height', uint(msg.height)],
        ['audio', bool(msg.audio)],
        ['quality', uint(msg.quality)],
        ['desktop_shell', optText(msg.desktop_shell)],
        ['encoder_preset', optText(msg.encoder_preset)],
        ['encoder_crf', optUint(msg.encoder_crf)],
        ['encoder_extra_params', optText(msg.encoder_extra_params)],
        ['supported_codecs', encodeCodecs(msg.supported_codecs)],
        ['supported_audio_codecs', encodeCodecs(msg.supported_audio_codecs)],
      ]);
    case 'SessionReady':
      return map([
        ['width', uint(msg.width)],
        ['height', uint(msg.height)],
        ['xkb_keymap', optText(msg.xkb_keymap)],
        ['codec', msg.codec ? text(msg.codec) : nil()],
        ['audio_codec', msg.audio_codec ? text(msg.audio_codec) : nil()],
        ['session_id', text(msg.session_id)],
      ]);
    case 'SessionResize':
      return map([
        ['width', uint(msg.width)],
        ['height', uint(msg.height)],
      ]);
    case 'SessionEnd':
      return map([['reason', text(msg.reason)]]);
    case 'Ping':
    case 'Pong':
      return map([['timestamp_us', uint(msg.timestamp_us)]]);
    case 'SessionList':
      return map([]);
    case 'SessionListResult':
      return map([
        [
          'sessions',
          array(
            msg.sessions.map((s) =>
              map([
                ['session_id', text(s.session_id)],
                ['mode', text(s.mode)],
                ['width', uint(s.width)],
                ['height', uint(s.height)],
                ['age_secs', uint(s.age_secs)],
                ['attached', bool(s.attached)],
              ]),
            ),
          ),
        ],
      ]);
    case 'SessionAttach':
      return map([
        ['session_id', text(msg.session_id)],
        ['audio', bool(msg.audio)],
        ['quality', uint(msg.quality)],
        ['encoder_preset', optText(msg.encoder_preset)],
        ['encoder_crf', optUint(msg.encoder_crf)],
        ['encoder_extra_params', optText(msg.encoder_extra_params)],
        ['supported_codecs', encodeCodecs(msg.supported_codecs)],
        ['supported_audio_codecs', encodeCodecs(msg.supported_audio_codecs)],
      ]);
    case 'SessionClose':
      return map([['session_id', text(msg.session_id)]]);
    case 'KeyEvent':
      return map([
        ['scancode', uint(msg.scancode)],
        ['keysym', uint(msg.keysym)],
        ['state', text(msg.state)],
        ['modifiers', uint(msg.modifiers)],
      ]);
    case 'TextInput':
      return map([['text', text(msg.text)]]);
    case 'MouseMove':
      return map([
        ['x', float(msg.x)],
        ['y', float(msg.y)],
        ['absolute', bool(msg.absolute)],
      ]);
    case 'MouseButton':
      return map([
        ['button', uint(msg.button)],
        ['state', text(msg.state)],
      ]);
    case 'MouseScroll':
      return map([
        ['dx', float(msg.dx)],
        ['dy', float(msg.dy)],
      ]);
    case 'Unknown':
      throw new Error(`cannot encode unknown message ${msg.tag}`);
  }
}

export function encodeMessage(msg: Message): Uint8Array {
  return cborEncode(tagged(msg.type, encodeBody(msg)));
}

export function encodeWire(msg: Message): Uint8Array {
  return encodeFrame(messageId(msg), encodeMessage(msg));
}

function decodeMode(v: CborValue): SessionMode {
  if (v.t === 'text' && v.v === 'Desktop') return { kind: 'Desktop' };
  const [tag, body] = singleKey(v);
  if (tag !== 'App') throw new Error(`unknown SessionMode ${tag}`);
  const m = asMap(body);
  return {
    kind: 'App',
    command: asText(field(m, 'command')),
    args: asArray(field(m, 'args')).map(asText),
  };
}

function decodeCodecs<T extends string>(v: CborValue | undefined): T[] {
  if (v === undefined) return [];
  return asArray(v).map((x) => asText(x) as T);
}

function decodeSessionInfo(v: CborValue): SessionInfo {
  const m = asMap(v);
  return {
    session_id: asText(field(m, 'session_id')),
    mode: asText(field(m, 'mode')),
    width: asUint(field(m, 'width')),
    height: asUint(field(m, 'height')),
    age_secs: asUint(field(m, 'age_secs')),
    attached: asBool(fieldOr(m, 'attached', { t: 'bool', v: false })),
  };
}

export function decodeMessage(payload: Uint8Array): Message {
  const [tag, body] = singleKey(cborDecode(payload));
  const m = body.t === 'map' ? asMap(body) : new Map<string, CborValue>();
  switch (tag) {
    case 'Hello':
      return {
        type: 'Hello',
        protocol_version: asUint(field(m, 'protocol_version')),
        client_name: asText(field(m, 'client_name')),
      };
    case 'HelloAck':
      return {
        type: 'HelloAck',
        protocol_version: asUint(field(m, 'protocol_version')),
        server_name: asText(field(m, 'server_name')),
        session_id: asText(fieldOr(m, 'session_id', text(''))),
        auth_required: asBool(fieldOr(m, 'auth_required', { t: 'bool', v: false })),
      };
    case 'AuthRequest':
      return { type: 'AuthRequest', methods: asArray(field(m, 'methods')).map(asText) };
    case 'AuthResponse':
      return {
        type: 'AuthResponse',
        username: asText(field(m, 'username')),
        credential: asText(field(m, 'credential')),
      };
    case 'AuthResult':
      return {
        type: 'AuthResult',
        success: asBool(field(m, 'success')),
        message: asText(field(m, 'message')),
      };
    case 'SessionCreate':
      return {
        type: 'SessionCreate',
        mode: decodeMode(field(m, 'mode')),
        width: asUint(field(m, 'width')),
        height: asUint(field(m, 'height')),
        audio: asBool(field(m, 'audio')),
        quality: asUint(fieldOr(m, 'quality', uint(75))),
        desktop_shell: opt(m.get('desktop_shell'), asText),
        encoder_preset: opt(m.get('encoder_preset'), asText),
        encoder_crf: opt(m.get('encoder_crf'), asUint),
        encoder_extra_params: opt(m.get('encoder_extra_params'), asText),
        supported_codecs: decodeCodecs(m.get('supported_codecs')),
        supported_audio_codecs: decodeCodecs(m.get('supported_audio_codecs')),
      };
    case 'SessionReady':
      return {
        type: 'SessionReady',
        width: asUint(field(m, 'width')),
        height: asUint(field(m, 'height')),
        xkb_keymap: opt(m.get('xkb_keymap'), asText),
        codec: opt(m.get('codec'), asText) as VideoCodec | null,
        audio_codec: opt(m.get('audio_codec'), asText) as AudioCodec | null,
        session_id: asText(fieldOr(m, 'session_id', text(''))),
      };
    case 'SessionResize':
      return {
        type: 'SessionResize',
        width: asUint(field(m, 'width')),
        height: asUint(field(m, 'height')),
      };
    case 'SessionEnd':
      return { type: 'SessionEnd', reason: asText(field(m, 'reason')) };
    case 'Ping':
      return { type: 'Ping', timestamp_us: asUint(field(m, 'timestamp_us')) };
    case 'Pong':
      return { type: 'Pong', timestamp_us: asUint(field(m, 'timestamp_us')) };
    case 'SessionList':
      return { type: 'SessionList' };
    case 'SessionListResult':
      return {
        type: 'SessionListResult',
        sessions: asArray(field(m, 'sessions')).map(decodeSessionInfo),
      };
    case 'SessionAttach':
      return {
        type: 'SessionAttach',
        session_id: asText(field(m, 'session_id')),
        audio: asBool(field(m, 'audio')),
        quality: asUint(fieldOr(m, 'quality', uint(75))),
        encoder_preset: opt(m.get('encoder_preset'), asText),
        encoder_crf: opt(m.get('encoder_crf'), asUint),
        encoder_extra_params: opt(m.get('encoder_extra_params'), asText),
        supported_codecs: decodeCodecs(m.get('supported_codecs')),
        supported_audio_codecs: decodeCodecs(m.get('supported_audio_codecs')),
      };
    case 'SessionClose':
      return { type: 'SessionClose', session_id: asText(field(m, 'session_id')) };
    case 'KeyEvent':
      return {
        type: 'KeyEvent',
        scancode: asUint(field(m, 'scancode')),
        keysym: asUint(field(m, 'keysym')),
        state: asText(field(m, 'state')) as KeyState,
        modifiers: asUint(field(m, 'modifiers')),
      };
    case 'TextInput':
      return { type: 'TextInput', text: asText(field(m, 'text')) };
    case 'MouseMove':
      return {
        type: 'MouseMove',
        x: asFloat(field(m, 'x')),
        y: asFloat(field(m, 'y')),
        absolute: asBool(field(m, 'absolute')),
      };
    case 'MouseButton':
      return {
        type: 'MouseButton',
        button: asUint(field(m, 'button')),
        state: asText(field(m, 'state')) as ButtonState,
      };
    case 'MouseScroll':
      return {
        type: 'MouseScroll',
        dx: asFloat(field(m, 'dx')),
        dy: asFloat(field(m, 'dy')),
      };
    default:
      return { type: 'Unknown', tag };
  }
}
