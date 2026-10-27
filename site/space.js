import * as THREE from "three";

const reduced = matchMedia("(prefers-reduced-motion: reduce)");
const canvas = document.getElementById("space");
const journey = window.telescopeJourney;

const hex = h => new THREE.Vector3(parseInt(h.slice(1, 3), 16) / 255, parseInt(h.slice(3, 5), 16) / 255, parseInt(h.slice(5, 7), 16) / 255);
const smooth = t => t * t * (3 - 2 * t);
// Like smooth, but it also starts and ends with no acceleration, for motion that has to merge in without a jolt
const smoother = t => t * t * t * (t * (6 * t - 15) + 10);
const clamp01 = t => Math.min(1, Math.max(0, t));
const LIGHT = new THREE.Vector3(-0.55, 0.45, 0.7).normalize();
const BG = "#0f1216";

let renderer;
try {
  if (reduced.matches) throw new Error("reduced motion");
  // Falls back to the static page when WebGL would run on the CPU, which can't keep up with the scene
  renderer = new THREE.WebGLRenderer({ canvas, antialias: true, powerPreference: "high-performance", failIfMajorPerformanceCaveat: true });
} catch {
  renderer = null;
}

if (renderer) start();

function start() {
  document.body.classList.remove("static");
  document.body.classList.add("space");
  renderer.setClearColor(BG);
  const scene = new THREE.Scene();
  const camera = new THREE.PerspectiveCamera(50, 1, 0.1, 900);
  const timer = new THREE.Timer();

  // Shared GLSL: value noise and fbm, enough for planet surfaces
  const NOISE = /* glsl */`
    float hash(vec3 p) { p = fract(p * 0.3183099 + 0.1); p *= 17.0; return fract(p.x * p.y * p.z * (p.x + p.y + p.z)); }
    float noise(vec3 x) {
      vec3 i = floor(x), f = fract(x); f = f * f * (3.0 - 2.0 * f);
      return mix(mix(mix(hash(i), hash(i + vec3(1, 0, 0)), f.x), mix(hash(i + vec3(0, 1, 0)), hash(i + vec3(1, 1, 0)), f.x), f.y),
                 mix(mix(hash(i + vec3(0, 0, 1)), hash(i + vec3(1, 0, 1)), f.x), mix(hash(i + vec3(0, 1, 1)), hash(i + vec3(1, 1, 1)), f.x), f.y), f.z);
    }
    float fbm3(vec3 p) { float v = 0.0, a = 0.5; for (int i = 0; i < 3; i++) { v += a * noise(p); p = p * 2.03 + 11.7; a *= 0.5; } return v; }
    float fbm(vec3 p) { float v = 0.0, a = 0.5; for (int i = 0; i < 5; i++) { v += a * noise(p); p = p * 2.03 + 11.7; a *= 0.5; } return v; }
  `;

  // ── Star trails: every star is a ribbon arc around the travel axis, turned by uTime and stretched by speed ──
  const TRAIL_SEGS = 14;
  const STAR_COUNT = innerWidth < 700 ? 3200 : 5300;
  const trails = (() => {
    const verts = STAR_COUNT * (TRAIL_SEGS + 1) * 2;
    const star = new Float32Array(verts * 4), along = new Float32Array(verts * 2), tone = new Float32Array(verts);
    const index = [];
    let seed = 1609;
    const rand = () => (seed = (seed * 16807) % 2147483647) / 2147483647;
    let v = 0;
    for (let s = 0; s < STAR_COUNT; s++) {
      const r = 3.2 + Math.pow(rand(), 0.75) * 78;
      const theta = rand() * Math.PI * 2;
      const z = 24 - rand() * 600;
      const mag = Math.pow(rand(), 3.2);
      const t = rand();
      for (let k = 0; k <= TRAIL_SEGS; k++) {
        for (const side of [-1, 1]) {
          star.set([r, theta, z, mag], v * 4);
          along.set([k / TRAIL_SEGS, side], v * 2);
          tone[v] = t;
          v++;
        }
        if (k < TRAIL_SEGS) {
          const a = v - 2;
          index.push(a, a + 1, a + 2, a + 1, a + 3, a + 2);
        }
      }
    }
    const geo = new THREE.BufferGeometry();
    geo.setAttribute("position", new THREE.BufferAttribute(new Float32Array(verts * 3), 3));
    geo.setAttribute("aStar", new THREE.BufferAttribute(star, 4));
    geo.setAttribute("aAlong", new THREE.BufferAttribute(along, 2));
    geo.setAttribute("aTone", new THREE.BufferAttribute(tone, 1));
    geo.setIndex(index);
    // uFocus goes 0 -> 1 as you pass through the pupil: the arcs shrink into sharp points
    const uniforms = {
      uSpin: { value: 0 }, uStretch: { value: 0 }, uPx: { value: 0.002 }, uLen: { value: 0.42 },
      uFocus: { value: 0 }, uBoost: { value: 1 }, uDpr: { value: 1 },
    };
    const TONE = /* glsl */`
      vec3 tone(float t) {
        vec3 col = mix(vec3(0.89, 0.9, 1.0), vec3(0.86, 0.82, 1.0), step(0.6, t));
        return mix(col, vec3(1.0, 0.94, 0.86), step(0.9, t));
      }`;
    const mat = new THREE.ShaderMaterial({
      transparent: false, depthWrite: false, depthTest: false, blending: THREE.AdditiveBlending, uniforms,
      side: THREE.DoubleSide, // streak ribbons stand on edge, so either face can point at the camera
      vertexShader: /* glsl */`
        attribute vec4 aStar; attribute vec2 aAlong; attribute float aTone;
        uniform float uSpin, uStretch, uPx, uLen, uFocus, uBoost;
        varying float vAlpha; varying float vSide; varying float vTone;
        void main() {
          float t = aAlong.x;
          float ang = aStar.y + uSpin - (1.0 - t) * uLen;
          vec2 dir = vec2(cos(ang), sin(ang));
          // Signed: the streak trails behind the star the way it is moving, and never reaches past the camera
          vec3 p = vec3(dir * aStar.x, min(aStar.z - (1.0 - t) * uStretch, max(aStar.z, cameraPosition.z - 2.0)));
          vec4 mv = viewMatrix * vec4(p, 1.0);
          float depth = max(0.1, -mv.z);
          // Width goes across the arc for trails and across the streak when speed stretches it toward you
          float w = abs(uStretch) / (abs(uStretch) + aStar.x * uLen + 0.001);
          vec2 across = normalize(mix(dir, vec2(-dir.y, dir.x), w));
          float widthPx = (0.7 + aStar.w * 1.7) * mix(1.0, 0.85 + 0.2 * uBoost, uFocus);
          p.xy += across * aAlong.y * 0.5 * widthPx * uPx * depth;
          gl_Position = projectionMatrix * viewMatrix * vec4(p, 1.0);
          float fade = smoothstep(330.0, 140.0, depth) * smoothstep(1.5, 9.0, depth);
          float streak = smoothstep(0.2, 3.0, abs(uStretch)) * uBoost;
          float base = mix(0.045 + aStar.w * 0.7, 0.07 + aStar.w * 0.75, uFocus);
          vAlpha = base * pow(t, mix(1.7, 2.4, uFocus)) * fade * mix(1.0, streak, uFocus);
          vSide = aAlong.y; vTone = aTone;
        }`,
      fragmentShader: TONE + /* glsl */`
        varying float vAlpha; varying float vSide; varying float vTone;
        void main() {
          float edge = 1.0 - abs(vSide);
          gl_FragColor = vec4(tone(vTone) * vAlpha * smoothstep(0.0, 0.6, edge + 0.35), 1.0);
        }`,
    });
    const mesh = new THREE.Mesh(geo, mat);
    // Drawn first, in the opaque pass, so planets cover the stars behind and in front of them alike
    mesh.frustumCulled = false;
    mesh.renderOrder = -10;
    scene.add(mesh);

    // The same stars as sharp points, one per star at the head of its trail
    const pts = new THREE.BufferGeometry();
    const pStar = new Float32Array(STAR_COUNT * 4), pTone = new Float32Array(STAR_COUNT);
    for (let i = 0; i < STAR_COUNT; i++) {
      pStar.set(star.subarray(i * (TRAIL_SEGS + 1) * 8, i * (TRAIL_SEGS + 1) * 8 + 4), i * 4);
      pTone[i] = tone[i * (TRAIL_SEGS + 1) * 2];
    }
    pts.setAttribute("position", new THREE.BufferAttribute(new Float32Array(STAR_COUNT * 3), 3));
    pts.setAttribute("aStar", new THREE.BufferAttribute(pStar, 4));
    pts.setAttribute("aTone", new THREE.BufferAttribute(pTone, 1));
    const points = new THREE.Points(pts, new THREE.ShaderMaterial({
      transparent: false, depthWrite: false, depthTest: false, blending: THREE.AdditiveBlending, uniforms,
      vertexShader: /* glsl */`
        attribute vec4 aStar; attribute float aTone;
        uniform float uSpin, uFocus, uDpr;
        varying float vAlpha; varying float vTone;
        void main() {
          float ang = aStar.y + uSpin;
          vec4 mv = viewMatrix * vec4(vec2(cos(ang), sin(ang)) * aStar.x, aStar.z, 1.0);
          float depth = max(0.1, -mv.z);
          gl_Position = projectionMatrix * mv;
          gl_PointSize = (1.5 + aStar.w * 3.6) * uDpr * mix(0.75, 1.0, smoothstep(220.0, 40.0, depth));
          vAlpha = (0.22 + aStar.w * 0.78) * smoothstep(330.0, 140.0, depth) * smoothstep(1.5, 9.0, depth) * uFocus;
          vTone = aTone;
        }`,
      fragmentShader: TONE + /* glsl */`
        varying float vAlpha; varying float vTone;
        void main() {
          float d = length(gl_PointCoord - 0.5) * 2.0;
          float a = (1.0 - smoothstep(0.35, 1.0, d)) + 0.6 * (1.0 - smoothstep(0.0, 0.35, d));
          gl_FragColor = vec4(tone(vTone) * vAlpha * a, 1.0);
        }`,
    }));
    points.frustumCulled = false;
    points.renderOrder = -9;
    scene.add(points);
    return mat;
  })();

  // ── The lens from the banner, with an open pupil to fly through ──
  const lens = new THREE.Group();
  scene.add(lens);
  const LR = 5.7, PR = LR * 86 / 190;
  const flatMat = (frag, extra = {}) => new THREE.ShaderMaterial({
    transparent: true, depthWrite: false, side: THREE.DoubleSide,
    uniforms: { uFade: { value: 1 }, ...extra },
    vertexShader: `varying vec2 vP; void main() { vP = position.xy; gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0); }`,
    fragmentShader: frag,
  });
  const glow = new THREE.Mesh(new THREE.PlaneGeometry(LR * 6, LR * 6), flatMat(`
    varying vec2 vP; uniform float uFade;
    void main() { float d = length(vP) / ${(LR * 2.4).toFixed(2)}; float a = (0.42 * (1.0 - smoothstep(0.0, 0.55, d)) + 0.1 * (1.0 - smoothstep(0.55, 1.0, d))) * (1.0 - smoothstep(0.9, 1.0, d));
      gl_FragColor = vec4(0.416, 0.345, 0.812, a * uFade); }`));
  glow.position.z = -0.6;
  lens.add(glow);
  const disc = new THREE.Mesh(new THREE.RingGeometry(PR, LR, 160, 1), flatMat(`
    varying vec2 vP; uniform float uFade;
    void main() { float d = clamp(length(vP - vec2(${(-LR * 0.32).toFixed(2)}, ${(LR * 0.37).toFixed(2)})) / ${(LR * 1.37).toFixed(2)}, 0.0, 1.0);
      gl_FragColor = vec4(mix(vec3(0.804, 0.769, 0.98), vec3(0.667, 0.612, 0.961), d), uFade); }`));
  lens.add(disc);
  const pupil = new THREE.Mesh(new THREE.CircleGeometry(PR, 96), flatMat(`
    varying vec2 vP; uniform float uFade;
    void main() { float d = clamp(length(vP - vec2(${(PR * 0.2).toFixed(2)}, ${(-PR * 0.25).toFixed(2)})) / ${(PR * 1.25).toFixed(2)}, 0.0, 1.0);
      vec3 c = mix(vec3(0.086, 0.102, 0.125), vec3(0.039, 0.047, 0.059), d);
      vec2 g = (vP - vec2(${(-PR * 0.33).toFixed(2)}, ${(PR * 0.36).toFixed(2)})) * mat2(0.88, -0.47, 0.47, 0.88) / vec2(${(PR * 0.22).toFixed(2)}, ${(PR * 0.15).toFixed(2)});
      float glint = (1.0 - smoothstep(0.85, 1.0, length(g))) * 0.22;
      gl_FragColor = vec4(mix(c, vec3(1.0), glint), uFade); }`));
  pupil.position.z = -0.02;
  lens.add(pupil);

  // ── Planets ──
  // Fades in as real transparency, so the stars behind show through until the planet is fully there
  const planetMat = (mode, cols, atmo, seed) => new THREE.ShaderMaterial({
    transparent: true,
    uniforms: {
      uA: { value: hex(cols[0]) }, uB: { value: hex(cols[1]) }, uC: { value: hex(cols[2]) }, uAtmo: { value: hex(atmo) },
      uLight: { value: LIGHT }, uSeed: { value: seed }, uTime: { value: 0 }, uFade: { value: 1 },
    },
    vertexShader: /* glsl */`
      varying vec3 vObj; varying vec3 vN; varying vec3 vW;
      void main() { vObj = normalize(position); vN = normalize(mat3(modelMatrix) * normal); vec4 w = modelMatrix * vec4(position, 1.0); vW = w.xyz; gl_Position = projectionMatrix * viewMatrix * w; }`,
    fragmentShader: NOISE + /* glsl */`
      uniform vec3 uA, uB, uC, uAtmo, uLight; uniform float uSeed, uTime, uFade; uniform mat4 modelMatrix;
      varying vec3 vObj; varying vec3 vN; varying vec3 vW;
      // Height for the surface relief, which only bends the shading normal
      float relief(vec3 p) {
        #if ${mode} == 0
          return fbm3(p * 2.4) * 0.6 + fbm3(p * 8.0) * 0.4;
        #elif ${mode} == 1
          return fbm3(p * 3.0) * 0.8 + noise(p * 12.0) * 0.2;
        #else
          return fbm3(p * 1.9) * 0.7 + noise(p * 12.0) * 0.3;
        #endif
      }
      void main() {
        vec3 p = vObj + uSeed;
        vec3 col; float lights = 0.0; float bump = ${[0.03, 0.014, 0, 0.02][mode].toFixed(3)};
        #if ${mode} == 0
          float n = fbm(p * 2.4); float d = fbm(p * 9.0);
          col = mix(uA, uB, smoothstep(0.38, 0.66, n)); col = mix(col, uC, smoothstep(0.62, 0.8, d) * 0.5);
        #elif ${mode} == 1
          vec3 q = p * 1.5 + fbm(p * 1.7) * 1.4; float n = fbm(q);
          float b = 0.5 + 0.5 * sin(n * 11.0 + vObj.y * 3.0);
          col = mix(uA, uB, b); col = mix(col, uC, smoothstep(0.62, 0.78, fbm(q * 2.0)));
        #elif ${mode} == 2
          float v = vObj.y * 6.0 + fbm(p * vec3(2.0, 9.0, 2.0) + vec3(uTime * 0.01, 0.0, 0.0)) * 1.3;
          float b = 0.5 + 0.5 * sin(v * 2.6);
          col = mix(uA, uB, b); col = mix(col, uC, smoothstep(0.55, 0.95, sin(v * 1.1 + 1.3)) * 0.7);
        #else
          float land = smoothstep(0.5, 0.54, fbm(p * 1.9));
          col = mix(uA, uB, land);
          float cloud = smoothstep(0.56, 0.78, fbm(p * 3.2 + vec3(uTime * 0.006, 0.0, 0.0)));
          col = mix(col, vec3(0.86, 0.85, 0.95), cloud * 0.75);
          lights = land * step(0.74, noise(p * 70.0)) * (1.0 - cloud);
          bump *= land * (1.0 - cloud);
        #endif
        vec3 N0 = normalize(vN), N = N0; vec3 V = normalize(cameraPosition - vW);
        #if ${mode} != 2
          bump *= smoothstep(0.05, 0.5, dot(N0, V));
          float e = 0.006, h0 = relief(p);
          vec3 g = (vec3(relief(p + vec3(e, 0.0, 0.0)), relief(p + vec3(0.0, e, 0.0)), relief(p + vec3(0.0, 0.0, e))) - h0) / e;
          g -= dot(g, vObj) * vObj;
          N = normalize(mat3(modelMatrix) * normalize(vObj - g * bump));
        #endif
        float ndl = dot(N, uLight);
        float day = smoothstep(-0.12, 0.55, ndl);
        vec3 lit = col * (0.035 + day * 1.05);
        lit += uC * lights * (1.0 - smoothstep(-0.25, 0.05, ndl)) * 1.4;
        float rim = pow(1.0 - max(dot(N0, V), 0.0), 2.6);
        lit += uAtmo * rim * (0.15 + day * 0.85);
        gl_FragColor = vec4(lit, uFade);
      }`,
  });
  // Atmosphere: glow by how close each view ray passes the planet, so it fades out instead of ending in a hard ring
  const atmosphere = (radius, color) => new THREE.Mesh(new THREE.SphereGeometry(radius * 1.16, 64, 48), new THREE.ShaderMaterial({
    transparent: true, depthWrite: false, side: THREE.BackSide, blending: THREE.AdditiveBlending,
    uniforms: { uC: { value: hex(color) }, uLight: { value: LIGHT }, uFade: { value: 1 }, uR: { value: radius }, uRs: { value: radius * 1.16 } },
    vertexShader: `varying vec3 vW; varying vec3 vCenter; varying float vScale;
      void main() { vec4 w = modelMatrix * vec4(position, 1.0); vW = w.xyz; vCenter = (modelMatrix * vec4(0.0, 0.0, 0.0, 1.0)).xyz; vScale = length(modelMatrix[0].xyz); gl_Position = projectionMatrix * viewMatrix * w; }`,
    fragmentShader: `uniform vec3 uC, uLight; uniform float uFade, uR, uRs; varying vec3 vW; varying vec3 vCenter; varying float vScale;
      void main() {
        vec3 dir = normalize(vW - cameraPosition); vec3 toC = vCenter - cameraPosition;
        vec3 closest = cameraPosition + dir * dot(toC, dir);
        float d = length(closest - vCenter) / vScale;
        float g = 1.0 - smoothstep(uR * 0.985, uRs, d); g *= g;
        float day = smoothstep(-0.35, 0.6, dot(normalize(closest - vCenter), uLight));
        gl_FragColor = vec4(uC * g * (0.12 + day) * 0.85 * uFade, 1.0); }`,
  }));
  const orbitLine = (radius, opacity = 0.16) => {
    const pts = [];
    for (let i = 0; i <= 160; i++) { const a = i / 160 * Math.PI * 2; pts.push(new THREE.Vector3(Math.cos(a) * radius, 0, Math.sin(a) * radius)); }
    return new THREE.Line(new THREE.BufferGeometry().setFromPoints(pts), new THREE.LineBasicMaterial({ color: 0xaa9cf5, transparent: true, opacity, depthWrite: false }));
  };
  const moon = (radius, cols, seed) => new THREE.Mesh(new THREE.SphereGeometry(radius, 48, 32), planetMat(0, cols, "#8b93a8", seed));

  const planets = [];
  const makeStop = (build) => {
    const group = new THREE.Group();
    scene.add(group);
    const stop = { group, tick: () => {}, ...build(group) };
    planets.push(stop);
    return stop;
  };

  const glowTex = (() => {
    const c = document.createElement("canvas");
    c.width = c.height = 64;
    const g = c.getContext("2d"), grad = g.createRadialGradient(32, 32, 0, 32, 32, 32);
    grad.addColorStop(0, "rgba(255,255,255,1)"); grad.addColorStop(0.18, "rgba(255,255,255,0.85)"); grad.addColorStop(0.45, "rgba(255,255,255,0.18)"); grad.addColorStop(1, "rgba(255,255,255,0)");
    g.fillStyle = grad;
    g.fillRect(0, 0, 64, 64);
    return new THREE.CanvasTexture(c);
  })();
  // Sprites that set their own opacity every frame, so the stop fade is applied by their tick instead
  const glowSprite = color => {
    const m = new THREE.Sprite(new THREE.SpriteMaterial({ map: glowTex, color, transparent: true, depthWrite: false, blending: THREE.AdditiveBlending }));
    m.material.userData.own = true;
    return m;
  };

  // 1: connection, a slate world whose moons stream data from one to the other
  makeStop(group => {
    const R = 8, D = R * 1.75, GAP = 1.35;
    const body = new THREE.Mesh(new THREE.SphereGeometry(R, 96, 64), planetMat(0, ["#2c3648", "#5d6f91", "#a9b6d4"], "#8fa3d9", 3.1));
    group.add(body, atmosphere(R, "#7d8fd0"));
    const orbit = new THREE.Group();
    orbit.rotation.set(0.42, 0, -0.22);
    group.add(orbit);
    orbit.add(orbitLine(D, 0.12));
    // The moons ride a carrier that turns, so the link between them keeps its shape
    const carrier = new THREE.Group();
    orbit.add(carrier);
    const a = moon(0.95, ["#5a5f6e", "#9aa0b2", "#c9cede"], 7.2), b = moon(0.8, ["#5f5a6e", "#a59db8", "#d6d0e6"], 1.4);
    a.position.set(D, 0, 0);
    b.position.set(Math.cos(GAP) * D, 0, Math.sin(GAP) * D);
    carrier.add(a, b);
    // The link hops out of the orbit plane like a beam between the two
    const arc = u => new THREE.Vector3(Math.cos(GAP * u) * D, Math.sin(Math.PI * u) * D * 0.2, Math.sin(GAP * u) * D);
    const arcPts = [];
    for (let i = 0; i <= 80; i++) arcPts.push(arc(0.06 + i / 80 * 0.88));
    carrier.add(new THREE.Line(new THREE.BufferGeometry().setFromPoints(arcPts), new THREE.LineBasicMaterial({ color: 0xaa9cf5, transparent: true, opacity: 0.32, depthWrite: false })));
    const PACKETS = 6, TAIL = 5;
    const packets = [];
    for (let i = 0; i < PACKETS; i++) {
      const parts = [];
      for (let j = 0; j < TAIL; j++) { const sp = glowSprite(j ? 0xaa9cf5 : 0xf1edff); carrier.add(sp); parts.push(sp); }
      packets.push(parts);
    }
    const halo = glowSprite(0xcdc4fa);
    halo.position.copy(b.position);
    carrier.add(halo);
    const RATE = 0.32;
    return {
      body, R,
      tick(t, { fade }) {
        body.rotation.y = t * 0.03;
        carrier.rotation.y = -1.72 + 0.12 * Math.sin(t * 0.06);
        a.rotation.y = b.rotation.y = t * 0.2;
        packets.forEach((parts, i) => {
          const u = (t * RATE + i / PACKETS) % 1;
          const life = Math.sin(Math.PI * clamp01((u - 0.04) / 0.92));
          parts.forEach((sp, j) => {
            const uj = Math.max(0, u - j * 0.012);
            sp.position.copy(arc(uj));
            sp.scale.setScalar((j ? 0.75 - j * 0.1 : 0.85));
            sp.material.opacity = life * fade * (j ? 0.5 - j * 0.08 : 1);
          });
        });
        // The receiving moon brightens a little each time a packet lands
        const since = ((t * RATE * PACKETS) % 1) / (RATE * PACKETS);
        halo.scale.setScalar(3.4 + 0.5 * Math.exp(-since * 5));
        halo.material.opacity = (0.16 + 0.32 * Math.exp(-since * 5)) * fade;
      },
    };
  });

  // 2: lenses, a small lavender world
  makeStop(group => {
    const R = 6.4;
    const body = new THREE.Mesh(new THREE.SphereGeometry(R, 96, 64), planetMat(1, ["#4a3d99", "#8e80e0", "#cfc6fb"], "#b5a8ff", 8.3));
    group.add(body, atmosphere(R, "#a090ff"));
    return { body, R, tick(t) { body.rotation.y = t * 0.02; body.material.uniforms.uTime.value = t; } };
  });

  // 3: setup, a small icy world with one moon on a slow orbit
  makeStop(group => {
    const R = 5.2, D = R * 2.1;
    const body = new THREE.Mesh(new THREE.SphereGeometry(R, 96, 64), planetMat(0, ["#2f4a5c", "#6f9bb0", "#d4e6ee"], "#8fc4dd", 11.7));
    group.add(body, atmosphere(R, "#8fc4dd"));
    const orbit = new THREE.Group();
    orbit.rotation.set(0.3, 0, 0.18);
    orbit.add(orbitLine(D, 0.1));
    const m = moon(0.6, ["#5b6470", "#97a1ad", "#d0d6de"], 4.4);
    orbit.add(m);
    group.add(orbit);
    return {
      body, R,
      tick(t) {
        body.rotation.y = t * 0.025;
        const a = 2.2 + t * 0.05;
        m.position.set(Math.cos(a) * D, 0, Math.sin(a) * D);
      },
    };
  });

  // 4: plugins, two small worlds circling each other, one cool and one warm
  makeStop(group => {
    const R = 6;
    const pair = new THREE.Group();
    pair.rotation.set(0.35, 0, -0.25);
    group.add(pair);
    const cool = new THREE.Mesh(new THREE.SphereGeometry(3.4, 80, 56), planetMat(1, ["#33405e", "#7486b4", "#c9d3ef"], "#98a9e0", 2.6));
    const warm = new THREE.Mesh(new THREE.SphereGeometry(2.5, 80, 56), planetMat(0, ["#6b3f52", "#c27f8f", "#f0cfc4"], "#e6a2b4", 9.9));
    const coolAt = new THREE.Group(), warmAt = new THREE.Group();
    coolAt.add(cool, atmosphere(3.4, "#98a9e0"));
    warmAt.add(warm, atmosphere(2.5, "#e6a2b4"));
    pair.add(coolAt, warmAt, orbitLine(5.6, 0.08));
    return {
      body: cool, R,
      tick(t) {
        // The heavier one swings less, around the shared middle
        const a = 0.6 + t * 0.07;
        coolAt.position.set(-Math.cos(a) * 2.4, 0, -Math.sin(a) * 2.4);
        warmAt.position.set(Math.cos(a) * 3.2, 0, Math.sin(a) * 3.2);
        cool.rotation.y = t * 0.05;
        warm.rotation.y = -t * 0.08;
      },
    };
  });

  // 5: set and forget, a ringed giant
  makeStop(group => {
    const R = 8;
    const body = new THREE.Mesh(new THREE.SphereGeometry(R, 96, 64), planetMat(2, ["#4f4380", "#9d90c4", "#cdbba8"], "#a898e0", 5.5));
    const tilt = new THREE.Group();
    tilt.rotation.set(0.38, 0, 0.32);
    tilt.add(body, atmosphere(R, "#b3a3e8"));
    const ringMat = new THREE.ShaderMaterial({
      transparent: true, depthWrite: false, side: THREE.DoubleSide,
      uniforms: { uLight: { value: LIGHT }, uCenter: { value: new THREE.Vector3() }, uR: { value: R }, uShadowR: { value: R }, uFade: { value: 1 } },
      vertexShader: `varying vec2 vP; varying vec3 vW; void main() { vP = position.xy; vec4 w = modelMatrix * vec4(position, 1.0); vW = w.xyz; gl_Position = projectionMatrix * viewMatrix * w; }`,
      fragmentShader: NOISE + `uniform vec3 uLight, uCenter; uniform float uR, uShadowR, uFade; varying vec2 vP; varying vec3 vW;
        void main() { float r = length(vP) / uR;
          float bands = 0.45 + 0.55 * noise(vec3(r * 22.0, 0.0, 0.0)) * (0.6 + 0.4 * sin(r * 70.0));
          float a = bands * smoothstep(1.35, 1.5, r) * (1.0 - smoothstep(2.15, 2.35, r)) * (1.0 - 0.7 * smoothstep(1.78, 1.82, r) * (1.0 - smoothstep(1.86, 1.9, r)));
          vec3 d = vW - uCenter; float along = dot(d, uLight); float perp = length(d - along * uLight);
          float shadow = along < 0.0 ? smoothstep(uShadowR * 0.92, uShadowR * 1.04, perp) : 1.0;
          vec3 col = mix(vec3(0.78, 0.72, 0.9), vec3(0.93, 0.86, 0.8), noise(vec3(r * 9.0, 2.0, 0.0)));
          gl_FragColor = vec4(col * (0.15 + 0.85 * shadow), a * 0.75 * uFade); }`,
    });
    const ring = new THREE.Mesh(new THREE.RingGeometry(R * 1.3, R * 2.4, 256, 1), ringMat);
    ring.rotation.x = Math.PI / 2;
    tilt.add(ring);
    group.add(tilt);

    return {
      body, R,
      tick(t) {
        body.material.uniforms.uTime.value = t;
        group.getWorldPosition(ringMat.uniforms.uCenter.value);
        // The shadow is worked out in world space, so it shrinks with the planet on narrow screens
        ringMat.uniforms.uShadowR.value = R * group.scale.x;
      },
    };
  });

  // 6: questions, a small dusky world
  makeStop(group => {
    const R = 4.6;
    const body = new THREE.Mesh(new THREE.SphereGeometry(R, 96, 64), planetMat(1, ["#4a2f48", "#93617f", "#e2bfc9"], "#d79ab8", 6.6));
    group.add(body, atmosphere(R, "#d79ab8"));
    return { body, R, tick(t) { body.rotation.y = -t * 0.02; } };
  });

  // 7: home, a big world you arrive over, with a lit night side
  makeStop(group => {
    const R = 40;
    const body = new THREE.Mesh(new THREE.SphereGeometry(R, 160, 120), planetMat(3, ["#1b2148", "#3b3f6e", "#cdb8ff"], "#8f9cff", 4.4));
    group.add(body, atmosphere(R, "#7f8cf0"));
    return { body, R, tick(t) { body.rotation.y = t * 0.008; body.material.uniforms.uTime.value = t; } };
  });

  // ── Nebulae: faint dust far behind everything, so it only grows a little over the whole trip ──
  const nebula = (color, seed, size, x, y, z, strength) => {
    const m = new THREE.Mesh(new THREE.PlaneGeometry(size, size), new THREE.ShaderMaterial({
      transparent: true, depthWrite: false, blending: THREE.AdditiveBlending,
      uniforms: { uC: { value: hex(color) }, uSeed: { value: seed }, uK: { value: strength }, uTime: { value: 0 } },
      vertexShader: `varying vec2 vU; void main() { vU = uv * 2.0 - 1.0; gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0); }`,
      fragmentShader: NOISE + `uniform vec3 uC; uniform float uSeed, uK, uTime; varying vec2 vU;
        void main() {
          vec3 p = vec3(vU * 1.6, uSeed) + vec3(uTime * 0.004, 0.0, 0.0);
          vec2 warp = vec2(fbm(p + 3.1), fbm(p + 7.7)) - 0.5;
          float d = fbm(vec3(vU * 2.2 + warp * 1.8, uSeed));
          float edge = 1.0 - smoothstep(0.35, 1.0, length(vU + warp * 0.4));
          float a = smoothstep(0.42, 0.85, d) * edge;
          gl_FragColor = vec4(uC * a * uK, 1.0); }`,
    }));
    m.position.set(x, y, z);
    m.renderOrder = -1;
    scene.add(m);
    return m;
  };
  const nebulae = [
    nebula("#7b6cd9", 1.3, 520, -230, 70, -820, 0.16),
    nebula("#4f8fae", 5.8, 420, 260, -90, -760, 0.11),
    nebula("#b56d93", 9.2, 380, 90, 170, -880, 0.09),
  ];

  // ── Where everything sits for the current screen shape ──
  const STOP_GAP = 64, PLANET_AHEAD = 32;
  const stations = [];
  let viewW = 0, viewH = 0;
  let halfW = 10, halfH = 10, narrow = false;
  const layout = () => {
    // The canvas's own size, which leaves out the phone's address bar sliding in and out
    const w = canvas.clientWidth, h = canvas.clientHeight;
    if (w === viewW && h === viewH) return;
    renderer.setPixelRatio(Math.min(devicePixelRatio, 2));
    renderer.setSize(w, h, false);
    camera.aspect = w / h;
    camera.updateProjectionMatrix();
    narrow = w < 860;
    halfH = Math.tan(THREE.MathUtils.degToRad(camera.fov / 2));
    halfW = halfH * camera.aspect;
    trails.uniforms.uPx.value = 2 * halfH / (h);
    trails.uniforms.uDpr.value = renderer.getPixelRatio();
    // Hero: the lens sits right of the title on wide screens, above it on narrow ones
    const heroDist = 30;
    // Hero: the camera looks straight down the axis the sky turns around and the picture is shifted instead (like a
    // shift lens), so every star circles the pupil wherever the lens sits on screen. Shift is a fraction of the screen.
    lens.scale.setScalar(narrow ? Math.min(1, halfW * heroDist * 0.62 / LR) : 1);
    stations[0] = { z: heroDist, x: 0, y: 0, yaw: 0, shiftX: narrow ? 0 : 0.26, shiftY: narrow ? -0.21 : 0 };
    viewW = w; viewH = h;
    const D = PLANET_AHEAD;
    const place = [
      { side: 1, k: 0.96 }, { side: -1, k: 0.92 }, { side: 1, k: 0.9 }, { side: -1, k: 0.9 }, { side: 1, k: 1.02 }, { side: -1, k: 0.9 }, { home: true },
    ];
    planets.forEach((p, i) => {
      const z = -STOP_GAP * (i + 1) + 10;
      stations[i + 1] = { z, x: 0, y: 0, yaw: 0, shiftX: 0, shiftY: 0 };
      p.station = z;
      const cfg = place[i];
      if (cfg.home) {
        p.group.position.set(0, -(p.R + halfH * D * 0.55), z - D);
        p.group.scale.setScalar(1);
      } else if (narrow) {
        p.group.position.set(0, halfH * D * 0.5, z - D);
        p.group.scale.setScalar(Math.min(1, halfW * D * 0.95 / (p.R * 2.2)));
      } else {
        p.group.position.set(cfg.side * halfW * D * cfg.k, 0, z - D);
        p.group.scale.setScalar(1);
        stations[i + 1].yaw = -cfg.side * 0.05;
      }
    });
  };
  layout();
  addEventListener("resize", layout);

  // Hermite step from 0 to 1 that leaves with slope m0 and arrives with slope m1
  const hermite = (f, m0, m1) => { const f2 = f * f, f3 = f2 * f; return 3 * f2 - 2 * f3 + m0 * (f3 - 2 * f2 + f) + m1 * (f3 - f2); };
  // The first leg eases into the pupil at this point of the scroll and glides through it at a steady pace
  const PUPIL_AT = 0.45, PUPIL_PACE = 0.55;
  // Camera position for a point on the trip, in straight legs at the pace of the scroll (the spring below rounds the corners)
  const pose = (p, out) => {
    const i = Math.min(stations.length - 2, Math.max(0, Math.floor(p) || 0)), u = clamp01(p - i), f = u;
    const a = stations[i], b = stations[i + 1];
    if (i === 0) {
      // The lens sits at z = 0; the pace through it is a share of the average speed over the whole leg
      const m = PUPIL_PACE * (b.z - a.z);
      out.z = u < PUPIL_AT
        ? a.z - a.z * hermite(u / PUPIL_AT, 0, m * PUPIL_AT / -a.z)
        : b.z * hermite((u - PUPIL_AT) / (1 - PUPIL_AT), m * (1 - PUPIL_AT) / b.z, 0);
    } else out.z = a.z + (b.z - a.z) * f;
    out.x = a.x + (b.x - a.x) * f; out.y = a.y + (b.y - a.y) * f;
    out.yaw = a.yaw + (b.yaw - a.yaw) * f;
    // The hero shift eases out with the scroll and settles with no jolt a little before the pupil, so it is centred there
    out.shift = i === 0 ? 1 - smoother(clamp01(u / (PUPIL_AT * 0.8))) : 0;
    return out;
  };

  const cur = pose(journey.p, {}), want = {}, curV = {};
  const AXES = ["x", "y", "z", "yaw", "shift"];
  for (const key of AXES) curV[key] = 0;
  let lastZ = cur.z, vel = 0, spin = 0, focus = null;
  const tick = now => {
    timer.update(now);
    const dt = Math.min(timer.getDelta(), 0.05), t = timer.getElapsed();
    pose(journey.p, want);
    // A critically damped spring per axis, so the camera picks up and sheds speed gradually instead of lurching
    const W = 7.5;
    for (const key of AXES) {
      curV[key] += (W * W * (want[key] - cur[key]) - 2 * W * curV[key]) * dt;
      cur[key] += curV[key] * dt;
    }
    camera.position.set(cur.x, cur.y, cur.z);
    camera.rotation.set(0, cur.yaw, 0);
    camera.setViewOffset(viewW, viewH, -stations[0].shiftX * cur.shift * viewW, -stations[0].shiftY * cur.shift * viewH, viewW, viewH);

    const v = (lastZ - cur.z) / Math.max(dt, 0.001);
    lastZ = cur.z;
    vel += (v - vel) * (1 - Math.exp(-dt * 6));
    const speed = Math.abs(vel);
    // Past the pupil the sky stops turning and comes into focus; speed stretches the stars into long streaks.
    // Focus also eases over time, so even a fast scroll through the pupil turns the trails over gently.
    const aim = smooth(clamp01((10 - cur.z) / 34));
    focus = focus === null ? aim : focus + (aim - focus) * (1 - Math.exp(-dt * 2.6));
    spin += dt * (Math.PI * 2 / 480) * (1 + speed * 0.01) * (1 - focus);
    trails.uniforms.uSpin.value = spin;
    trails.uniforms.uFocus.value = focus;
    trails.uniforms.uLen.value = 0.42 * (1 - focus);
    // Rises steeply so a gentle scroll already pulls clear streaks, and levels off for a fast one
    trails.uniforms.uStretch.value = Math.sign(vel) * 55 * (1 - Math.exp(-speed / 26));
    trails.uniforms.uBoost.value = 1 + 0.7 * (1 - Math.exp(-speed / 60));

    // The lens closes up as you reach it: the pupil and the disc fade so space shows through
    const near = cur.z;
    const pupilFade = smooth(clamp01((near - 3) / 22));
    pupil.material.uniforms.uFade.value = pupilFade;
    disc.material.uniforms.uFade.value = smooth(clamp01((near - 0.5) / 6));
    glow.material.uniforms.uFade.value = smooth(clamp01(near / 26));
    lens.visible = near > -2;

    // Planets come out of the dark as you approach, instead of all being visible from the start
    for (const p of planets) {
      // Fades in on the approach and out once you've passed, so you never fly through a planet or its rings
      const fade = clamp01(1 - (cur.z - p.station - 18) / 46) * clamp01(1 - (p.station - cur.z - 8) / 22);
      p.group.visible = fade > 0;
      if (fade !== p.fade) {
        p.fade = fade;
        p.group.traverse(o => {
          const m = o.material;
          if (!m) return;
          if (m.userData.own) return;
          if (m.uniforms?.uFade) m.uniforms.uFade.value = fade;
          else { m.userData.base ??= m.opacity; m.opacity = m.userData.base * fade; }
        });
      }
      p.tick(t, { dt, fade, here: clamp01(1 - Math.abs(cur.z - p.station) / 14) });
    }
    for (const n of nebulae) n.material.uniforms.uTime.value = t;
    renderer.render(scene, camera);
  };
  renderer.setAnimationLoop(tick);
  document.addEventListener("visibilitychange", () => renderer.setAnimationLoop(document.hidden ? null : tick));
}
