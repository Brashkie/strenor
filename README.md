# Strenor

*Read this in [Español](./README.es.md).*

Embedded high-performance key-value store for Node.js, with a core written in
Rust. **In-process** — no server to launch, no network, no configuration.
`npm install`, import, use.

> ⚠️ **Alpha / experimental.** Format and API may change before `0.1.0`.
> Install with `npm install strenor@alpha`.

## Why

Redis is a separate server: you run it, connect over a socket, and serialize
everything across the wire even on localhost. Strenor lives **inside your Node
process** as a native addon — zero network latency, zero setup. It's closer to
`better-sqlite3` than to Redis: the right tool when a single process needs a
fast local store.

Born from a real need: giving WinsiBot fast local storage without depending on
an external service.

## What it is (and isn't), today

- ✅ In-process embedded KV store (single process).
- ✅ Tagged binary value format: `Buffer`, `string`, and JSON-encoded objects.
- ✅ TTL with lazy expiration + optional background sweeper.
- ✅ Self-describing binary snapshot (`dump`/`load`).
- ✅ Pluggable object codec (default JSON; msgpack/cbor can plug in).
- ❌ No Pub/Sub, no multi-process sharing, no server mode (possible later).
- ❌ Snapshot is a full dump, not an append-only log (no crash-durable writes yet).
- ⚠️ The default JSON codec inherits JSON's limits: `Date` becomes a string,
  `undefined` is dropped, `BigInt` throws, functions are ignored. Use a
  richer codec (msgpack/cbor) if you need those.

## Usage

```js
const { Strenor } = require('strenor');
// or, with ESM / TypeScript:  import { Strenor } from 'strenor';

const db = new Strenor();

// Smart API — dispatches by type
db.set('user:1', { name: 'Brashkie', age: 20 });
db.get('user:1'); // -> { name: 'Brashkie', age: 20 }

db.set('hello', 'world');
db.get('hello'); // -> 'world'

db.set('avatar', someBuffer);
db.get('avatar'); // -> Buffer

// TTL (milliseconds)
db.set('session', token, { ttl: 60_000 });
db.ttl('session'); // remaining ms, -1 none, -2 missing

// Persistence
db.dump('./strenor.snap');
db.load('./strenor.snap');
```

### Custom codec

```js
// A codec is { tag, encode(value) -> Buffer, decode(bytes) -> value }
// Custom tags must live in 0x20..0xFE.
const msgpackCodec = {
  tag: 0x20,
  encode: (v) => Buffer.from(encode(v)),
  decode: (b) => decode(b),
};

const db = new Strenor({ codec: msgpackCodec });
db.registerCodec(msgpackCodec); // so existing tagged values still decode
```

## Value format

Every stored value is `[tag:u8][payload]`. The Rust core never interprets it —
decoding is entirely in the JS layer, which makes the on-disk snapshot
self-describing and the format safe to extend.

| Tag         | Meaning                 |
| ----------- | ----------------------- |
| `0x00`      | Raw bytes (Buffer)      |
| `0x01`      | UTF-8 string            |
| `0x02`      | JSON                    |
| `0x03–0x06` | Reserved                |
| `0x20–0xFE` | Pluggable codecs        |

## Project structure

```
strenor/
├── Cargo.toml             # at the repo root (napi convention)
├── build.rs
├── crates/
│   └── lib.rs             # Rust core: agnostic byte store + TTL + snapshot
├── src/                   # TypeScript (pure)
│   ├── index.ts           # public API: tags, codecs, helpers, TTL sweeper
│   └── native.ts          # typed loader for the compiled .node
├── __tests__/             # Vitest suite (instruments src/)
├── scripts/
│   └── smoke.ts           # end-to-end smoke test (tsx)
├── .github/workflows/     # CI + release
├── biome.json             # lint + format
├── vitest.config.ts       # tests + v8 coverage
├── tsup.config.ts         # bundler: CJS + ESM + .d.ts
├── tsconfig.json          # strict type-check (TS6, noEmit)
└── package.json
```

## Build & test

```bash
npm install
# build the native addon (Vekziun, or `cargo build --release` in the crate)
# and place strenor.node at the package root
npm run build         # tsup -> dist/index.js (ESM) + index.cjs (CJS) + index.d.ts
npm run typecheck     # tsc (TS6)
npm run test:coverage # vitest + v8 coverage (instruments src/)
npm run smoke         # quick end-to-end check against the built bundle
```

Lint / format with Biome: `npm run check`, `npm run format`. Type-check only:
`npm run typecheck`.

> `Cargo.toml` sits at the repo root (napi convention); its `[lib] path` points
> at `crates/lib.rs`, so `src/` stays pure TypeScript. Point Vekziun at the root
> `Cargo.toml`. The compiled `.node` should land at the package root as
> `strenor.node`; `src/native.ts` resolves it. The bundled CI/release workflows
> build a Linux binary as a baseline — real multiplatform distribution goes
> through Vekziun.

## License

Apache-2.0 © Hepein Oficial (Brashkie)
