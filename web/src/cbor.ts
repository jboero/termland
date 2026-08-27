//! CBOR encode/decode matching ciborium + serde's externally-tagged enums.
//!
//! Structs are name-keyed maps, unit enums are text, `Option::None` is null,
//! `serde_bytes` is a CBOR byte string. Integer additional-info is the
//! shortest form, same as ciborium.

export type CborValue =
  | { t: 'uint'; v: bigint }
  | { t: 'nint'; v: bigint }
  | { t: 'bytes'; v: Uint8Array }
  | { t: 'text'; v: string }
  | { t: 'array'; v: CborValue[] }
  | { t: 'map'; v: [CborValue, CborValue][] }
  | { t: 'bool'; v: boolean }
  | { t: 'null' }
  | { t: 'float'; v: number };

export class CborError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'CborError';
  }
}

export function encode(value: CborValue): Uint8Array {
  const parts: number[] = [];
  write(value, parts);
  return Uint8Array.from(parts);
}

function write(value: CborValue, out: number[]): void {
  switch (value.t) {
    case 'uint':
      writeUint(0, value.v, out);
      break;
    case 'nint':
      writeUint(1, value.v, out);
      break;
    case 'bytes':
      writeUint(2, BigInt(value.v.byteLength), out);
      for (const b of value.v) out.push(b);
      break;
    case 'text': {
      const bytes = new TextEncoder().encode(value.v);
      writeUint(3, BigInt(bytes.byteLength), out);
      for (const b of bytes) out.push(b);
      break;
    }
    case 'array':
      writeUint(4, BigInt(value.v.length), out);
      for (const item of value.v) write(item, out);
      break;
    case 'map':
      writeUint(5, BigInt(value.v.length), out);
      for (const [k, v] of value.v) {
        write(k, out);
        write(v, out);
      }
      break;
    case 'bool':
      out.push(value.v ? 0xf5 : 0xf4);
      break;
    case 'null':
      out.push(0xf6);
      break;
    case 'float': {
      out.push(0xfb);
      const buf = new ArrayBuffer(8);
      new DataView(buf).setFloat64(0, value.v, false);
      for (const b of new Uint8Array(buf)) out.push(b);
      break;
    }
  }
}

function writeUint(major: number, n: bigint, out: number[]): void {
  const hi = major << 5;
  if (n < 24n) out.push(hi | Number(n));
  else if (n < 256n) {
    out.push(hi | 24);
    out.push(Number(n));
  } else if (n < 65536n) {
    out.push(hi | 25);
    out.push(Number((n >> 8n) & 0xffn), Number(n & 0xffn));
  } else if (n < 4294967296n) {
    out.push(hi | 26);
    out.push(
      Number((n >> 24n) & 0xffn),
      Number((n >> 16n) & 0xffn),
      Number((n >> 8n) & 0xffn),
      Number(n & 0xffn),
    );
  } else {
    out.push(hi | 27);
    for (let s = 56n; s >= 0n; s -= 8n) out.push(Number((n >> s) & 0xffn));
  }
}

export function decode(data: Uint8Array): CborValue {
  const { value, offset } = read(data, 0);
  if (offset !== data.byteLength) {
    throw new CborError(`trailing ${data.byteLength - offset} bytes after CBOR value`);
  }
  return value;
}

/** IEEE-754 binary16 → number. ciborium sometimes emits f16 for f64 fields. */
function decodeF16(u16: number): number {
  const sign = (u16 >> 15) & 1;
  const exp = (u16 >> 10) & 0x1f;
  const frac = u16 & 0x3ff;
  let mag: number;
  if (exp === 0) {
    mag = frac === 0 ? 0 : Math.pow(2, -14) * (frac / 1024);
  } else if (exp === 31) {
    mag = frac ? Number.NaN : Number.POSITIVE_INFINITY;
  } else {
    mag = Math.pow(2, exp - 15) * (1 + frac / 1024);
  }
  return sign ? -mag : mag;
}

function read(data: Uint8Array, offset: number): { value: CborValue; offset: number } {
  if (offset >= data.byteLength) throw new CborError('unexpected end of CBOR');
  const ib = data[offset];
  const major = ib >> 5;
  const ai = ib & 0x1f;
  offset += 1;

  if (major === 7) {
    if (ai === 20) return { value: { t: 'bool', v: false }, offset };
    if (ai === 21) return { value: { t: 'bool', v: true }, offset };
    if (ai === 22) return { value: { t: 'null' }, offset };
    if (ai === 25) {
      const view = new DataView(data.buffer, data.byteOffset + offset, 2);
      offset += 2;
      return { value: { t: 'float', v: decodeF16(view.getUint16(0, false)) }, offset };
    }
    if (ai === 26) {
      const view = new DataView(data.buffer, data.byteOffset + offset, 4);
      offset += 4;
      return { value: { t: 'float', v: view.getFloat32(0, false) }, offset };
    }
    if (ai === 27) {
      const view = new DataView(data.buffer, data.byteOffset + offset, 8);
      offset += 8;
      return { value: { t: 'float', v: view.getFloat64(0, false) }, offset };
    }
    throw new CborError(`unsupported simple/float additional info ${ai}`);
  }

  const { n, offset: after } = readAdditional(data, offset, ai);
  offset = after;

  switch (major) {
    case 0:
      return { value: { t: 'uint', v: n }, offset };
    case 1:
      return { value: { t: 'nint', v: n }, offset };
    case 2: {
      const end = offset + Number(n);
      if (end > data.byteLength) throw new CborError('truncated byte string');
      return { value: { t: 'bytes', v: data.slice(offset, end) }, offset: end };
    }
    case 3: {
      const end = offset + Number(n);
      if (end > data.byteLength) throw new CborError('truncated text');
      return {
        value: { t: 'text', v: new TextDecoder().decode(data.slice(offset, end)) },
        offset: end,
      };
    }
    case 4: {
      const items: CborValue[] = [];
      for (let i = 0; i < n; i++) {
        const r = read(data, offset);
        items.push(r.value);
        offset = r.offset;
      }
      return { value: { t: 'array', v: items }, offset };
    }
    case 5: {
      const items: [CborValue, CborValue][] = [];
      for (let i = 0; i < n; i++) {
        const k = read(data, offset);
        const v = read(data, k.offset);
        items.push([k.value, v.value]);
        offset = v.offset;
      }
      return { value: { t: 'map', v: items }, offset };
    }
    default:
      throw new CborError(`unsupported major type ${major}`);
  }
}

