import { existsSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

/** Replay stats returned by the native layer when a log is attached. */
export interface NativeRecovery {
  applied: number;
  truncated: boolean;
}

/** Typed surface of the compiled Rust addon (no `.d.ts` ships with the .node). */
export interface NativeStrenorInstance {
  readonly recovery: NativeRecovery | null;
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
  pushFront(key: string, value: Buffer): number;
  pushBack(key: string, value: Buffer): number;
  popFront(key: string): Buffer | null;
  popBack(key: string): Buffer | null;
  llen(key: string): number;
  lrange(key: string, start: number, stop: number): Buffer[];
  hset(key: string, field: string, value: Buffer): boolean;
  hget(key: string, field: string): Buffer | null;
  hdel(key: string, field: string): boolean;
  hexists(key: string, field: string): boolean;
  hkeys(key: string): string[];
  hlen(key: string): number;
  hgetall(key: string): Buffer[];
  sadd(key: string, member: Buffer): boolean;
  srem(key: string, member: Buffer): boolean;
  sismember(key: string, member: Buffer): boolean;
  smembers(key: string): Buffer[];
  scard(key: string): number;
  zadd(key: string, score: number, member: Buffer): boolean;
  zincrby(key: string, delta: number, member: Buffer): number;
  zrem(key: string, member: Buffer): boolean;
  zscore(key: string, member: Buffer): number | null;
  zrank(key: string, member: Buffer): number | null;
  zcard(key: string): number;
  zrange(key: string, start: number, stop: number): Buffer[];
  zrangeWithScores(key: string, start: number, stop: number): Buffer[];
  txBegin(): void;
  txCommit(): void;
  txRollback(): void;
  inTransaction(): boolean;
  close(): void;
  hasAof(): boolean;
  aofSize(): number;
  compact(): number;
}

export interface NativeBinding {
  Strenor: new (aofPath?: string | null, fsync?: boolean | null) => NativeStrenorInstance;
}

// Self-contained loader using the @napi-rs/cli naming convention, so binaries
// produced by `napi build --platform` and packages published as
// optionalDependencies both resolve. Works in CJS and ESM output.
const require = createRequire(import.meta.url);
const here = dirname(fileURLToPath(import.meta.url));

/**
 * musl vs glibc detection — where most native addons silently break on
 * Alpine/Docker. glibc exposes glibcVersionRuntime in process.report; musl does not.
 */
function isMusl(): boolean {
  try {
    const report = process.report?.getReport?.() as
      | { header?: { glibcVersionRuntime?: string } }
      | undefined;
    return !report?.header?.glibcVersionRuntime;
  } catch {
    return false; // no process.report -> assume glibc (the common case)
  }
}

/** Platform suffix matching @napi-rs/cli (e.g. "linux-x64-gnu", "win32-arm64-msvc"). */
function currentSuffix(): string {
  const { platform, arch } = process;
  switch (platform) {
    case 'win32':
      return `win32-${arch}-msvc`;
    case 'darwin':
      return `darwin-${arch}`;
    case 'linux':
      return `linux-${arch}-${isMusl() ? 'musl' : 'gnu'}`;
    case 'android':
      return arch === 'arm' ? 'android-arm-eabi' : `android-${arch}`;
    default:
      throw new Error(`strenor: unsupported platform ${platform}-${arch}`);
  }
}

function load(): NativeBinding {
  const suffix = currentSuffix();
  // 1) local binary first (after `napi build --platform`) — dev/test without publishing.
  const local = join(here, '..', `strenor.${suffix}.node`);
  if (existsSync(local)) {
    return require(local) as NativeBinding;
  }
  // 2) platform package from optionalDependencies.
  const pkg = `@strenor/binary-${suffix}`;
  try {
    return require(pkg) as NativeBinding;
  } catch (err) {
    if ((err as NodeJS.ErrnoException)?.code !== 'MODULE_NOT_FOUND') throw err;
    throw new Error(
      `strenor: no native binary for "${suffix}".\n  - looked for local: ${local}\n  - looked for package: ${pkg}\nIs this platform unsupported, or did the optional dependency fail to install?`
    );
  }
}

const binding = load();

export default binding;
