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
  // The visitor's own download is the one big button and the rest become small links next to it
  if (pick) {
    pick.classList.add("primary");
    pick.textContent = pick.dataset.primary;
    row.prepend(pick);
    const others = $$(".dl:not(.primary)", row);
    for (const a of others) {
      a.classList.add("other");
      a.textContent = a.dataset.other;
    }
    others[0].before(Object.assign(document.createElement("span"), { className: "dl-or", textContent: "or" }));
  }
  if (os === "android") note.textContent = "That's the phone half. Telescope itself runs on a Linux or Windows computer, so open this page there too (the desktop app can also put the phone app on your phone for you).";
  if (os === "apple") note.textContent = "The desktop app needs a Linux or Windows computer (there's no Mac version). An iPhone or iPad works as the camera straight from its browser with nothing to install.";
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

  // Where we are on the trip: 0 at the top, and stop i once its middle reaches the middle of the screen. The camera
  // flies between them at the pace you scroll, so the planets drift past instead of waiting at each section
  const stops = $$(".stop");
  const journey = window.telescopeJourney = { p: 0, stops: stops.length };
  // Screen height with the phone's address bar showing, which stays put while the bar slides in and out
  const probe = document.createElement("div");
  probe.style.cssText = "position: absolute; top: 0; width: 0; height: 100svh; visibility: hidden; pointer-events: none";
  document.body.append(probe);
  const screenH = () => probe.offsetHeight || innerHeight;
  let anchors = [];
  const measure = () => {
    const vh = screenH(), max = document.documentElement.scrollHeight - innerHeight;
    anchors = stops.map((s, i) => i ? Math.min(max, s.offsetTop + s.offsetHeight / 2 - vh / 2) : 0);
    for (let i = 1; i < anchors.length; i++) anchors[i] = Math.max(anchors[i], anchors[i - 1] + 1);
  };
  const update = () => {
    const y = scrollY;
    let p = stops.length - 1;
    for (let i = 0; i < stops.length - 1; i++) {
      if (y < anchors[i + 1]) { p = i + Math.max(0, (y - anchors[i]) / (anchors[i + 1] - anchors[i])); break; }
    }
    journey.p = p;
    const lit = Math.round(p / (stops.length - 1) * MARK_TICKS);
    markTicks.forEach((t, i) => t.classList.toggle("on", i < lit));
    document.documentElement.style.setProperty("--mark", Math.min(1, p * 2).toFixed(2));
    document.body.classList.toggle("at-top", scrollY < 40);
  };
  measure();
  update();
  addEventListener("scroll", update, { passive: true });
  addEventListener("resize", () => { measure(); update(); });
  new ResizeObserver(() => { measure(); update(); }).observe(document.body);

  // Clips play while they're on screen and wait while they aren't. With reduced motion they stay on their poster
  // until clicked
  const still = matchMedia("(prefers-reduced-motion: reduce)").matches;
  for (const box of $$(".tile-media[data-src]")) {
    const video = document.createElement("video");
    Object.assign(video, { muted: true, loop: true, playsInline: true, preload: "metadata" });
    video.src = box.dataset.src;
    video.poster = box.dataset.src.replace(/\.mp4$/, "-poster.webp");
    box.append(video);
  }
  const clips = $$(".clip video, .tile-media video");
  const seen = new IntersectionObserver(entries => {
    for (const e of entries) {
      if (e.isIntersecting && !still) e.target.play().catch(() => {});
      else e.target.pause();
    }
  }, { threshold: 0.25 });
  for (const v of clips) {
    if (still) v.controls = true;
    seen.observe(v);
  }

  // The setup steps light up along with the part of the clip that shows them
  for (const video of $$("video[data-steps]")) {
    const steps = $$("li[data-at]", video.closest("section"));
    const mark = () => {
      const at = video.duration ? video.currentTime / video.duration : 0;
      let on = 0;
      steps.forEach((s, i) => { if (at >= +s.dataset.at) on = i; });
      steps.forEach((s, i) => s.classList.toggle("now", i === on));
      if (!video.paused) requestAnimationFrame(mark);
    };
    video.addEventListener("play", mark);
    video.addEventListener("seeked", mark);
    mark();
  }

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
