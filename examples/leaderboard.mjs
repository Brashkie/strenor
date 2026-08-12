// Sorted sets: a game leaderboard ranked by score.
import { Strenor } from '../dist/index.js';

const db = new Strenor();

db.zadd('scores', 1500, 'alice');
db.zadd('scores', 900, 'bob');
db.zadd('scores', 1200, 'carol');

// Award points during play — zincrby adds to the current score.
db.zincrby('scores', 400, 'bob'); // bob: 900 -> 1300

// Top 3, highest first: take the last 3 by rank and reverse.
const top = db.zrangeWithScores('scores', -3, -1).reverse();
console.log('🏆 Leaderboard:');
top.forEach((e, i) => console.log(`  ${i + 1}. ${e.member} — ${e.score}`));

// Where does a specific player stand?
console.log("\nbob's score:", db.zscore('scores', 'bob'));
// rank is low-to-high (0 = lowest); flip it for "place from the top".
const place = db.zcard('scores') - db.zrank('scores', 'bob');
console.log(`bob is in place #${place} of ${db.zcard('scores')}`);
