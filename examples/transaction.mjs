// Transactions: all-or-nothing. A transfer that must never half-apply.
import { Strenor } from '../dist/index.js';

const db = new Strenor();
db.set('alice', 100);
db.set('bob', 50);

function transfer(from, to, amount) {
  db.transaction(() => {
    const balance = db.get(from);
    if (balance < amount) throw new Error('insufficient funds');
    db.set(from, balance - amount);
    db.set(to, db.get(to) + amount);
  });
}

// A valid transfer commits.
transfer('alice', 'bob', 30);
console.log('after transfer:', { alice: db.get('alice'), bob: db.get('bob') });

// An invalid one rolls back — no partial state.
try {
  transfer('alice', 'bob', 9999);
} catch (e) {
  console.log('rejected:', e.message);
}
console.log('unchanged:  ', { alice: db.get('alice'), bob: db.get('bob') });

// batch: many writes, one journal pass (no rollback needed).
db.batch(() => {
  for (let i = 0; i < 1000; i++) db.set(`k:${i}`, i);
});
console.log('batch wrote:', db.size(), 'keys');
