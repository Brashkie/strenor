// Cache with TTL and a background sweeper.
import { Strenor } from '../dist/index.js';

const cache = new Strenor({ sweepInterval: 60_000 });

function getUser(id) {
  const hit = cache.get(`user:${id}`);
  if (hit) return { ...hit, cached: true };
  const user = { id, name: `user-${id}` }; // pretend this is an API call
  cache.set(`user:${id}`, user, { ttl: 5 * 60_000 }); // cache 5 minutes
  return { ...user, cached: false };
}

console.log(getUser('42')); // miss
console.log(getUser('42')); // hit
console.log('ttl(ms):', cache.ttl('user:42'));
cache.close(); // stop the sweeper
