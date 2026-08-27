//! WebTransport Termland client: handshake, session control, Q2 video, reconnect.

import { FrameDecoder } from './frame.js';
import {
  decodeMessage,
  encodeWire,
  PROTOCOL_VERSION,
  type Message,
  type SessionInfo,
  type VideoCodec,
} from './messages.js';
import { probeSupportedCodecs, type CodecConfig } from './codecs.js';
import { readQ2Frames, type Q2Frame } from './q2.js';

export type ClientEvent =
  | { type: 'status'; message: string }
  | { type: 'hello'; session_id: string; auth_required: boolean; server_name: string }
  | { type: 'session-list'; sessions: SessionInfo[] }
  | { type: 'session-ready'; width: number; height: number; codec: VideoCodec | null; session_id: string }
  | { type: 'session-end'; reason: string }
  | { type: 'video'; frame: Q2Frame }
  | { type: 'reconnecting'; attempt: number }
  | { type: 'error'; error: string };

export interface ConnectOptions {
  url: string;
  certHashHex?: string;
  username?: string;
  password?: string;
}

function decodeHex(s: string): Uint8Array | null {
  const cleaned = s.replace(/[\s:-]/g, '');
  if (cleaned.length % 2 !== 0) return null;
  const out = new Uint8Array(cleaned.length / 2);
  for (let i = 0; i < out.length; i++) {
    const n = Number.parseInt(cleaned.slice(i * 2, i * 2 + 2), 16);
    if (Number.isNaN(n)) return null;
    out[i] = n;
  }
  return out;
}

/** 1s, 2s, 4s, … capped at 30s. `attempt` is 1 after the first drop. */
export function backoffDelay(attempt: number): number {
  const shift = Math.min(Math.max(attempt - 1, 0), 5);
  return Math.min(1000 * (1 << shift), 30_000);
}

export class TermlandClient {
  private transport: WebTransport | null = null;
  private writer: WritableStreamDefaultWriter<Uint8Array> | null = null;
  private closed = false;
  private sessionId: string | null = null;
  private attachNext = false;
  private pingTimer: ReturnType<typeof setInterval> | null = null;
  private codecs: CodecConfig[] = [];
  private reconnectEnabled = true;

  constructor(
    private readonly opts: ConnectOptions,
    private readonly emit: (ev: ClientEvent) => void,
  ) {}

  supportedCodecs(): VideoCodec[] {
    return this.codecs.map((c) => c.codec);
  }

  probed(): CodecConfig[] {
    return this.codecs;
  }

  async start(): Promise<void> {
    this.codecs = await probeSupportedCodecs();
    if (this.codecs.length === 0) {
      this.emit({ type: 'error', error: 'this browser cannot decode any Termland video codec' });
      return;
    }
    await this.connectLoop();
  }

  stop(): void {
    this.closed = true;
    this.reconnectEnabled = false;
    this.stopPing();
    try {
      this.send({ type: 'SessionEnd', reason: 'client quit' });
    } catch {
      // writer may already be gone
    }
    this.transport?.close();
  }

  send(msg: Message): void {
    const writer = this.writer;
    if (!writer) return;
    void writer.write(encodeWire(msg)).catch((e) => {
      this.emit({ type: 'error', error: String(e) });
    });
  }

  listSessions(): void {
    this.send({ type: 'SessionList' });
  }

  createSession(width: number, height: number): void {
    this.send({
      type: 'SessionCreate',
      mode: { kind: 'Desktop' },
      width,
      height,
      audio: false,
      quality: 75,
      desktop_shell: null,
      encoder_preset: null,
      encoder_crf: null,
      encoder_extra_params: null,
      supported_codecs: this.supportedCodecs(),
      supported_audio_codecs: [],
    });
  }

  attachSession(id: string, width: number, height: number): void {
    this.sessionId = id;
    void width;
    void height;
    this.send({
      type: 'SessionAttach',
      session_id: id,
      audio: false,
      quality: 75,
      encoder_preset: null,
      encoder_crf: null,
      encoder_extra_params: null,
      supported_codecs: this.supportedCodecs(),
      supported_audio_codecs: [],
    });
  }

  closeSession(id: string): void {
    this.send({ type: 'SessionClose', session_id: id });
  }

