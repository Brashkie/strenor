import { existsSync, rmSync, writeFileSync } from 'node:fs';
import { afterAll, beforeEach, describe, expect, it } from 'vitest';

// Import the compiled CJS output so the native loader (require/__dirname) works.
// The native `.node` must be built first (`npm run build:native`).
import { type Codec, Strenor, TAG, jsonCodec } from '../src/index.js';

const SNAP = './test.snapshot.tmp';

describe('Strenor', () => {
  let db: Strenor;

  beforeEach(() => {
    db = new Strenor();
  });

  afterAll(() => {
    if (existsSync(SNAP)) rmSync(SNAP, { force: true });
  });

  describe('smart set/get', () => {
    it('round-trips objects via the JSON codec', () => {
      db.set('user:1', { name: 'Brashkie', age: 20 });
      expect(db.get('user:1')).toEqual({ name: 'Brashkie', age: 20 });
    });

    it('round-trips strings', () => {
      db.set('hello', 'world');
      expect(db.get('hello')).toBe('world');
    });

    it('round-trips arrays and numbers as JSON', () => {
      db.set('list', [1, 2, 3]);
      db.set('n', 42);
      expect(db.get('list')).toEqual([1, 2, 3]);
      expect(db.get('n')).toBe(42);
    });

    it('round-trips Buffers as raw bytes', () => {
      const buf = Buffer.from([0, 255, 13, 37]);
      db.set('avatar', buf);
      const out = db.getBuffer('avatar');
      expect(Buffer.isBuffer(out)).toBe(true);
      expect(out?.equals(buf)).toBe(true);
    });

    it('returns null for missing keys', () => {
      expect(db.get('nope')).toBeNull();
    });
  });

  describe('typed helpers', () => {
    it('getString throws when the value is not a string', () => {
      db.set('obj', { a: 1 });
      expect(() => db.getString('obj')).toThrow(TypeError);
    });

    it('getBuffer throws when the value is not a Buffer', () => {
      db.setString('s', 'text');
      expect(() => db.getBuffer('s')).toThrow(TypeError);
    });

    it('setBuffer rejects non-Buffer input', () => {
      // @ts-expect-error intentional misuse
      expect(() => db.setBuffer('x', 'not a buffer')).toThrow(TypeError);
    });
  });

  describe('key management', () => {
    it('del / exists / size behave correctly', () => {
      db.set('a', 1);
      db.set('b', 2);
      expect(db.size()).toBe(2);
      expect(db.exists('a')).toBe(true);
      expect(db.del('a')).toBe(true);
      expect(db.del('a')).toBe(false);
      expect(db.exists('a')).toBe(false);
    });

    it('keys lists live keys and clear empties the store', () => {
      db.set('a', 1);
      db.set('b', 2);
      expect(db.keys().sort()).toEqual(['a', 'b']);
      db.clear();
      expect(db.size()).toBe(0);
    });
  });

  describe('TTL', () => {
    it('reports -1 for no expiry and -2 for missing', () => {
      db.set('persistent', 1);
      expect(db.ttl('persistent')).toBe(-1);
      expect(db.ttl('ghost')).toBe(-2);
    });

    it('expires keys after the TTL elapses', async () => {
      db.set('temp', 'v', { ttl: 40 });
      expect(db.ttl('temp')).toBeGreaterThan(0);
      await new Promise((r) => setTimeout(r, 70));
      expect(db.get('temp')).toBeNull();
    });

    it('expire() sets and persist() removes a TTL', () => {
      db.set('k', 1);
      expect(db.expire('k', 10_000)).toBe(true);
      expect(db.ttl('k')).toBeGreaterThan(0);
      expect(db.persist('k')).toBe(true);
      expect(db.ttl('k')).toBe(-1);
      expect(db.expire('missing', 1000)).toBe(false);
    });

    it('sweep() purges expired entries eagerly', async () => {
      db.set('x', 1, { ttl: 20 });
      db.set('y', 2);
      await new Promise((r) => setTimeout(r, 50));
      const removed = db.sweep();
      expect(removed).toBe(1);
      expect(db.size()).toBe(1);
    });
  });

  describe('persistence', () => {
    it('dumps and reloads state', () => {
      db.set('keep', { ok: true });
      db.set('name', 'strenor');
      db.dump(SNAP);

      const fresh = new Strenor();
      fresh.load(SNAP);
      expect(fresh.get('keep')).toEqual({ ok: true });
      expect(fresh.get('name')).toBe('strenor');
    });

    it('rejects a non-Strenor file', () => {
      rmSync(SNAP, { force: true });
      writeFileSync(SNAP, Buffer.from('not a snapshot at all'));
      const fresh = new Strenor();
      expect(() => fresh.load(SNAP)).toThrow();
    });
  });

  describe('codecs', () => {
    it('exposes the default JSON codec on tag 0x02', () => {
      expect(jsonCodec.tag).toBe(TAG.JSON);
    });

    it('supports a custom per-write codec and decodes it back', () => {
      // Trivial codec that uppercases strings on encode.
      const upper: Codec = {
        tag: 0x20,
        encode: (v) => Buffer.from(String(v).toUpperCase(), 'utf8'),
        decode: (b) => b.toString('utf8'),
      };
      db.registerCodec(upper);
      db.set('greet', 'hola', { codec: upper });
      expect(db.get('greet')).toBe('HOLA');
    });

    it('rejects custom codec tags outside 0x20..0xFE', () => {
      const bad: Codec = { tag: 0x05, encode: () => Buffer.alloc(0), decode: () => null };
      expect(() => db.registerCodec(bad)).toThrow(RangeError);
    });

    it('throws when reading a value whose tag has no codec', () => {
      // Write with a per-write codec (tag 0x30) that is never registered for reads.
      const ghost: Codec = {
        tag: 0x30,
        encode: (v) => Buffer.from(String(v), 'utf8'),
        decode: (b) => b.toString('utf8'),
      };
      db.set('ghost', 'x', { codec: ghost });
      expect(() => db.get('ghost')).toThrow(/unknown value tag/);
    });
  });

  describe('typed round-trips', () => {
    it('setString / getString round-trip and null for missing', () => {
      db.setString('s', 'hola');
      expect(db.getString('s')).toBe('hola');
      expect(db.getString('missing')).toBeNull();
    });

    it('setBuffer / getBuffer return null for missing', () => {
      expect(db.getBuffer('missing')).toBeNull();
    });

    it('setJSON / getJSON round-trip via the object codec', () => {
      db.setJSON('j', { a: 1, b: [2, 3] });
      expect(db.getJSON<{ a: number; b: number[] }>('j')).toEqual({ a: 1, b: [2, 3] });
      expect(db.getJSON('missing')).toBeNull();
    });

    it('typed setters accept a TTL option', () => {
      db.setString('s-ttl', 'v', { ttl: 5000 });
      db.setBuffer('b-ttl', Buffer.from([1]), { ttl: 5000 });
      db.setJSON('j-ttl', { ok: true }, { ttl: 5000 });
      expect(db.ttl('s-ttl')).toBeGreaterThan(0);
      expect(db.ttl('b-ttl')).toBeGreaterThan(0);
      expect(db.ttl('j-ttl')).toBeGreaterThan(0);
    });
  });

  describe('counters', () => {
    it('incr/decr are atomic and create the key at 0', () => {
      expect(db.incr('c')).toBe(1);
      expect(db.incr('c')).toBe(2);
      expect(db.incr('c', 5)).toBe(7);
      expect(db.decr('c')).toBe(6);
      expect(db.decr('c', 2)).toBe(4);
      expect(db.get('c')).toBe(4); // readable as a plain number
    });

    it('rejects non-numeric and non-integer values', () => {
      db.set('s', 'hola');
      expect(() => db.incr('s')).toThrow(TypeError);
      db.set('f', 1.5);
      expect(() => db.incr('f')).toThrow(/integer/);
    });

    it('rejects overflow beyond the safe integer range', () => {
      db.set('big', Number.MAX_SAFE_INTEGER);
      expect(() => db.incr('big')).toThrow(RangeError);
    });

    it('throws WRONGTYPE when the key holds a list', () => {
      db.enqueue('l', 'x');
      expect(() => db.incr('l')).toThrow(/WRONGTYPE/);
    });
  });

  describe('lists', () => {
    it('enqueue/dequeue is FIFO and returns length / null', () => {
      expect(db.enqueue('q', 'a')).toBe(1);
      expect(db.enqueue('q', 'b')).toBe(2);
      expect(db.llen('q')).toBe(2);
      expect(db.dequeue('q')).toBe('a');
      expect(db.dequeue('q')).toBe('b');
      expect(db.dequeue('q')).toBeNull(); // empty
      expect(db.exists('q')).toBe(false); // empty list key removed
    });

    it('push/pop is LIFO', () => {
      expect(db.push('s', 1)).toBe(1);
      db.push('s', 2);
      expect(db.pop('s')).toBe(2);
      expect(db.pop('s')).toBe(1);
      expect(db.pop('s')).toBeNull();
    });

    it('preserves element types via codecs', () => {
      db.enqueue('mix', { id: 1 });
      db.enqueue('mix', 'hola');
      db.enqueue('mix', Buffer.from([1, 2, 3]));
      expect(db.dequeue('mix')).toEqual({ id: 1 });
      expect(db.dequeue('mix')).toBe('hola');
      const buf = db.dequeue<Buffer>('mix');
      expect(Buffer.isBuffer(buf) && buf.equals(Buffer.from([1, 2, 3]))).toBe(true);
    });

    it('lrange supports positive and negative indices', () => {
      for (const c of ['a', 'b', 'c', 'd']) db.enqueue('l', c);
      expect(db.lrange('l', 0, -1)).toEqual(['a', 'b', 'c', 'd']);
      expect(db.lrange('l', 1, 2)).toEqual(['b', 'c']);
      expect(db.lrange('l', -2, -1)).toEqual(['c', 'd']);
      expect(db.lrange('missing', 0, -1)).toEqual([]);
      expect(db.llen('missing')).toBe(0);
    });

    it('supports Redis-style directional aliases', () => {
      expect(db.rpush('r', 'tail')).toBe(1);
      expect(db.lpush('r', 'head')).toBe(2);
      expect(db.lrange('r', 0, -1)).toEqual(['head', 'tail']);
      expect(db.lpop('r')).toBe('head');
      expect(db.rpop('r')).toBe('tail');
      expect(db.lpop('r')).toBeNull();
      expect(db.rpop('r')).toBeNull();
    });

    it('lists honor TTL', () => {
      db.enqueue('tl', 'x');
      expect(db.expire('tl', 5000)).toBe(true);
      expect(db.ttl('tl')).toBeGreaterThan(0);
    });

    it('throws WRONGTYPE on mismatched operations', () => {
      db.set('bytes', 'hola');
      expect(() => db.enqueue('bytes', 'x')).toThrow(/WRONGTYPE/);
      expect(() => db.dequeue('bytes')).toThrow(/WRONGTYPE/);
      expect(() => db.llen('bytes')).toThrow(/WRONGTYPE/);
      expect(() => db.lrange('bytes', 0, -1)).toThrow(/WRONGTYPE/);

      db.enqueue('list', 'x');
      expect(() => db.get('list')).toThrow(/WRONGTYPE/);
    });
  });

  describe('lifecycle', () => {
    it('close() stops a background sweeper without throwing', () => {
      const withSweeper = new Strenor({ sweepInterval: 10 });
      expect(() => withSweeper.close()).not.toThrow();
    });
  });
});
