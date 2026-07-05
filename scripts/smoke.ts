// Quick end-to-end smoke test (run with: npm run smoke).
// Builds the package first (presmoke) and exercises the public API against
// the compiled output in dist/. The native addon must be built (root .node).

import { rmSync } from 'node:fs';
import { Strenor } from '../dist/index.js';

const db = new Strenor();
let failed = 0;

function check(name: string, cond: boolean): void {
  if (cond) {
    console.log(`  ok   ${name}`);
  } else {
    console.error(`  FAIL ${name}`);
    failed++;
  }
}

db.set('hello', 'world');
check('string round-trip', db.get('hello') === 'world');

db.set('user:1', { name: 'Brashkie', age: 20 });
const user = db.get<{ name: string; age: number }>('user:1');
check('object round-trip', user?.name === 'Brashkie' && user?.age === 20);

const buf = Buffer.from([1, 2, 3, 4]);
db.setBuffer('avatar', buf);
const got = db.getBuffer('avatar');
check('buffer round-trip', Buffer.isBuffer(got) && got.equals(buf));

check('exists true', db.exists('hello') === true);
db.del('hello');
check('exists false after del', db.exists('hello') === false);

db.set('session', 'token', { ttl: 50 });
check('ttl positive', db.ttl('session') > 0);
await new Promise((r) => setTimeout(r, 80));
check('expired after ttl', db.get('session') === null);

const snap = './strenor.smoke.snap';
db.set('persisted', { ok: true });
db.enqueue('jobs', { id: 1 });
db.enqueue('jobs', { id: 2 });
check('list FIFO', db.dequeue<{ id: number }>('jobs')?.id === 1 && db.llen('jobs') === 1);
db.dump(snap);

const db2 = new Strenor();
db2.load(snap);
check('snapshot reload', db2.get<{ ok: boolean }>('persisted')?.ok === true);
rmSync(snap, { force: true });

console.log(failed === 0 ? '\nALL PASSED' : `\n${failed} CHECK(S) FAILED`);
process.exit(failed === 0 ? 0 : 1);
