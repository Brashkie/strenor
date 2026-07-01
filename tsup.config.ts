import { defineConfig } from 'tsup';

export default defineConfig({
  entry: { index: 'src/index.ts' },
  format: ['cjs', 'esm'],
  dts: true,
  sourcemap: true,
  clean: true,
  target: 'node16',
  platform: 'node',
  // shims:true makes tsup define import.meta.url in the CJS output; combined
  // with our own createRequire(import.meta.url) the native loader works in both
  // CJS and ESM (our require bypasses esbuild's dynamic-require stub).
  shims: true,
  minify: false,
  outDir: 'dist',
});
