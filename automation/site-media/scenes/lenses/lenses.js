// The lenses loop: main, then ultra-wide, then tele, then main again, clicked on the real Camera card
const DURATION = 12;
window.FEED_LOOP = DURATION;
const clamp = (v, a = 0, b = 1) => Math.min(b, Math.max(a, v));
const lerp = (a, b, t) => a + (b - a) * t;
const E = {
  out3: t => 1 - Math.pow(1 - t, 3),
  io3: t => t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2,
  io5: t => t * t * t * (t * (6 * t - 15) + 10),
};
const ramp = (t, a, b, e = E.io3) => e(clamp((t - a) / (b - a)));
const $ = id => document.getElementById(id);
const vis = (el, o) => { el.style.opacity = (+o).toFixed(4); el.style.visibility = o <= 0.0005 ? "hidden" : "visible"; };

// the part of the window shown, in window coordinates, and how big it is drawn
const CROP = { x: 436, y: 50, w: 856, h: 470 }, S = 2.1;
const crop = $("crop");
crop.style.width = CROP.w * S + "px"; crop.style.height = CROP.h * S + "px";
crop.style.left = (1920 - CROP.w * S) / 2 + "px"; crop.style.top = (1080 - CROP.h * S) / 2 + "px";
$("world").style.transform = `translate(${-CROP.x * S}px, ${-CROP.y * S}px) scale(${S})`;

// each lens: what it frames of the drawing, how deep its focus is, and the ultra-wide's barrel
const LENS = {
  ultra: { view: [0, 0, 3200], bg: 0, mug: 0, barrel: 1 },
  main: { view: [800, 450, 1600], bg: 1.4, mug: 0.4, barrel: 0 },
  tele: { view: [1470, 760, 560], bg: 7, mug: 5, barrel: 0 },
};
const feeds = {};
for (const k of Object.keys(LENS)) {
  const f = makeFeed("main");
  $("feedbox").insertBefore(f, $("vign"));
  feeds[k] = f;
}

// barrel distortion map: each pixel samples further in towards the middle, most around halfway out
(() => {
  const w = 406, h = 228, c = document.createElement("canvas"); c.width = w; c.height = h;
  const g = c.getContext("2d"), im = g.createImageData(w, h), hd = Math.hypot(w / 2, h / 2), a = 0.35, SC = 60;
  for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) {
    const dx = x + 0.5 - w / 2, dy = y + 0.5 - h / 2, r = Math.hypot(dx, dy) / hd;
    const f = ((1 + a * r * r) / (1 + a)) - 1;
    const i = (y * w + x) * 4;
    im.data[i] = clamp(128 + dx * f / SC * 255, 0, 255); im.data[i + 1] = clamp(128 + dy * f / SC * 255, 0, 255); im.data[i + 2] = 128; im.data[i + 3] = 255;
  }
  g.putImageData(im, 0, 0);
  $("barrelmap").setAttribute("href", c.toDataURL());
})();

// [click time, lens it switches to, button centre]
const BTN = { ultra: [980, 156], main: [1097, 156], tele: [1214, 156] };
const SWITCHES = [[2.5, "ultra"], [6.2, "tele"], [9.8, "main"]];
const REST = [1104, 216];
const PATH = [
  [1.55, 2.4, REST, BTN.ultra, 26],
  [5.15, 6.1, BTN.ultra, BTN.tele, -30],
  [8.9, 9.7, BTN.tele, BTN.main, 18],
  [10.5, 11.5, BTN.main, REST, -14],
];
function cursorAt(t) {
  let p = REST;
  for (const [t0, t1, a, b, bend] of PATH) {
    if (t < t0) break;
    const k = E.io5(clamp((t - t0) / (t1 - t0)));
    const nx = -(b[1] - a[1]), ny = b[0] - a[0], l = Math.hypot(nx, ny) || 1, off = Math.sin(Math.PI * k) * bend;
    p = [lerp(a[0], b[0], k) + nx / l * off, lerp(a[1], b[1], k) + ny / l * off];
  }
  return p;
}

