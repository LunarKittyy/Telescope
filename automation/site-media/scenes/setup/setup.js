// The setup loop: seek(t) draws the frame at t seconds; frame 0 and frame DURATION are the same picture
const STORY = 15;  // the animation is written in story seconds; holds stretch it in real time
// [story second, real seconds of stillness added there]: each sits where nothing in the story is moving
const HOLDS = [[0.4, 1.0], [3.27, 0.9], [4.9, 0.8], [5.66, 1.3], [7.0, 0.6], [9.6, 1.6], [13.3, 1.7], [14.9, 0.5]];
const PACE = 1.1;  // and everything plays a little slower than written
const DURATION = +(STORY * PACE + HOLDS.reduce((a, h) => a + h[1], 0)).toFixed(3);
window.FEED_LOOP = DURATION;
function story(r) {
  let u = 0, real = 0;
  for (const [at, hold] of HOLDS) {
    const run = (at - u) * PACE;
    if (r < real + run) return u + (r - real) / PACE;
    real += run; u = at;
    if (r < real + hold) return u;
    real += hold;
  }
  return Math.min(STORY, u + (r - real) / PACE);
}
const realOf = u => { let lo = 0, hi = DURATION; for (let i = 0; i < 60; i++) { const m = (lo + hi) / 2; if (story(m) < u) lo = m; else hi = m; } return +lo.toFixed(2); };
const clamp = (v, a = 0, b = 1) => Math.min(b, Math.max(a, v));
const lerp = (a, b, t) => a + (b - a) * t;
const E = {
  lin: t => t,
  out3: t => 1 - Math.pow(1 - t, 3),
  out5: t => 1 - Math.pow(1 - t, 5),
  expo: t => t >= 1 ? 1 : 1 - Math.pow(2, -10 * t),
  io3: t => t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2,
  io5: t => t * t * t * (t * (6 * t - 15) + 10),
};
const ramp = (t, a, b, e = E.io3) => e(clamp((t - a) / (b - a)));
const mix = (a, b, k) => Array.isArray(a) ? a.map((v, i) => lerp(v, b[i], k)) : lerp(a, b, k);
// keyframes [[t, value, ease]]; the ease on a key shapes the move that ends there
function track(keys) {
  return t => {
    if (t <= keys[0][0]) return keys[0][1];
    for (let i = 1; i < keys.length; i++) {
      const [t1, v1, e] = keys[i];
      if (t < t1) { const [t0, v0] = keys[i - 1]; return mix(v0, v1, (e || E.io3)((t - t0) / (t1 - t0))); }
    }
    return keys[keys.length - 1][1];
  };
}
const $ = id => document.getElementById(id);
const vis = (el, o) => { el.style.opacity = (+o).toFixed(4); el.style.visibility = o <= 0.0005 ? "hidden" : "visible"; };

// ── camera on the monitor (world = the app window's own 1300x760 coordinates) ──
const BASE = [650, 380, 1.18];
const camera = track([
  [0, BASE], [2.1, BASE], [2.9, [650, 410, 1.2], E.io5], [7.25, [650, 410, 1.2]],
  [8.15, [770, 300, 1.27], E.io5], [8.5, [770, 300, 1.27]], [9.6, [655, 368, 1.24], E.io5],
  [10.5, [1018, 418, 1.36], E.io5], [13.6, [1018, 418, 1.36]], [14.7, BASE, E.io5], [STORY, BASE],
]);
const worldToScreen = (cam, x, y) => [(x - cam[0]) * cam[2] + 960, (y - cam[1]) * cam[2] + 540];
const screenToWorld = (cam, x, y) => [(x - 960) / cam[2] + cam[0], (y - 540) / cam[2] + cam[1]];

