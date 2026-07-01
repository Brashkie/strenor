import { existsSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

/** Typed surface of the compiled Rust addon (no `.d.ts` ships with the .node). */
export interface NativeStrenorInstance {
  set(key: string, value: Buffer, ttlMs?: number): void;
  get(key: string): Buffer | null;
  del(key: string): boolean;
  exists(key: string): boolean;
  expire(key: string, ttlMs: number): boolean;
  persist(key: string): boolean;
  ttl(key: string): number;
  keys(): string[];
  size(): number;
  clear(): void;
  sweep(): number;
  dump(path: string): void;
  load(path: string): void;
}

export interface NativeBinding {
  Strenor: new () => NativeStrenorInstance;
}

// Works in both CJS and ESM output: createRequire + import.meta.url give a real
// require and a stable directory regardless of module format.
const require = createRequire(import.meta.url);
const here = dirname(fileURLToPath(import.meta.url));

// Single point that resolves the platform-specific `.node` binary.
// Strenor is built/published multiplatform via Vekziun (@vekziun/napi); this is
// where you wire whatever it emits. Candidates cover both the bundle dir and the
// package root.
const names = [
  'strenor.node',
  `strenor.${process.platform}-${process.arch}.node`,
  `strenor.${process.platform}-${process.arch}-gnu.node`,
  `strenor.${process.platform}-${process.arch}-msvc.node`,
];

const dirs = [here, join(here, '..')];

let native: NativeBinding | null = null;
let lastErr: unknown = null;

for (const dir of dirs) {
  for (const name of names) {
    const abs = join(dir, name);
    if (!existsSync(abs)) continue;
    try {
      native = require(abs) as NativeBinding;
      break;
    } catch (err) {
      lastErr = err;
    }
  }
  if (native) break;
}

if (!native) {
  const detail = lastErr instanceof Error ? `\nLast error: ${lastErr.message}` : '';
  throw new Error(
    `strenor: could not load native addon. Build it first (e.g. with Vekziun) and make sure the .node binary sits next to the compiled output.${detail}`
  );
}

const binding: NativeBinding = native;

export default binding;
