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
  /**
   * Path to an append-only log. Every mutation is journalled and the log is
   * replayed on open, so state survives a restart or a crash. Without it, the
   * store is memory-only (you can still snapshot with `dump`/`load`).
   */
  aof?: string;
  /**
   * `fsync` every write, surviving an OS crash or power loss at a large cost in
   * throughput. Default `false`: writes reach the OS immediately (a *process*
   * crash loses nothing), but a power cut can lose the last writes.
   */
  fsync?: boolean;
}

/** A member of a sorted set together with its numeric score. */
export interface ScoredMember<T = unknown> {
  member: T;
  score: number;
}

/** What replaying the log on open found. `null` when there is no log. */
export interface Recovery {
  /** Records applied from the log. */
  applied: number;
  /** A torn tail was dropped — the previous process died mid-write. */
  truncated: boolean;
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
    this.db = new native.Strenor(opts.aof ?? null, opts.fsync ?? null);
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

  // ---- hashes (field maps) ----

  /** Set `field` in the hash at `key`. Returns true if the field was new. */
  hset(key: string, field: string, value: unknown, opts?: WriteOptions): boolean {
    return this.db.hset(key, field, this.encode(value, opts));
  }

  /** Get one field from the hash at `key`. `null` if key or field is missing. */
  hget<T = unknown>(key: string, field: string): T | null {
    const b = this.db.hget(key, field);
    return b == null ? null : (this.decode(b) as T);
  }

  /** Delete a field. Returns true if it existed. An emptied hash key is removed. */
  hdel(key: string, field: string): boolean {
    return this.db.hdel(key, field);
  }

  /** Whether `field` exists in the hash at `key`. */
  hexists(key: string, field: string): boolean {
    return this.db.hexists(key, field);
  }

  /** All field names of the hash at `key` (empty if missing). */
  hkeys(key: string): string[] {
    return this.db.hkeys(key);
  }

  /** Number of fields in the hash at `key` (0 if missing). */
  hlen(key: string): number {
    return this.db.hlen(key);
  }

  /** The whole hash at `key` as an object, with values decoded via codecs. */
  hgetall<T = Record<string, unknown>>(key: string): T {
    const flat = this.db.hgetall(key); // [field0, value0, field1, value1, …]
    const out: Record<string, unknown> = {};
    for (let i = 0; i < flat.length; i += 2) {
      out[flat[i].toString('utf8')] = this.decode(flat[i + 1]);
    }
    return out as T;
  }

  // ---- sets (unique members) ----

  /** Add `member` to the set at `key`. Returns true if it was new. */
  sadd(key: string, member: unknown, opts?: WriteOptions): boolean {
    return this.db.sadd(key, this.encode(member, opts));
  }

  /** Remove `member`. Returns true if it was present. An emptied set is removed. */
  srem(key: string, member: unknown, opts?: WriteOptions): boolean {
    return this.db.srem(key, this.encode(member, opts));
  }

  /** Whether `member` is in the set at `key`. */
  sismember(key: string, member: unknown, opts?: WriteOptions): boolean {
    return this.db.sismember(key, this.encode(member, opts));
  }

  /** All members of the set at `key`, decoded (order unspecified). */
  smembers<T = unknown>(key: string): T[] {
    return this.db.smembers(key).map((b) => this.decode(b) as T);
  }

  /** Number of members in the set at `key` (0 if missing). */
  scard(key: string): number {
    return this.db.scard(key);
  }

  // ---- sorted sets (rankings & leaderboards) ----

  /** Add/update `member` with `score`. Returns true if the member is new. */
  zadd(key: string, score: number, member: unknown, opts?: WriteOptions): boolean {
    return this.db.zadd(key, score, this.encode(member, opts));
  }

  /** Add `delta` to a member's score (creating it at `delta`). Returns the new score. */
  zincrby(key: string, delta: number, member: unknown, opts?: WriteOptions): number {
    return this.db.zincrby(key, delta, this.encode(member, opts));
  }

  /** Remove `member`. Returns true if it was present. An emptied key is removed. */
  zrem(key: string, member: unknown, opts?: WriteOptions): boolean {
    return this.db.zrem(key, this.encode(member, opts));
  }

  /** The score of `member`, or `null` if the member/key is missing. */
  zscore(key: string, member: unknown, opts?: WriteOptions): number | null {
    return this.db.zscore(key, this.encode(member, opts));
  }

