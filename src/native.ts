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
  const pkg = `strenor-${suffix}`;
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