function readAdditional(
  data: Uint8Array,
  offset: number,
  ai: number,
): { n: bigint; offset: number } {
  if (ai < 24) return { n: BigInt(ai), offset };
  if (ai === 24) {
    if (offset >= data.byteLength) throw new CborError('truncated u8');
    return { n: BigInt(data[offset]), offset: offset + 1 };
  }
  if (ai === 25) {
    if (offset + 2 > data.byteLength) throw new CborError('truncated u16');
    return { n: (BigInt(data[offset]) << 8n) | BigInt(data[offset + 1]), offset: offset + 2 };
  }
  if (ai === 26) {
    if (offset + 4 > data.byteLength) throw new CborError('truncated u32');
    let n = 0n;
    for (let i = 0; i < 4; i++) n = (n << 8n) | BigInt(data[offset + i]);
    return { n, offset: offset + 4 };
  }
  if (ai === 27) {
    if (offset + 8 > data.byteLength) throw new CborError('truncated u64');
    let n = 0n;
    for (let i = 0; i < 8; i++) n = (n << 8n) | BigInt(data[offset + i]);
    return { n, offset: offset + 8 };
  }
  throw new CborError(`indefinite / reserved additional info ${ai}`);
}

export function uint(n: number | bigint): CborValue {
  return { t: 'uint', v: BigInt(n) };
}
export function text(s: string): CborValue {
  return { t: 'text', v: s };
}
export function bytes(v: Uint8Array): CborValue {
  return { t: 'bytes', v };
}
export function bool(v: boolean): CborValue {
  return { t: 'bool', v };
}
export function nil(): CborValue {
  return { t: 'null' };
}
export function float(v: number): CborValue {
  return { t: 'float', v };
}
export function array(v: CborValue[]): CborValue {
  return { t: 'array', v };
}
export function map(entries: [string, CborValue][]): CborValue {
  return { t: 'map', v: entries.map(([k, val]) => [text(k), val]) };
}
export function tagged(name: string, body: CborValue): CborValue {
  return map([[name, body]]);
}

export function asMap(v: CborValue): Map<string, CborValue> {
  if (v.t !== 'map') throw new CborError(`expected map, got ${v.t}`);
  const out = new Map<string, CborValue>();
  for (const [k, val] of v.v) {
    if (k.t !== 'text') throw new CborError('map key is not text');
    out.set(k.v, val);
  }
  return out;
}

export function asText(v: CborValue): string {
  if (v.t !== 'text') throw new CborError(`expected text, got ${v.t}`);
  return v.v;
}

export function asUint(v: CborValue): number {
  if (v.t === 'uint') return Number(v.v);
  if (v.t === 'nint') return -1 - Number(v.v);
  if (v.t === 'float') return v.v;
  throw new CborError(`expected number, got ${v.t}`);
}

export function asBool(v: CborValue): boolean {
  if (v.t !== 'bool') throw new CborError(`expected bool, got ${v.t}`);
  return v.v;
}

export function asFloat(v: CborValue): number {
  if (v.t === 'float') return v.v;
  if (v.t === 'uint') return Number(v.v);
  if (v.t === 'nint') return -1 - Number(v.v);
  throw new CborError(`expected float, got ${v.t}`);
}

export function asBytes(v: CborValue): Uint8Array {
  if (v.t === 'bytes') return v.v;
  if (v.t === 'array') return Uint8Array.from(v.v.map(asUint));
  throw new CborError(`expected bytes, got ${v.t}`);
}

export function asArray(v: CborValue): CborValue[] {
  if (v.t !== 'array') throw new CborError(`expected array, got ${v.t}`);
  return v.v;
}

export function opt<T>(v: CborValue | undefined, f: (x: CborValue) => T): T | null {
  if (v === undefined || v.t === 'null') return null;
  return f(v);
}

export function field(m: Map<string, CborValue>, name: string): CborValue {
  const v = m.get(name);
  if (v === undefined) throw new CborError(`missing field ${name}`);
  return v;
}

export function fieldOr(m: Map<string, CborValue>, name: string, fallback: CborValue): CborValue {
  return m.get(name) ?? fallback;
}

export function singleKey(v: CborValue): [string, CborValue] {
  if (v.t !== 'map' || v.v.length !== 1) {
    throw new CborError('expected a single-key (externally tagged) map');
  }
  const [k, body] = v.v[0];
  if (k.t !== 'text') throw new CborError('enum tag is not text');
  return [k.v, body];
}
