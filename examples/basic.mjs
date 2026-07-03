// Basic usage: the smart API dispatches by value type.
// Run after building:  npm run build:native && npm run build && node examples/basic.mjs
import { Strenor } from '../dist/index.js';

const db = new Strenor();

db.set('user:1', { name: 'Brashkie', age: 20 }); // object -> JSON
db.set('token', 'abc123'); //                       string -> UTF-8
db.set('avatar', Buffer.from([0x89, 0x50, 0x4e, 0x47])); // Buffer -> raw bytes

console.log('object:', db.get('user:1'));
console.log('string:', db.get('token'));
console.log('buffer:', db.getBuffer('avatar'));
console.log('size:  ', db.size());
