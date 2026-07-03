// Persisting sessions across restarts with dump/load.
import { rmSync } from 'node:fs';
import { Strenor } from '../dist/index.js';

const SNAP = './sessions.snap';
const sessions = new Strenor();

// restore (first run: no snapshot yet)
try {
  sessions.load(SNAP);
  console.log('restored:', sessions.keys());
} catch {
  console.log('no snapshot yet — first boot');
}

sessions.set('sess:alice', { step: 'menu', ts: Date.now() }, { ttl: 30 * 60_000 });
sessions.dump(SNAP);
console.log('saved:', sessions.get('sess:alice'));

rmSync(SNAP, { force: true }); // cleanup for the example
