// Prints thumbnail URLs (largest of each list) from probe files, resized like images.rs does.
const fs = require('fs');
const urls = new Set();
for (const f of process.argv.slice(2)) {
  (function w(v) {
    if (!v || typeof v !== 'object') return;
    if (Array.isArray(v.thumbnails) && v.thumbnails.length) urls.add(v.thumbnails[v.thumbnails.length - 1].url);
    for (const c of Object.values(v)) w(c);
  })(JSON.parse(fs.readFileSync(f, 'utf8')));
}
for (let u of urls) {
  if ((u.includes('googleusercontent.com') || u.includes('ggpht.com')) && u.lastIndexOf('=') > 0) u = u.slice(0, u.lastIndexOf('=')) + '=w120-h120-l90-rj';
  console.log(u);
}
