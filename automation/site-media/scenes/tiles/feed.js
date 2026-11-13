// The camera feed: a desk at night with a cat, drawn in a 3200x1800 world so lenses can crop it
(function () {
  const W = 3200, H = 1800;
  let seed = 7;
  const rand = () => (seed = (seed * 16807) % 2147483647) / 2147483647;
  const jit = a => (rand() * 2 - 1) * a;
  const f1 = v => v.toFixed(1);

  // Catmull-Rom through the points, as cubic Béziers; closed shapes wrap around
  function curve(pts, closed = true, k = 1) {
    const n = pts.length, P = i => closed ? pts[(i + n) % n] : pts[Math.max(0, Math.min(n - 1, i))];
    let d = `M${f1(pts[0][0])} ${f1(pts[0][1])}`;
    for (let i = 0; i < (closed ? n : n - 1); i++) {
      const p0 = P(i - 1), p1 = P(i), p2 = P(i + 1), p3 = P(i + 2);
      d += ` C${f1(p1[0] + (p2[0] - p0[0]) / 6 * k)} ${f1(p1[1] + (p2[1] - p0[1]) / 6 * k)} ${f1(p2[0] - (p3[0] - p1[0]) / 6 * k)} ${f1(p2[1] - (p3[1] - p1[1]) / 6 * k)} ${f1(p2[0])} ${f1(p2[1])}`;
    }
    return d + (closed ? "Z" : "");
  }
  // a shape whose control points are nudged a little, so nothing is quite regular
  const blob = (pts, j = 0, k = 1) => curve(pts.map(([x, y]) => [x + jit(j), y + jit(j)]), true, k);
  // a tapered brush stroke along the points: thin at both ends, a little uneven in between
  function brush(pts, w, { taper = [0.5, 0.9], wobble = 0.18 } = {}) {
    const S = [];
    const n = pts.length, P = i => pts[Math.max(0, Math.min(n - 1, i))];
    for (let i = 0; i < n - 1; i++) {
      const p0 = P(i - 1), p1 = P(i), p2 = P(i + 1), p3 = P(i + 2);
      for (let s = 0; s < 8; s++) {
        const t = s / 8, t2 = t * t, t3 = t2 * t;
        const c = (a, b, cc, d) => 0.5 * (2 * b + (-a + cc) * t + (2 * a - 5 * b + 4 * cc - d) * t2 + (-a + 3 * b - 3 * cc + d) * t3);
        S.push([c(p0[0], p1[0], p2[0], p3[0]), c(p0[1], p1[1], p2[1], p3[1])]);
      }
    }
    S.push(pts[n - 1]);
    const L = [], R = [], m = S.length;
    const noise = Array.from({ length: 5 }, () => 1 + jit(wobble));
    for (let i = 0; i < m; i++) {
      const a = S[Math.max(0, i - 1)], b = S[Math.min(m - 1, i + 1)];
      let dx = b[0] - a[0], dy = b[1] - a[1]; const l = Math.hypot(dx, dy) || 1; dx /= l; dy /= l;
      const u = i / (m - 1);
      const prof = Math.min(1, u / taper[0] * 1.6, (1 - u) / (1 - taper[1]) * 1.2);
      const nz = noise[Math.min(4, Math.floor(u * 5))];
      const hw = w / 2 * Math.max(0.12, Math.pow(Math.max(0, prof), 0.7)) * nz;
      L.push([S[i][0] - dy * hw, S[i][1] + dx * hw]); R.push([S[i][0] + dy * hw, S[i][1] - dx * hw]);
    }
    const poly = L.concat(R.reverse());
    return "M" + poly.map(p => `${f1(p[0])} ${f1(p[1])}`).join(" L") + "Z";
  }
  const path = (d, fill, extra = "") => `<path d="${d}" fill="${fill}" ${extra}/>`;

  // ── window: a night skyline, not a planet ──
  let skyline = "", lit = "";
  for (let x = 548; x < 1240;) {
    const w = 46 + rand() * 90, h = 90 + Math.pow(rand(), 1.4) * 230, top = 962 - h;
    const roof = rand();
    let pts = [[x, 965], [x, top]];
    if (roof < 0.25) pts.push([x + w * 0.5, top - 26 - rand() * 20]);
    else if (roof < 0.45) pts.push([x + w * 0.2, top], [x + w * 0.2, top - 22], [x + w * 0.62, top - 22], [x + w * 0.62, top]);
    pts.push([x + w, top], [x + w, 965]);
    skyline += `<path d="M${pts.map(p => `${f1(p[0])} ${f1(p[1])}`).join(" L")}Z" fill="${rand() < 0.5 ? "#121731" : "#161b37"}"/>`;
    if (roof > 0.8) skyline += `<path d="M${f1(x + w * 0.7)} ${f1(top)} L${f1(x + w * 0.72)} ${f1(top - 70)}" stroke="#121731" stroke-width="4"/>`;
    for (let wy = top + 16; wy < 945; wy += 22 + rand() * 6) {
      for (let wx = x + 8; wx < x + w - 14; wx += 15 + rand() * 7) {
        if (rand() < 0.14) {
          const warm = rand() < 0.75;
          lit += `<rect x="${f1(wx)}" y="${f1(wy)}" width="${f1(6 + rand() * 4)}" height="${f1(8 + rand() * 4)}" fill="${warm ? "#f2c27c" : "#93a8e4"}" opacity="${(0.55 + rand() * 0.45).toFixed(2)}"/>`;
        }
      }
    }
    x += w + (rand() < 0.3 ? 6 + rand() * 18 : 0);
  }
  let skyStars = "";
  for (let i = 0; i < 34; i++) {
    const x = 580 + rand() * 620, y = 300 + Math.pow(rand(), 1.6) * 380, r = 1 + Math.pow(rand(), 3) * 3.4;
    skyStars += `<circle class="fs-star" cx="${f1(x)}" cy="${f1(y)}" r="${f1(r)}" fill="#f4f1ff" opacity="${(0.35 + rand() * 0.55).toFixed(2)}"/>`;
  }

  // ── books: leaning, different heights, one lying flat ──
  const bookCols = ["#4f6b7d", "#a1574e", "#d6c6a2", "#6f5c8f", "#3e4a5e", "#b38b4a", "#7d8c6a", "#c47e6b"];
  let shelfBooks = "", x = 2566;
  for (let i = 0; i < 8; i++) {
    const w = 30 + rand() * 30, h = 140 + rand() * 90, lean = i === 6 ? -14 : i === 7 ? 0 : jit(2.5);
    if (i === 7) {
      shelfBooks += `<g transform="rotate(${f1(lean)} ${f1(x)} 700)"><rect x="${f1(x + 8)}" y="${f1(700 - 34)}" width="150" height="34" fill="${bookCols[i]}"/><rect x="${f1(x + 14)}" y="${f1(700 - 66)}" width="132" height="32" fill="${bookCols[2]}"/><rect x="${f1(x + 14)}" y="${f1(700 - 58)}" width="132" height="5" fill="#00000022"/></g>`;
      x += 170; continue;
    }
    shelfBooks += `<g transform="rotate(${f1(lean)} ${f1(lean < 0 ? x : x + w)} 700)"><rect x="${f1(x)}" y="${f1(700 - h)}" width="${f1(w)}" height="${f1(h)}" fill="${bookCols[i]}"/>`
      + `<rect x="${f1(x + 4)}" y="${f1(700 - h + 18 + rand() * 20)}" width="${f1(w - 8)}" height="${f1(4 + rand() * 4)}" fill="#ffffff" opacity="0.18"/>`
      + `<rect x="${f1(x + w - 6)}" y="${f1(700 - h)}" width="6" height="${f1(h)}" fill="#000" opacity="0.18"/></g>`;
    x += w + (i === 5 ? 34 : 2 + rand() * 3);
  }
  // trailing plant at the end of the shelf
  let vines = "";
  for (let v = 0; v < 5; v++) {
    const x0 = 3046 + v * 16 + jit(6), len = 160 + rand() * 260;
    const pts = [[x0, 700]];
    for (let s = 1; s <= 5; s++) pts.push([x0 + Math.sin(s * 1.3 + v) * 26 + jit(8), 700 + len * s / 5]);
    vines += path(brush(pts, 7, { taper: [0.05, 0.95] }), "#3f6d57");
    for (let s = 1; s <= 5; s++) {
      const p = pts[s], a = jit(50) + (s % 2 ? 40 : -40);
      vines += `<path d="${blob([[0, 0], [16, -12], [30, -2], [20, 14], [4, 10]], 2)}" fill="${s % 3 ? "#5d8f6f" : "#4c7d60"}" transform="translate(${f1(p[0])} ${f1(p[1])}) rotate(${f1(a)})"/>`;
    }
  }

  // ── string lights: one drooping run across the upper left, bulbs hung at uneven gaps ──
  const sag = (a, b, depth, t) => [lerp0(a[0], b[0], t), lerp0(a[1], b[1], t) + depth * 4 * t * (1 - t)];
  function lerp0(a, b, t) { return a + (b - a) * t; }
  const spans = [[[0, 118], [760, 168], 92], [[760, 168], [1590, 112], 64]];
  let wire = "", bulbs = "";
  for (const [a, b, d] of spans) {
    const pts = []; for (let i = 0; i <= 12; i++) pts.push(sag(a, b, d, i / 12));
    wire += path(brush(pts, 5, { taper: [0.02, 0.98], wobble: 0.1 }), "#1c1928");
  }
  const bulbAt = [0.06, 0.15, 0.21, 0.33, 0.4, 0.52, 0.61, 0.69, 0.8, 0.87, 0.95];
  const glass = ["#ffd38a", "#ffe6b8", "#ffb48a", "#ffd38a", "#fff0c9"];
  let bi = 0;
  for (const [a, b, d] of spans) for (const t of bulbAt) {
    if ((bi++ % 4) === 3 && rand() < 0.5) continue;
    const [px, py] = sag(a, b, d, t + jit(0.02)), r = jit(16);
    bulbs += `<g transform="translate(${f1(px)} ${f1(py)}) rotate(${f1(r)})"><rect x="-5" y="-2" width="10" height="15" fill="#2a2636"/>`
      + `<path class="fs-bulb" d="M-8 13 C-12 22 -9 34 0 37 C9 34 12 22 8 13 Z" fill="${glass[bi % glass.length]}"/></g>`;
  }

  const svg = `
  <defs>
    <radialGradient id="fs-torchg" cx="1720" cy="980" r="900" gradientUnits="userSpaceOnUse"><stop offset="0" stop-color="#ffffff" stop-opacity="0.75"/><stop offset="0.35" stop-color="#fff4e0" stop-opacity="0.3"/><stop offset="1" stop-color="#fff4e0" stop-opacity="0"/></radialGradient>
    <mask id="fs-moon-m" maskUnits="userSpaceOnUse" x="900" y="250" width="400" height="300"><rect x="900" y="250" width="400" height="300" fill="#fff"/><circle cx="1104" cy="366" r="44" fill="#000"/></mask>
    <clipPath id="fs-glass"><path d="M566 290 L1214 284 L1218 958 L560 962 Z"/></clipPath>
    <clipPath id="fs-body-clip"><path d="M-176 0 C-204 -52 -206 -150 -168 -216 C-140 -266 -94 -306 -32 -320 C26 -330 88 -306 120 -258 C150 -210 166 -132 162 -62 C160 -30 152 -10 140 0 C80 9 -100 10 -176 0 Z"/></clipPath>
    <pattern id="fs-grain" patternUnits="userSpaceOnUse" width="150" height="150"><image id="fs-grain-img" width="150" height="150"/></pattern>
    <filter id="fs-soft" x="-50%" y="-200%" width="200%" height="500%"><feGaussianBlur stdDeviation="7"/></filter>
    <clipPath id="fs-mug-clip"><path d="M-92 -190 L-88 -20 Q0 2 88 -22 L92 -192 Z"/></clipPath>
    <filter id="fs-blur-bg" x="-5%" y="-5%" width="110%" height="110%"><feGaussianBlur id="fs-bb" stdDeviation="0"/></filter>
    <filter id="fs-blur-mug" x="-30%" y="-30%" width="160%" height="160%"><feGaussianBlur id="fs-bm" stdDeviation="0"/></filter>
    <filter id="fs-blur-cat" x="-30%" y="-30%" width="160%" height="160%"><feGaussianBlur id="fs-bc" stdDeviation="0"/></filter>
  </defs>
  <g id="fs-bg" filter="url(#fs-blur-bg)">
    <rect width="${W}" height="${H}" fill="#2a2437"/>
    ${path(blob([[0, 0], [1500, 0], [1380, 420], [900, 1240], [0, 1240]], 30), "#272134")}
    <g>
      ${path("M540 262 L1238 256 L1244 986 L534 990 Z", "#171523")}
      <g clip-path="url(#fs-glass)">
        <rect x="540" y="260" width="700" height="720" fill="#1c2343"/>
        ${path(blob([[540, 700], [800, 668], [1080, 690], [1240, 660], [1240, 980], [540, 980]], 6), "#262c52")}
        <g id="fs-sky-stars">${skyStars}</g>
        <circle cx="1080" cy="380" r="48" fill="#f3ead2" mask="url(#fs-moon-m)"/>
        <line id="fs-shoot" x1="0" y1="0" x2="0" y2="0" stroke="#fff" stroke-width="4" stroke-linecap="round" opacity="0"/>
        ${skyline}${lit}
        ${path(blob([[566, 300], [700, 292], [640, 520], [600, 700], [566, 760]], 0), "#ffffff", 'opacity="0.035"')}
      </g>
      ${path("M872 286 L888 285 L892 960 L876 961 Z", "#171523")}
      ${path("M562 598 L1216 590 L1216 604 L562 612 Z", "#171523")}
      ${path("M504 986 L1276 979 L1282 1014 L498 1020 Z", "#3a3150")}
      ${path("M504 986 L1276 979 L1277 986 L505 993 Z", "#4c4262")}
      <g transform="translate(1168 984)">
        ${path("M-34 0 L-40 -54 L40 -56 L34 0 Z", "#a8624e")}
        ${path("M-44 -54 L44 -58 L44 -46 L-44 -42 Z", "#bb7258")}
        ${[[-8, -60, -30], [8, -62, 25], [-22, -56, -60], [24, -58, 62], [0, -66, 4]].map(([lx, ly, r]) => `<path d="${blob([[0, 0], [10, -22], [0, -46], [-10, -22]], 1.5)}" fill="${r % 2 ? "#6f9479" : "#5c8268"}" transform="translate(${lx} ${ly}) rotate(${r})"/>`).join("")}
      </g>
      ${[[448, 498, 410, 520, "#5f3d50"], [492, 560, 470, 600, "#704a5e"], [552, 612, 556, 676, "#62404f"], [604, 668, 640, 736, "#7a5468"]].map(([t0, t1, b0, b1, c], i) =>
        path(`M${t0} 232 L${t1} 232 C${t1 + 10} 420 ${b1 - 20} 700 ${b1} ${1046 + i * 9} L${b0} ${1052 - i * 6} C${b0 + 6} 760 ${t0 - 12} 460 ${t0} 232 Z`, c)).join("")}
      ${path(brush([[500, 240], [492, 560], [474, 1040]], 6, { taper: [0.1, 0.95] }), "#4e3142", 'opacity="0.7"')}
      ${path(brush([[612, 240], [628, 600], [660, 1040]], 5, { taper: [0.1, 0.95] }), "#4e3142", 'opacity="0.6"')}
      ${path(brush([[700, 300], [712, 620], [728, 1000]], 4, { taper: [0.1, 0.9] }), "#8e677c", 'opacity="0.6"')}
      ${path(brush([[410, 230], [900, 226], [1300, 222]], 9, { taper: [0.01, 0.99], wobble: 0.05 }), "#14121d")}
      <circle cx="406" cy="230" r="11" fill="#14121d"/><circle cx="1304" cy="222" r="11" fill="#14121d"/>
    </g>
    <g transform="translate(2034 430) rotate(-3.2)">
      ${path("M-154 -176 L150 -172 L154 186 L-150 182 Z", "#e6dfd0")}
      ${path("M150 -172 L154 186 L138 185 L136 -170 Z", "#000", 'opacity="0.06"')}
      ${path("M-130 -150 L128 -147 L130 92 L-128 90 Z", "#1a1c24")}
      ${path("M-130 -150 L128 -147 L129 -20 L-129 -22 Z", "#2c3352")}
      ${path("M-129 -40 L128 -46 L129 0 L-129 0 Z", "#5a4f78", 'opacity="0.55"')}
      <circle cx="70" cy="-96" r="11" fill="#e9e2c8"/>
      ${path("M-129 30 L-88 -38 L-64 -14 L-22 -76 L4 -48 L18 -60 L64 2 L96 -22 L129 16 L130 92 L-128 90 Z", "#4b5878")}
      ${path("M-22 -76 L-10 -60 L-16 -58 L-24 -64 L-32 -62 Z", "#c9cde0", 'opacity="0.8"')}
      ${path("M-128 60 L-80 22 L-30 48 L30 8 L84 44 L130 30 L130 92 L-128 90 Z", "#23283a")}
      ${path(brush([[-100, 122], [40, 124]], 14, { wobble: 0.05 }), "#2a2c36")}
      ${path(brush([[-100, 150], [-10, 151]], 9, { wobble: 0.05 }), "#8b8d98")}
      <rect x="-60" y="-196" width="96" height="36" fill="#efe3c4" opacity="0.72" transform="rotate(5 -12 -178)"/>
      <rect x="96" y="160" width="74" height="30" fill="#efe3c4" opacity="0.65" transform="rotate(-38 133 175)"/>
    </g>
    <g>
      ${path("M2520 700 L3200 694 L3200 722 L2520 727 Z", "#3a3150")}
      ${path("M2520 700 L3200 694 L3200 700 L2520 705 Z", "#4c4262")}
      ${path(brush([[2600, 724], [2604, 780], [2640, 800]], 9), "#14121d")}
      ${shelfBooks}
      ${path("M3030 700 L3022 640 L3112 638 L3104 700 Z", "#b9b0a2")}
      ${path("M3018 640 L3116 637 L3116 628 L3018 631 Z", "#cfc6b8")}
      ${vines}
    </g>
    ${wire}
    <g id="fs-bulbs">${bulbs}</g>
    ${path("M0 1230 L3200 1238 L3200 1800 L0 1800 Z", "#44333f")}
    ${path("M0 1230 L3200 1238 L3200 1256 L0 1250 Z", "#6c5262")}
    ${path(brush([[60, 1330], [900, 1342], [1700, 1326], [2600, 1340], [3160, 1330]], 5, { wobble: 0.3 }), "#503c4b")}
    ${path(brush([[200, 1460], [1100, 1470], [2100, 1452], [3000, 1470]], 6, { wobble: 0.3 }), "#3c2d38")}
    ${path(brush([[0, 1600], [700, 1610], [1600, 1596]], 5, { wobble: 0.3 }), "#503c4b")}
    <g transform="translate(470 1246)">
      ${path("M-170 -6 L164 -14 L168 -54 L-166 -46 Z", "#4f6b7d")}
      ${path("M-170 -6 L164 -14 L164 -20 L-170 -12 Z", "#000", 'opacity="0.18"')}
      ${path("M-150 -48 L150 -62 L154 -98 L-146 -84 Z", "#d6c6a2")}
      ${path("M-150 -48 L-146 -84 L-130 -85 L-134 -49 Z", "#b9a983")}
      ${path("M-120 -86 L176 -104 L180 -134 L-116 -116 Z", "#a1574e")}
      ${path(brush([[-60, -140], [110, -152]], 12, { taper: [0.1, 0.8], wobble: 0.05 }), "#e3b54f")}
      ${path("M110 -158 L132 -152 L110 -146 Z", "#2a2636")}
    </g>
    <g transform="translate(940 1314) rotate(-5)">
      ${path("M-190 -18 L196 -22 L200 18 L-186 22 Z", "#d9d2e2")}
      ${path("M-190 -18 L196 -22 L196 -12 L-190 -8 Z", "#000", 'opacity="0.08"')}
      ${path(brush([[-150, 2], [60, -2]], 5, { wobble: 0.1 }), "#9b93ad")}
      ${path(brush([[-150, 10], [-20, 8]], 4, { wobble: 0.1 }), "#9b93ad")}
      ${path(brush([[-70, -40], [140, -50]], 13, { taper: [0.04, 0.9], wobble: 0.03 }), "#6a58cf")}
    </g>
    <g transform="translate(2372 1244)">
      ${path(blob([[-120, -4], [-96, -34], [0, -46], [104, -32], [124, -6], [40, 6], [-60, 6]], 3), "#2c2837")}
      ${path(brush([[0, -40], [-62, -230], [-96, -392]], 24, { taper: [0.02, 0.98], wobble: 0.06 }), "#3b3649")}
      ${path(brush([[-96, -392], [-10, -520], [70, -604]], 20, { taper: [0.02, 0.98], wobble: 0.06 }), "#3b3649")}
      <circle cx="-96" cy="-392" r="18" fill="#4b4560"/><circle cx="-96" cy="-392" r="7" fill="#24202e"/>
      ${path("M-16 -44 L16 -44 L13 -30 L-13 -30 Z", "#34304a")}
      <circle cx="0" cy="-46" r="17" fill="#4b4560"/><circle cx="0" cy="-46" r="6.5" fill="#24202e"/>
      <g transform="translate(70 -604) rotate(-38)">
        <circle r="22" fill="#4b4560"/>
        ${path("M-30 6 L30 6 L96 156 L-104 160 Z", "#4a4459")}
        ${path("M14 6 L30 6 L96 156 L64 157 Z", "#000", 'opacity="0.18"')}
        <path d="M-104 160 Q-4 186 96 156 Q-4 140 -104 160 Z" fill="#ffe4ad"/>
      </g>
      ${path(brush([[110, -16], [200, -2], [300, 30], [420, 20]], 9, { taper: [0.02, 0.99] }), "#1d1a26")}
    </g>
  </g>
  <g id="fs-green" opacity="0">
    <defs>
      <linearGradient id="fs-green-g" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#2f8a3d"/><stop offset="0.55" stop-color="#3fa34b"/><stop offset="1" stop-color="#358f42"/></linearGradient>
      <radialGradient id="fs-green-hot" cx="1650" cy="560" r="1500" gradientUnits="userSpaceOnUse"><stop offset="0" stop-color="#5cc062" stop-opacity="0.55"/><stop offset="1" stop-color="#5cc062" stop-opacity="0"/></radialGradient>
      <filter id="fs-fold" x="-10%" y="-10%" width="120%" height="120%"><feGaussianBlur stdDeviation="38"/></filter>
    </defs>
    <path d="M-20 -20 L3220 -20 L3220 1242 L-20 1234 Z" fill="url(#fs-green-g)"/>
    <path d="M-20 -20 L3220 -20 L3220 1242 L-20 1234 Z" fill="url(#fs-green-hot)"/>
    <g filter="url(#fs-fold)" opacity="0.5">
      <path d="M420 -40 C470 300 380 700 450 1240 L520 1240 C470 760 560 330 500 -40 Z" fill="#256f31"/>
      <path d="M1080 -40 C1120 420 1030 820 1110 1240 L1150 1240 C1090 830 1180 400 1130 -40 Z" fill="#62c46a"/>
      <path d="M2310 -40 C2260 380 2380 760 2300 1240 L2390 1240 C2450 780 2330 360 2400 -40 Z" fill="#256f31"/>
      <path d="M2860 -40 C2900 520 2820 900 2880 1240 L2930 1240 C2870 880 2960 500 2910 -40 Z" fill="#62c46a"/>
    </g>
    <path d="M-20 1196 L3220 1206 L3220 1244 L-20 1236 Z" fill="#000" opacity="0.16" filter="url(#fs-soft)"/>
  </g>
  <g id="fs-mug" filter="url(#fs-blur-mug)" transform="translate(1250 1300)">
    <ellipse cx="-70" cy="-6" rx="150" ry="16" fill="#000" opacity="0.2" filter="url(#fs-soft)"/>
    <ellipse cx="0" cy="-12" rx="97" ry="14" fill="#000" opacity="0.42"/>
    ${path(brush([[84, -158], [150, -150], [158, -96], [140, -66], [86, -64]], 24, { taper: [0.02, 0.98], wobble: 0.08 }), "#dedbe6")}
    <path d="M-92 -190 L-88 -20 Q0 2 88 -22 L92 -192 Z" fill="#e6e3ec"/>
    <g clip-path="url(#fs-mug-clip)">
      <path d="M-92 -150 Q0 -136 92 -152 L92 -128 Q0 -112 -92 -126 Z" fill="#8fb3a7"/>
      <path d="M-92 -116 Q0 -102 92 -118 L92 -111 Q0 -95 -92 -109 Z" fill="#8fb3a7" opacity="0.8"/>
      <path d="M-100 -200 L-56 -200 L-52 10 L-100 10 Z" fill="#000" opacity="0.13"/>
      <path d="M60 -200 L100 -200 L100 10 L56 10 Z" fill="#fff" opacity="0.4"/>
    </g>
    <ellipse cx="0" cy="-191" rx="92" ry="17" fill="#d4cfde"/>
    <ellipse cx="2" cy="-190" rx="76" ry="11.5" fill="#4e3226"/>
    <path d="M-40 -194 Q2 -201 44 -193" stroke="#7a5442" stroke-width="3" fill="none" opacity="0.7"/>
    ${path(brush([[-70, -178], [-72, -150], [-68, -126]], 7, { taper: [0.05, 0.9] }), "#7a5442", 'opacity="0.55"')}
    <path id="fs-steam1" d="${brush([[-26, -218], [-50, -270], [-14, -318], [-34, -386]], 13)}" fill="#fff" opacity="0.26"/>
    <path id="fs-steam2" d="${brush([[22, -226], [0, -284], [42, -332], [22, -404]], 11)}" fill="#fff" opacity="0.2"/>
  </g>
  <g id="fs-cat" filter="url(#fs-blur-cat)" transform="translate(1720 1290)">
    <path d="M-214 4 C-150 -6 80 -6 186 0 C200 8 150 18 0 18 C-150 18 -226 12 -214 4 Z" fill="#000" opacity="0.32" filter="url(#fs-soft)"/>
    <path id="fs-tail-sh" fill="#000" opacity="0.25" filter="url(#fs-soft)"/>
    <path id="fs-tail" fill="#55576a"/>
    <path id="fs-tail-tip" fill="#3f4050"/>
    <path d="M-176 0 C-204 -52 -206 -150 -168 -216 C-140 -266 -94 -306 -32 -320 C26 -330 88 -306 120 -258 C150 -210 166 -132 162 -62 C160 -30 152 -10 140 0 C80 9 -100 10 -176 0 Z" fill="#5d5f71"/>
    <g clip-path="url(#fs-body-clip)">
      <path d="M-212 4 C-216 -70 -196 -150 -128 -166 C-84 -172 -66 -120 -74 -60 C-80 -20 -100 2 -130 6 Z" fill="#555668"/>
      <path d="M-206 -40 C-190 -110 -150 -150 -100 -152" stroke="#646679" stroke-width="3" fill="none" opacity="0.8"/>
      ${path(brush([[-200, -150], [-170, -142], [-150, -118]], 16, { taper: [0.2, 0.7], wobble: 0 }), "#4c4d5e")}
      ${path(brush([[-206, -96], [-176, -94], [-156, -74]], 15, { taper: [0.2, 0.7], wobble: 0 }), "#4c4d5e")}
      ${path(brush([[-164, -232], [-136, -214], [-122, -186]], 14, { taper: [0.2, 0.7], wobble: 0 }), "#4c4d5e")}
      ${path(brush([[168, -170], [142, -158], [130, -132]], 13, { taper: [0.2, 0.7], wobble: 0 }), "#4c4d5e")}
      ${path(brush([[170, -104], [146, -96], [136, -72]], 12, { taper: [0.2, 0.7], wobble: 0 }), "#4c4d5e")}
      <path d="M170 0 C172 -50 160 -112 118 -126 C88 -132 80 -80 88 -24 C90 -8 96 0 104 2 Z" fill="#585a6c"/>
    </g>
    <ellipse cx="-36" cy="3" rx="38" ry="6" fill="#000" opacity="0.3" filter="url(#fs-soft)"/>
    <ellipse cx="-155" cy="4" rx="34" ry="5" fill="#000" opacity="0.3" filter="url(#fs-soft)"/>
    <path d="M-184 2 C-188 -10 -170 -16 -150 -15 C-130 -14 -122 -6 -126 2 C-144 7 -172 7 -184 2 Z" fill="#cfc9bf"/>
    <path d="M-76 -200 C-88 -224 -102 -250 -102 -272 L-100 -306 L96 -306 L98 -272 C98 -250 82 -226 70 -210 C72 -150 70 -90 66 -40 C40 -28 -40 -28 -70 -40 C-74 -90 -76 -150 -76 -200 Z" fill="#ebe6de"/>
    <path d="M-74 -196 C-76 -130 -68 -72 -64 -38 C-75 -28 -77 -8 -67 0 C-50 5 -14 5 -4 -2 C2 -10 0 -26 -6 -38 C-3 -84 -2 -140 -2 -196 Z" fill="#ebe6de"/>
    ${path(brush([[-48, -12], [-48, 0]], 3, { wobble: 0 }), "#b6aea3")}${path(brush([[-28, -12], [-27, 1]], 3, { wobble: 0 }), "#b6aea3")}
    <g id="fs-paw-r">
      <ellipse cx="36" cy="3" rx="38" ry="6" fill="#000" opacity="0.3" filter="url(#fs-soft)"/>
      ${path(brush([[-1, -112], [0, -70], [1, -36], [-1, -4]], 4, { taper: [0.45, 0.97], wobble: 0 }), "#c9c1b5")}
      <path d="M2 -194 C2 -140 4 -82 6 -38 C0 -27 -1 -9 7 -2 C19 4 53 4 65 -2 C73 -9 72 -27 63 -37 C67 -82 70 -140 68 -190 Z" fill="#ebe6de"/>
      ${path(brush([[24, -12], [24, 1]], 3, { wobble: 0 }), "#b6aea3")}${path(brush([[44, -12], [45, 1]], 3, { wobble: 0 }), "#b6aea3")}
    </g>
    <g clip-path="url(#fs-body-clip)">
      <path d="M-130 -276 Q-4 -196 132 -280" stroke="#000" stroke-width="8" fill="none" opacity="0.14" transform="translate(0 10)"/>
      <path d="M-130 -276 Q-4 -196 132 -280" stroke="#7565b5" stroke-width="17" fill="none"/>
      <path d="M-130 -282 Q-4 -202 132 -286" stroke="#a093db" stroke-width="3" fill="none" opacity="0.8"/>
    </g>
    <g id="fs-tag" transform="rotate(0 -4 -232)">
      <path d="M-4 -232 L-4 -224" stroke="#9d8a52" stroke-width="3"/>
      <circle cx="-4" cy="-212" r="12" fill="#c9ad5e"/><circle cx="-4" cy="-212" r="12" fill="none" stroke="#a48c48" stroke-width="2"/>
      <path d="M-11 -215 A8 8 0 0 1 -2 -221" stroke="#f3e3a8" stroke-width="2.4" fill="none"/>
    </g>
    <g id="fs-head" transform="translate(0 -360)">
      <g id="fs-ear-l">
        <path d="M-134 -84 C-148 -134 -152 -184 -144 -212 C-140 -222 -131 -222 -124 -214 C-102 -190 -78 -162 -60 -136 L-44 -104 L-118 -56 Z" fill="#5d5f71"/>
        <path d="M-124 -102 C-134 -140 -137 -174 -133 -194 C-116 -174 -96 -152 -80 -132 Z" fill="#d39aa8"/>
        <path d="M-126 -112 C-124 -130 -122 -146 -122 -158" stroke="#efe9e2" stroke-width="3" fill="none" opacity="0.6"/>
      </g>
      <g id="fs-ear-r">
        <path d="M58 -138 C84 -164 116 -192 140 -206 C148 -210 154 -205 154 -196 C155 -160 150 -122 142 -90 L118 -58 L44 -106 Z" fill="#5d5f71"/>
        <path d="M76 -134 C98 -154 120 -174 138 -186 C140 -158 137 -128 131 -104 Z" fill="#c98f9d"/>
      </g>
      <clipPath id="fs-head-clip"><path d="M-152 30 C-172 -12 -162 -72 -130 -108 C-98 -142 -48 -156 2 -156 C58 -156 106 -138 136 -104 C166 -68 170 -12 152 28 C158 33 165 37 172 40 C163 47 152 51 141 54 C116 80 66 100 4 102 C-66 102 -116 80 -138 56 C-150 56 -163 53 -176 48 C-168 44 -159 38 -152 30 Z"/></clipPath>
      <path d="M-152 30 C-172 -12 -162 -72 -130 -108 C-98 -142 -48 -156 2 -156 C58 -156 106 -138 136 -104 C166 -68 170 -12 152 28 C158 33 165 37 172 40 C163 47 152 51 141 54 C116 80 66 100 4 102 C-66 102 -116 80 -138 56 C-150 56 -163 53 -176 48 C-168 44 -159 38 -152 30 Z" fill="#5d5f71"/>
      <g clip-path="url(#fs-head-clip)">
      ${path(brush([[-30, -150], [-26, -124], [-20, -104]], 11, { taper: [0.15, 0.6], wobble: 0 }), "#4e4f61")}
      ${path(brush([[2, -156], [3, -128], [2, -102]], 12, { taper: [0.15, 0.6], wobble: 0 }), "#4e4f61")}
      ${path(brush([[34, -150], [29, -124], [23, -106]], 11, { taper: [0.15, 0.6], wobble: 0 }), "#4e4f61")}
      ${path(brush([[-164, -2], [-140, -6], [-118, 0]], 9, { taper: [0.1, 0.6], wobble: 0 }), "#4e4f61")}
      ${path(brush([[-160, 24], [-138, 22], [-118, 28]], 8, { taper: [0.1, 0.6], wobble: 0 }), "#4e4f61")}
      ${path(brush([[166, -8], [142, -10], [122, -4]], 9, { taper: [0.1, 0.6], wobble: 0 }), "#4e4f61")}
        <path d="M-72 30 C-68 4 -38 -2 -14 6 C-6 9 6 9 14 6 C40 -2 68 4 72 30 C75 54 84 76 98 92 L98 140 L-102 140 L-102 92 C-86 76 -75 54 -72 30 Z" fill="#ebe6de"/>
      </g>
      <g id="fs-eyes">
        <g id="fs-eye-l" transform="translate(-58 -14) rotate(-6)">
          <path d="M-34 0 C-24 -25 24 -25 34 0 C24 23 -24 23 -34 0 Z" fill="#e3c25a"/>
          <path d="M-34 0 C-24 -25 24 -25 34 0 C27 -15 -27 -15 -34 0 Z" fill="#c9a540"/>
          <ellipse id="fs-pupil-l" rx="12" ry="23" fill="#15141b"/>
          <circle cx="-9" cy="-10" r="6.2" fill="#fff" opacity="0.9"/>
        </g>
        <g id="fs-eye-r" transform="translate(58 -14) rotate(6)">
          <path d="M-34 0 C-24 -25 24 -25 34 0 C24 23 -24 23 -34 0 Z" fill="#e3c25a"/>
          <path d="M-34 0 C-24 -25 24 -25 34 0 C27 -15 -27 -15 -34 0 Z" fill="#c9a540"/>
          <ellipse id="fs-pupil-r" rx="12" ry="23" fill="#15141b"/>
          <circle cx="-9" cy="-10" r="6.2" fill="#fff" opacity="0.9"/>
        </g>
      </g>
      <g id="fs-lids" opacity="0">
        ${path(brush([[-92, -14], [-58, 0], [-24, -16]], 7, { wobble: 0 }), "#22232d")}
        ${path(brush([[24, -16], [58, 0], [92, -14]], 7, { wobble: 0 }), "#22232d")}
      </g>
      ${path(blob([[-20, 34], [6, 32], [10, 42], [-4, 54], [-18, 44]], 0.5), "#df9aa8")}
      ${path(brush([[-5, 52], [-5, 58], [-5, 64]], 4.5, { taper: [0.2, 0.8], wobble: 0 }), "#2c2d38")}
      ${path(brush([[-5, 63], [-12, 71], [-23, 73], [-33, 66]], 4.5, { taper: [0.12, 0.8], wobble: 0 }), "#2c2d38")}
      ${path(brush([[-5, 63], [2, 71], [13, 73], [23, 66]], 4.5, { taper: [0.12, 0.8], wobble: 0 }), "#2c2d38")}
      <g fill="#ece8e0" opacity="0.85">
        ${path(brush([[-64, 48], [-130, 34], [-200, 30]], 4, { taper: [0.05, 0.6] }))}
        ${path(brush([[-62, 58], [-136, 62], [-198, 74]], 3.5, { taper: [0.05, 0.6] }))}
        ${path(brush([[-58, 66], [-120, 84], [-170, 104]], 3, { taper: [0.05, 0.6] }))}
        ${path(brush([[46, 44], [120, 26], [196, 22]], 4, { taper: [0.05, 0.6] }))}
        ${path(brush([[48, 56], [124, 54], [188, 62]], 3.5, { taper: [0.05, 0.6] }))}
      </g>
    </g>
    <path id="fs-armpit" fill="#ebe6de" opacity="0"/>
    <g id="fs-wave" opacity="0" transform="translate(38 -190)">
      <circle r="31" fill="#ebe6de"/>
      <path d="M-27 -40 C-31 -70 -29 -112 -24 -150 C-14 -168 16 -168 24 -150 C30 -112 31 -70 28 -40 C27 -70 26 -110 20 -146 C12 -160 -12 -160 -20 -146 C-25 -110 -26 -70 -27 -40 Z" fill="#d6cfc4"/>
      <path d="M-26 10 C-30 -50 -26 -110 -21 -148 C-12 -164 13 -164 21 -148 C27 -110 29 -50 26 10 C10 18 -10 18 -26 10 Z" fill="#ebe6de"/>
      <path d="M-23 -128 C-25 -170 -12 -194 4 -196 C22 -196 33 -176 30.5 -150 C29.5 -140 29 -130 28.5 -120 C10 -124 -10 -124 -23 -128 Z" fill="#ebe6de"/>
      <g id="fs-beans">
      <path d="M-10 -158 C-8 -148 10 -146 14 -156 C14 -164 -8 -166 -10 -158 Z" fill="#df9aa8"/>
      <circle cx="-10" cy="-172" r="4.5" fill="#df9aa8"/><circle cx="2" cy="-180" r="5" fill="#df9aa8"/><circle cx="15" cy="-172" r="4.5" fill="#df9aa8"/>
      </g>
    </g>
  </g>
  <image id="fs-light" x="0" y="0" width="${W}" height="${H}" preserveAspectRatio="none" style="mix-blend-mode:screen" opacity="0.9"/>
  <rect width="${W}" height="${H}" fill="url(#fs-grain)" style="mix-blend-mode:overlay" opacity="0.28"/>
  <rect id="fs-torch" width="${W}" height="${H}" fill="url(#fs-torchg)" opacity="0"/>
  `;

  const holder = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  holder.setAttribute("width", "0"); holder.setAttribute("height", "0");
  holder.style.position = "absolute";
  holder.innerHTML = `<defs><g id="feedArt">${svg}</g></defs>`;
  document.body.prepend(holder);

  // lamp light and window spill, painted once with soft edges so no gradient shapes show
  (() => {
    const c = document.createElement("canvas"); c.width = 800; c.height = 450; const g = c.getContext("2d");
    g.filter = "blur(26px)";
    g.fillStyle = "rgba(255,196,128,0.22)";
    g.beginPath(); g.moveTo(583, 138); g.lineTo(612, 160); g.lineTo(560, 318); g.lineTo(330, 330); g.lineTo(380, 250); g.closePath(); g.fill();
    g.fillStyle = "rgba(255,190,120,0.2)";
    g.beginPath(); g.ellipse(470, 316, 210, 34, -0.02, 0, Math.PI * 2); g.fill();
    g.fillStyle = "rgba(255,200,140,0.12)";
    g.beginPath(); g.ellipse(500, 220, 170, 120, 0.3, 0, Math.PI * 2); g.fill();
    g.fillStyle = "rgba(120,140,220,0.07)";
    g.beginPath(); g.moveTo(140, 70); g.lineTo(310, 68); g.lineTo(380, 320); g.lineTo(120, 330); g.closePath(); g.fill();
    holder.querySelector("#fs-light").setAttribute("href", c.toDataURL());
    // print grain: clumped mono noise in a tile
    const n = document.createElement("canvas"); n.width = n.height = 320; const q = n.getContext("2d");
    const im = q.createImageData(320, 320); let sd = 99;
    for (let i = 0; i < im.data.length; i += 4) { sd = (sd * 16807) % 2147483647; const v = 128 + ((sd % 1000) / 1000 - 0.5) * 120; im.data[i] = im.data[i + 1] = im.data[i + 2] = v; im.data[i + 3] = 255; }
    q.putImageData(im, 0, 0);
    holder.querySelector("#fs-grain-img").setAttribute("href", n.toDataURL());
  })();

  const $ = id => document.getElementById(id);
  const el = {
    bb: $("fs-bb"), bm: $("fs-bm"), bc: $("fs-bc"), tail: $("fs-tail"), tailTip: $("fs-tail-tip"), tailSh: $("fs-tail-sh"), tag: $("fs-tag"), head: $("fs-head"),
    earL: $("fs-ear-l"), earR: $("fs-ear-r"), eyes: $("fs-eyes"), lids: $("fs-lids"),
    pl: $("fs-pupil-l"), pr: $("fs-pupil-r"), s1: $("fs-steam1"), s2: $("fs-steam2"),
    wave: $("fs-wave"), armpit: $("fs-armpit"), beans: $("fs-beans"), pawR: $("fs-paw-r"), torch: $("fs-torch"), shoot: $("fs-shoot"),
    bulbs: [...holder.querySelectorAll(".fs-bulb")], stars: [...holder.querySelectorAll(".fs-star")],
  };

  // a tapered tube along a Catmull-Rom curve with a rounded tip; from > 0 draws only the end part
  function tailPath(pts, w, from) {
    const S = [], n = pts.length, P = i => pts[Math.max(0, Math.min(n - 1, i))];
    for (let i = 0; i < n - 1; i++) for (let k = 0; k < 10; k++) {
      const t = k / 10, t2 = t * t, t3 = t2 * t, p0 = P(i - 1), p1 = P(i), p2 = P(i + 1), p3 = P(i + 2);
      const c = j => 0.5 * (2 * p1[j] + (-p0[j] + p2[j]) * t + (2 * p0[j] - 5 * p1[j] + 4 * p2[j] - p3[j]) * t2 + (-p0[j] + 3 * p1[j] - 3 * p2[j] + p3[j]) * t3);
      S.push([c(0), c(1)]);
    }
    S.push(pts[n - 1]);
    const m = S.length, L = [], R = [];
    let rTip = 0;
    for (let i = Math.floor(from * (m - 1)); i < m; i++) {
      const a = S[Math.max(0, i - 1)], b = S[Math.min(m - 1, i + 1)];
      let dx = b[0] - a[0], dy = b[1] - a[1]; const l = Math.hypot(dx, dy) || 1; dx /= l; dy /= l;
      const u = i / (m - 1), hw = w / 2 * (1 - 0.42 * u * u) * (from ? 1.01 : 1);
      L.push([S[i][0] - dy * hw, S[i][1] + dx * hw]); R.push([S[i][0] + dy * hw, S[i][1] - dx * hw]); rTip = hw;
    }
    const f = q => `${q[0].toFixed(1)} ${q[1].toFixed(1)}`;
    return `M${f(L[0])} ` + L.slice(1).map(q => "L" + f(q)).join(" ") + ` A${rTip.toFixed(1)} ${rTip.toFixed(1)} 0 0 0 ${f(R[R.length - 1])} ` + R.reverse().map(q => "L" + f(q)).join(" ") + " Z";
  }
  window.FEED_VIEWS = { ultra: [0, 0, 3200], main: [800, 450, 1600], tele: [1430, 740, 580] };
  window.FEED_SIZE = [W, H];

  // st: { t, beat, kick, hat, blurBg, blurMug, blurCat, wave, torch, squint }
  const clampF = v => Math.min(1, Math.max(0, v));
  // every repeating motion runs a whole number of times per loop, so the loop's last frame meets its first
  const per = (want) => { const L = window.FEED_LOOP; return L ? L / Math.max(1, Math.round(L / want)) : want; };
  window.updateFeed = function (st) {
    const t = st.t, L = window.FEED_LOOP || 0;
    el.bb.setAttribute("stdDeviation", st.blurBg.toFixed(2));
    el.bm.setAttribute("stdDeviation", st.blurMug.toFixed(2));
    el.bc.setAttribute("stdDeviation", st.blurCat.toFixed(2));
    const vb = st.vb ?? t / 0.75, lv = st.level ?? 1;
    // the tail curls round the front paw and its tip lifts lazily, rebuilt each frame so it bends as one piece
    const lift = 0.5 - 0.5 * Math.cos(Math.PI * vb / 2);
    const tp = [[100, -64], [160, -46], [212, -24], [254, -12], [290, -12 - 4 * lift], [316 - 4 * lift, -30 - 14 * lift], [320 - 12 * lift, -60 - 22 * lift]];
    el.tail.setAttribute("d", tailPath(tp, 36, 0));
    el.tailTip.setAttribute("d", tailPath(tp, 36, 0.74));
    el.tailSh.setAttribute("d", tailPath(tp.map(([x, y], i) => [x - 6, Math.max(y, -14) + 16 - i * 0.5]), 30, 0));
    el.tag.setAttribute("transform", `rotate(${(Math.sin(Math.PI * vb + 0.6) * 5 * lv).toFixed(2)} -4 -232)`);
    // a gentle nod: down on each beat, back up between, and a slow sway every two beats
    const bob = Math.cos(2 * Math.PI * vb) * 8 * lv;
    const tilt = Math.sin(Math.PI * vb) * 3 * lv + (st.headTilt || 0);
    el.head.setAttribute("transform", `translate(0 ${(-360 + bob).toFixed(1)}) rotate(${tilt.toFixed(2)})`);
    const twitch = Math.max(0, Math.sin(2 * Math.PI * t / per(st.twitchEvery ?? 2.73) + 1)) ** 30 * 14 * (st.twitch ?? 1);
    el.earL.setAttribute("transform", `rotate(${(-twitch).toFixed(2)} -112 -84)`);
    el.earR.setAttribute("transform", `rotate(${(twitch * 0.5).toFixed(2)} 104 -88)`);
    // blink every ~3.4s, plus squint when the torch is on
    const bp = per(3.4), ph = (t % bp) / bp;
    const blink = ph > 0.94 ? Math.sin((ph - 0.94) / 0.06 * Math.PI) : 0;
    const shut = Math.max(blink, st.squint || 0);
    el.eyes.setAttribute("transform", `scale(1 ${(1 - shut * 0.92).toFixed(3)})`);
    const lidOp = clampF((shut - 0.7) / 0.25) * clampF((st.squint || 0) * 2);
    el.lids.setAttribute("opacity", lidOp.toFixed(3));
    el.eyes.setAttribute("opacity", (1 - lidOp).toFixed(3));
    const look = Math.sin(2 * Math.PI * t / per(14)) * 7 * (st.lookK ?? 1) + (st.look || 0);
    el.pl.setAttribute("cx", look.toFixed(1)); el.pr.setAttribute("cx", look.toFixed(1));
    const pupil = 1 - (st.torch || 0) * 0.65;
    el.pl.setAttribute("rx", (12 * pupil + 2).toFixed(1)); el.pr.setAttribute("rx", (12 * pupil + 2).toFixed(1));
    // each wisp fades in, rises and drifts, then fades out fully before it starts again, so the restart never shows
    const sper = per(2.6);
    [[el.s1, sper, 0, 0.3], [el.s2, sper, 0.5, 0.24]].forEach(([e, pp, off, op]) => {
      const p = ((t / pp + off) % 1 + 1) % 1;
      e.setAttribute("transform", `translate(${(Math.sin(2 * Math.PI * t / per(3.7) + off * 6) * 8 + p * 6).toFixed(1)} ${(-p * 70).toFixed(1)})`);
      e.setAttribute("opacity", (op * Math.sin(Math.PI * p) ** 1.5).toFixed(3));
    });
    // waving paw
    const w = st.wave || 0;
    el.wave.setAttribute("opacity", clampF(w / 0.1).toFixed(3));
    const armA = 178 - 118 * w + Math.sin(t * 9) * 12 * w, ar = armA * Math.PI / 180;
    el.wave.setAttribute("transform", `translate(38 -190) rotate(${armA.toFixed(2)})`);
    // fur fills the armpit between the chest's edge and the underside of the raised arm, so no notch opens
    const under = sd => [38 + 26 * Math.cos(ar) + sd * Math.sin(ar), -190 + 26 * Math.sin(ar) - sd * Math.cos(ar)];
    const u1 = under(48), u0 = under(6);
    el.armpit.setAttribute("d", `M69 -112 Q71 -176 ${u1[0].toFixed(1)} ${u1[1].toFixed(1)} L${u0[0].toFixed(1)} ${u0[1].toFixed(1)} L44 -112 Z`);
    el.armpit.setAttribute("opacity", clampF((w - 0.35) / 0.3).toFixed(3));
    el.pawR.setAttribute("opacity", (1 - clampF(w / 0.15)).toFixed(3));
    el.beans.setAttribute("opacity", clampF((w - 0.35) / 0.3).toFixed(3));
    el.torch.setAttribute("opacity", (st.torch || 0).toFixed(3));
    // a slow chase that travels along the string once every two beats, lifted a little by the hi-hats
    el.bulbs.forEach((b, i) => {
      const on = 0.42 + 0.38 * (0.5 + 0.5 * Math.cos(2 * Math.PI * (vb / 2 - i / 6))) + 0.2 * (st.hat || 0);
      b.setAttribute("opacity", Math.min(1, on).toFixed(3));
    });
    el.stars.forEach((s, i) => { if (i % 5 === 0) s.setAttribute("opacity", (0.3 + 0.5 * (0.5 + 0.5 * Math.sin(2 * Math.PI * t / per(3.14) + i))).toFixed(2)); });
    // a shooting star every 8 seconds
    const shp = per(8), sp = st.noShoot ? 9 : (t % shp) / 0.7;
    if (sp < 1) {
      const x = 640 + sp * 420, y = 330 + sp * 150;
      el.shoot.setAttribute("x1", x.toFixed(0)); el.shoot.setAttribute("y1", y.toFixed(0));
      el.shoot.setAttribute("x2", (x - 90).toFixed(0)); el.shoot.setAttribute("y2", (y - 32).toFixed(0));
      el.shoot.setAttribute("opacity", Math.sin(sp * Math.PI).toFixed(2));
    } else el.shoot.setAttribute("opacity", "0");
  };

  // A feed viewport: an <svg> showing the art through a view box, with exposure and white balance overlays
  window.makeFeed = function (cls) {
    const ns = "http://www.w3.org/2000/svg";
    const s = document.createElementNS(ns, "svg");
    s.setAttribute("class", "feed " + (cls || ""));
    s.setAttribute("preserveAspectRatio", "xMidYMid slice");
    s.innerHTML = `<g class="feed-xf"><use href="#feedArt"/></g>
      <rect class="feed-wb" x="-9999" y="-9999" width="29999" height="29999" fill="#ff9a3c" opacity="0" style="mix-blend-mode:soft-light"/>
      <rect class="feed-ex" x="-9999" y="-9999" width="29999" height="29999" fill="#000" opacity="0"/>`;
    const xf = s.querySelector(".feed-xf"), wb = s.querySelector(".feed-wb"), ex = s.querySelector(".feed-ex");
    s.setView = (v, opts = {}) => {
      const [x, y, w] = v, h = w * 9 / 16;
      s.setAttribute("viewBox", `${x.toFixed(2)} ${y.toFixed(2)} ${w.toFixed(2)} ${h.toFixed(2)}`);
      wb.setAttribute("x", x); wb.setAttribute("y", y); wb.setAttribute("width", w); wb.setAttribute("height", h);
      ex.setAttribute("x", x); ex.setAttribute("y", y); ex.setAttribute("width", w); ex.setAttribute("height", h);
      // flips and rotation happen around the view centre
      const cx = x + w / 2, cy = y + h / 2;
      const fx = opts.flipX ?? 1, fy = opts.flipY ?? 1, rot = opts.rot || 0, rs = opts.rotScale ?? 1;
      xf.setAttribute("transform", `translate(${cx} ${cy}) rotate(${rot}) scale(${fx * rs} ${fy * rs}) translate(${-cx} ${-cy})`);
      // exposure: below 0 darkens with black, above 0 brightens with a CSS filter
      const e = opts.exposure || 0;
      ex.setAttribute("fill", e < 0 ? "#000" : "#fff");
      ex.setAttribute("opacity", Math.min(0.85, Math.abs(e) * (e < 0 ? 0.42 : 0.3)).toFixed(3));
      // white balance: a low Kelvin setting makes the picture bluer, a high one warmer
      const k = opts.wb || 0;
      wb.setAttribute("fill", k < 0 ? "#3b7bff" : "#ff8a2a");
      wb.setAttribute("opacity", Math.min(1, Math.abs(k)).toFixed(3));
      s.style.filter = opts.filter || "";
    };
    s.setView(window.FEED_VIEWS.main);
    return s;
  };
})();
