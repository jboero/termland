import { describe, expect, it } from 'vitest';
import { readFileSync, existsSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { decodeMessage, encodePayload } from '../src/protocol.ts';
import type { Message } from '../src/messages.ts';

const here = dirname(fileURLToPath(import.meta.url));
const rustDir = join(here, '../fixtures/from-rust');

function canonical(): [string, Message][] {
  return [
    ['hello', { type: 'Hello', protocol_version: 1, client_name: 'fixture-client' }],
    [
      'hello_ack',
      {
        type: 'HelloAck',
        protocol_version: 1,
        server_name: 'termland-server',
        session_id: 'session-fixture',
        auth_required: true,
      },
    ],
    ['auth_request', { type: 'AuthRequest', methods: ['password'] }],
    ['auth_response', { type: 'AuthResponse', username: 'alice', credential: 'secret' }],
    ['auth_result', { type: 'AuthResult', success: true, message: 'authenticated' }],
    ['session_list', { type: 'SessionList' }],
    [
      'session_list_result',
      {
        type: 'SessionListResult',
        sessions: [
          {
            session_id: 's1',
            mode: 'desktop',
            width: 1920,
            height: 1080,
            age_secs: 42,
            attached: false,
          },
        ],
      },
    ],
    [
      'session_create',
      {
        type: 'SessionCreate',
        mode: { kind: 'Desktop' },
        width: 1280,
        height: 720,
        audio: false,
        quality: 75,
        desktop_shell: null,
        encoder_preset: null,
        encoder_crf: null,
        encoder_extra_params: null,
        supported_codecs: ['Av1', 'Vp9'],
        supported_audio_codecs: ['Opus'],
      },
    ],
    [
      'session_attach',
      {
        type: 'SessionAttach',
        session_id: 's1',
        audio: false,
        quality: 75,
        encoder_preset: null,
        encoder_crf: null,
        encoder_extra_params: null,
        supported_codecs: ['Av1'],
        supported_audio_codecs: ['Opus'],
      },
    ],
    ['session_close', { type: 'SessionClose', session_id: 's1' }],
    [
      'session_ready',
      {
        type: 'SessionReady',
        width: 1280,
        height: 720,
        xkb_keymap: null,
        codec: 'Av1',
        audio_codec: null,
        session_id: 's1',
      },
    ],
    ['session_resize', { type: 'SessionResize', width: 800, height: 600 }],
    ['session_end', { type: 'SessionEnd', reason: 'closed by fixture' }],
    ['ping', { type: 'Ping', timestamp_us: 1_000_000 }],
    ['pong', { type: 'Pong', timestamp_us: 1_000_000 }],
    [
      'key_event',
      { type: 'KeyEvent', scancode: 30, keysym: 0, state: 'Pressed', modifiers: 0 },
    ],
    ['text_input', { type: 'TextInput', text: 'héllo 世界' }],
    ['mouse_move', { type: 'MouseMove', x: 100.5, y: 200.25, absolute: true }],
    ['mouse_button', { type: 'MouseButton', button: 0x110, state: 'Pressed' }],
    ['mouse_scroll', { type: 'MouseScroll', dx: 0.0, dy: -15.0 }],
  ];
}

describe('wasm protocol codec', () => {
  it('decodes every Rust-originated fixture', () => {
    if (!existsSync(rustDir)) {
      throw new Error(
        'web/fixtures/from-rust is missing; run UPDATE_WEB_FIXTURES=1 cargo test -p termland-protocol --test web_cross_language',
      );
    }
    for (const [name, expected] of canonical()) {
      const bytes = new Uint8Array(readFileSync(join(rustDir, `${name}.cbor`)));
      const decoded = decodeMessage(bytes);
      expect(decoded.type, name).toBe(expected.type);
      if (decoded.type === 'Hello' && expected.type === 'Hello') {
        expect(decoded.client_name).toBe(expected.client_name);
        expect(decoded.protocol_version).toBe(expected.protocol_version);
      }
      if (decoded.type === 'HelloAck' && expected.type === 'HelloAck') {
        expect(decoded.auth_required).toBe(true);
        expect(decoded.session_id).toBe('session-fixture');
      }
      if (decoded.type === 'SessionCreate' && expected.type === 'SessionCreate') {
        expect(decoded.width).toBe(1280);
        expect(decoded.supported_codecs).toEqual(['Av1', 'Vp9']);
      }
      if (decoded.type === 'TextInput' && expected.type === 'TextInput') {
        expect(decoded.text).toBe(expected.text);
      }
      if (decoded.type === 'KeyEvent' && expected.type === 'KeyEvent') {
        expect(decoded.scancode).toBe(30);
      }
      if (decoded.type === 'MouseMove' && expected.type === 'MouseMove') {
        expect(decoded.x).toBeCloseTo(expected.x);
        expect(decoded.absolute).toBe(true);
      }
    }
  });

  it('encodes the same CBOR bytes as the committed Rust fixtures', () => {
    for (const [name, msg] of canonical()) {
      const encoded = encodePayload(msg);
      const path = join(rustDir, `${name}.cbor`);
      const committed = readFileSync(path);
      expect(Buffer.from(encoded).equals(committed), `${name}.cbor drifted from wasm encode`).toBe(
        true,
      );
      expect(decodeMessage(encoded).type).toBe(msg.type);
    }
  });
});
