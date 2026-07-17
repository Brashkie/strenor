// Durability: an append-only log makes state survive a restart or a crash.
import { rmSync } from 'node:fs';
import { Strenor } from '../dist/index.js';

const AOF = './bot.aof';
rmSync(AOF, { force: true }); // start clean so the example is repeatable

// ── First "process": every mutation is journalled as it happens ──────────
const first = new Strenor({ aof: AOF });
console.log('first boot, recovery:', first.recovery); // { applied: 0, truncated: false }

first.set('session:alice', { step: 'menu' });
first.enqueue('outbox', { to: 'bob', text: 'hi' });
first.incr('messages:sent');
first.incr('messages:sent');
console.log('sent:', first.get('messages:sent'), '| log:', first.aofSize(), 'bytes');
first.close(); // the process goes away

// ── Second "process": same log, state comes back ─────────────────────────
const second = new Strenor({ aof: AOF });
console.log('after restart, recovery:', second.recovery);
console.log('  session:', second.get('session:alice'));
console.log('  outbox: ', second.lrange('outbox', 0, -1));
console.log('  sent:   ', second.get('messages:sent')); // 2 — counters survive too

// ── Compaction: a busy queue appends forever, even holding one item ───────
for (let i = 0; i < 200; i++) {
  second.enqueue('work', { i });
  second.dequeue('work');
}
const before = second.aofSize();
console.log(`\nlog grew to ${before} bytes after 400 no-op ops`);
console.log('compacted to:', second.compact(), 'bytes — same state, less history');
console.log('  session still there:', second.get('session:alice'));

second.close();
rmSync(AOF, { force: true });
