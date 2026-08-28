//! Probe WebCodecs for the codecs this browser can actually decode.
//!
//! The strings are concrete configurations, not codec families: `av01` alone
//! is not enough for `VideoDecoder.isConfigSupported`. Preference order
//! matches `VideoCodec::all_preferred` so the server still picks AV1 first
//! when both sides support it.

import type { VideoCodec } from './messages.js';

export interface CodecConfig {
  codec: VideoCodec;
  webcodecs: string;
}

/** WebCodecs `VideoDecoderConfig.codec` strings to probe, in the same
 * preference order as `VideoCodec::all_preferred`. */
const CANDIDATES: { codec: VideoCodec; strings: string[] }[] = [
  { codec: 'Av1', strings: ['av01.0.04M.08', 'av01.0.08M.08', 'av01.0.13M.08'] },
  { codec: 'Vp9', strings: ['vp09.00.10.08', 'vp09.00.40.08', 'vp09.00.51.08'] },
  { codec: 'Vp8', strings: ['vp8'] },
  { codec: 'H264', strings: ['avc1.42E01E', 'avc1.4D401F', 'avc1.64001F'] },
  { codec: 'H265', strings: ['hvc1.1.6.L93.B0', 'hev1.1.6.L93.B0'] },
];

export async function probeSupportedCodecs(): Promise<CodecConfig[]> {
  if (typeof VideoDecoder === 'undefined') {
    // Node / tests: advertise the open codecs. The live client always runs
    // this in a browser that has WebCodecs.
    return [
      { codec: 'Av1', webcodecs: 'av01.0.04M.08' },
      { codec: 'Vp9', webcodecs: 'vp09.00.10.08' },
    ];
  }
  const supported: CodecConfig[] = [];
  for (const { codec, strings } of CANDIDATES) {
    for (const webcodecs of strings) {
      try {
        const result = await VideoDecoder.isConfigSupported({
          codec: webcodecs,
          codedWidth: 640,
          codedHeight: 360,
        });
        if (result.supported) {
          supported.push({ codec, webcodecs });
          break;
        }
      } catch {
        // Unknown codec string — try the next candidate.
      }
    }
  }
  return supported;
}

export function webCodecsString(codec: VideoCodec, probed: CodecConfig[]): string {
  const hit = probed.find((c) => c.codec === codec);
  if (hit) return hit.webcodecs;
  const fallback = CANDIDATES.find((c) => c.codec === codec);
  return fallback?.strings[0] ?? 'av01.0.04M.08';
}
