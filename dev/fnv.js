// FNV-1a 64 of a file with CR removed (as config.rs computes it).
const t = require('fs').readFileSync(process.argv[2], 'utf8').replace(/\r/g, '');
let h = 0xcbf29ce484222325n;
for (const b of Buffer.from(t, 'utf8')) { h ^= BigInt(b); h = (h * 0x100000001b3n) & 0xffffffffffffffffn; }
console.log('0x' + h.toString(16).padStart(16, '0'));
