# Strenor Roadmap

Strenor's positioning is deliberate: **the best embedded key-value store for
Node.js**. Not another Redis, not another SQLite — a different space.

**Philosophy:** embedded-first · native-first · zero configuration · single process.

Strenor competes with embedded KV engines (LMDB, RocksDB, LevelDB, sled,
`better-sqlite3` used as a KV store). It overlaps *partially* with SQLite and
Redis for simple key-value needs, and does **not** compete with analytical or
server databases (DuckDB, PostgreSQL, MySQL, MongoDB) — those solve different
problems.

## How to read this roadmap

This roadmap is organized by **phase** — the capabilities the project gains as it
matures — not by version number. Phases describe *evolution*; versions are how a
phase reaches users. Each phase notes the releases that delivered it, but the
authoritative per-release history lives in [CHANGELOG.md](./CHANGELOG.md).

Statuses are kept honest: **shipped**, **in progress**, **planned**, or
**exploratory**.

---

## Phase 1 — Core KV · **shipped** · _delivered in 0.0.x–0.1.x_

The foundation: a fast, embedded byte store with a clean value model.

- KV store with `Buffer` / `String` / `Object` values.
- Tagged binary value format `[tag][payload]`, with pluggable custom codecs.
- TTL with lazy expiration plus a background sweeper.
- Snapshot persistence (`dump` / `load`).
- Atomic counters — `incr` / `decr`.
- Native Rust addon, multi-platform (Windows, macOS, Linux glibc & musl,
  x64/arm64), Node 18+, dual ESM + CJS, 100% test coverage.

## Phase 2 — Persistence · **shipped** · _delivered in 0.2.0_

Turning Strenor from "memory with a dump" into a durable store.

- Append-only log (AOF) — every mutation journalled, replayed on open.
- Crash recovery — a torn tail is dropped and reported via `recovery`.
- Compaction — `compact()` collapses history into current state.
- Snapshot versioning with backward compatibility.
- CRC-32 checksums + corruption detection on snapshots and every log record.
- Atomic snapshot writes (temp file + rename).

## Phase 3 — Data structures · **shipped** · _delivered in 0.3.0–0.3.2_

The useful subset of Redis, not all of it. Every structure is journalled to the
AOF and included in snapshots, and raises `WRONGTYPE` on mismatched operations.

- `list` — queues and stacks, O(1) at both ends (`0.1.0`).
- `hash` — field maps (`0.3.0`).
- `set` — unique members (`0.3.1`).
- `sorted set` — members ranked by score (`0.3.2`).

Still planned for this phase:

- `bitset`, optional bloom filter.
- Built-in MsgPack and CBOR codecs.

## Phase 4 — Transactions · **shipped** · _delivered in 0.4.0_

Something many embedded stores don't get right.

- `transaction(fn)` — all-or-nothing: snapshot up front, atomic batch on commit,
  full rollback if the callback throws.
- `batch(fn)` — grouped writes in one journal pass, without a rollback snapshot.

Still planned for this phase:

- Compare-and-swap / optimistic locking primitives.

## Phase 5 — Performance & robustness · **in progress**

Make it measurably fast, and prove it — with numbers, not claims.

- **Benchmark infrastructure (Criterion)** — reproducible micro-benchmarks for
  the pure engine (`cargo bench`), covering KV, list, hash, sorted set, and
  transactions (`0.5.0`).
- Optimize only what the benchmarks flag as a real bottleneck — e.g. the
  transaction rollback snapshot is currently O(state size); an undo-log per
  operation is the candidate replacement *if the numbers justify it*.
- Public, reproducible benchmarks vs LMDB, LevelDB, RocksDB, and SQLite-as-KV —
  published only alongside their methodology and hardware, never as bare claims.

Exploratory (only if benchmarks point here): zero-copy reads, arena/pool
allocation, cache-friendly layouts.

## Phase 6 — The 1.0 goal · **planned**

A `1.0` is not "more features" — it's **trust**:

- Stable API (no breaking changes).
- Stable snapshot format.
- Excellent documentation.
- Public benchmarks against other embedded stores.
- Full support for Windows, Linux, macOS, and Android (Termux), on x64 and arm64.

A solid 1.0 earns more confidence than piling on features before the base is
stable.

---

## Beyond the core

Larger directions, pursued only when the core is solid and a real need exists.
Full detail lives in [ECOSYSTEM.md](./ECOSYSTEM.md).

| Direction | Theme | Highlights | Status |
|---|---|---|---|
| **Ecosystem** | Tooling packages | `@strenor/cli`, `@strenor/bench`, `@strenor/inspector`, `@strenor/backup` | planned |
| **Storage engine** | On-disk features | compression (LZ4/Zstd), encryption, page cache, incremental snapshots, hot backup | exploratory |
| **Plugins** | Extensibility | custom codecs, hooks, events, storage drivers | exploratory |
| **Optional server** | Only when needed | TCP / Unix socket / HTTP / WebSocket / Pub/Sub — always opt-in | exploratory |
| **Enterprise** | After all of the above | replication, cluster, metrics, tracing, backup scheduler, access control | exploratory |

The server is a **late, optional** layer — `new Strenor()` stays embedded by
default; `strenor serve` would be the opt-in. Never the other way around.

---

## Non-goals

Strenor will not try to become SQLite, PostgreSQL, DuckDB, or a Redis cluster.
Each of those solves a different problem. Strenor stays the fast, embedded,
single-process KV store.
