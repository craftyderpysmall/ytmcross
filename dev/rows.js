// node rows.js <file> [max]: summarises list rows, two-row cards and shelves.
const r = JSON.parse(require('fs').readFileSync(process.argv[2], 'utf8'));
const max = +process.argv[3] || 6;
const runs = t => (t?.runs || []).map(x => x.text + (x.navigationEndpoint?.browseEndpoint ? `[${x.navigationEndpoint.browseEndpoint.browseId}]` : '') + (x.navigationEndpoint?.watchEndpoint ? `{w:${x.navigationEndpoint.watchEndpoint.videoId}}` : '')).join('');
const counts = {};
(function w(v, path) {
  if (!v || typeof v !== 'object') return;
  for (const [k, c] of Object.entries(v)) {
    if (k === 'musicShelfRenderer' || k === 'musicCarouselShelfRenderer' || k === 'gridRenderer' || k === 'musicCardShelfRenderer' || k === 'musicPlaylistShelfRenderer') {
      const title = runs(c.title) || runs(c.header?.musicCarouselShelfBasicHeaderRenderer?.title) || runs(c.header?.gridHeaderRenderer?.title);
      console.log(`== ${k}: ${title}  (${(c.contents || c.items || []).length} items)`);
      if (k === 'musicCardShelfRenderer') console.log('   card nav:', JSON.stringify(c.title?.runs?.[0]?.navigationEndpoint || c.thumbnail && 'thumb').slice(0, 200), '| subtitle:', runs(c.subtitle));
    }
    if (k === 'musicResponsiveListItemRenderer' || k === 'musicTwoRowItemRenderer') {
      counts[k] = (counts[k] || 0) + 1;
      if (counts[k] <= max) {
        const nav = c.navigationEndpoint?.browseEndpoint;
        const cols = k === 'musicTwoRowItemRenderer' ? [runs(c.title), runs(c.subtitle)] : (c.flexColumns || []).map(f => runs(f.musicResponsiveListItemFlexColumnRenderer?.text));
        const fixed = (c.fixedColumns || []).map(f => runs(f.musicResponsiveListItemFixedColumnRenderer?.text));
        const pl = JSON.stringify(c.overlay || c.thumbnailOverlay || {}).match(/"playlistId":"[^"]+"/);
        console.log(`  ${k === 'musicTwoRowItemRenderer' ? 'card' : 'row'}: ${cols.join(' | ')} ${fixed.length ? '|| ' + fixed.join(',') : ''} nav=${nav ? nav.browseId + ' ' + (nav.browseEndpointContextSupportedConfigs?.browseEndpointContextMusicConfig?.pageType || '') : '-'} ${pl ? pl[0] : ''} vid=${c.playlistItemData?.videoId || ''}`);
      }
    }
    w(c);
  }
})(r);
