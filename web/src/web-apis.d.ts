//! Browser APIs used by the client. TypeScript's shipped DOM lib still
//! treats some of these as experimental, so the shapes we actually call
//! are declared here rather than pulling in a second @types package.

interface WebTransportHash {
  algorithm: string;
  value: BufferSource;
}

interface WebTransportOptions {
  serverCertificateHashes?: WebTransportHash[];
}

interface WebTransportBidirectionalStream {
  readonly readable: ReadableStream<Uint8Array>;
  readonly writable: WritableStream<Uint8Array>;
}

declare class WebTransport {
  constructor(url: string, options?: WebTransportOptions);
  readonly ready: Promise<void>;
  readonly closed: Promise<unknown>;
  readonly incomingUnidirectionalStreams: ReadableStream<ReadableStream<Uint8Array>>;
  createBidirectionalStream(): Promise<WebTransportBidirectionalStream>;
  close(): void;
}

interface VideoDecoderConfig {
  codec: string;
  codedWidth?: number;
  codedHeight?: number;
  optimizeForLatency?: boolean;
  description?: BufferSource;
}

interface VideoDecoderInit {
  output: (frame: VideoFrame) => void;
  error: (error: DOMException) => void;
}

interface VideoDecoderSupport {
  supported?: boolean;
  config?: VideoDecoderConfig;
}

declare class VideoDecoder {
  constructor(init: VideoDecoderInit);
  readonly decodeQueueSize: number;
  static isConfigSupported(config: VideoDecoderConfig): Promise<VideoDecoderSupport>;
  configure(config: VideoDecoderConfig): void;
  decode(chunk: EncodedVideoChunk): void;
  close(): void;
}

interface EncodedVideoChunkInit {
  type: 'key' | 'delta';
  timestamp: number;
  duration?: number;
  data: BufferSource;
}

declare class EncodedVideoChunk {
  constructor(init: EncodedVideoChunkInit);
  readonly type: 'key' | 'delta';
  readonly timestamp: number;
}

interface VideoFrame {
  readonly displayWidth: number;
  readonly displayHeight: number;
  close(): void;
}
