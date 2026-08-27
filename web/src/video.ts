//! Decode Q2 frames with WebCodecs and paint them to a canvas.
//!
//! Bounded queues: at most a handful of encoded chunks wait to decode, and
//! only the latest decoded VideoFrame is kept for render. Stale inter frames
//! are dropped rather than played late.

import type { Q2Frame } from './q2.js';
import { webCodecsString, type CodecConfig } from './codecs.js';
import type { VideoCodec } from './messages.js';

const MAX_ENCODED = 3;

export class VideoPipeline {
  private decoder: VideoDecoder | null = null;
  private pending: VideoFrame | null = null;
  private encoded: EncodedVideoChunk[] = [];
  private waitingKeyframe = true;
  private drawing = false;
  private closed = false;

  constructor(
    private readonly canvas: HTMLCanvasElement,
    private readonly probed: CodecConfig[],
  ) {}

  configure(codec: VideoCodec, width: number, height: number): void {
    this.closeDecoder();
    this.waitingKeyframe = true;
    const string = webCodecsString(codec, this.probed);
    this.decoder = new VideoDecoder({
      output: (frame) => this.onFrame(frame),
      error: (e) => console.error('VideoDecoder:', e),
    });
    this.decoder.configure({
      codec: string,
      codedWidth: width,
      codedHeight: height,
      optimizeForLatency: true,
    });
  }

  push(frame: Q2Frame): void {
    if (this.closed || !this.decoder) return;
    if (this.waitingKeyframe && !frame.keyframe) return;
    if (frame.keyframe) this.waitingKeyframe = false;

    const chunk = new EncodedVideoChunk({
      type: frame.keyframe ? 'key' : 'delta',
      timestamp: frame.timestamp_us,
      data: frame.data,
    });

    this.encoded.push(chunk);
    while (this.encoded.length > MAX_ENCODED) {
      const dropped = this.encoded.shift();
      if (dropped?.type === 'key' && this.encoded.length > 0) {
        // Prefer to keep a keyframe; drop a later delta instead.
        this.encoded.unshift(dropped);
        this.encoded.pop();
        break;
      }
    }
    this.drain();
  }

  close(): void {
    this.closed = true;
    this.closeDecoder();
    this.pending?.close();
    this.pending = null;
  }

  private drain(): void {
    if (!this.decoder || this.decoder.decodeQueueSize > 2) return;
    const chunk = this.encoded.shift();
    if (!chunk) return;
    try {
      this.decoder.decode(chunk);
    } catch (e) {
      console.error('decode failed', e);
      this.waitingKeyframe = true;
    }
  }

  private onFrame(frame: VideoFrame): void {
    this.pending?.close();
    this.pending = frame;
    if (!this.drawing) {
      this.drawing = true;
      requestAnimationFrame(() => this.draw());
    }
    this.drain();
  }

  private draw(): void {
    this.drawing = false;
    const frame = this.pending;
    if (!frame || this.closed) return;
    const ctx = this.canvas.getContext('2d');
    if (!ctx) {
      frame.close();
      this.pending = null;
      return;
    }
    if (this.canvas.width !== frame.displayWidth || this.canvas.height !== frame.displayHeight) {
      this.canvas.width = frame.displayWidth;
      this.canvas.height = frame.displayHeight;
    }
    ctx.drawImage(frame, 0, 0);
    frame.close();
    if (this.pending === frame) this.pending = null;
  }

  private closeDecoder(): void {
    try {
      this.decoder?.close();
    } catch {
      // already closed
    }
    this.decoder = null;
    this.encoded = [];
  }
}