// ── stars: a fixed field that twinkles on periods that divide the loop ──
const stars = (() => {
  let s = 11; const r = () => (s = (s * 16807) % 2147483647) / 2147483647;
  return Array.from({ length: 190 }, () => ({ x: r() * 2200 - 140, y: r() * 1300 - 110, z: 0.3 + r() * 0.7, a: 0.12 + r() * 0.4, k: 1 + Math.floor(r() * 3), p: r() * 6.283 }));
})();
const sctx = $("stars").getContext("2d");
function drawStars(t, cam) {
  sctx.clearRect(0, 0, 1920, 1080);
  const dx = (cam[0] - BASE[0]) * 0.06, dy = (cam[1] - BASE[1]) * 0.06;
  for (const st of stars) {
    const tw = 0.65 + 0.35 * Math.sin(2 * Math.PI * st.k * t / DURATION + st.p);
    sctx.globalAlpha = st.a * tw;
    sctx.fillStyle = "#dfe2ff";
    sctx.beginPath(); sctx.arc(st.x - dx * st.z, st.y - dy * st.z, 0.6 + st.z * 0.9, 0, 6.283); sctx.fill();
  }
  sctx.globalAlpha = 1;
}

// ── the feed: one drawing shared by the preview and the call tile ──
const appFeed = makeFeed("main"); $("feedbox").appendChild(appFeed);
const callFeed = makeFeed("main"); $("tilefeed").appendChild(callFeed);
function feedAt(r) {
  updateFeed({ t: r, blurBg: 1.2, blurMug: 0.3, blurCat: 0 });
}

// ── where things sit in the world, measured once from the layout ──
function worldPos(el) {
  let x = 0, y = 0;
  while (el && el.id !== "world") { x += el.offsetLeft; y += el.offsetTop; el = el.offsetParent; }
  return [x, y];
}
let P = {};
function measure() {
  // runs before the first seek, while nothing is transformed, so page boxes are world boxes
  const box = el => el.getBoundingClientRect();
  const c = box($("cambtn").querySelector(".chev"));
  P.chev = [c.x + c.width / 2 + 1, c.y + c.height / 2 + 3];
  vis($("menu"), 1);
  const m = box($("mi2"));
  P.mi2 = [m.x + 64, m.y + m.height / 2 + 2];
  P.hlTop = $("mi2").offsetTop;
  vis($("menu"), 0);
}

// ── the camera view inside the scanner: the monitor as it looks right then ──
(() => {
  const cam = $("vfcam");
  cam.innerHTML = `<img src="img/s2-vcam.png" style="position:absolute;left:0;top:0;width:1300px;height:760px">
    <div style="position:absolute;inset:0;background:#07090c;opacity:0.45"></div>
    <img src="img/d-add.png" style="position:absolute;left:420px;top:195px;width:460px;border-radius:10px;box-shadow:0 0 0 1px #2c323c">`;
  $("vfworld").style.background = "#0f1216";
})();

// ── tracks ──
const cursorPath = [
  // [t0, t1, from, to, bend]
  [1.15, 2.0, [1010, 650], [762, 419], 40],
  [2.35, 3.0, [762, 419], [820, 535], -14],
  [7.25, 8.1, [820, 535], [1196, 30], 70],
  [8.35, 9.1, [1196, 30], [1150, 126], -16],
];
function cursorAt(t) {
  let p = cursorPath[0][2];
  for (const [t0, t1, a, b, bend] of cursorPath) {
    if (t < t0) break;
    const k = E.io5(clamp((t - t0) / (t1 - t0)));
    // a gentle arc: the hand curves on its way rather than sliding on a ruler
    const nx = -(b[1] - a[1]), ny = b[0] - a[0], l = Math.hypot(nx, ny) || 1, off = Math.sin(Math.PI * k) * bend;
    p = [lerp(a[0], b[0], k) + nx / l * off, lerp(a[1], b[1], k) + ny / l * off];
  }
  if (P.chev) {
    const late = [[10.45, 11.2, [1150, 126], P.chev, 50], [11.55, 12.0, P.chev, P.mi2, -20], [12.45, 13.2, P.mi2, [P.mi2[0] + 60, P.mi2[1] + 90], 10]];
    for (const [t0, t1, a, b, bend] of late) {
      if (t < t0) break;
      const k = E.io5(clamp((t - t0) / (t1 - t0)));
      const nx = -(b[1] - a[1]), ny = b[0] - a[0], l = Math.hypot(nx, ny) || 1, off = Math.sin(Math.PI * k) * bend;
      p = [lerp(a[0], b[0], k) + nx / l * off, lerp(a[1], b[1], k) + ny / l * off];
    }
  }
  return p;
}
const clicks = [2.08, 8.2, 11.25, 12.05];
const press = t => Math.max(...clicks.map(c => Math.max(0, 1 - Math.abs(t - c) / 0.09)));

