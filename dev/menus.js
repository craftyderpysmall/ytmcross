// node menus.js <file> <rendererKey> [max]: menu item labels (+ endpoint kinds) per item.
const r = JSON.parse(require('fs').readFileSync(process.argv[2], 'utf8'));
const key = process.argv[3], max = +process.argv[4] || 10;
let n = 0;
const txt = t => (t?.runs || []).map(x => x.text).join('') || t?.simpleText || '';
(function w(v) {
  if (!v || typeof v !== 'object') return;
  for (const [k, c] of Object.entries(v)) {
    if (k === key && n++ < max) {
      const title = txt(c.title) || txt(c.flexColumns?.[0]?.musicResponsiveListItemFlexColumnRenderer?.text);
      const items = (c.menu?.menuRenderer?.items || []).map(i => {
        const [kind, body] = Object.entries(i)[0];
        const label = txt(body.text) || txt(body.defaultText);
        const ep = body.serviceEndpoint || body.navigationEndpoint || body.defaultServiceEndpoint || {};
        const epk = Object.keys(ep).filter(x => x !== 'clickTrackingParams');
        let extra = '';
        if (ep.likeEndpoint) extra = JSON.stringify(ep.likeEndpoint.target) + ep.likeEndpoint.status;
        if (ep.confirmDialogEndpoint) extra = 'confirm';
        return `${label}<${epk.join(',')}${extra ? ' ' + extra : ''}>${body.isToggled !== undefined ? ' toggled=' + body.isToggled : ''}`;
      });
      const top = (c.menu?.menuRenderer?.topLevelButtons || []).map(b => Object.keys(b)[0] + ':' + JSON.stringify(Object.values(b)[0].likeStatus || ''));
      console.log(`* ${title}\n    ${items.join('\n    ')}\n    top: ${top.join(', ')}`);
    }
    w(c);
  }
})(r);
