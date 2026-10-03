(() => {
  const $ = (sel, root = document) => root.querySelector(sel);
  const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];
  const svgLine = (r1, r2, deg) => {
    const a = deg * Math.PI / 180, c = Math.cos(a), s = Math.sin(a);
    return `<line x1="${(r1 * c).toFixed(2)}" y1="${(r1 * s).toFixed(2)}" x2="${(r2 * c).toFixed(2)}" y2="${(r2 * s).toFixed(2)}"/>`;
  };

  // Ticks on the corner mark (one per 7.5 degrees, starting at the top) and on the flat fallback lens
  const MARK_TICKS = 48;
  $(".mark-ticks").innerHTML = Array.from({ length: MARK_TICKS }, (_, i) => svgLine(16, i % 6 ? 19 : 21, i * 7.5 - 90)).join("");
  const markTicks = $$(".mark-ticks line");
  let flat = "";
  for (let deg = 0; deg < 360; deg += 3) {
    const major = deg % 30 === 0, cardinal = deg % 90 === 0;
    flat += svgLine(232, cardinal ? 252 : major ? 246 : 240, deg).replace("/>", ` stroke="#aa9cf5" stroke-opacity="${cardinal ? 0.6 : major ? 0.42 : 0.2}" stroke-width="${major ? 2 : 1.4}"/>`);
  }
  $(".flat-ticks").innerHTML = flat;
  // The scroll hint is the bottom of a dial, fading out from its long tick at the bottom
  let hint = "";
  for (let deg = 60; deg <= 120; deg += 6) {
    const off = Math.abs(deg - 90) / 6;
    hint += svgLine(36, off ? 42 - off * 0.5 : 49, deg).replace("/>", ` stroke-opacity="${off ? (0.62 - off * 0.1).toFixed(2) : 0.95}"/>`);
  }
  $(".scroll-hint svg").innerHTML = hint;

  // Downloads: the visitor's platform first, then a copy of the row for the last stop
  const ua = navigator.userAgent;
  const os = /Android/i.test(ua) ? "android" : /Windows/i.test(ua) ? "windows" : /Linux|X11|CrOS/i.test(ua) ? "linux" : /Mac|iPhone|iPad/i.test(ua) ? "apple" : "windows";
  const downloads = $("#downloads");
  const row = $(".dl-row", downloads);
  const note = $(".dl-note", downloads);
  const pick = $(`[data-os="${os}"]`, row);
  if (pick) {
    pick.classList.add("primary");
    pick.textContent = "Download for " + pick.textContent;
    row.prepend(pick);
  }
  if (os === "android") note.innerHTML = 'Telescope runs on a Linux or Windows computer, so open this page there. The phone half is the <a href="https://github.com/LunarKittyy/Telescope/releases/latest/download/Telescope.apk">APK</a>, but the desktop app can install it for you.';
  if (os === "apple") note.textContent = "Telescope needs a Linux or Windows computer and an Android phone. There's no Mac or iPhone version.";
  for (const slot of $$("[data-clone=downloads]")) slot.innerHTML = downloads.innerHTML;

  document.addEventListener("click", e => {
    const btn = e.target.closest(".copy-btn");
    if (!btn) return;
    navigator.clipboard.writeText(btn.previousElementSibling.textContent).then(() => {
      btn.textContent = "Copied";
      btn.classList.add("done");
      setTimeout(() => { btn.textContent = "Copy"; btn.classList.remove("done"); }, 1600);
    });
  });

  // Faint fixed stars for the flat version
  const tile = document.createElement("canvas");
  tile.width = tile.height = 560;
  const tctx = tile.getContext("2d");
  let seed = 42;
  const rand = () => (seed = (seed * 16807) % 2147483647) / 2147483647;
  for (let i = 0; i < 80; i++) {
    const mag = Math.pow(rand(), 3);
    tctx.globalAlpha = 0.12 + mag * 0.5;
    tctx.fillStyle = "#e3e6ff";
    tctx.beginPath();
    tctx.arc(rand() * 560, rand() * 560, 0.5 + mag * 1.1, 0, Math.PI * 2);
    tctx.fill();
  }
  document.body.style.backgroundImage = `url(${tile.toDataURL()})`;

  // Where we are on the trip: stop i sits at its top edge, and a stop taller than the screen holds while you read it
  const stops = $$(".stop");
  const journey = window.telescopeJourney = { p: 0, stops: stops.length };
  // Screen height with the phone's address bar showing, which stays put while the bar slides in and out
  const probe = document.createElement("div");
  probe.style.cssText = "position: absolute; top: 0; width: 0; height: 100svh; visibility: hidden; pointer-events: none";
  document.body.append(probe);
  const screenH = () => probe.offsetHeight || innerHeight;
  let anchors = [], holds = [];
  const measure = () => {
    anchors = stops.map(s => s.offsetTop);
    holds = stops.map(s => Math.max(0, s.offsetHeight - screenH()));
  };
  const update = () => {
    const y = scrollY, vh = screenH();
    let p = 0;
    for (let i = 0; i < stops.length; i++) {
      if (i === stops.length - 1 || y < anchors[i + 1]) {
        const span = i < stops.length - 1 ? anchors[i + 1] - anchors[i] - holds[i] : 1;
        p = i + (i < stops.length - 1 ? Math.min(1, Math.max(0, (y - anchors[i] - holds[i]) / span)) : 0);
        break;
      }
    }
    journey.p = p;
    stops.forEach((s, i) => {
      const d = y - anchors[i];
      const reach = i === 0 ? 0.3 * vh : 0.6 * vh;
      const f = d < 0 ? 1 + d / reach : 1 - (d - holds[i]) / reach;
      s.style.setProperty("--f", Math.min(1, Math.max(0, f)).toFixed(3));
    });
    const lit = Math.round(p / (stops.length - 1) * MARK_TICKS);
    markTicks.forEach((t, i) => t.classList.toggle("on", i < lit));
    document.documentElement.style.setProperty("--mark", Math.min(1, p * 2).toFixed(2));
    document.body.classList.toggle("at-top", scrollY < 40);
  };
  measure();
  update();
  addEventListener("scroll", update, { passive: true });

  // One driver moves the scroll at a time, frame by frame, and keeps its position and speed so the next can carry on
  // from exactly there. Each step also updates the page in the same frame, so the text never lags a frame behind.
  let drive = null, frame = 0, last = 0, y = scrollY, v = 0;
  const loop = now => {
    const dt = Math.min(0.05, Math.max(0.001, (now - last) / 1000));
    last = now;
    const [ny, done] = drive(now, dt);
    v = (ny - y) / dt;
    y = ny;
    scrollTo(0, y);
    update();
    frame = done ? 0 : requestAnimationFrame(loop);
    if (done) { drive = null; v = 0; document.documentElement.classList.remove("driving"); }
  };
  // While a driver runs, CSS snapping is off so it can't pull each step to a stop
  const run = (d, from = scrollY, speed = 0) => {
    if (!frame) { y = from; v = speed; last = performance.now(); frame = requestAnimationFrame(loop); }
    document.documentElement.classList.add("driving");
    drive = d;
  };
  const halt = () => { cancelAnimationFrame(frame); frame = 0; drive = null; document.documentElement.classList.remove("driving"); };
  // A path over ms, where path(t) gives the position for t from 0 to 1, counted from start
  const glide = (path, ms, start = performance.now()) => {
    run(now => { const t = Math.min(1, (now - start) / ms); return [path(t), t >= 1]; });
  };

  // The scroll hint glides to the next stop over 2s on an ease in out cubic
  const hintLink = $(".scroll-hint");
  hintLink.addEventListener("click", e => {
    if (matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    e.preventDefault();
    const from = scrollY, to = $(hintLink.hash).offsetTop;
    restY = to;
    glide(t => from + (to - from) * (t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2), 2000);
  });

  // The page settles like a ball rolling over hills, with a stop in each dip. The moment you stop scrolling it carries
  // on at your speed: heading at least 15% of the way to the next stop takes it over there, a smaller nudge lets it
  // roll back, and it eases into the dip without rocking. A stop taller than the screen is a flat dip, so it can rest
  // anywhere inside it. Only for the 3D trip; the flat page has no gaps between stops.
  const trip = () => document.body.classList.contains("space") && !zoom;
  let restY = scrollY;
  // Quintic from (0, slope m) to (1, slope 0) with no acceleration at either end; it never overshoots while m <= 2.5
  const roll = (s, m) => { const s3 = s * s * s; return s3 * (10 - 15 * s + 6 * s * s) + m * (s - 6 * s3 + 8 * s3 * s - 3 * s3 * s * s); };
  // Rolls from the current position and speed into the dip nearest where it was heading, however many hills away;
  // false if it's already resting there
  const settle = (heading, start) => {
    // Which way it was going: by where it was heading, else since it last rested
    const dir = Math.sign(Math.abs(heading - y) > 10 ? heading - y : y - restY);
    let to = heading;
    for (let i = 0; i < stops.length - 1; i++) {
      const top = anchors[i] + holds[i], bottom = anchors[i + 1];
      if (heading <= top) { to = Math.max(heading, anchors[i]); break; }
      if (heading < bottom) { to = heading - top > (bottom - top) * (0.5 - dir * 0.35) ? bottom : top; break; }
    }
    return rollTo(to, start);
  };
  // Rolls from the current position and speed to y = to; false if it's already there
  const rollTo = (to, start) => {
    const from = y, d = to - from;
    restY = to;
    if (Math.abs(d) < 1) return false;
    // Longer for a longer way, but quicker when it's already heading there fast, so it never has to brake past the dip
    let sec = Math.min(2.4, Math.max(0.5, 0.45 + Math.abs(d) / 2400));
    if (v * d > 0) sec = Math.max(0.35, Math.min(sec, 2.5 * Math.abs(d) / Math.abs(v)));
    const m = Math.min(2.5, v * sec / d);
    glide(t => from + d * roll(t, m), sec * 1000, start);
    return true;
  };

  // Wheel and keys move a target that the page follows on a spring, and the moment they stop it rolls into a dip
  let aim = 0, nudgedAt = 0;
  const FOLLOW = 16;
  const follow = (now, dt) => {
    // The roll starts from where the last frame left off, so this frame already carries on at the same speed
    if (now - nudgedAt > 60 && settle(aim, now - dt * 1000)) return drive(now, dt);
    const nv = v + (FOLLOW * FOLLOW * (aim - y) - 2 * FOLLOW * v) * dt;
    const ny = y + nv * dt;
    return [ny, now - nudgedAt > 60 && Math.abs(aim - ny) < 0.5 && Math.abs(nv) < 5];
  };
  const nudge = (e, by, to = null) => {
    if (!trip()) return false;
    e.preventDefault();
    const max = document.documentElement.scrollHeight - innerHeight;
    if (drive !== follow) aim = frame ? y : scrollY;
    aim = Math.min(max, Math.max(0, to ?? aim + by));
    nudgedAt = performance.now();
    run(follow);
    return true;
  };
  // Each wheel gesture or page key moves one stop that way; inside a stop taller than the screen it scrolls up to the edge first
  const lo = i => anchors[i];
  const hi = i => i === stops.length - 1 ? document.documentElement.scrollHeight - innerHeight : anchors[i] + holds[i];
  // A gesture is a burst of wheel events with no pause, so a touchpad's coasting after the swipe counts as the same one
  let gestureAt = 0, gestureDir = 0, gestureUsed = false;
  const step = (e, by, fresh) => {
    if (!trip()) return;
    e.preventDefault();
    const dir = Math.sign(by), now = performance.now();
    if (fresh || now - gestureAt > 250 || dir !== gestureDir) gestureUsed = false;
    gestureAt = now; gestureDir = dir;
    if (!frame) y = scrollY, v = 0;
    const at = drive === follow ? aim : frame ? restY : scrollY;
    const tall = stops.findIndex((_, i) => holds[i] > 80 && at >= lo(i) - 1 && at <= hi(i) + 1);
    if (tall >= 0 && (dir > 0 ? at < hi(tall) - 1 : at > lo(tall) + 1)) {
      gestureUsed = true;
      nudge(e, 0, Math.min(hi(tall), Math.max(lo(tall), at + by)));
      return;
    }
    if (gestureUsed) return;
    gestureUsed = true;
    const next = dir > 0 ? stops.findIndex((_, i) => lo(i) > at + 1) : stops.findLastIndex((_, i) => hi(i) < at - 1);
    if (next >= 0) { if (drive === follow) drive = null; rollTo(dir > 0 ? lo(next) : hi(next), performance.now()); }
  };
  addEventListener("wheel", e => {
    if (e.ctrlKey || !e.deltaY) return;
    step(e, e.deltaY * (e.deltaMode === 1 ? 40 : e.deltaMode === 2 ? innerHeight : 1), false);
  }, { passive: false });
  addEventListener("keydown", e => {
    if (e.altKey || e.ctrlKey || e.metaKey || e.target.closest("input, textarea, select, [contenteditable]")) return;
    const page = innerHeight * 0.85;
    const keys = { ArrowDown: 60, ArrowUp: -60 };
    if (e.key === " " && !e.target.closest("button, a")) step(e, e.shiftKey ? -page : page, true);
    else if (e.key === "PageDown" || e.key === "PageUp") step(e, e.key === "PageDown" ? page : -page, true);
    else if (e.key in keys) nudge(e, keys[e.key]);
    else if (e.key === "Home") nudge(e, 0, 0);
    else if (e.key === "End") nudge(e, 0, Infinity);
  });

  // A finger or a dragged scrollbar scrolls natively; once it lets go and the page goes quiet, it rolls on from there
  // Touch screens skip this: CSS snapping lands a flick on a stop without stopping first
  const touchScreen = matchMedia("(pointer: coarse)");
  let idle = 0, held = false;
  const recent = [];
  const quiet = () => requestAnimationFrame(() => {
    if (frame || held || !trip() || touchScreen.matches) return;
    const now = performance.now(), b = recent.at(-1), a = recent.at(-3);
    y = scrollY;
    v = a && now - b.t < 50 && b.t - a.t < 60 ? (b.y - a.y) / (b.t - a.t) * 1000 : 0;
    settle(y + v * 0.35);
  });
  addEventListener("scroll", () => {
    if (frame) return;
    recent.push({ t: performance.now(), y: scrollY });
    if (recent.length > 20) recent.shift();
    clearTimeout(idle);
    idle = setTimeout(quiet, 140);
  }, { passive: true });
  addEventListener("pointerdown", () => { held = true; halt(); }, { passive: true });
  addEventListener("touchstart", () => { held = true; halt(); }, { passive: true });
  for (const ev of ["pointerup", "pointercancel", "touchend", "touchcancel"]) addEventListener(ev, () => {
    held = false;
    clearTimeout(idle);
    idle = setTimeout(quiet, 60);
  }, { passive: true });
  addEventListener("resize", () => { measure(); update(); });
  new ResizeObserver(() => { measure(); update(); }).observe(document.body);

  // Screenshots: a click floats one to the middle of the screen, and any click, Esc or scroll puts it back
  const veil = document.createElement("div");
  veil.className = "zoom-veil";
  document.body.append(veil);
  let zoom = null;
  const placeOver = (el, r, box) => { el.style.transform = `translate(${r.left - box.left}px, ${r.top - box.top}px) scale(${r.width / box.width})`; };
  const closeZoom = () => {
    if (!zoom) return;
    const { clone, img, btn, box } = zoom;
    zoom = null;
    placeOver(clone, img.getBoundingClientRect(), box);
    veil.classList.remove("on");
    const done = () => { clone.remove(); img.style.visibility = ""; };
    clone.addEventListener("transitionend", done, { once: true });
    setTimeout(done, 450);
    btn.focus({ preventScroll: true });
  };
  for (const btn of $$(".shot")) btn.addEventListener("click", () => {
    const img = $("img", btn), r = img.getBoundingClientRect();
    const ratio = img.naturalWidth / img.naturalHeight || r.width / r.height;
    const w = Math.min(innerWidth * 0.92, innerHeight * 0.88 * ratio), h = w / ratio;
    const box = { left: (innerWidth - w) / 2, top: (innerHeight - h) / 2, width: w };
    const clone = img.cloneNode();
    clone.className = "zoomed";
    clone.removeAttribute("width"); clone.removeAttribute("height");
    Object.assign(clone.style, { left: box.left + "px", top: box.top + "px", width: w + "px", height: h + "px", borderRadius: getComputedStyle(btn).borderRadius });
    placeOver(clone, r, box);
    document.body.append(clone);
    img.style.visibility = "hidden";
    clone.getBoundingClientRect();
    clone.style.transform = "none";
    veil.classList.add("on");
    clone.addEventListener("click", closeZoom);
    zoom = { clone, img, btn, box, y: scrollY };
  });
  veil.addEventListener("click", closeZoom);
  addEventListener("keydown", e => { if (e.key === "Escape") closeZoom(); });
  addEventListener("scroll", () => { if (zoom && Math.abs(scrollY - zoom.y) > 40) closeZoom(); }, { passive: true });
  addEventListener("resize", closeZoom);
})();
