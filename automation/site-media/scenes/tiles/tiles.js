// One seekable page per shot: CFG (set by the shot's own html) says what to draw; seek(t) draws the frame at t
const CFG = window.CFG;
const DURATION = CFG.L;
window.FEED_LOOP = CFG.L;
const clamp = (v, a = 0, b = 1) => Math.min(b, Math.max(a, v));
const lerp = (a, b, k) => a + (b - a) * k;
const E = {
  io3: t => t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2,
  io5: t => t * t * t * (t * (6 * t - 15) + 10),
};
const mix = (a, b, k) => Array.isArray(a) ? a.map((v, i) => lerp(v, b[i], k)) : lerp(a, b, k);
function track(keys) {
  return t => {
    if (t <= keys[0][0]) return keys[0][1];
    for (let i = 1; i < keys.length; i++) {
      const [t1, v1, e] = keys[i];
      if (t < t1) { const [t0, v0] = keys[i - 1]; return mix(v0, v1, (e || E.io5)((t - t0) / (t1 - t0))); }
    }
    return keys[keys.length - 1][1];
  };
}
const $ = id => document.getElementById(id);

let seek;
if (CFG.kind === "feed") {
  const feed = makeFeed("main");
  $("stage").appendChild(feed);
  if (CFG.green) document.getElementById("fs-green").setAttribute("opacity", "1");
  for (const id of CFG.hide || []) document.getElementById(id).setAttribute("display", "none");
  const view = CFG.views ? track(CFG.views) : () => CFG.view;
  // the cat sits nearly still: a blink, a breath, one slow ear flick a loop
  const beats = 4 * Math.max(1, Math.round(CFG.L / 6));
  seek = t => {
    updateFeed({ t, vb: t * beats / CFG.L, level: 0.1, blurBg: CFG.blurBg ?? 1.2, blurMug: 0.3, blurCat: 0, wave: 0, torch: 0, squint: 0,
      twitch: 0.5, twitchEvery: 1e9, lookK: 0.35, noShoot: !!CFG.noShoot });
    feed.setView(view(t));
  };
} else if (CFG.kind === "desk") {
  seek = () => {};
  // where the sheet's corners land on screen, for the compositor
  const pts = [...document.querySelectorAll(".pin")].map(p => { const r = p.getBoundingClientRect(); return [r.left + r.width / 2, r.top + r.height / 2]; });
  window.CORNERS = pts;
}
window.seek = seek;
window.READY = true;
