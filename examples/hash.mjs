// Hashes: field maps — perfect for bot sessions and configs.
import { Strenor } from '../dist/index.js';

const db = new Strenor();

// A user session as a hash: each field is an independent Strenor value.
db.hset('session:alice', 'step', 'checkout');
db.hset('session:alice', 'cart', ['book', 'pen']); // arrays/objects via codecs
db.hset('session:alice', 'items', 2); //               numbers preserved

console.log('step: ', db.hget('session:alice', 'step')); // "checkout"
console.log('cart: ', db.hget('session:alice', 'cart')); // [ 'book', 'pen' ]
console.log('items:', db.hget('session:alice', 'items')); // 2 (number)

console.log('has cart?', db.hexists('session:alice', 'cart')); // true
console.log('fields:  ', db.hkeys('session:alice'));
console.log('count:   ', db.hlen('session:alice'));

// The whole session at once, decoded:
console.log('all:', db.hgetall('session:alice'));

// Advance the flow and drop a field.
db.hset('session:alice', 'step', 'done');
db.hdel('session:alice', 'cart');
console.log('after update:', db.hgetall('session:alice'));
