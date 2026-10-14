// node jq.js <file> keys <key>          -> paths where key occurs (first 30)
// node jq.js <file> first <key> [depth] -> first value under key, pruned to depth
// node jq.js <file> nth <key> <n> [depth]
const fs = require('fs');
const [, , file, cmd, key, a, b] = process.argv;
const root = JSON.parse(fs.readFileSync(file, 'utf8'));
const hits = [];
(function walk(v, path) {
  if (v && typeof v === 'object') {
    for (const [k, c] of Object.entries(v)) {
      const p = path + (Array.isArray(v) ? `[${k}]` : `.${k}`);
      if (k === key) hits.push([p, c]);
      walk(c, p);
    }
  }
})(root, '');
const prune = (v, d) => {
  if (v && typeof v === 'object') {
    if (d <= 0) return Array.isArray(v) ? `[${v.length}]` : '{' + Object.keys(v).join(',') + '}';
    if (Array.isArray(v)) return v.slice(0, 6).map(x => prune(x, d - 1));
    const o = {};
    for (const [k, c] of Object.entries(v)) if (k !== 'trackingParams' && k !== 'clickTrackingParams' && k !== 'loggingContext') o[k] = prune(c, d - 1);
    return o;
  }
  return v;
};
if (cmd === 'keys') { console.log(hits.length + ' hits'); hits.slice(0, +a || 30).forEach(h => console.log(h[0])); }
if (cmd === 'first') console.log(JSON.stringify(prune(hits[0] && hits[0][1], +a || 8), null, 1));
if (cmd === 'nth') console.log(JSON.stringify(prune(hits[+a] && hits[+a][1], +b || 8), null, 1));
