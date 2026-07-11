import native, { type NativeStrenorInstance } from './native.js';

/**
 * Value format (stable, versionable). Byte 0 of every stored value is a tag.
 * The native Rust core never reads these — decoding lives entirely here.
 */
export const TAG = {
  RAW: 0x00, // raw bytes -> Buffer
  STRING: 0x01, // UTF-8 -> string
  JSON: 0x02, // JSON bytes -> any (default object codec)
  // Reserved (defined now so the format can grow without breaking compat):
  INT64: 0x03,
  FLOAT64: 0x04,
  BOOL: 0x05,
  NULL: 0x06,
  // 0x20..0xFE: pluggable codecs (msgpack, cbor, bson, ...)
} as const;

export interface Codec {
  /** Tag byte written as byte 0 of the stored value. Custom codecs: 0x20..0xFE. */
  tag: number;
  encode(value: unknown): Buffer;
  decode(bytes: Buffer): unknown;
}

export interface StrenorOptions {
  /** Default codec for objects passed to `set`. Defaults to JSON. */
  codec?: Codec;
  /** If set (ms), purge expired keys on a background (unref'd) timer. */
  sweepInterval?: number;
}

export interface WriteOptions {
  /** Time-to-live in milliseconds. */
  ttl?: number;
  /** Override the object codec for this write. */
  codec?: Codec;
}

/** Built-in default object codec. */
export const jsonCodec: Codec = {
  tag: TAG.JSON,
  encode: (v) => Buffer.from(JSON.stringify(v), 'utf8'),
  decode: (b) => JSON.parse(b.toString('utf8')) as unknown,
};

function frame(tag: number, payload: Buffer): Buffer {
  const out = Buffer.allocUnsafe(payload.length + 1);
  out[0] = tag;
  payload.copy(out, 1);
  return out;
}

export class Strenor {
  private readonly db: NativeStrenorInstance;
  private readonly codec: Codec;
  private readonly byTag: Map<number, Codec>;
  private timer: ReturnType<typeof setInterval> | null = null;

  constructor(opts: StrenorOptions = {}) {
    this.db = new native.Strenor();
    this.codec = opts.codec ?? jsonCodec;

    // Codec registry keyed by tag, so decode() can dispatch on the stored tag.
    this.byTag = new Map<number, Codec>();
    this.byTag.set(jsonCodec.tag, jsonCodec);
    this.byTag.set(this.codec.tag, this.codec);

    if (opts.sweepInterval !== undefined && opts.sweepInterval > 0) {
      this.timer = setInterval(() => this.db.sweep(), opts.sweepInterval);
      if (typeof this.timer.unref === 'function') this.timer.unref();
    }
  }

  /** Register an extra codec (e.g. msgpack) so its tag decodes automatically. */
  registerCodec(codec: Codec): this {
    if (codec.tag < 0x20 || codec.tag > 0xfe) {
      throw new RangeError('custom codec tag must be in 0x20..0xFE');
    }
    this.byTag.set(codec.tag, codec);
    return this;
  }

  private encode(value: unknown, opts?: WriteOptions): Buffer {
    if (opts?.codec) return frame(opts.codec.tag, opts.codec.encode(value));
    if (Buffer.isBuffer(value)) return frame(TAG.RAW, value);
    if (typeof value === 'string') return frame(TAG.STRING, Buffer.from(value, 'utf8'));
    // Objects, arrays, numbers, booleans, null -> default object codec (JSON).
    return frame(this.codec.tag, this.codec.encode(value));
  }

  private decode(buf: Buffer): unknown {
    const tag = buf[0];
    const payload = buf.subarray(1);
    switch (tag) {
      case TAG.RAW:
        return Buffer.from(payload); // detached copy
      case TAG.STRING:
        return payload.toString('utf8');
      default: {
        const codec = this.byTag.get(tag);
        if (codec) return codec.decode(Buffer.from(payload));
        throw new Error(`strenor: unknown value tag 0x${tag.toString(16)}`);
      }
    }
  }

  // ---- smart API (dispatches by runtime type) ----

  set(key: string, value: unknown, opts?: WriteOptions): this {
    this.db.set(key, this.encode(value, opts), opts?.ttl);
    return this;
  }

  get<T = unknown>(key: string): T | null {
    const b = this.db.get(key);
    return b == null ? null : (this.decode(b) as T);
  }

  // ---- explicit typed helpers (force a tag / assert on read) ----

  setString(key: string, value: string, opts?: WriteOptions): this {
    this.db.set(key, frame(TAG.STRING, Buffer.from(value, 'utf8')), opts?.ttl);
    return this;
  }

  getString(key: string): string | null {
    const v = this.get(key);
    if (v === null) return null;
    if (typeof v !== 'string') throw new TypeError(`strenor: "${key}" is not a string`);
    return v;
  }

  setBuffer(key: string, value: Buffer, opts?: WriteOptions): this {
    if (!Buffer.isBuffer(value)) throw new TypeError('setBuffer expects a Buffer');
    this.db.set(key, frame(TAG.RAW, value), opts?.ttl);
    return this;
  }

