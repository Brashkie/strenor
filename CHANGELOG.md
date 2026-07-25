# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project aims
to follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0] - 2026-07-24

### Added

- **Hashes** (field maps): `hset`, `hget`, `hdel`, `hexists`, `hkeys`, `hlen`,
  `hgetall`. Field values reuse the same tagged format and codecs as `set`/`get`,
  so a hash can hold strings, numbers, buffers, or objects. Journalled to the AOF
  and included in snapshots; an emptied hash key is removed (Redis-like).

### Changed

- Snapshot format bumped to v4 (adds the hash type); v1–v3 snapshots still load.

## [0.2.0] - 2026-07-17

Persistence: Strenor is no longer "memory with a dump".

### Added

- **Append-only log (AOF)** via the `aof` option: every mutation is journalled
  and replayed on open, so state survives a restart or a crash. Optional
  `fsync` for power-loss durability.
- **Crash recovery**: a torn tail (the process died mid-append) is dropped and
  the log truncated to the last intact record, reported via `recovery`
  (`{ applied, truncated }`) instead of failing.
- **Compaction**: `compact()` rewrites the log to the shortest sequence that
  reproduces current state; `aofSize()` and `durable` to drive it.
- **CRC-32 checksums** on every log record and on snapshots (format v3), with
  corruption detection — a damaged snapshot is rejected, not silently loaded.

### Changed

- Snapshots are now written **atomically** (temp file + rename). Previously a
  crash mid-`dump` could truncate the file and lose the data.
- Snapshot format bumped to v3; v1 and v2 snapshots still load.

### Fixed

- Write errors are no longer swallowed: a mutation that could not be journalled
  now throws instead of reporting success.
- `close()` now releases the log's file handle instead of only stopping the
  sweeper. The handle is a real OS resource: on Windows the log file could not
  be deleted, moved, or reopened for the life of the process (`Access denied`,
  os error 5). Closing is idempotent; after it, reads still work from memory
  while mutations throw, so a write is never dropped from the journal unnoticed.
- Compaction releases its handle before replacing the log, which Windows
  requires in order to rename over an existing file.
- `npm test`, `npm run test:coverage`, and `npm run smoke` now rebuild the native
  addon first (`pretest` hooks). Previously they could silently run against a
  stale `.node`, producing errors that looked like logic bugs.

## [0.1.0] - 2026-07-11

First stable release — out of alpha. The KV + list + counter API is stable for
the `0.1.x` line.

### Added

- **Atomic counters**: `incr` / `decr` (integer, with an optional step). Atomic
  within Node's single-threaded event loop; a missing key starts at 0.

## [0.1.0-alpha.0] - 2026-07-04

### Added

- **Lists** — queues and stacks backed by a native `VecDeque`, O(1) at both ends:
  `enqueue`/`dequeue` (FIFO), `push`/`pop` (LIFO), `llen`, `lrange`, and Redis-style
  aliases `lpush`/`rpush`/`lpop`/`rpop`. Elements reuse the same tagged value
  format and codecs as `set`/`get`.
- Redis-style `WRONGTYPE` errors when an operation targets the wrong structure.

### Changed

- Snapshot format bumped to v2 (adds list entries); v1 snapshots still load.

## [0.0.1-alpha.1] - 2026-07-04

### Added

- `NOTICE` and `SECURITY.md`.

### Changed

- Restructured the Rust side into a Cargo **workspace**: a pure, unit-tested
  `strenor-store` core and a thin `strenor-node` NAPI binding. Adds Rust unit
  tests, clippy, and rustfmt to CI.
- Scoped per-platform native packages under `@strenor/binary-*`.

## [0.0.1-alpha.0]

### Added

- Initial alpha. Embedded, in-process key-value store with a Rust core.
- Agnostic byte core: values stored as `[tag][payload]`; the core never
  interprets contents.
- Smart API (`set`/`get`) with type dispatch: Buffer, string, and JSON objects.
- Typed helpers: `setString`/`getString`, `setBuffer`/`getBuffer`,
  `setJSON`/`getJSON`.
- TTL with lazy expiration plus an optional background sweeper.
- Self-describing binary snapshot via `dump`/`load`.
- Pluggable codec interface (`registerCodec`, per-write `codec` option); JSON
  is the default object codec.

[Unreleased]: https://github.com/Brashkie/strenor/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/Brashkie/strenor/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/Brashkie/strenor/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Brashkie/strenor/compare/v0.1.0-alpha.0...v0.1.0
[0.1.0-alpha.0]: https://github.com/Brashkie/strenor/compare/v0.0.1-alpha.1...v0.1.0-alpha.0
[0.0.1-alpha.1]: https://github.com/Brashkie/strenor/compare/v0.0.1-alpha.0...v0.0.1-alpha.1
[0.0.1-alpha.0]: https://github.com/Brashkie/strenor/releases/tag/v0.0.1-alpha.0
