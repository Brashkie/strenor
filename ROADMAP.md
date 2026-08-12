# Strenor Roadmap

Strenor's positioning is deliberate: **the best embedded key-value store for
Node.js**. Not another Redis, not another SQLite — a different space.

**Philosophy:** embedded-first · native-first · zero configuration · single process.

Strenor competes with embedded KV engines (LMDB, RocksDB, LevelDB, sled,
`better-sqlite3` used as a KV store). It overlaps *partially* with SQLite and
Redis for simple key-value needs, and does **not** compete with analytical or
server databases (DuckDB, PostgreSQL, MySQL, MongoDB) — those solve different
problems.

This roadmap follows a "ship what works, then grow" rule. Statuses are kept
honest: **shipped**, **planned**, or **exploratory**.

---

## Phase 0 — Foundation (0.x)

### v0.0.x — Alpha core · **shipped**

- KV store with `Buffer` / `String` / `Object` values
- Tagged binary value format `[tag][payload]`
- Custom, pluggable codecs
- Snapshot persistence (`dump` / `load`)
- TTL with lazy expiration + background sweeper
- Native Rust addon, multi-platform (Windows, macOS, Linux glibc & musl, x64/arm64)
- Node 18+, dual ESM + CJS, 100% test coverage

### v0.1.x — Bot primitives · **shipped**

The data structures bots actually reach for, beyond plain KV.

Shipped in `0.1.0`:

- `list` — queues and stacks (`enqueue`/`dequeue`, `push`/`pop`, `llen`, `lrange`,
  plus Redis aliases), O(1) at both ends via a native `VecDeque`.
- Atomic counters — `incr` / `decr`.
- `WRONGTYPE` errors for mismatched structure operations.

```ts
db.enqueue(key, value);   db.dequeue(key);
db.push(key, value);      db.pop(key);
db.incr(key);             db.decr(key);
```

### v0.2.x — Persistence · **shipped**

Turning Strenor from "memory with a dump" into a durable store.

Shipped in `0.2.0`:

- Append-only log (AOF) — every mutation journalled, replayed on open.
- Crash recovery — a torn tail is dropped and reported via `recovery`.
- Compaction — `compact()` collapses history into current state.
- Snapshot versioning — v3; v1/v2 snapshots still load.
- CRC-32 checksums + corruption detection on snapshots and every log record.
- Atomic snapshot writes (temp file + rename).

### v0.3.x — Data structures · **shipped**

Grow the type set — the useful subset of Redis, not all of it.

- `hash` — shipped in `0.3.0` (`hset`/`hget`/`hdel`/`hexists`/`hkeys`/`hlen`/`hgetall`).
- `set` — shipped in `0.3.1` (`sadd`/`srem`/`sismember`/`smembers`/`scard`).
- `sorted set` — shipped in `0.3.2` (`zadd`/`zincrby`/`zrem`/`zscore`/`zrank`/`zcard`/`zrange`/`zrangeWithScores`).
- `bitset`, optional bloom filter — planned.
- Built-in MsgPack and CBOR codecs — planned.

### v0.4.x — Transactions · **planned**

Something many embedded stores don't get right.

- `batch()`, `transaction()`, `rollback()`
- Compare-and-swap, optimistic locking

### v0.5.x — Performance · **exploratory**

- SIMD, arena allocator, memory pools
- Zero-copy reads, cache optimizations
- Public benchmarks vs LMDB, LevelDB, RocksDB, SQLite-as-KV

---

## Fase 1+ — Beyond the core

Larger directions, pursued only when the core is solid and a real need exists.
Full detail lives in [ECOSYSTEM.md](./ECOSYSTEM.md).

| Phase | Theme | Highlights | Status |
|---|---|---|---|
| **Ecosystem** | Tooling packages | `@strenor/cli`, `@strenor/bench`, `@strenor/inspector`, `@strenor/backup` | planned |
| **Storage engine** | On-disk features | compression (LZ4/Zstd), encryption, page cache, incremental snapshots, hot backup | exploratory |
| **Plugins** | Extensibility | custom codecs, hooks, events, storage drivers | exploratory |
| **Optional server** | Only when needed | TCP / Unix socket / HTTP / WebSocket / Pub/Sub — always opt-in | exploratory |
| **Enterprise** | After all of the above | replication, cluster, metrics, tracing, backup scheduler, access control | exploratory |

The server is a **late, optional** layer — `new Strenor()` stays embedded by
default; `strenor serve` would be the opt-in. Never the other way around.

---

## The v1.0 goal

A `1.0` is not "more features" — it's **trust**:

- Stable API (no breaking changes)
- Stable snapshot format
- Excellent documentation
- Public benchmarks against other embedded stores
- Full support for Windows, Linux, macOS, and Android (Termux), on x64 and arm64

A solid 1.0 earns more confidence than piling on features before the base is
stable.

---

## Non-goals

Strenor will not try to become SQLite, PostgreSQL, DuckDB, or a Redis cluster.
Each of those solves a different problem. Strenor stays the fast, embedded,
single-process KV store.