  getBuffer(key: string): Buffer | null {
    const v = this.get(key);
    if (v === null) return null;
    if (!Buffer.isBuffer(v)) throw new TypeError(`strenor: "${key}" is not a Buffer`);
    return v;
  }

  setJSON(key: string, value: unknown, opts?: WriteOptions): this {
    this.db.set(key, frame(this.codec.tag, this.codec.encode(value)), opts?.ttl);
    return this;
  }

  getJSON<T = unknown>(key: string): T | null {
    return this.get<T>(key);
  }

  // ---- key management ----

  del(key: string): boolean {
    return this.db.del(key);
  }
  exists(key: string): boolean {
    return this.db.exists(key);
  }
  /** Set/replace TTL (ms) of an existing key. */
  expire(key: string, ttlMs: number): boolean {
    return this.db.expire(key, ttlMs);
  }
  /** Remove TTL (make persistent). */
  persist(key: string): boolean {
    return this.db.persist(key);
  }
  /** Remaining TTL in ms; -1 = no expiry, -2 = missing. */
  ttl(key: string): number {
    return this.db.ttl(key);
  }
  keys(): string[] {
    return this.db.keys();
  }
  size(): number {
    return this.db.size();
  }
  clear(): void {
    this.db.clear();
  }
  /** Eagerly purge expired keys; returns count removed. */
  sweep(): number {
    return this.db.sweep();
  }

  // ---- counters (atomic within Node's single-threaded event loop) ----

  /** Atomically add to an integer counter by `by` (default 1); missing key = 0. */
  incr(key: string, by = 1): number {
    return this.addTo(key, by);
  }

  /** Atomically subtract from an integer counter by `by` (default 1); missing = 0. */
  decr(key: string, by = 1): number {
    return this.addTo(key, -by);
  }

  // get()+set() with nothing awaited between them: no other JS can interleave in
  // a single-threaded process, so the read-modify-write is effectively atomic.
  private addTo(key: string, delta: number): number {
    const cur = this.get(key); // throws WRONGTYPE if the key holds a list
    let n: number;
    if (cur === null) n = 0;
    else if (typeof cur === 'number') n = cur;
    else throw new TypeError(`strenor: "${key}" holds a non-numeric value`);

    if (!Number.isInteger(n) || !Number.isInteger(delta)) {
      throw new TypeError('strenor: counters must be integers');
    }
    const next = n + delta;
    if (!Number.isSafeInteger(next)) {
      throw new RangeError('strenor: counter would overflow Number.MAX_SAFE_INTEGER');
    }
    this.set(key, next);
    return next;
  }

  // ---- lists (queues & stacks) ----

  /** Append `value` to the tail of the list at `key`. Returns the new length. */
  enqueue(key: string, value: unknown, opts?: WriteOptions): number {
    return this.db.pushBack(key, this.encode(value, opts));
  }

  /** Remove and return the head of the list (FIFO). `null` if empty/missing. */
  dequeue<T = unknown>(key: string): T | null {
    const b = this.db.popFront(key);
    return b == null ? null : (this.decode(b) as T);
  }

  /** Append `value` to the tail of the list. Returns the new length. */
  push(key: string, value: unknown, opts?: WriteOptions): number {
    return this.db.pushBack(key, this.encode(value, opts));
  }

  /** Remove and return the tail of the list (LIFO). `null` if empty/missing. */
  pop<T = unknown>(key: string): T | null {
    const b = this.db.popBack(key);
    return b == null ? null : (this.decode(b) as T);
  }

  /** List length (0 if missing). Throws `WRONGTYPE` if `key` holds a value. */
  llen(key: string): number {
    return this.db.llen(key);
  }

  /** Elements in `[start, stop]` (Redis-style; negative indices from the end). */
  lrange<T = unknown>(key: string, start: number, stop: number): T[] {
    return this.db.lrange(key, start, stop).map((b) => this.decode(b) as T);
  }

  // Redis-style directional aliases.
  /** Prepend to the head. Returns the new length. */
  lpush(key: string, value: unknown, opts?: WriteOptions): number {
    return this.db.pushFront(key, this.encode(value, opts));
  }
  /** Append to the tail. Returns the new length. */
  rpush(key: string, value: unknown, opts?: WriteOptions): number {
    return this.db.pushBack(key, this.encode(value, opts));
  }
  /** Remove and return the head. `null` if empty/missing. */
  lpop<T = unknown>(key: string): T | null {
    const b = this.db.popFront(key);
    return b == null ? null : (this.decode(b) as T);
  }
  /** Remove and return the tail. `null` if empty/missing. */
  rpop<T = unknown>(key: string): T | null {
    const b = this.db.popBack(key);
    return b == null ? null : (this.decode(b) as T);
  }

  // ---- persistence ----

  /** Dump full state to a self-describing binary snapshot. */
  dump(path: string): this {
    this.db.dump(path);
    return this;
  }
  /** Load a snapshot, replacing current state. */
  load(path: string): this {
    this.db.load(path);
    return this;
  }

  /** Stop the background sweeper (if any). Call on shutdown. */
  close(): void {
    if (this.timer) {
      clearInterval(this.timer);
      this.timer = null;
    }
  }
}

export default Strenor;
