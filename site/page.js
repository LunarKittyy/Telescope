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
  let anchors = [], holds = [];
  const measure = () => {
    anchors = stops.map(s => s.offsetTop);
    holds = stops.map(s => Math.max(0, s.offsetHeight - innerHeight));
  };
  const update = () => {
    const y = scrollY, vh = innerHeight;
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
