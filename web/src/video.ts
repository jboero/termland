//! Decode Q2 frames with WebCodecs and paint them to a canvas.
//!
//! Bounded queues: at most a handful of encoded chunks wait to decode, and
//! only the latest decoded VideoFrame is kept for render. Stale inter frames
//! are dropped rather than played late.
//!
//! Chromium freezes a background tab after a few minutes (Page Lifecycle).
//! A frozen tab drops the pending `requestAnimationFrame` without running it,
//! which latches `drawing` and stops painting even after the user comes back.
//! Hardware `VideoDecoder` can also enter the closed/error state across that
//! freeze. `resume()` unlatches rAF and, after a long hide, rebuilds the
//! decoder so the next keyframe paints.

import type { Q2Frame } from './q2.js';
import { webCodecsString, type CodecConfig } from './codecs.js';
import type { VideoCodec } from './messages.js';

const MAX_ENCODED = 3;

/** Rebuild the decoder after this long in the background (keyframe wait ~1s). */
export const RECONFIGURE_AFTER_HIDDEN_MS = 5_000;

export function shouldReconfigureDecoder(
  hiddenMs: number,
  decoderState: string | null,
): boolean {
  return decoderState !== 'configured' || hiddenMs >= RECONFIGURE_AFTER_HIDDEN_MS;
}

export class VideoPipeline {
  private decoder: VideoDecoder | null = null;
  private pending: VideoFrame | null = null;
  private encoded: EncodedVideoChunk[] = [];
  private waitingKeyframe = true;
  private drawing = false;
  private closed = false;
  private hiddenAt: number | null = null;
  private lastCodec: VideoCodec | null = null;
  private lastWidth = 0;
  private lastHeight = 0;
  private needsReconfigure = false;

  constructor(
    private readonly canvas: HTMLCanvasElement,
    private readonly probed: CodecConfig[],
  ) {
    if (typeof document !== 'undefined') {
      document.addEventListener('visibilitychange', this.onVisibility);
    }
  }

  configure(codec: VideoCodec, width: number, height: number): void {
    this.lastCodec = codec;
    this.lastWidth = width;
    this.lastHeight = height;
    this.closeDecoder();
    this.waitingKeyframe = true;
    const string = webCodecsString(codec, this.probed);
    this.decoder = new VideoDecoder({
      output: (frame) => this.onFrame(frame),
      error: (e) => {
        console.error('VideoDecoder:', e);
        this.waitingKeyframe = true;
        this.needsReconfigure = true;
      },
    });
    this.decoder.configure({
      codec: string,
      codedWidth: width,
      codedHeight: height,
      optimizeForLatency: true,
    });
  }

  push(frame: Q2Frame): void {
    if (this.closed) return;
    if (this.needsReconfigure) {
      if (!frame.keyframe) return;
      this.reconfigure();
      this.needsReconfigure = false;
    }
    if (!this.decoder || this.decoder.state !== 'configured') return;
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

  /**
   * Called when the tab is visible again. Always unlatches a dropped rAF;
   * rebuilds the decoder after a long hide or if WebCodecs already died.
   */
  resume(): void {
    if (this.closed) return;
    const hiddenMs = this.hiddenAt == null ? 0 : Date.now() - this.hiddenAt;
    this.hiddenAt = null;
    this.drawing = false;
    const state = this.decoder?.state ?? null;
    if (shouldReconfigureDecoder(hiddenMs, state)) {
      this.reconfigure();
      return;
    }
    if (this.pending) this.scheduleDraw();
  }

  close(): void {
    this.closed = true;
    if (typeof document !== 'undefined') {
      document.removeEventListener('visibilitychange', this.onVisibility);
    }
    this.closeDecoder();
    this.pending?.close();
    this.pending = null;
  }

  private onVisibility = (): void => {
    if (typeof document === 'undefined') return;
    if (document.hidden) {
      this.hiddenAt = Date.now();
      return;
    }
    this.resume();
  };

  private reconfigure(): void {
    if (this.closed || !this.lastCodec) return;
    this.pending?.close();
    this.pending = null;
    this.encoded = [];
    this.configure(this.lastCodec, this.lastWidth, this.lastHeight);
  }

  private drain(): void {
    if (!this.decoder || this.decoder.state !== 'configured') return;
    if (this.decoder.decodeQueueSize > 2) return;
    const chunk = this.encoded.shift();
    if (!chunk) return;
    try {
      this.decoder.decode(chunk);
    } catch (e) {
      console.error('decode failed', e);
      this.waitingKeyframe = true;
      this.needsReconfigure = true;
    }
  }

  private onFrame(frame: VideoFrame): void {
    this.pending?.close();
    this.pending = frame;
    this.scheduleDraw();
    this.drain();
  }

  private scheduleDraw(): void {
    if (this.drawing || this.closed) return;
    this.drawing = true;
    requestAnimationFrame(() => this.draw());
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
