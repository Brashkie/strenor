# Contributing to Strenor

Thanks for your interest! Strenor is in alpha, so the internals can still move.

## Layout

- `crates/strenor-store/` — pure Rust core (byte store, TTL, snapshot; `cargo test`).
- `crates/strenor-node/` — thin NAPI binding (cdylib).
- `src/` — TypeScript public API (tags, codecs, helpers).
- `test/` — Vitest suite (runs against the compiled `dist/`).

## Local setup

```bash
npm install

# Build the native addon (Rust). Locally this is done via @napi-rs/cli;
# for a plain Linux build:
cargo build --release
cp target/release/libstrenor.so strenor.node   # Windows: target\release\strenor.dll

npm run build          # tsup: CJS + ESM + .d.ts
npm run test:coverage
```

## Before opening a PR

- `npm run check` — Biome lint + format must pass.
- `npm run typecheck` — no TypeScript errors.
- `npm run test:coverage` — tests pass and coverage thresholds hold.
- Add a `CHANGELOG.md` entry under **Unreleased**.

## Design rule worth knowing

The Rust core must stay **value-agnostic**: it only stores opaque
`[tag][payload]` bytes and never interprets them. Any type/codec logic belongs
in the TypeScript layer. Keep that boundary intact.

> **The native addon must match your Rust source.** `npm test`, `npm run
> test:coverage`, and `npm run smoke` rebuild it automatically (`pretest` hooks);
> the rebuild is incremental, so it costs ~1s when nothing changed. If you run
> `vitest` directly, build first with `npm run build:native` — otherwise you are
> testing a stale `.node` and will see confusing errors like
> `TypeError: this.db.<method> is not a function`.
