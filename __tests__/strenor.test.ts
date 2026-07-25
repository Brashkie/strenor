import { appendFileSync, existsSync, rmSync, writeFileSync } from 'node:fs';
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

  describe('hashes', () => {
    it('hset/hget round-trip values via codecs', () => {
      expect(db.hset('u:1', 'name', 'alice')).toBe(true); // new field
      expect(db.hset('u:1', 'name', 'bob')).toBe(false); // overwrite
      db.hset('u:1', 'age', 20);
      db.hset('u:1', 'meta', { admin: true });
      expect(db.hget('u:1', 'name')).toBe('bob');
      expect(db.hget<number>('u:1', 'age')).toBe(20); // number preserved
      expect(db.hget('u:1', 'meta')).toEqual({ admin: true });
      expect(db.hget('u:1', 'missing')).toBeNull();
      expect(db.hget('nokey', 'f')).toBeNull();
    });

    it('hexists / hkeys / hlen', () => {
      db.hset('h', 'a', 1);
      db.hset('h', 'b', 2);
      expect(db.hlen('h')).toBe(2);
      expect(db.hexists('h', 'a')).toBe(true);
      expect(db.hexists('h', 'z')).toBe(false);
      expect(db.hkeys('h').sort()).toEqual(['a', 'b']);
      expect(db.hlen('missing')).toBe(0);
      expect(db.hkeys('missing')).toEqual([]);
      expect(db.hexists('missing', 'a')).toBe(false);
    });

    it('hgetall returns a decoded object', () => {
      db.hset('cfg', 'theme', 'dark');
      db.hset('cfg', 'count', 3);
      expect(db.hgetall('cfg')).toEqual({ theme: 'dark', count: 3 });
      expect(db.hgetall('missing')).toEqual({});
    });

    it('hdel removes fields and deletes the emptied key', () => {
      db.hset('h', 'only', 'v');
      expect(db.hdel('h', 'only')).toBe(true);
      expect(db.hdel('h', 'only')).toBe(false);
      expect(db.exists('h')).toBe(false); // last field gone -> key removed
    });

    it('throws WRONGTYPE on mismatched operations', () => {
      db.set('str', 'hola');
      expect(() => db.hset('str', 'f', 1)).toThrow(/WRONGTYPE/);
      expect(() => db.hget('str', 'f')).toThrow(/WRONGTYPE/);
      expect(() => db.hdel('str', 'f')).toThrow(/WRONGTYPE/);
      expect(() => db.hexists('str', 'f')).toThrow(/WRONGTYPE/);
      expect(() => db.hkeys('str')).toThrow(/WRONGTYPE/);
      expect(() => db.hlen('str')).toThrow(/WRONGTYPE/);
      expect(() => db.hgetall('str')).toThrow(/WRONGTYPE/);

      db.hset('h', 'f', 'v');
      expect(() => db.get('h')).toThrow(/WRONGTYPE/); // a hash is not bytes
    });

    it('hashes survive a snapshot round-trip', () => {
      db.hset('u:1', 'name', 'alice');
      db.hset('u:1', 'age', 20);
      db.dump(SNAP);
      const fresh = new Strenor();
      fresh.load(SNAP);
      expect(fresh.hgetall('u:1')).toEqual({ name: 'alice', age: 20 });
    });
  });

  describe('durability (append-only log)', () => {
    const AOF = './test.aof.tmp';
    const clean = () => {
      for (const f of [AOF, `${AOF}.compact`]) if (existsSync(f)) rmSync(f, { force: true });
    };
    beforeEach(clean);
    afterAll(clean);

    it('is memory-only by default', () => {
      expect(db.durable).toBe(false);
      expect(db.recovery).toBeNull();
      expect(db.aofSize()).toBe(0);
      expect(db.compact()).toBe(0); // no-op, not an error
    });

    it('survives a restart by replaying the log', () => {
      const first = new Strenor({ aof: AOF });
      expect(first.durable).toBe(true);
      expect(first.recovery).toEqual({ applied: 0, truncated: false });
      first.set('user', { name: 'brashkie' });
      first.enqueue('jobs', { id: 1 });
      first.enqueue('jobs', { id: 2 });
      first.dequeue('jobs');
      first.incr('hits', 5);
      first.close();

      // A brand-new instance reading the same log = a process restart.
      const reopened = new Strenor({ aof: AOF });
      expect(reopened.recovery?.truncated).toBe(false);
      expect(reopened.recovery?.applied).toBeGreaterThan(0);
      expect(reopened.get('user')).toEqual({ name: 'brashkie' });
      expect(reopened.lrange('jobs', 0, -1)).toEqual([{ id: 2 }]);
      expect(reopened.get('hits')).toBe(5);
      reopened.close();
    });

    it('recovers from a crash that tore the last write', () => {
      const first = new Strenor({ aof: AOF });
      first.set('good', 'kept');
      first.close();
      // Garbage appended = the process died mid-write.
      appendFileSync(AOF, Buffer.from([0xff, 0x00, 0x00, 0x00, 0xde, 0xad]));

      const reopened = new Strenor({ aof: AOF });
      expect(reopened.recovery?.truncated).toBe(true); // reported, not fatal
      expect(reopened.get('good')).toBe('kept'); // intact data survived
      reopened.close();
    });

    it('close() releases the log so the file can be deleted and reopened', () => {
      // On Windows an open handle leaves the file "delete pending", and the next
      // open fails with Access Denied. close() must release it for real.
      const first = new Strenor({ aof: AOF });
      first.set('a', 1);
      first.close();
      expect(() => rmSync(AOF, { force: true })).not.toThrow();

      const fresh = new Strenor({ aof: AOF });
      expect(fresh.recovery).toEqual({ applied: 0, truncated: false });
      fresh.set('b', 2);
      expect(fresh.get('b')).toBe(2);
      fresh.close();
    });

    it('reads still work after close but writes throw', () => {
      const db2 = new Strenor({ aof: AOF });
      db2.set('kept', 'value');
      db2.close();
      expect(db2.get('kept')).toBe('value'); // memory is still readable
      expect(() => db2.set('x', 1)).toThrow(/closed/); // never silently unjournalled
      expect(() => db2.enqueue('q', 1)).toThrow(/closed/);
      expect(() => db2.compact()).toThrow(/closed/);
      expect(() => db2.close()).not.toThrow(); // idempotent
    });

    it('compaction shrinks the log and preserves state', () => {
      const db2 = new Strenor({ aof: AOF });
      for (let i = 0; i < 100; i++) {
        db2.enqueue('q', { i });
        db2.dequeue('q');
      }
      db2.set('keep', 'value');
      db2.enqueue('q', 'last');

      const before = db2.aofSize();
      const after = db2.compact();
      expect(after).toBeLessThan(before);
      db2.close();

      const reopened = new Strenor({ aof: AOF });
      expect(reopened.get('keep')).toBe('value');
      expect(reopened.lrange('q', 0, -1)).toEqual(['last']);
      reopened.close();
    });
  });

  describe('lifecycle', () => {
    it('close() stops a background sweeper without throwing', () => {
      const withSweeper = new Strenor({ sweepInterval: 10 });
      expect(() => withSweeper.close()).not.toThrow();
    });
  });
});
