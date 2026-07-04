# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project aims
to follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.1-alpha.1] - 2026-07-04

### Changed

- Restructured the Rust side into a Cargo **workspace**: a pure, unit-tested
  `strenor-store` core and a thin `strenor-node` NAPI binding. Adds Rust unit
  tests, clippy, and rustfmt to CI.
- Scoped per-platform native packages under `@strenor/binary-*`.

### Added

- `NOTICE` and `SECURITY.md`.

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

[Unreleased]: https://github.com/Brashkie/strenor/compare/v0.0.1-alpha.1...HEAD
[0.0.1-alpha.1]: https://github.com/Brashkie/strenor/compare/v0.0.1-alpha.0...v0.0.1-alpha.1
[0.0.1-alpha.0]: https://github.com/Brashkie/strenor/releases/tag/v0.0.1-alpha.0
