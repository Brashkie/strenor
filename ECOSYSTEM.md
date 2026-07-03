# Strenor Ecosystem

Strenor follows the same rule as the rest of the stack: **one package until a
real need forces a split.** No splitting for symmetry — the question is always
*who installs this alone, and why?*

---

## Package model

Today, three kinds of packages exist (or are planned) around Strenor:

```
strenor                            ← the core (flagship, unscoped, memorable)
├── @strenor/binary-win32-x64-msvc ← native binaries, one per platform
├── @strenor/binary-darwin-arm64     (resolved automatically via
├── @strenor/binary-linux-x64-gnu     optionalDependencies — never installed
└── @strenor/binary-linux-x64-musl …  by hand)

@strenor/cli · @strenor/bench …    ← tooling, under the @strenor scope
```

- **`strenor`** — the core engine. Unscoped and flagship, so `npm install strenor`
  stays the memorable entry point (the `sharp` model: unscoped main + scoped
  binaries).
- **`@strenor/binary-<platform>`** — prebuilt native binaries. Managed by
  `@napi-rs/cli`, installed transparently as `optionalDependencies`. Users never
  touch these.
- **`@strenor/*`** — the tooling ecosystem, under the same npm scope so the names
  are clean and grouped.

> The `@strenor` scope requires an npm **organization** named `strenor` (free for
> public packages). Create it once; the core package stays the unscoped `strenor`,
> and everything else — binaries and tools — lives under `@strenor/*`.

## Planned `@strenor/*` packages

None of these exist yet — they're the intended shape, added when the core is
solid and each has a concrete user.

| Package | Purpose | Status |
|---|---|---|
| `@strenor/cli` | `strenor inspect · dump · load · stats · benchmark · repair` | planned |
| `@strenor/bench` | Benchmark harness vs LMDB / LevelDB / RocksDB / SQLite-as-KV | planned |
| `@strenor/inspector` | Read and explore snapshot files (keys, tags, TTLs, sizes) | planned |
| `@strenor/backup` | Hot backup + scheduled/incremental snapshots | exploratory |
| `@strenor/tools` | Shared internals for the tools above | exploratory |

The split from "one package" to "scope of packages" happens only when a concrete
consumer needs a piece alone — e.g. a CI job that needs the benchmark harness
without the CLI.

---

## The wider Hepein stack

Strenor is one clear responsibility inside a larger set of independent projects.
Each can evolve on its own; each does exactly one thing.

```
Vekziun          Build & publish multi-platform native (NAPI) addons
   │
   ▼
Strenor          Embedded key-value engine  ← this project
   │
   ▼
Signalis-core    Cryptographic primitives (Ed25519, X25519, AEAD, …)
   │
   ▼
Signalis         Signal Protocol (sessions, ratchet) on top of signalis-core
   │
   ▼
HepeinBaileys    WhatsApp (uses the layers above)
```

The dependency arrows are conceptual responsibility, not hard coupling. Strenor
doesn't force any of the others on you — it's a standalone embedded KV store that
happens to fit naturally as the storage layer for bots like **WinsiBot**, which
is where its API is validated against real usage.

---

## Splitting philosophy

The rule, applied everywhere:

1. Start as **one package** with modular internals.
2. Use `exports` subpaths for fine-grained imports before reaching for multiple
   packages.
3. Split into `@strenor/*` **only** when someone would install a piece on its own
   for a real reason.

This keeps the install story simple (`npm install strenor` and you're done) while
leaving a clean path to grow.