const OFF = [1660, 1580, 12];
const phonePose = track([
  [0, OFF], [2.4, OFF], [3.25, [1530, 612, -2.5], E.out5], [3.8, [1530, 612, -2.5]],
  [4.75, [972, 642, -3.5], E.io5], [5.65, [972, 642, -3.5]], [6.65, [1680, 1600, 9], E.io5], [STORY, OFF],
]);
const CAMERA_UP = 196;  // the phone's camera sits at the top of its back, so it sees above the phone's middle

function seek(r) {
  r = ((r % DURATION) + DURATION) % DURATION;
  const t = story(r);
  const cam = camera(t);
  const world = $("world");
  world.style.transform = `translate(${(960 - cam[0] * cam[2]).toFixed(2)}px, ${(540 - cam[1] * cam[2]).toFixed(2)}px) scale(${cam[2].toFixed(4)})`;
  drawStars(r, cam);
  feedAt(r);

  // app screens stack: each later state covers the one before; only the last one fades to bring frame 0 back
  // back to frame 0 through a short dip of the window's contents, so two layouts never ghost over each other
  const dip = ramp(t, 13.95, 14.3, E.io3) * (1 - ramp(t, 14.3, 14.8, E.out3));
  const s4 = t < 14.3 ? ramp(t, 8.32, 8.47) : 0;
  vis($("s2"), t < 8.8 ? ramp(t, 0.85, 1.0) : 0);
  vis($("s3"), t < 8.8 ? ramp(t, 5.3, 5.45) : 0);
  vis($("s4"), s4);
  vis($("feedbox"), s4);
  vis(appFeed, ramp(t, 8.5, 8.95, E.out3) * (1 - ramp(t, 13.75, 14.15)));
  const dlgIn = ramp(t, 2.15, 2.55, E.expo) * (1 - ramp(t, 6.65, 6.95, E.io3));
  const callIn = ramp(t, 9.6, 10.3, E.expo), callOut = ramp(t, 13.6, 14.15, E.io3);
  vis($("appdim"), Math.max(0.45 * dlgIn, 0.16 * callIn * (1 - callOut), dip));

  // checklist ticks: a ring opens around each badge as it turns
  [[$("r1"), [460, 148], 0.98], [$("r2"), [460, 211], 7.05], [$("r3"), [460, 274], 7.17]].forEach(([el, [x, y], at]) => {
    const k = clamp((t - at) / 0.7);
    el.style.left = x + "px"; el.style.top = y + "px";
    el.style.transform = `scale(${(1 + 0.85 * E.out3(k)).toFixed(3)})`;
    vis(el, k > 0 && k < 1 ? 0.75 * (1 - E.out3(k)) : 0);
  });

  // the Add phone dialog
  const d = $("dlg");
  vis(d, dlgIn);
  d.style.transform = `scale(${(0.965 + 0.035 * dlgIn).toFixed(4)})`;
  const paired = ramp(t, 5.2, 5.5);
  d.style.height = lerp(369, 146, ramp(t, 5.2, 5.65, E.io5)).toFixed(2) + "px";
  vis($("dadd"), 1 - paired);
  vis($("dpaired"), paired);

  // the call window, and its camera menu
  const c = $("call");
  vis(c, callIn * (1 - callOut));
  c.style.transform = `translate(0, ${(54 * (1 - callIn) + 40 * callOut).toFixed(2)}px) scale(${(0.97 + 0.03 * callIn).toFixed(4)})`;
  const menuIn = ramp(t, 11.3, 11.5, E.expo) * (1 - ramp(t, 12.15, 12.32, E.io3));
  vis($("menu"), menuIn);
  $("menu").style.transform = `translate(0, ${(8 * (1 - menuIn)).toFixed(2)}px) scale(${(0.97 + 0.03 * menuIn).toFixed(4)})`;
  $("menuhl").style.top = P.hlTop + "px";
  vis($("menuhl"), ramp(t, 11.85, 12.0));
  const picked = t >= 12.08 && t < 14.3;
  vis($("ck1"), picked ? 0 : 1); vis($("ck2"), picked ? 1 : 0);
  vis($("tilefeed"), ramp(t, 12.2, 12.6, E.out3));

  // the pointer
  const [px, py] = cursorAt(t);
  const cur = $("cursor");
  vis(cur, ramp(t, 1.0, 1.25) * (1 - ramp(t, 13.55, 13.85)));
  cur.style.transform = `translate(${(px - 2).toFixed(2)}px, ${(py - 2).toFixed(2)}px) scale(${(1 - 0.12 * press(t)).toFixed(3)})`;
  const c0 = clicks.find(c => t > c && t < c + 0.45);
  const cl = $("click");
  if (c0 !== undefined) {
    const ck = (t - c0) / 0.45, [qx, qy] = cursorAt(c0);
    cl.style.left = qx + "px"; cl.style.top = qy + "px";
    cl.style.transform = `scale(${(0.5 + 0.9 * E.out3(ck)).toFixed(3)})`;
    vis(cl, 0.9 * (1 - ck));
  } else vis(cl, 0);

  // the phone, held in front of the monitor with a little life in the hand
  const pose = phonePose(t);
  const held = ramp(t, 2.9, 3.3) * (1 - ramp(t, 5.7, 6.2));
  const hx = (Math.sin(2 * Math.PI * r / 1.875) * 3 + Math.sin(2 * Math.PI * r / 3.75 + 1) * 2) * held;
  const hy = (Math.cos(2 * Math.PI * r / 2.5) * 2.5) * held;
  const hr = Math.sin(2 * Math.PI * r / 3 + 0.5) * 0.35 * held;
  const X = pose[0] + hx, Y = pose[1] + hy, R = pose[2] + hr;
  $("phone").style.transform = `translate(${(X - 203.5).toFixed(2)}px, ${(Y - 429).toFixed(2)}px) rotate(${R.toFixed(3)}deg)`;
  $("shadow").style.transform = `translate(${(X - 203.5 + 30).toFixed(2)}px, ${(Y - 429 + 44).toFixed(2)}px) rotate(${R.toFixed(3)}deg) scale(0.97)`;
  // phone screens: home, then the scanner, then home again with the computer paired
  vis($("phome"), t < 3.85 ? 1 : 0);
  const scanIn = ramp(t, 3.48, 3.82, E.out3), scanOut = ramp(t, 5.1, 5.3, E.io3);
  vis($("scan"), scanIn * (1 - scanOut));
  $("scan").style.transform = `scale(${(0.96 + 0.04 * scanIn - 0.03 * scanOut).toFixed(4)})`;
  $("scan").style.transformOrigin = "540px 1200px";
  vis($("ppaired"), t >= 5.0 && t < 7 ? 1 : 0);
  const tk = clamp((t - 3.3) / 0.38);
  const touch = $("touch");
  touch.style.left = "540px"; touch.style.top = "662px";
  touch.style.transform = `scale(${(0.55 + 0.45 * E.out3(tk)).toFixed(3)})`;
  vis(touch, tk > 0 && tk < 1 ? Math.sin(Math.PI * tk) ** 0.6 : 0);

  // the scanner sees what's behind the phone: the world point above the phone's middle, turned against the phone
  if (scanIn * (1 - scanOut) > 0) {
    const a = R * Math.PI / 180;
    const sx = X + Math.sin(a) * CAMERA_UP, sy = Y - Math.cos(a) * CAMERA_UP;
    const [wx, wy] = screenToWorld(cam, sx, sy);
    const M = 2.85;
    $("vfworld").style.transform = `translate(540px, 965px) rotate(${(-R).toFixed(3)}deg) scale(${M.toFixed(4)}) translate(${(-wx).toFixed(2)}px, ${(-wy).toFixed(2)}px)`;
  }
}

window.DURATION = DURATION;
window.realOf = realOf;
window.seek = seek;
window.READY = (async () => {
  await document.fonts.ready;
  await Promise.all([...document.images].map(i => i.decode().catch(() => {})));
  measure();
  seek(0);
  return true;
})();
