// Sets: unique members — deduplication, "seen" tracking, unique visitors.
import { Strenor } from '../dist/index.js';

const db = new Strenor();

// Track unique users online — adding twice is a no-op.
db.sadd('online', 'alice');
db.sadd('online', 'bob');
db.sadd('online', 'alice'); // duplicate, ignored
console.log('online count:', db.scard('online')); // 2
console.log('is alice on?', db.sismember('online', 'alice')); // true
console.log('members:', db.smembers('online').sort());

// Deduplicate processed message IDs so you never handle one twice.
const ids = [101, 102, 101, 103, 102];
for (const id of ids) {
  if (db.sadd('processed', id)) {
    console.log('handling new message', id);
  } else {
    console.log('skipping duplicate', id);
  }
}
console.log('unique processed:', db.scard('processed')); // 3

db.srem('online', 'bob');
console.log('after bob leaves:', db.smembers('online'));
