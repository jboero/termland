//! Public API of the Termland TypeScript client.
//!
//! This barrel is what a React/Vue/Svelte (or any) frontend should import:
//! `import { TermlandClient, VideoPipeline, InputCapture } from './dist/index.js'`
//! (or the package name after `npm run build`).
//! The sample page chrome is `web/app.js` and is not re-exported here.

export { TermlandClient, backoffDelay } from './client.js';
export type { ClientEvent, ConnectOptions } from './client.js';

export { encodeMessage, decodeMessage, encodeWire } from './messages.js';
export type { Message, SessionInfo, VideoCodec } from './messages.js';

export { FrameDecoder, encodeFrame } from './frame.js';

export { VideoPipeline } from './video.js';
export { probeSupportedCodecs } from './codecs.js';
export type { CodecConfig } from './codecs.js';

export { InputCapture, codeToEvdev } from './input.js';

export {
  parseVideoHeader,
  encodeVideoHeader,
  readQ2Frames,
  VIDEO_HEADER_LEN,
  MAX_FRAME_BYTES,
} from './q2.js';
export type { Q2Frame } from './q2.js';