  private async connectLoop(): Promise<void> {
    let attempt = 0;
    for (;;) {
      if (this.closed) return;
      try {
        if (attempt > 0) {
          this.emit({ type: 'reconnecting', attempt });
          await sleep(backoffDelay(attempt));
          if (this.closed) return;
        }
        await this.runOnce();
        if (this.closed || !this.reconnectEnabled) return;
        attempt += 1;
      } catch (e) {
        this.emit({ type: 'error', error: String(e) });
        if (this.closed || !this.reconnectEnabled) return;
        attempt += 1;
      }
    }
  }

  private async runOnce(): Promise<void> {
    this.emit({ type: 'status', message: 'opening WebTransport…' });
    const transport = openTransport(this.opts.url, this.opts.certHashHex);
    this.transport = transport;
    await transport.ready;
    this.emit({ type: 'status', message: 'transport ready' });

    const bidi = await transport.createBidirectionalStream();
    this.writer = bidi.writable.getWriter();

    const videoTask = this.readVideo(transport);
    const controlTask = this.readControl(bidi.readable);

    this.send({
      type: 'Hello',
      protocol_version: PROTOCOL_VERSION,
      client_name: 'termland-web',
    });

    await Promise.race([controlTask, videoTask, transport.closed]);
    this.stopPing();
    this.writer = null;
  }

  private async readControl(readable: ReadableStream<Uint8Array>): Promise<void> {
    const reader = readable.getReader();
    const decoder = new FrameDecoder();
    for (;;) {
      const { value, done } = await reader.read();
      if (done) {
        if (!this.closed) this.emit({ type: 'status', message: 'control stream closed' });
        return;
      }
      for (const frame of decoder.push(value)) {
        const msg = decodeMessage(frame.payload);
        await this.onControl(msg);
      }
    }
  }

  private async onControl(msg: Message): Promise<void> {
    switch (msg.type) {
      case 'HelloAck':
        this.emit({
          type: 'hello',
          session_id: msg.session_id,
          auth_required: msg.auth_required,
          server_name: msg.server_name,
        });
        if (!msg.auth_required) {
          if (this.attachNext && this.sessionId) {
            this.attachSession(this.sessionId, 0, 0);
            this.attachNext = false;
          } else {
            this.listSessions();
          }
        }
        this.startPing();
        break;
      case 'AuthRequest':
        this.send({
          type: 'AuthResponse',
          username: this.opts.username ?? '',
          credential: this.opts.password ?? '',
        });
        break;
      case 'AuthResult':
        if (!msg.success) {
          this.reconnectEnabled = false;
          this.emit({ type: 'error', error: `auth failed: ${msg.message}` });
          return;
        }
        if (this.attachNext && this.sessionId) {
          this.attachSession(this.sessionId, 0, 0);
          this.attachNext = false;
        } else {
          this.listSessions();
        }
        break;
      case 'SessionListResult':
        this.emit({ type: 'session-list', sessions: msg.sessions });
        break;
      case 'SessionReady':
        this.sessionId = msg.session_id || this.sessionId;
        this.attachNext = true;
        this.emit({
          type: 'session-ready',
          width: msg.width,
          height: msg.height,
          codec: msg.codec,
          session_id: msg.session_id,
        });
        break;
      case 'SessionEnd':
        this.reconnectEnabled = false;
        this.emit({ type: 'session-end', reason: msg.reason });
        this.closed = true;
        this.transport?.close();
        break;
      case 'Pong':
        break;
      case 'Unknown':
        break;
      default:
        break;
    }
  }

  private async readVideo(transport: WebTransport): Promise<void> {
    const incoming = transport.incomingUnidirectionalStreams.getReader();
    for (;;) {
      const { value: stream, done } = await incoming.read();
      if (done) return;
      for await (const frame of readQ2Frames(stream)) {
        this.emit({ type: 'video', frame });
      }
    }
  }

  private startPing(): void {
    this.stopPing();
    this.pingTimer = setInterval(() => {
      this.send({ type: 'Ping', timestamp_us: Date.now() * 1000 });
    }, 5000);
  }

  private stopPing(): void {
    if (this.pingTimer) {
      clearInterval(this.pingTimer);
      this.pingTimer = null;
    }
  }
}

function openTransport(url: string, certHashHex?: string): WebTransport {
  if (!certHashHex) return new WebTransport(url);
  const bytes = decodeHex(certHashHex);
  if (!bytes || bytes.byteLength !== 32) {
    throw new Error('certificate hash must be 32 bytes of hex');
  }
  return new WebTransport(url, {
    serverCertificateHashes: [{ algorithm: 'sha-256', value: bytes as BufferSource }],
  });
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

export { decodeHex };
