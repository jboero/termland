import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { initProtocol } from '../src/protocol.ts';

const wasm = readFileSync(join(dirname(fileURLToPath(import.meta.url)), '../pkg/termland_web_bg.wasm'));
await initProtocol(wasm);
