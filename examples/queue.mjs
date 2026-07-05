// Lists as a message queue (FIFO) — what WinsiBot uses for pending messages.
import { Strenor } from '../dist/index.js';

const db = new Strenor();

// producer: enqueue jobs (objects, preserved via the JSON codec)
db.enqueue('messages', { to: 'alice', text: 'hi' });
db.enqueue('messages', { to: 'bob', text: 'hey' });
console.log('pending:', db.llen('messages')); // 2
console.log('peek:', db.lrange('messages', 0, -1)); // both, without removing

// consumer: drain in order
while (true) {
  const job = db.dequeue('messages');
  if (job === null) break;
  console.log('processing:', job);
}
console.log('drained, exists:', db.exists('messages')); // false (empty list removed)
