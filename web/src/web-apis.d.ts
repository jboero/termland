//! Browser APIs the shipped DOM lib still treats as experimental.

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