// stars, as on the site
const stars = (() => {
  let s = 5; const r = () => (s = (s * 16807) % 2147483647) / 2147483647;
  return Array.from({ length: 170 }, () => ({ x: r() * 1920, y: r() * 1080, z: r(), a: 0.1 + r() * 0.35, k: 1 + Math.floor(r() * 3), p: r() * 6.283 }));
})();
const sctx = $("stars").getContext("2d");
function drawStars(t) {
  sctx.clearRect(0, 0, 1920, 1080);
  sctx.fillStyle = "#dfe2ff";
  for (const st of stars) {
    sctx.globalAlpha = st.a * (0.65 + 0.35 * Math.sin(2 * Math.PI * st.k * t / DURATION + st.p));
    sctx.beginPath(); sctx.arc(st.x, st.y, 0.6 + st.z * 0.9, 0, 6.283); sctx.fill();
  }
  sctx.globalAlpha = 1;
}

function seek(t) {
  t = ((t % DURATION) + DURATION) % DURATION;
  drawStars(t);
  // which lens is on, and how far into the switch to the next one
  let from = "main", to = "main", k = 1, at = -9;
  for (const [c, lens] of SWITCHES) if (t >= c) { from = to; to = lens; at = c; }
  k = clamp((t - at - 0.06) / 0.34);
  // the card's highlight moves straight away, as the app does
  const btn = clamp((t - at) / 0.12);
  for (const [id, lens] of [["lultra", "ultra"], ["ltele", "tele"]]) {
    const on = lens === to ? btn : lens === from && btn < 1 ? 1 - btn : 0;
    vis($(id), on);
  }
  // the picture: the old lens pushes on a little and fades, the new one settles in from a touch larger
  const fade = E.io3(k);
  for (const lens of Object.keys(LENS)) {
    const L = LENS[lens], f = feeds[lens];
    let o = 0, push = 1;
    if (lens === to) { o = from === to ? 1 : fade; push = lerp(1.04, 1, E.out3(k)); }
    else if (lens === from) { o = 1; push = lerp(1, 0.965, E.io3(k)); }
    const [x, y, w] = L.view, nw = w * push;
    f.setView([x + (w - nw) / 2, y + (w - nw) * 9 / 32, nw]);
    f.style.zIndex = lens === to ? 2 : 1;
    // a lens hop is a cut softened by a moment of defocus, never a zoom between the two
    const soft = from === to ? 0 : Math.sin(Math.PI * k) * 2.2;
    f.style.filter = (L.barrel ? "url(#barrel) " : "") + (soft > 0.01 ? `blur(${soft.toFixed(2)}px)` : "");
    vis(f, o);
  }
  const A = LENS[from], B = LENS[to];
  vis($("vign"), lerp(A.barrel, B.barrel, fade));
  updateFeed({ t, blurBg: lerp(A.bg, B.bg, fade), blurMug: lerp(A.mug, B.mug, fade), blurCat: 0 });

  const [px, py] = cursorAt(t);
  const press = Math.max(...SWITCHES.map(([c]) => Math.max(0, 1 - Math.abs(t - c) / 0.09)));
  $("cursor").style.transform = `translate(${(px - 2).toFixed(2)}px, ${(py - 2).toFixed(2)}px) scale(${(1 - 0.12 * press).toFixed(3)})`;
  const c0 = SWITCHES.map(s => s[0]).find(c => t > c && t < c + 0.45), cl = $("click");
  if (c0 !== undefined) {
    const ck = (t - c0) / 0.45, [qx, qy] = cursorAt(c0);
    cl.style.left = qx + "px"; cl.style.top = qy + "px";
    cl.style.transform = `scale(${(0.5 + 0.9 * E.out3(ck)).toFixed(3)})`;
    vis(cl, 0.9 * (1 - ck));
  } else vis(cl, 0);
}

window.DURATION = DURATION;
window.seek = seek;
window.READY = (async () => {
  await document.fonts.ready;
  await Promise.all([...document.images].map(i => i.decode().catch(() => {})));
  seek(0);
  return true;
})();
