// Custom codec: the default object codec is JSON; swap or add your own.
// Tags 0x20..0xFE are reserved for custom codecs.
import { Strenor } from '../dist/index.js';

// A toy codec that stores objects as "key=value;" text (illustrative only).
const kvText = {
  tag: 0x20,
  encode: (v) =>
    Buffer.from(
      Object.entries(v)
        .map(([k, x]) => `${k}=${x}`)
        .join(';'),
      'utf8'
    ),
  decode: (b) =>
    Object.fromEntries(
      b
        .toString('utf8')
        .split(';')
        .filter(Boolean)
        .map((pair) => pair.split('='))
    ),
};

const db = new Strenor();
db.registerCodec(kvText);
db.set('cfg', { theme: 'dark', lang: 'es' }, { codec: kvText });

console.log('decoded:', db.get('cfg')); // { theme: 'dark', lang: 'es' }
