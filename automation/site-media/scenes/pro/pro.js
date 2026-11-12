// The pro loop: ISO, white balance and focus set by hand on the real Camera card, the picture following each one.
// pro_build.py writes the timeline (STATES, in states.js) this page plays and the Camera card pictures it swaps between.
const FPS = 60, DURATION = STATES.length / FPS;
window.FEED_LOOP = DURATION;
const clamp = (v, a = 0, b = 1) => Math.min(b, Math.max(a, v));
const $ = id => document.getElementById(id);

const CROP = { x: 436, y: 48, w: 856, h: 560 }, S = 1.9;
const crop = $("crop");
crop.style.width = CROP.w * S + "px"; crop.style.height = CROP.h * S + "px";
crop.style.left = (1920 - CROP.w * S) / 2 + "px"; crop.style.top = (1080 - CROP.h * S) / 2 + "px";
$("world").style.transform = `translate(${-CROP.x * S}px, ${-CROP.y * S}px) scale(${S})`;

const feed = makeFeed("main");
$("feedbox").appendChild(feed);

// where each thing sits from the phone, in diopters (1 / metres): the cat half a metre away, the mug a bit nearer
const CAT = 2.0, MUG = 2.4, BACK = 0.33, BLUR = 2.0;

const stars = (() => {
  let s = 9; const r = () => (s = (s * 16807) % 2147483647) / 2147483647;
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

async function seek(t) {
  t = ((t % DURATION) + DURATION) % DURATION;
  const f = Math.min(STATES.length - 1, Math.round(t * FPS)), st = STATES[f];
  drawStars(t);
  const card = $("card"), src = `card/u${String(st.u).padStart(3, "0")}.png`;
  if (!card.src.endsWith(src)) { card.src = src; await card.decode().catch(() => {}); }

  // ISO: each doubling is one stop brighter; white balance: a higher setting warms the picture
  const ev = Math.log2(st.iso / 400);
  const wb = clamp((st.k - 5500) / 2600, -1, 1) * 0.55;
  feed.setView([800, 450, 1600], { wb, filter: `brightness(${Math.pow(2, ev * 0.42).toFixed(3)}) contrast(${(1 + Math.max(0, ev) * 0.04).toFixed(3)})` });
  updateFeed({ t, blurCat: Math.abs(st.d - CAT) * BLUR, blurMug: Math.abs(st.d - MUG) * BLUR, blurBg: Math.abs(st.d - BACK) * BLUR });

  $("cursor").style.transform = `translate(${(st.cx - 2).toFixed(2)}px, ${(st.cy - 2).toFixed(2)}px) scale(${(1 - 0.1 * st.press).toFixed(3)})`;
  $("click").style.opacity = 0;
}

window.DURATION = DURATION;
window.seek = seek;
window.READY = (async () => {
  await document.fonts.ready;
  await Promise.all([...document.images].map(i => i.decode().catch(() => {})));
  await seek(0);
  return true;
})();
