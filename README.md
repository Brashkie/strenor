<div align="center">

<img src="https://raw.githubusercontent.com/Brashkie/strenor/main/media/strenor.png" alt="Strenor" width="360" />

### Embedded high-performance key-value store for Node.js, powered by Rust

<em>In-process&nbsp; · &nbsp;No server&nbsp; · &nbsp;No network&nbsp; · &nbsp;No configuration</em>

<br />

[![npm](https://img.shields.io/npm/v/strenor.svg?color=cb3837&label=npm)](https://www.npmjs.com/package/strenor)
[![node](https://img.shields.io/badge/node-%3E%3D18-339933.svg?logo=node.js&logoColor=white)](https://nodejs.org)
[![rust](https://img.shields.io/badge/core-Rust-dea584.svg?logo=rust&logoColor=white)](https://www.rust-lang.org)
[![coverage](https://img.shields.io/badge/coverage-100%25-brightgreen.svg)](#)
[![types](https://img.shields.io/badge/types-included-3178c6.svg?logo=typescript&logoColor=white)](./dist/index.d.ts)
[![license](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

<br />

**[Quick start](#quick-start)&nbsp; · &nbsp;[Examples](#examples)&nbsp; · &nbsp;[API](#api)&nbsp; · &nbsp;[Comparison](#comparison)&nbsp; · &nbsp;[Roadmap](./ROADMAP.md)&nbsp; · &nbsp;[Ecosystem](./ECOSYSTEM.md)**

<sub>Read this in <a href="./README.es.md">Español</a></sub>

</div>

<br />

> [!NOTE]
> **Stable (0.x).** Out of alpha — the API is stable within the `0.1.x` line.
> Minor releases (0.2, 0.3, …) may still evolve it before `1.0`.

<details>
<summary><b>Table of contents</b></summary>

- [Why Strenor](#why-strenor) · [When to use it](#when-to-use-it) · [Install](#install)
- [Quick start](#quick-start) · [Examples](#examples) · [Comparison](#comparison)
- [API](#api) · [Durability](#durability) · [Value format](#value-format) · [How it works](#how-it-works) · [Performance](#performance)
- [Multi-platform builds & publishing](#multi-platform-builds--publishing) · [Project structure](#project-structure)
- [Roadmap](./ROADMAP.md) · [Ecosystem](./ECOSYSTEM.md) · [Contributing](#contributing) · [License](#license)

</details>

---

## Why Strenor

Redis is a **separate server**. Even on localhost you run a process, open a
socket, and serialize every value across the wire — twice per round trip. For a
single-process app or bot, that's latency and operational weight you pay for
capabilities you may not use.

Strenor lives **inside your Node process** as a native addon written in Rust.
There is no server to start, no port, no network hop, no connection pool. You
`npm install`, import, and call methods — the store is just there, in memory,
with the option to persist to disk.

The mental model is closer to **`better-sqlite3`** than to Redis: the right tool
when one process needs a fast local store, not when many services need to share
state over a network.

Strenor was born from a real need — giving the **WinsiBot** WhatsApp bot fast
local storage without operating an external service.

## When to use it

Strenor is a good fit when:

- You run a **single Node process** (a bot, a CLI, a worker, an edge function)
  that needs fast key-value access.
- You want **zero-setup** caching, session storage, or ephemeral state with TTL.
- You want **local persistence** (survive restarts) without standing up a database.
- You store **binary blobs** (avatars, thumbnails, serialized payloads) and don't
  want to base64 them through a network protocol.

Strenor is **not** the right tool when:

- Multiple processes or services must **share** the same store -> use Redis.
- You need **Pub/Sub**, replication, or clustering -> use Redis.
- You need **relational queries** or cross-table transactions -> use SQLite/Postgres.

## Install

```bash
npm install strenor
```

Prebuilt native binaries are shipped per platform, so there is no compile step
for consumers. Supported platforms: Windows, macOS (x64/arm64), and Linux
(glibc **and** musl), on x64 and arm64.

## Quick start

```ts
import { Strenor } from 'strenor';
// CommonJS:  const { Strenor } = require('strenor');

const db = new Strenor();

// Smart API — dispatches by runtime type
db.set('user:1', { name: 'Brashkie', age: 20 }); // object -> JSON
db.set('token', 'abc123'); //                        string -> UTF-8
db.set('avatar', pngBuffer); //                      Buffer -> raw bytes

db.get('user:1'); // -> { name: 'Brashkie', age: 20 }
db.get('token'); //  -> 'abc123'
db.get('avatar'); // -> Buffer

// TTL in milliseconds
db.set('session:42', sessionData, { ttl: 30 * 60_000 });
db.ttl('session:42'); // remaining ms (-1 = no expiry, -2 = missing)

// Persist to disk and reload
db.dump('./strenor.snap');
db.load('./strenor.snap');
```

## Examples

### Cache with expiration

```ts
const cache = new Strenor({ sweepInterval: 60_000 }); // purge expired every 60s

function getUser(id: string) {
  const hit = cache.get<User>(`user:${id}`);
  if (hit) return hit;
  const user = fetchUserFromApi(id);
  cache.set(`user:${id}`, user, { ttl: 5 * 60_000 }); // cache 5 minutes
  return user;
}
```

### Session store for a bot

```ts
const sessions = new Strenor();

function touchSession(userId: string, state: SessionState) {
  sessions.set(`sess:${userId}`, state, { ttl: 30 * 60_000 });
}

// Persist on shutdown, restore on boot
process.on('SIGTERM', () => sessions.dump('./sessions.snap'));
try {
  sessions.load('./sessions.snap');
} catch {
  /* first boot: no snapshot yet */
}
```

### Binary values

```ts
db.setBuffer('thumb:1', await sharp(input).resize(128).toBuffer());
const thumb = db.getBuffer('thumb:1'); // Buffer, byte-for-byte
```

### Custom codec (msgpack, cbor, ...)

The default object codec is JSON. Swap it for one that preserves richer types:

```ts
import { encode, decode } from '@msgpack/msgpack';

const msgpack = {
  tag: 0x20, // custom tags live in 0x20..0xFE
  encode: (v: unknown) => Buffer.from(encode(v)),
  decode: (b: Buffer) => decode(b),
};

const db = new Strenor({ codec: msgpack });
db.registerCodec(msgpack); // so previously written msgpack values still decode
```

## Where Strenor fits

Strenor is an **embedded key-value store**. Its real peers are other embedded KV
engines — not networked or analytical databases.

- **Direct competition** (embedded KV): LMDB, RocksDB, LevelDB, sled,
  `better-sqlite3` *used purely as a KV store*.
- **Partial overlap**: SQLite and Redis, for the common case where all you need
  is `set` / `get`. If a bot stores sessions with `SELECT data FROM sessions
  WHERE id = ?` or a Redis round trip, Strenor does the same with `db.get(id)` —
  no schema, no server, no port.
- **Not competition**: DuckDB, PostgreSQL, MySQL, MongoDB. Analytical queries,
  relational joins, and multi-node servers are different problems Strenor does
  not try to solve.

## Comparison

| | **Strenor** | LMDB / RocksDB | better-sqlite3 | Redis | SQLite |
|---|---|---|---|---|---|
| Category | embedded KV | embedded KV | embedded SQL | server KV | embedded SQL |
| Runs in-process | yes | yes | yes | no (server) | yes |
| Network overhead | none | none | none | socket + protocol | none |
| Node-native API | yes (Rust addon) | via bindings | yes (C addon) | via client | yes (C addon) |
| Values | bytes + codecs | bytes | SQL rows / BLOB | strings/structures | SQL rows / BLOB |
| `get`/`set` ergonomics | `db.get(id)` | cursor/txn | SQL statement | client round trip | SQL statement |
| TTL / expiration | built-in | manual | manual | built-in | manual |
| Disk persistence | snapshot (AOF planned) | full engine | full DB | RDB/AOF | full DB |
| Setup / ops | none | none | none | run a server | none |

Strenor trades the networked, multi-process, and relational capabilities of the
others for zero setup and zero-latency in-process access. Pick it when one
process needs a fast local store; pick the others when you need what they add.

## API

Construction:

```ts
new Strenor(options?: {
  codec?: Codec;          // default object codec (default: JSON)
  sweepInterval?: number; // ms; background purge of expired keys (unref'd)
})
```

Core operations (all synchronous):

| Method | Description |
|---|---|
| `set(key, value, opts?)` | Store any value; `opts.ttl` (ms), `opts.codec` to override |
| `get<T>(key)` | Read and auto-decode by tag; `null` if missing/expired |
| `setString` / `getString` | Force/assert a UTF-8 string value |
| `setBuffer` / `getBuffer` | Force/assert a raw `Buffer` value |
| `setJSON` / `getJSON` | Force the object codec explicitly |
| `del(key)` | Delete; returns `true` if it existed |
| `exists(key)` | Whether a live (non-expired) key exists |
| `expire(key, ttlMs)` | Set/replace a key's TTL |
| `persist(key)` | Remove a key's TTL |
| `ttl(key)` | Remaining ms; `-1` no expiry, `-2` missing |
| `keys()` | All live keys (O(n); for small sets/debugging) |
| `size()` | Number of entries |
| `clear()` | Remove everything |
| `sweep()` | Eagerly purge expired keys; returns count removed |
| `dump(path)` / `load(path)` | Snapshot to / from a self-describing file |
| `registerCodec(codec)` | Register an extra codec for a custom tag |
| `close()` | Stop the background sweeper (if any) |

A `Codec` is `{ tag, encode(value) => Buffer, decode(bytes) => value }` with a
`tag` byte in `0x20..0xFE`. Full type definitions ship in `dist/index.d.ts`.

### Hashes (field maps)

A hash stores independent fields under one key — ideal for sessions and configs.
Field values reuse the same tagged format and codecs as `set`/`get`, so a field
can hold a string, number, buffer, or object.

| Method | Description |
|---|---|
| `hset(key, field, value, opts?)` | Set a field; returns `true` if it was new |
| `hget<T>(key, field)` | Get one field, decoded; `null` if missing |
| `hdel(key, field)` | Delete a field; `true` if it existed |
| `hexists(key, field)` | Whether the field is present |
| `hkeys(key)` | Field names (empty if missing) |
| `hlen(key)` | Number of fields (0 if missing) |
| `hgetall<T>(key)` | The whole hash as a decoded object |

```ts
db.hset('session:alice', 'step', 'checkout');
db.hset('session:alice', 'cart', ['book', 'pen']);
db.hgetall('session:alice'); // { step: 'checkout', cart: ['book', 'pen'] }
```

An emptied hash key is removed automatically. Operations on a non-hash key raise
`WRONGTYPE`.

### Counters

Atomic integer counters — ideal for rate limits, message counts, and sequence
IDs. A missing key starts at 0. Atomic within Node's single-threaded event loop.

| Method | Description |
|---|---|
| `incr(key, by?)` | Add `by` (default 1); returns the new value |
| `decr(key, by?)` | Subtract `by` (default 1); returns the new value |

```ts
db.incr('hits');       // -> 1
db.incr('hits', 10);   // -> 11
db.decr('hits');       // -> 10
db.get('hits');        // -> 10  (a counter is just a number)
```

A counter must be an integer within `Number.MAX_SAFE_INTEGER` (±2^53); other
values throw. `incr`/`decr` on a list raise `WRONGTYPE`.

### Lists (queues & stacks)

Lists are backed by a native double-ended queue — **O(1) at both ends**. Elements
use the same tagged format and codecs as values, so you can enqueue objects,
strings, or buffers and get them back decoded.

| Method | Description |
|---|---|
| `enqueue(key, value, opts?)` | Append to the tail; returns new length |
| `dequeue<T>(key)` | Remove & return the head (FIFO); `null` if empty |
| `push(key, value, opts?)` | Append to the tail; returns new length |
| `pop<T>(key)` | Remove & return the tail (LIFO); `null` if empty |
| `llen(key)` | List length (0 if missing) |
| `lrange<T>(key, start, stop)` | Elements in `[start, stop]` (negative from end) |
| `lpush` / `rpush` / `lpop` / `rpop` | Redis-style directional aliases |

Mixing types raises a Redis-style `WRONGTYPE` error: `get` on a list, or a list
operation on a plain value, throws.

```ts
db.enqueue('jobs', { id: 1 }); // -> 1
db.enqueue('jobs', { id: 2 }); // -> 2
db.dequeue('jobs'); //            -> { id: 1 }  (FIFO)
db.llen('jobs'); //               -> 1
```

## Durability

By default Strenor is memory-only (with `dump`/`load` for snapshots). Point it at
an **append-only log** and every mutation is journalled as it happens, then
replayed on open — so state survives a restart *or* a crash.

```ts
const db = new Strenor({ aof: './bot.aof' });

db.recovery; // { applied: 42, truncated: false }  — what the log replay found
```

| Member | Description |
|---|---|
| `aof` option | Path to the log. Omit for memory-only. |
| `fsync` option | Force every write to disk. Survives power loss; much slower. |
| `recovery` | `{ applied, truncated }` after replay, or `null` without a log |
| `durable` | Whether a log is attached |
| `aofSize()` | Current log size in bytes |
| `compact()` | Rewrite the log to the shortest form that reproduces state |
| `close()` | Flush and release the log's file handle. Always call on shutdown. |

**Crash recovery.** If the process dies mid-write, the last record is torn.
On open, Strenor replays every intact record, drops the torn tail, and reports
`truncated: true`. The store is consistent; only writes that never reached the
file are lost.

**Compaction.** A queue that pushes and pops forever grows the log without bound
even while holding two items. `compact()` collapses that history into the state
itself — in `examples/durability.mjs`, 400 no-op operations grow the log to
~9.6 KB, and compaction returns it to ~142 bytes. It writes a temp file and
renames, so a crash mid-compaction leaves the previous log intact.

**Durability levels** — pick the trade-off honestly:

| Mode | Survives a process crash | Survives power loss | Speed |
|---|---|---|---|
| memory-only (default) | no | no | fastest |
| `aof` | **yes** | no (last writes may be lost) | fast |
| `aof` + `fsync: true` | **yes** | **yes** | slow (a disk sync per write) |

**Closing matters.** `close()` releases the log's file handle, not just the
sweeper timer. On Windows a file with an open handle cannot be deleted, moved, or
reopened, so a store left open keeps its log locked for the life of the process.
After closing, reads still work from memory while mutations throw — a write is
never quietly left out of the journal.

Snapshots are written atomically (temp file + rename) and carry a CRC-32, so a
corrupted or truncated file is rejected instead of silently loading garbage.

## Value format

Every stored value is `[tag:u8][payload]`. The **Rust core never interprets the
bytes** — decoding lives entirely in the TypeScript layer. This makes the on-disk
snapshot self-describing (each value knows how to decode itself) and the format
safe to extend without breaking existing data.

| Tag | Meaning |
|---|---|
| `0x00` | Raw bytes (`Buffer`) |
| `0x01` | UTF-8 string |
| `0x02` | JSON (default object codec) |
| `0x03–0x06` | Reserved |
| `0x20–0xFE` | Pluggable codecs (msgpack, cbor, ...) |

## How it works

Three layers, each with one job:

1. **Rust core (`crates/strenor-store`)** — an in-memory map of `key -> (bytes, expiry)`
   behind a lock. It knows nothing about types; it stores opaque `[tag][payload]`
   bytes, handles TTL (lazy expiration + `sweep`), and reads/writes the binary
   snapshot. Small, fast, value-agnostic by design.
2. **TypeScript layer (`src/index.ts`)** — the ergonomic API. It owns the tag
   contract and codecs, dispatches `set`/`get` by runtime type, and manages the
   optional background sweeper.
3. **Native loader (`src/native.ts`)** — a small, self-contained loader that
   resolves the correct `.node` for the current platform using the @napi-rs/cli
   naming convention, telling glibc and musl apart so the addon doesn't silently
   break on Alpine/Docker.

## Performance

Strenor's performance story is structural, not benchmarked-marketing:

- **No network, no protocol.** Operations are direct in-process calls into Rust —
  no socket, no request framing, no serialization across a wire.
- **Synchronous by design.** Reads and writes to the in-memory map return
  immediately, avoiding event-loop round trips on hot paths (the same reason
  `better-sqlite3` is fast).
- **One serialization, at the boundary.** Values are encoded once as they enter
  the store and decoded once on the way out; the core just moves bytes.

A reproducible benchmark harness (vs Redis-over-localhost and pure-JS caches) is
on the roadmap. Until then no numbers are claimed here — measure against your own
workload.

## Multi-platform builds & publishing

Native distribution uses [**@napi-rs/cli**](https://napi.rs). The `napi` field in
`package.json` declares the target triples; each platform ships as its own npm
package, wired to the main package via `optionalDependencies`, so consumers just
`npm install strenor` and get the right binary.

Local development (host target only):

```bash
npm run build:native   # napi build --platform --release -> strenor.<suffix>.node
npm run build:ts       # tsup -> dist (ESM + CJS + types)
npm run test:coverage
```

Multi-platform release is tag-driven and runs in CI: a matrix builds each target
(cross-compiling musl/arm64 with zig), then a publish job collects the binaries,
packs the per-platform packages, and publishes them (platform packages first,
main package last). See [`.github/workflows/release.yml`](./.github/workflows/release.yml).

Supported targets: Windows (x64/arm64), macOS (x64/arm64), Linux glibc & musl
(x64/arm64), and Android (arm64/armv7).

## Project structure

```
strenor/
├── Cargo.toml             # Rust workspace (at the repo root)
├── crates/
│   ├── strenor-store/     # pure Rust core (byte store + TTL + snapshot, unit-tested)
│   └── strenor-node/      # thin NAPI binding (cdylib)
├── src/                   # TypeScript (pure)
│   ├── index.ts           # public API: tags, codecs, helpers, TTL sweeper
│   └── native.ts          # native loader (napi-rs convention)
├── __tests__/             # Vitest suite (instruments src/)
├── examples/               # runnable usage examples
├── scripts/
│   └── smoke.ts           # end-to-end smoke test (tsx)
├── .github/workflows/     # CI + multi-platform release
├── tsup.config.ts         # bundler: ESM + CJS + .d.ts
├── biome.json             # lint + format
├── vitest.config.ts       # tests + v8 coverage
├── tsconfig.json          # strict type-check (TS6)
├── ROADMAP.md · ECOSYSTEM.md
└── package.json
```

## Roadmap

Strenor follows a "ship what works, then grow" philosophy. Highlights:

- **v0.0.x — Alpha core** *(shipped)*: KV, tags/codecs, TTL, snapshot, multi-platform native.
- **v0.1.x — Bot primitives**: `list` / `queue` / `stack` / `deque`, atomic `incr`/`decr`.
- **v0.2.x — Persistence**: append-only log (AOF), crash recovery, compaction, checksums.
- **v0.3.x — Data structures**: `hash`, `set`, `sorted set`; built-in MsgPack/CBOR codecs.
- **v0.4.x — Transactions**: `batch()`, `transaction()`, compare-and-swap.
- **v0.5.x — Performance**: zero-copy reads, memory pools, public benchmarks.

The **v1.0** goal is trust, not features: stable API, stable snapshot format,
public benchmarks, and full Windows/Linux/macOS/Android (x64/arm64) support.

**Non-goals:** becoming SQLite, PostgreSQL, DuckDB, or a Redis cluster.

→ Full phased roadmap: **[ROADMAP.md](./ROADMAP.md)**

## Ecosystem

Today Strenor is a **single package**: `strenor` (the core) plus scoped
per-platform native packages (`@strenor/binary-*`) resolved automatically via
`optionalDependencies`. Planned
tooling lives under the `@strenor/*` scope (`@strenor/cli`, `@strenor/bench`,
`@strenor/inspector`, `@strenor/backup`) — added only when a piece has a real
standalone user.

Strenor is one clear responsibility inside a wider set of independent projects:
**Vekziun** (native build tooling) → **Strenor** (embedded KV) →
**signalis-core** (crypto) → **Signalis** (Signal Protocol) → **HepeinBaileys**
(WhatsApp). Its first real consumer is **WinsiBot**.

→ Full ecosystem, package model, and `@strenor/*` plan: **[ECOSYSTEM.md](./ECOSYSTEM.md)**

## Contributing

Contributions are welcome. See [CONTRIBUTING.md](./CONTRIBUTING.md). In short:
`npm run check` (Biome), `npm run typecheck`, and `npm run test:coverage` must
pass, and new behavior needs tests.

## License

Apache-2.0 © Hepein Oficial (Brashkie)