  /** 0-based rank of `member` (low score first), or `null` if missing. */
  zrank(key: string, member: unknown, opts?: WriteOptions): number | null {
    return this.db.zrank(key, this.encode(member, opts));
  }

  /** Number of members in the sorted set at `key` (0 if missing). */
  zcard(key: string): number {
    return this.db.zcard(key);
  }

  /** Members in rank range `[start, stop]`, low score first (negative from end). */
  zrange<T = unknown>(key: string, start: number, stop: number): T[] {
    return this.db.zrange(key, start, stop).map((b) => this.decode(b) as T);
  }

  /** Same as `zrange`, but each element is `{ member, score }`. */
  zrangeWithScores<T = unknown>(key: string, start: number, stop: number): ScoredMember<T>[] {
    const flat = this.db.zrangeWithScores(key, start, stop); // [m0, s0, m1, s1, …]
    const out: ScoredMember<T>[] = [];
    for (let i = 0; i < flat.length; i += 2) {
      out.push({ member: this.decode(flat[i]) as T, score: Number(flat[i + 1].toString('utf8')) });
    }
    return out;
  }

  // ---- transactions & batches ----

  /**
   * Run `fn` as an all-or-nothing transaction.
   *
   * State is snapshotted up front and `fn` runs against the store as usual. If
   * it returns normally, every write is committed to the log as one atomic
   * batch. If `fn` throws, the state is rolled back to the snapshot and nothing
   * is written — the throw then propagates to the caller.
   *
   * ```ts
   * db.transaction(() => {
   *   db.set('balance', 90);
   *   db.hset('orders', id, order);
   *   if (somethingWrong) throw new Error('abort'); // rolls everything back
   * });
   * ```
   *
   * Transactions can't be nested. The rollback snapshot is O(state size) —
   * intended for thousands of keys, not millions. Synchronous only: don't
   * `await` inside `fn`, since the transaction spans the whole call.
   */
  transaction<T>(fn: () => T): T {
    this.db.txBegin();
    let result: T;
    try {
      result = fn();
    } catch (err) {
      this.db.txRollback();
      throw err;
    }
    this.db.txCommit();
    return result;
  }

  /**
   * Run `fn` and journal all its writes as a single batch on success.
   *
   * Like `transaction`, but without the rollback snapshot: faster for bulk
   * writes, and if `fn` throws, whatever ran before the throw stays staged and
   * is still flushed. Use it when you want the write-batching speed-up but don't
   * need all-or-nothing semantics.
   */
  batch<T>(fn: () => T): T {
    this.db.txBegin();
    try {
      return fn();
    } finally {
      // Commit whatever was staged, even on throw — no rollback.
      this.db.txCommit();
    }
  }

  /** Whether a transaction/batch is currently open. */
  get inTransaction(): boolean {
    return this.db.inTransaction();
  }

  // ---- durability (append-only log) ----

  /**
   * What replaying the log found on open, or `null` without a log.
   * `truncated: true` means the previous process crashed mid-write and the torn
   * tail was dropped — the store is consistent, but those last writes are gone.
   */
  get recovery(): Recovery | null {
    const r = this.db.recovery;
    return r === null ? null : { applied: r.applied, truncated: r.truncated };
  }

  /** Whether this store journals to an append-only log. */
  get durable(): boolean {
    return this.db.hasAof();
  }

  /** Current log size in bytes (0 without a log). */
  aofSize(): number {
    return this.db.aofSize();
  }

  /**
   * Rewrite the log to the shortest form that reproduces current state, and
   * return its new size. A queue that pushes and pops forever grows the log
   * without bound even while holding two items; compaction collapses that
   * history. Safe to call at runtime — it writes a temp file and renames.
   */
  compact(): number {
    return this.db.compact();
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

  /**
   * Release the store: stop the sweeper and flush + close the log's file handle.
   * Idempotent.
   *
   * Always call this on shutdown when using `aof`. The handle is a real OS
   * resource — on Windows the log file cannot be deleted, moved, or reopened
   * while it stays open. After closing, reads still work from memory but
   * mutations throw, so a write is never silently dropped from the journal.
   */
  close(): void {
    if (this.timer) {
      clearInterval(this.timer);
      this.timer = null;
    }
    this.db.close();
  }
}

export default Strenor;
