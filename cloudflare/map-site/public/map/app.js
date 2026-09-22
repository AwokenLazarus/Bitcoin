// XBT Galaxy: every pool on the BLAKE2b Bitcoin network is a solar system, every DATUM gateway a
// planet. Data: /map/data.json (built from the chain by the Worker every five minutes).
//
// All text that comes from the chain (pool and gateway tags) is set with textContent, never as
// HTML: miners choose those strings.
import * as THREE from "three";
import { OrbitControls } from "three/addons/controls/OrbitControls.js";
import { EffectComposer } from "three/addons/postprocessing/EffectComposer.js";
import { RenderPass } from "three/addons/postprocessing/RenderPass.js";
import { UnrealBloomPass } from "three/addons/postprocessing/UnrealBloomPass.js";
import { OutputPass } from "three/addons/postprocessing/OutputPass.js";
import { CSS2DRenderer, CSS2DObject } from "three/addons/renderers/CSS2DRenderer.js";

const DATA_URL = "/map/data.json";
const POLL_MS = 60_000;
const $ = (id) => document.getElementById(id);
const store = {
  get: (k) => { try { return localStorage.getItem(k); } catch { return null; } },
  set: (k, v) => { try { localStorage.setItem(k, v); } catch { /* private mode */ } },
};
const reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;
const narrow = matchMedia("(max-width: 820px)").matches;
const lowPower = narrow || (navigator.hardwareConcurrency || 8) <= 4;
const MOTION = reduced ? 0.25 : 1;
const FAST = new URLSearchParams(location.search).has("fast"); // test hook for software renderers

// ---------- small helpers ----------
function hash(str) { let h = 2166136261 >>> 0; for (let i = 0; i < str.length; i++) { h ^= str.charCodeAt(i); h = Math.imul(h, 16777619) >>> 0; } return h; }
function rng(seed) { let s = seed >>> 0 || 1; return () => { s ^= s << 13; s >>>= 0; s ^= s >>> 17; s ^= s << 5; s >>>= 0; return s / 4294967296; }; }
const clamp = (x, a, b) => Math.max(a, Math.min(b, x));
const ease = (t) => (t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2);
const n = (x) => (x == null ? "–" : Number(x).toLocaleString("en-US"));
function fmtHash(hs) {
  if (!hs) return "–";
  const u = [["EH/s", 1e18], ["PH/s", 1e15], ["TH/s", 1e12], ["GH/s", 1e9]];
  for (const [k, v] of u) if (hs >= v) return (hs / v).toFixed(hs / v >= 100 ? 0 : hs / v >= 10 ? 1 : 2) + " " + k;
  return Math.round(hs / 1e6) + " MH/s";
}
function ago(ts) {
  if (!ts) return "–";
  const s = Math.max(0, Date.now() / 1000 - ts);
  if (s < 90) return Math.round(s) + " s ago";
  if (s < 5400) return Math.round(s / 60) + " min ago";
  if (s < 172800) return (s / 3600).toFixed(1) + " h ago";
  return Math.round(s / 86400) + " days ago";
}
function dur(s) { if (s == null) return "–"; if (s < 3600) return Math.round(s / 60) + " min"; if (s < 172800) return (s / 3600).toFixed(1) + " h"; return Math.round(s / 86400) + " days"; }
const xbt = (sats) => (sats == null ? "–" : (sats / 1e8).toFixed(4) + " XBT");
const el = (tag, cls, text) => { const e = document.createElement(tag); if (cls) e.className = cls; if (text != null) e.textContent = text; return e; };
function kv(dl, k, v, cls) { if (v == null || v === "") return; dl.append(el("dt", null, k)); dl.append(el("dd", cls || null, String(v))); }
function sec(dl, title) { dl.append(el("div", "sec", title)); }
const TYPE_LABEL = {
  datum: ["Rebel Alliance", "DATUM pool: each gateway builds its own block template"],
  stratum: ["Galactic Empire", "Stratum only: the pool builds every template, its miners mine blind"],
  independent: ["Outer Rim", "Independent: one operator, own node, whole blocks for themselves"],
};
const safeLink = (u) => (typeof u === "string" && /^https:\/\/[a-z0-9.-]+(\/[^\s"<>]*)?$/i.test(u) ? u : null);

// ---------- palettes ----------
const REBEL = [0xffcf5c, 0x6fd3ff, 0xffe9a8, 0x9ff0ff, 0xffb35c];
function planetColor(tag) {
  const r = rng(hash(tag));
  const hues = [0.55, 0.6, 0.08, 0.12, 0.33, 0.47, 0.75, 0.9, 0.02];
  const c = new THREE.Color().setHSL(hues[Math.floor(r() * hues.length)] + (r() - 0.5) * 0.04, 0.45 + r() * 0.4, 0.45 + r() * 0.2);
  return c;
}

// ---------- renderer ----------
let renderer;
try {
  renderer = new THREE.WebGLRenderer({ antialias: !lowPower, powerPreference: "high-performance" });
} catch (e) {
  fail("This map needs WebGL, which this browser or device does not provide. The data is at /map/data.json.");
  throw e;
}
renderer.setPixelRatio(Math.min(devicePixelRatio || 1, lowPower ? 1.25 : 1.75));
renderer.setSize(innerWidth, innerHeight);
renderer.toneMapping = THREE.ACESFilmicToneMapping;
renderer.toneMappingExposure = 0.92;
$("stage").append(renderer.domElement);
const labels = new CSS2DRenderer();
labels.setSize(innerWidth, innerHeight);
Object.assign(labels.domElement.style, { position: "fixed", inset: "0", pointerEvents: "none" });
$("stage").append(labels.domElement);

const scene = new THREE.Scene();
scene.background = new THREE.Color(0x03040a);
scene.fog = new THREE.FogExp2(0x03040a, 0.00006);
const camera = new THREE.PerspectiveCamera(55, innerWidth / innerHeight, 0.5, 30000);
camera.position.set(0, 1500, 2600);
const controls = new OrbitControls(camera, renderer.domElement);
Object.assign(controls, { enableDamping: true, dampingFactor: 0.06, minDistance: 12, maxDistance: 5200, zoomSpeed: 0.9, rotateSpeed: 0.6, panSpeed: 0.8, screenSpacePanning: false });
controls.maxPolarAngle = Math.PI * 0.94;
scene.add(new THREE.AmbientLight(0x8090b0, 0.55));
const sun = new THREE.DirectionalLight(0xffffff, 1.4);
sun.position.set(400, 600, 300);
scene.add(sun);

let composer = null;
if (!lowPower) {
  composer = new EffectComposer(renderer);
  composer.addPass(new RenderPass(scene, camera));
  composer.addPass(new UnrealBloomPass(new THREE.Vector2(innerWidth, innerHeight), 0.62, 0.5, 0.32));
  composer.addPass(new OutputPass());
}

// ---------- textures ----------
function glowTexture(stops) {
  const c = document.createElement("canvas"); c.width = c.height = 128;
  const g = c.getContext("2d"), grd = g.createRadialGradient(64, 64, 0, 64, 64, 64);
  for (const [o, col] of stops) grd.addColorStop(o, col);
  g.fillStyle = grd; g.fillRect(0, 0, 128, 128);
  const t = new THREE.CanvasTexture(c); t.colorSpace = THREE.SRGBColorSpace; return t;
}
const TEX = {
  glow: glowTexture([[0, "rgba(255,255,255,1)"], [0.18, "rgba(255,255,255,.55)"], [0.45, "rgba(255,255,255,.12)"], [1, "rgba(255,255,255,0)"]]),
  soft: glowTexture([[0, "rgba(255,255,255,.9)"], [0.5, "rgba(255,255,255,.18)"], [1, "rgba(255,255,255,0)"]]),
  nebula: glowTexture([[0, "rgba(255,255,255,.55)"], [0.35, "rgba(255,255,255,.2)"], [0.7, "rgba(255,255,255,.05)"], [1, "rgba(255,255,255,0)"]]),
  ring: glowTexture([[0, "rgba(255,255,255,.95)"], [0.12, "rgba(255,255,255,.5)"], [0.22, "rgba(255,255,255,0)"], [0.55, "rgba(255,255,255,0)"], [0.66, "rgba(255,255,255,.75)"], [0.76, "rgba(255,255,255,0)"], [1, "rgba(255,255,255,0)"]]),
};
function sprite(tex, color, scale, opacity = 1) {
  const m = new THREE.SpriteMaterial({ map: tex, color, transparent: true, opacity, blending: THREE.AdditiveBlending, depthWrite: false });
  const s = new THREE.Sprite(m); s.scale.setScalar(scale); return s;
}
function deathStarTexture(seed) {
  const W = 1024, H = 512, c = document.createElement("canvas"); c.width = W; c.height = H;
  const g = c.getContext("2d"), r = rng(seed);
  g.fillStyle = "#5a5e66"; g.fillRect(0, 0, W, H);
  for (let i = 0; i < 2600; i++) { const v = 70 + Math.floor(r() * 70); g.fillStyle = `rgb(${v},${v + 2},${v + 8})`; g.fillRect(r() * W, r() * H, 6 + r() * 38, 3 + r() * 16); }
  for (let i = 0; i < 900; i++) { g.fillStyle = r() < 0.12 ? "rgba(255,90,70,.9)" : "rgba(20,22,28,.8)"; g.fillRect(r() * W, r() * H, 2, 2); }
  g.fillStyle = "#1b1d22"; g.fillRect(0, H / 2 - 5, W, 10);                                   // equatorial trench
  g.fillStyle = "rgba(255,70,50,.6)"; for (let x = 0; x < W; x += 9) if (r() < 0.5) g.fillRect(x, H / 2 - 1, 4, 2);
  const cx = W * 0.3, cy = H * 0.3, rad = 62;                                                  // superlaser dish
  const grd = g.createRadialGradient(cx - 10, cy - 10, 5, cx, cy, rad);
  grd.addColorStop(0, "#9aa0a8"); grd.addColorStop(0.35, "#4a4e56"); grd.addColorStop(0.8, "#2c2f35"); grd.addColorStop(1, "#6a6e76");
  g.fillStyle = grd; g.beginPath(); g.ellipse(cx, cy, rad * 1.5, rad, 0, 0, Math.PI * 2); g.fill();
  g.strokeStyle = "#202226"; g.lineWidth = 3; g.stroke();
  g.fillStyle = "#7dff9a"; g.beginPath(); g.arc(cx, cy, 6, 0, Math.PI * 2); g.fill();
  const t = new THREE.CanvasTexture(c); t.colorSpace = THREE.SRGBColorSpace; t.anisotropy = 4; return t;
}

// ---------- shaders ----------
const pointsVS = `
  attribute float size; attribute vec3 color; attribute float phase;
  uniform float uTime; uniform float uScale; varying vec3 vColor; varying float vTw;
  void main() {
    vColor = color;
    vTw = 0.65 + 0.35 * sin(uTime * (0.6 + fract(phase * 13.1) * 1.8) + phase * 6.2831);
    vec4 mv = modelViewMatrix * vec4(position, 1.0);
    gl_PointSize = min(size * uScale * (300.0 / -mv.z), 9.0 * uScale);
    gl_Position = projectionMatrix * mv;
  }`;
const pointsFS = `
  varying vec3 vColor; varying float vTw;
  void main() {
    vec2 d = gl_PointCoord - 0.5; float r = length(d);
    if (r > 0.5) discard;
    float a = smoothstep(0.5, 0.0, r); a *= a;
    gl_FragColor = vec4(vColor * vTw, a * vTw);
  }`;
function pointsMaterial(scale = 1) {
  return new THREE.ShaderMaterial({ uniforms: { uTime: { value: 0 }, uScale: { value: scale * renderer.getPixelRatio() } }, vertexShader: pointsVS, fragmentShader: pointsFS, transparent: true, depthWrite: false, blending: THREE.AdditiveBlending });
}
const planetVS = `
  varying vec3 vN; varying vec3 vW; varying vec3 vO;
  void main() { vN = normalize(mat3(modelMatrix) * normal); vO = normal; vec4 w = modelMatrix * vec4(position, 1.0); vW = w.xyz; gl_Position = projectionMatrix * viewMatrix * w; }`;
const planetFS = `
  uniform vec3 uColor; uniform vec3 uStar; uniform float uTime; uniform float uLive; uniform float uHover; uniform float uSeed; uniform float uAlpha;
  varying vec3 vN; varying vec3 vW; varying vec3 vO;
  void main() {
    vec3 n = normalize(vN); vec3 L = normalize(uStar - vW); vec3 V = normalize(cameraPosition - vW);
    float diff = max(dot(n, L), 0.0) * 0.95 + 0.06;
    float bands = 0.82 + 0.18 * sin(vO.y * (6.0 + uSeed * 10.0) + sin(vO.x * 3.0 + uSeed * 20.0) * 1.4 + uTime * 0.05);
    vec3 col = uColor * diff * bands;
    float rim = pow(1.0 - max(dot(n, V), 0.0), 3.0);
    float pulse = uLive * (0.55 + 0.45 * sin(uTime * 2.4 + uSeed * 30.0));
    col += rim * mix(uColor, vec3(0.55, 0.95, 1.0), 0.55) * (0.5 + pulse * 1.3 + uHover * 2.2);
    gl_FragColor = vec4(col, uAlpha);
  }`;
const planetUniformsShared = { uTime: { value: 0 } };
function planetMaterial(color, star, seed, alpha = 1) {
  return new THREE.ShaderMaterial({
    uniforms: { uColor: { value: color }, uStar: { value: star }, uTime: planetUniformsShared.uTime, uLive: { value: 0 }, uHover: { value: 0 }, uSeed: { value: seed }, uAlpha: { value: alpha } },
    vertexShader: planetVS, fragmentShader: planetFS, transparent: alpha < 1, depthWrite: alpha >= 1,
  });
}

// ---------- the galaxy backdrop ----------
const timeMats = [];
const galaxy = new THREE.Group();
scene.add(galaxy);
const ARMS = 4, ARM_TWIST = 0.0052, DISK_R = 1500;
function armAngle(arm, r) { return (arm * Math.PI * 2) / ARMS + r * ARM_TWIST; }
(function backdrop() {
  // distant stars
  const N = lowPower ? 4000 : 9000, pos = new Float32Array(N * 3), col = new Float32Array(N * 3), size = new Float32Array(N), ph = new Float32Array(N);
  const r = rng(7);
  for (let i = 0; i < N; i++) {
    const u = r() * 2 - 1, t = r() * Math.PI * 2, R = 7000 + r() * 6000, s = Math.sqrt(1 - u * u);
    pos.set([R * s * Math.cos(t), R * u, R * s * Math.sin(t)], i * 3);
    const c = new THREE.Color().setHSL(0.55 + r() * 0.12 - (r() < 0.2 ? 0.5 : 0), 0.3 * r(), 0.75 + r() * 0.25);
    col.set([c.r, c.g, c.b], i * 3); size[i] = 10 + r() * 26; ph[i] = r();
  }
  const g = new THREE.BufferGeometry();
  g.setAttribute("position", new THREE.BufferAttribute(pos, 3)); g.setAttribute("color", new THREE.BufferAttribute(col, 3));
  g.setAttribute("size", new THREE.BufferAttribute(size, 1)); g.setAttribute("phase", new THREE.BufferAttribute(ph, 1));
  const m = pointsMaterial(1); timeMats.push(m);
  scene.add(new THREE.Points(g, m));

  // the disk: four spiral arms of stars
  const D = lowPower ? 26000 : 70000, dp = new Float32Array(D * 3), dc = new Float32Array(D * 3), ds = new Float32Array(D), dph = new Float32Array(D);
  const core = new THREE.Color(0xffd9a0), mid = new THREE.Color(0xb9c7ff), edge = new THREE.Color(0x6f7fe0), pink = new THREE.Color(0xff8fd0);
  for (let i = 0; i < D; i++) {
    const arm = i % ARMS, rr = Math.pow(r(), 0.72) * DISK_R + 30;
    const spread = (r() - 0.5) * (0.55 - (rr / DISK_R) * 0.25) + (r() - 0.5) * 0.25 * (r() < 0.15 ? 3 : 1);
    const a = armAngle(arm, rr) + spread;
    const thick = (40 * (1 - rr / DISK_R) + 10) * (r() + r() - 1);
    dp.set([Math.cos(a) * rr, thick, Math.sin(a) * rr], i * 3);
    const t = rr / DISK_R, c = core.clone().lerp(mid, clamp(t * 1.6, 0, 1)).lerp(edge, clamp((t - 0.55) * 2, 0, 1));
    if (r() < 0.05) c.lerp(pink, 0.6);
    dc.set([c.r, c.g, c.b], i * 3); ds[i] = (t < 0.15 ? 9 : 5) + r() * 9; dph[i] = r();
  }
  const dg = new THREE.BufferGeometry();
  dg.setAttribute("position", new THREE.BufferAttribute(dp, 3)); dg.setAttribute("color", new THREE.BufferAttribute(dc, 3));
  dg.setAttribute("size", new THREE.BufferAttribute(ds, 1)); dg.setAttribute("phase", new THREE.BufferAttribute(dph, 1));
  const dm = pointsMaterial(0.9); timeMats.push(dm);
  galaxy.add(new THREE.Points(dg, dm));

  // core and nebulae
  galaxy.add(sprite(TEX.glow, 0xffc890, 820, 0.32));
  galaxy.add(sprite(TEX.glow, 0xfff1d8, 220, 0.6));
  const hues = [0x3a6bff, 0x7a4dff, 0x2ec4ff, 0xff4da6, 0x5affd8];
  for (let i = 0; i < (lowPower ? 26 : 60); i++) {
    const arm = i % ARMS, rr = 250 + r() * (DISK_R - 250), a = armAngle(arm, rr) + (r() - 0.5) * 0.4;
    const s = sprite(TEX.nebula, hues[Math.floor(r() * hues.length)], 260 + r() * 520, 0.05 + r() * 0.08);
    s.position.set(Math.cos(a) * rr, (r() - 0.5) * 40, Math.sin(a) * rr);
    s.material.rotation = r() * Math.PI; s.userData.spin = (r() - 0.5) * 0.02;
    galaxy.add(s);
  }
})();

// ---------- building systems ----------
let DATA = null;
const systemsById = new Map();     // id -> runtime record
const pickables = [];              // meshes the pointer can hit
const labelled = [];               // CSS2D labels of systems
const animated = [];               // per-frame callbacks (dt, t)
const effects = [];                // short-lived effects
const typeGroups = { datum: new THREE.Group(), stratum: new THREE.Group(), independent: new THREE.Group() };
Object.values(typeGroups).forEach((g) => scene.add(g));

function systemScale(blocks) { return 3.5 + 3.2 * Math.log10(blocks + 1); }

function placeSystems(systems) {
  const out = new Map();
  const empire = systems.filter((s) => s.type === "stratum");
  const rebel = systems.filter((s) => s.type === "datum");
  const rim = systems.filter((s) => s.type === "independent");
  empire.forEach((s, i) => {
    const a = i * 2.39996 + 0.6, r = 150 + i * 34;
    out.set(s.id, new THREE.Vector3(Math.cos(a) * r, (rng(hash(s.id))() - 0.5) * 16, Math.sin(a) * r));
  });
  rebel.forEach((s, i) => {
    const arm = [0, 2, 1, 3][i % 4], k = Math.floor(i / 4), r = 430 + k * 170 + (i % 2) * 60;
    const a = armAngle(arm, r) + (rng(hash(s.id))() - 0.5) * 0.12;
    out.set(s.id, new THREE.Vector3(Math.cos(a) * r, (rng(hash(s.id) + 1)() - 0.5) * 24, Math.sin(a) * r));
  });
  rim.forEach((s) => {
    const rr = rng(hash(s.id)), a = rr() * Math.PI * 2, r = 1250 + Math.pow(rr(), 0.8) * 1100, y = (rr() + rr() + rr() - 1.5) * 140;
    out.set(s.id, new THREE.Vector3(Math.cos(a) * r, y, Math.sin(a) * r));
  });
  return out;
}

function makeLabel(text, cls) { const d = el("div", "label2d " + (cls || ""), text); return new CSS2DObject(d); }
// A marker that keeps its size on screen, so systems stay findable from across the galaxy.
const beacons = [];
function beacon(root, color, blocks, sys) {
  const m = new THREE.SpriteMaterial({ map: TEX.ring, color, sizeAttenuation: false, transparent: true, opacity: 0.9, blending: THREE.AdditiveBlending, depthWrite: false });
  const b = new THREE.Sprite(m); b.scale.setScalar(0.028 + 0.011 * Math.log10(blocks + 1)); b.userData = { kind: "system", sys };
  root.add(b); beacons.push(b); pickables.push(b); return b;
}

function buildRebel(s, pos, i) {
  const root = new THREE.Group(); root.position.copy(pos); typeGroups.datum.add(root);
  const rr = rng(hash(s.id)), starR = systemScale(s.blocks) * 0.8;
  const color = new THREE.Color(s.name === "Lazarus" ? 0xffcf5c : REBEL[i % REBEL.length]);
  const star = new THREE.Mesh(new THREE.SphereGeometry(starR, 32, 16), new THREE.MeshBasicMaterial({ color: color.clone().multiplyScalar(1.6) }));
  root.add(star);
  const halo = sprite(TEX.glow, color, starR * 9, 0.85), corona = sprite(TEX.soft, color, starR * 16, 0.25);
  root.add(halo, corona);
  animated.push((dt, t) => { corona.scale.setScalar(starR * (15 + Math.sin(t * 0.8 + i) * 1.6)); });
  const rec = { data: s, root, pos, star, planets: [], starR, extent: starR * 3, type: "datum" };
  const starWorld = pos.clone();
  const planets = s.planets.slice().sort((a, b) => (b.blocks - a.blocks) || ((b.live ? 1 : 0) - (a.live ? 1 : 0)));
  planets.forEach((p, k) => {
    const orbit = starR * 2.4 + 3 + 3.1 * Math.sqrt(k + 1) * (1 + (k % 3) * 0.08);
    const pr = p.blocks ? 0.7 + 1.35 * Math.log10(p.blocks + 1) : 0.45;
    const pivot = new THREE.Object3D();
    pivot.rotation.set((rr() - 0.5) * 0.35, rr() * Math.PI * 2, (rr() - 0.5) * 0.2);
    root.add(pivot);
    const c = p.house ? color.clone().lerp(new THREE.Color(0xffffff), 0.3) : planetColor(p.tag);
    const mat = planetMaterial(c, starWorld, (hash(p.tag) % 1000) / 1000, p.blocks ? 1 : 0.55);
    const mesh = new THREE.Mesh(new THREE.SphereGeometry(pr, 20, 14), mat);
    mesh.position.x = orbit; pivot.add(mesh);
    if (p.house) {
      const ring = new THREE.Mesh(new THREE.RingGeometry(pr * 1.5, pr * 2.3, 48), new THREE.MeshBasicMaterial({ color: c, side: THREE.DoubleSide, transparent: true, opacity: 0.55, blending: THREE.AdditiveBlending, depthWrite: false }));
      ring.rotation.x = Math.PI / 2.3; mesh.add(ring);
    }
    if (p.live && p.live.online) {
      mat.uniforms.uLive.value = 1;
      const beacon = sprite(TEX.soft, 0x7dffea, pr * 4.5, 0.5); mesh.add(beacon);
      animated.push((dt, t) => { beacon.material.opacity = 0.25 + 0.35 * (0.5 + 0.5 * Math.sin(t * 2.2 + k)); });
    }
    if (k < 36) {
      const pts = []; for (let a = 0; a <= 96; a++) pts.push(new THREE.Vector3(Math.cos((a / 96) * Math.PI * 2) * orbit, 0, Math.sin((a / 96) * Math.PI * 2) * orbit));
      const line = new THREE.Line(new THREE.BufferGeometry().setFromPoints(pts), new THREE.LineBasicMaterial({ color: c, transparent: true, opacity: p.house ? 0.22 : 0.09, depthWrite: false }));
      line.rotation.copy(pivot.rotation); line.rotation.y = 0; root.add(line);
      line.quaternion.copy(pivot.quaternion);
    }
    const speed = (0.9 / Math.sqrt(orbit)) * (0.6 + rr() * 0.5) * MOTION;
    animated.push((dt) => { pivot.rotateY(speed * dt); });
    mesh.userData = { kind: "planet", sys: s.id, tag: p.tag };
    pickables.push(mesh);
    rec.planets.push({ tag: p.tag, mesh, radius: pr });
    rec.extent = Math.max(rec.extent, orbit + pr);
  });
  const hit = new THREE.Mesh(new THREE.SphereGeometry(Math.max(starR * 2.2, 10), 12, 8), new THREE.MeshBasicMaterial({ visible: false }));
  hit.userData = { kind: "system", sys: s.id }; root.add(hit); pickables.push(hit);
  const label = makeLabel(s.name); label.position.set(0, starR * 2 + 4, 0); root.add(label); labelled.push(label);
  beacon(root, color, s.blocks, s.id);
  return rec;
}

function starDestroyer() {
  const shape = new THREE.Shape(); shape.moveTo(0, 3.2); shape.lineTo(1.3, -1.2); shape.lineTo(-1.3, -1.2); shape.closePath();
  const g = new THREE.ExtrudeGeometry(shape, { depth: 0.32, bevelEnabled: false }); g.rotateX(-Math.PI / 2); g.center();
  const hull = new THREE.Mesh(g, new THREE.MeshLambertMaterial({ color: 0xa9adb5, emissive: 0x111318 }));
  const tower = new THREE.Mesh(new THREE.BoxGeometry(0.5, 0.45, 0.45), new THREE.MeshLambertMaterial({ color: 0x9ea2aa }));
  tower.position.set(0, 0.35, -0.8); hull.add(tower);
  const engine = sprite(TEX.soft, 0x7fb6ff, 1.6, 0.9); engine.position.set(0, 0, -1.35); hull.add(engine);
  return hull;
}

function buildEmpire(s, pos, i) {
  const root = new THREE.Group(); root.position.copy(pos); typeGroups.stratum.add(root);
  const R = systemScale(s.blocks) * 0.9, rr = rng(hash(s.id));
  const ds = new THREE.Mesh(new THREE.SphereGeometry(R, 48, 32), new THREE.MeshLambertMaterial({ map: deathStarTexture(hash(s.id)), emissive: 0x2a0000 }));
  ds.rotation.z = 0.25; root.add(ds);
  const glow = sprite(TEX.glow, 0xff2020, R * 8, 0.55), corona = sprite(TEX.soft, 0xff1a1a, R * 14, 0.2);
  root.add(glow, corona);
  const neb = sprite(TEX.nebula, 0xb00010, R * 30, 0.1); neb.position.y = -2; root.add(neb);
  // a scanning ring, the Empire's perimeter
  const scan = new THREE.Mesh(new THREE.RingGeometry(R * 1.7, R * 1.78, 96), new THREE.MeshBasicMaterial({ color: 0xff3030, transparent: true, opacity: 0.5, side: THREE.DoubleSide, blending: THREE.AdditiveBlending, depthWrite: false }));
  scan.rotation.x = Math.PI / 2; root.add(scan);
  animated.push((dt, t) => {
    ds.rotation.y += 0.05 * dt * MOTION;
    const p = (t * 0.18 + i * 0.3) % 1; scan.scale.setScalar(1 + p * 2.4); scan.material.opacity = 0.5 * (1 - p);
    corona.scale.setScalar(R * (13 + Math.sin(t * 0.7 + i) * 1.2));
  });
  // Star Destroyers on patrol
  const fleet = clamp(Math.round(1 + Math.log10(s.blocks + 1) * 1.6), 1, 6);
  for (let k = 0; k < fleet; k++) {
    const pivot = new THREE.Object3D(); pivot.rotation.set((rr() - 0.5) * 0.9, rr() * Math.PI * 2, (rr() - 0.5) * 0.6); root.add(pivot);
    const sd = starDestroyer(); const sc = 0.9 + R * 0.06; sd.scale.setScalar(sc);
    const orbit = R * (2.4 + k * 0.55); sd.position.set(orbit, 0, 0); sd.rotation.y = Math.PI; pivot.add(sd);
    const sp = (0.35 / Math.sqrt(orbit)) * (rr() < 0.5 ? 1 : -1) * MOTION;
    if (sp < 0) sd.rotation.y = 0;
    animated.push((dt) => pivot.rotateY(sp * dt));
  }
  const rec = { data: s, root, pos, star: ds, planets: [], starR: R, extent: R * 4.5, type: "stratum", ds };
  // the rare gateway tags seen in an Imperial system: captured worlds
  s.planets.filter((p) => !p.house).slice(0, 12).forEach((p, k) => {
    const pivot = new THREE.Object3D(); pivot.rotation.set((rr() - 0.5) * 0.3, rr() * Math.PI * 2, 0); root.add(pivot);
    const pr = 0.6 + Math.log10(p.blocks + 1);
    const mesh = new THREE.Mesh(new THREE.SphereGeometry(pr, 16, 12), planetMaterial(new THREE.Color(0x8a2a2a), pos.clone(), (hash(p.tag) % 1000) / 1000));
    const orbit = R * 3.4 + k * 2.2; mesh.position.x = orbit; pivot.add(mesh);
    const sp = (0.7 / Math.sqrt(orbit)) * MOTION; animated.push((dt) => pivot.rotateY(sp * dt));
    mesh.userData = { kind: "planet", sys: s.id, tag: p.tag }; pickables.push(mesh); rec.planets.push({ tag: p.tag, mesh, radius: pr });
    rec.extent = Math.max(rec.extent, orbit + pr);
  });
  const hit = new THREE.Mesh(new THREE.SphereGeometry(R * 1.6, 12, 8), new THREE.MeshBasicMaterial({ visible: false }));
  hit.userData = { kind: "system", sys: s.id }; root.add(hit); pickables.push(hit);
  const label = makeLabel(s.name, "empire"); label.position.set(0, R * 2 + 5, 0); root.add(label); labelled.push(label);
  beacon(root, 0xff2a2a, s.blocks, s.id);
  return rec;
}

let rimPoints = null, rimIndex = [];
function buildRim(systems, positions) {
  const N = systems.length, pos = new Float32Array(N * 3), col = new Float32Array(N * 3), size = new Float32Array(N), ph = new Float32Array(N);
  systems.forEach((s, i) => {
    const p = positions.get(s.id); pos.set([p.x, p.y, p.z], i * 3);
    const c = planetColor(s.name).lerp(new THREE.Color(0x7fe0b0), 0.35); col.set([c.r, c.g, c.b], i * 3);
    size[i] = 14 + 16 * Math.log10(s.blocks + 1); ph[i] = (hash(s.id) % 1000) / 1000;
    const rec = { data: s, root: null, pos: p, star: null, planets: [], starR: 2, extent: 12, type: "independent", rimIndex: i };
    systemsById.set(s.id, rec);
  });
  const g = new THREE.BufferGeometry();
  g.setAttribute("position", new THREE.BufferAttribute(pos, 3)); g.setAttribute("color", new THREE.BufferAttribute(col, 3));
  g.setAttribute("size", new THREE.BufferAttribute(size, 1)); g.setAttribute("phase", new THREE.BufferAttribute(ph, 1));
  const m = pointsMaterial(1.3); timeMats.push(m);
  rimPoints = new THREE.Points(g, m); rimPoints.userData = { kind: "rim" };
  typeGroups.independent.add(rimPoints);
  rimIndex = systems.map((s) => s.id);
  // the biggest independents get a small lit world so they read as planets up close
  systems.slice(0, 60).forEach((s) => {
    const p = positions.get(s.id), pr = 0.8 + 1.1 * Math.log10(s.blocks + 1);
    const mesh = new THREE.Mesh(new THREE.SphereGeometry(pr, 16, 12), planetMaterial(planetColor(s.name), new THREE.Vector3(0, 0, 0), (hash(s.id) % 1000) / 1000));
    mesh.position.copy(p); mesh.userData = { kind: "system", sys: s.id }; typeGroups.independent.add(mesh); pickables.push(mesh);
    systemsById.get(s.id).planets.push({ tag: s.name, mesh, radius: pr });
  });
}

function empireHaze(positions, systems) {
  const r = rng(99);
  for (const s of systems.filter((x) => x.type === "stratum")) {
    const p = positions.get(s.id);
    for (let k = 0; k < 3; k++) {
      const h = sprite(TEX.nebula, k ? 0x8a0010 : 0xff1830, 180 + r() * 260, 0.07 + r() * 0.05);
      h.position.set(p.x + (r() - 0.5) * 120, p.y - 6, p.z + (r() - 0.5) * 120); typeGroups.stratum.add(h);
    }
  }
}
function buildAll(data) {
  const positions = placeSystems(data.systems);
  empireHaze(positions, data.systems);
  let ri = 0, ei = 0;
  for (const s of data.systems) {
    if (s.type === "datum") systemsById.set(s.id, buildRebel(s, positions.get(s.id), ri++));
    else if (s.type === "stratum") systemsById.set(s.id, buildEmpire(s, positions.get(s.id), ei++));
  }
  buildRim(data.systems.filter((s) => s.type === "independent"), positions);
}

// ---------- effects ----------
const FX_COLOR = { datum: 0xffd27a, stratum: 0xff3a3a, independent: 0x8fffc8 };
function blockFlash(sysId, tag) {
  const rec = systemsById.get(sysId);
  if (!rec) return;
  const at = new THREE.Vector3();
  const planet = tag && rec.planets.find((p) => p.tag === tag);
  if (planet) planet.mesh.getWorldPosition(at); else at.copy(rec.pos);
  const color = FX_COLOR[rec.type] || 0xffffff;
  const flash = sprite(TEX.glow, color, 10, 1); flash.position.copy(at); scene.add(flash);
  const ring = new THREE.Mesh(new THREE.RingGeometry(0.9, 1, 96), new THREE.MeshBasicMaterial({ color, transparent: true, opacity: 0.9, side: THREE.DoubleSide, blending: THREE.AdditiveBlending, depthWrite: false }));
  ring.position.copy(at); ring.lookAt(camera.position); scene.add(ring);
  // a courier to the galactic core, where the chain lives
  const courier = sprite(TEX.glow, color, 14, 1); courier.position.copy(at); scene.add(courier);
  const from = at.clone(), mid = at.clone().multiplyScalar(0.5).add(new THREE.Vector3(0, 180, 0)), curve = new THREE.QuadraticBezierCurve3(from, mid, new THREE.Vector3(0, 0, 0));
  effects.push({ age: 0, life: 4.5, step(t) {
    flash.scale.setScalar(10 + t * 90); flash.material.opacity = Math.max(0, 1 - t * 1.4);
    ring.scale.setScalar(4 + t * 160); ring.material.opacity = 0.9 * Math.max(0, 1 - t);
    const c = clamp(t * 1.3, 0, 1); courier.position.copy(curve.getPoint(ease(c))); courier.material.opacity = c >= 1 ? Math.max(0, 1 - (t - 0.77) * 4) : 1;
  }, done() { scene.remove(flash, ring, courier); flash.material.dispose(); ring.geometry.dispose(); ring.material.dispose(); courier.material.dispose(); } });
  if (rec.type === "stratum" && rec.ds) superlaser(rec);
}
function superlaser(rec) {
  const from = new THREE.Vector3(); rec.ds.getWorldPosition(from);
  const dir = new THREE.Vector3(Math.random() - 0.5, (Math.random() - 0.5) * 0.3, Math.random() - 0.5).normalize();
  const to = from.clone().addScaledVector(dir, 600);
  const g = new THREE.BufferGeometry().setFromPoints([from, to]);
  const beam = new THREE.Line(g, new THREE.LineBasicMaterial({ color: 0x7dff9a, transparent: true, opacity: 1, blending: THREE.AdditiveBlending }));
  scene.add(beam);
  effects.push({ age: 0, life: 1.4, step(t) { beam.material.opacity = t < 0.2 ? 1 : Math.max(0, 1 - (t - 0.2) * 1.4); }, done() { scene.remove(beam); g.dispose(); beam.material.dispose(); } });
}
function shootingStar() {
  const r = Math.random, a = r() * Math.PI * 2, R = 2600 + r() * 1400;
  const from = new THREE.Vector3(Math.cos(a) * R, 300 + r() * 900, Math.sin(a) * R), to = from.clone().add(new THREE.Vector3((r() - 0.5) * 1800, -600 - r() * 600, (r() - 0.5) * 1800));
  const s = sprite(TEX.glow, 0xcfe8ff, 26, 1); scene.add(s);
  effects.push({ age: 0, life: 1.8, step(t) { s.position.lerpVectors(from, to, t); s.material.opacity = Math.sin(Math.PI * t); }, done() { scene.remove(s); s.material.dispose(); } });
}
function warp() {
  const w = el("div", "warp-fx"); document.body.append(w); setTimeout(() => w.remove(), 900);
}

// ---------- camera flight ----------
let flight = null, focused = null;
function flyTo(target, distance, dur = 1.8) {
  const fromPos = camera.position.clone(), fromTarget = controls.target.clone();
  const dir = camera.position.clone().sub(controls.target).normalize();
  if (Math.abs(dir.y) > 0.92) dir.set(0.3, 0.6, 0.74).normalize();
  const toPos = target.clone().addScaledVector(dir.lerp(new THREE.Vector3(0, 0.45, 1).normalize(), 0.35).normalize(), distance);
  flight = { t: 0, dur: FAST ? 0.05 : dur / Math.max(MOTION, 0.5), fromPos, fromTarget, toPos, toTarget: target.clone() };
  if (fromPos.distanceTo(toPos) > 400 && !reduced) warp();
}
function focusSystem(id, tag) {
  const rec = systemsById.get(id);
  if (!rec) return;
  focused = { id, tag: tag || null };
  let target = rec.pos.clone(), dist = rec.extent * 2.6 + 30;
  const planet = tag && rec.planets.find((p) => p.tag === tag);
  if (planet) { planet.mesh.getWorldPosition(target); dist = Math.max(10, planet.radius * 14); }
  flyTo(target, dist);
  showPanel(rec, tag);
  setPlanetLabels(rec);
}
function unfocus() {
  focused = null; hidePanel(); setPlanetLabels(null);
  flyTo(new THREE.Vector3(0, 0, 0), 1750);
}
let planetLabels = [];
function setPlanetLabels(rec) {
  for (const l of planetLabels) l.parent && l.parent.remove(l);
  planetLabels = [];
  if (!rec || rec.type === "independent") return;
  rec.planets.filter((p, k) => k < 10).forEach((p) => {
    const l = makeLabel(p.tag, "planet"); l.position.set(0, p.radius + 1.6, 0); p.mesh.add(l); planetLabels.push(l);
  });
}

// ---------- readouts ----------
const tip = $("tip");
function planetData(sysId, tag) { const s = DATA.systems.find((x) => x.id === sysId); return s && [s, s.planets.find((p) => p.tag === tag)]; }
function systemData(sysId) { return DATA.systems.find((x) => x.id === sysId); }
function policyRows(dl, pol, title = "Template policy") {
  if (!pol) return;
  sec(dl, title);
  kv(dl, "Transactions", `${n(pol.txs)} per block`);
  kv(dl, "Block weight", `${n(pol.weightKwu)} kWU avg`);
  kv(dl, "Fees", `${xbt(pol.feesSats)} per block`);
  kv(dl, "Fee rates", `median ${pol.medianFee ?? "–"} · lowest taken ${pol.minFee ?? "–"} sat/vB`);
  if (pol.matchRate != null) kv(dl, "Template match", `${pol.matchRate}% of the expected block`);
  kv(dl, "Empty blocks", pol.emptyBlocks ? n(pol.emptyBlocks) : "none");
  kv(dl, "Coinbase", `${n(pol.outputs)} outputs avg ${pol.outputs > 5 ? "(paid in the coinbase)" : ""}`);
}
function fillPlanet(box, s, p) {
  box.className = "tip " + (s.type === "stratum" ? "empire" : s.type === "independent" ? "rim" : "");
  box.replaceChildren();
  box.append(el("div", "t-kind", s.type === "independent" ? "Independent world" : p.house ? "Capital world" : p.blocks ? "DATUM gateway" : "DATUM gateway · forming"));
  box.append(el("div", "t-name", p.tag));
  box.append(el("div", "t-sub", `${s.name} system · ${TYPE_LABEL[s.type][0]}`));
  const dl = el("dl", "kv");
  kv(dl, "Blocks hit", p.blocks ? `${n(p.blocks)} since the fork · ${n(p.blocks7d)} this week` : "none yet");
  if (p.last) kv(dl, "Last block", `${n(p.last.height)} · ${ago(p.last.ts)}`);
  if (p.estHashrate) kv(dl, "Est. hashrate", `${fmtHash(p.estHashrate)} (7-day, from blocks)`);
  if (p.unnamed) kv(dl, "Tag", "software default: several operators may share it");
  if (p.alsoIn && p.alsoIn.length) kv(dl, "Also mines in", p.alsoIn.slice(0, 4).join(", "));
  if (p.live) {
    sec(dl, "Live · Lazarus Prime");
    kv(dl, "Sessions", `${p.live.online} of ${p.live.sessions} online`, p.live.online ? "live-on" : "live-off");
    if (p.live.software && p.live.software.length) kv(dl, "Software", p.live.software.join(", "));
    kv(dl, "Connected", dur(p.live.connectedS));
    if (p.live.lastShareS != null) kv(dl, "Last share", `${Math.round(p.live.lastShareS)} s ago`);
    kv(dl, "Rejected", `${p.live.rejectPct}%`);
  }
  policyRows(dl, p.policy);
  box.append(dl);
}
function fillSystem(box, s) {
  box.className = "tip " + (s.type === "stratum" ? "empire" : s.type === "independent" ? "rim" : "");
  box.replaceChildren();
  box.append(el("div", "t-kind", s.type === "stratum" ? "Imperial stronghold" : s.type === "datum" ? "Star system" : "Independent world"));
  box.append(el("div", "t-name", s.name));
  box.append(el("div", "t-sub", `${TYPE_LABEL[s.type][0]} · ${TYPE_LABEL[s.type][1]}`));
  const dl = el("dl", "kv");
  kv(dl, "Blocks", `${n(s.blocks)} since the fork · ${n(s.blocks7d)} this week`);
  kv(dl, "Share (7d)", `${s.share7d}% of blocks`);
  if (s.estHashrate) kv(dl, "Est. hashrate", fmtHash(s.estHashrate));
  if (s.type !== "independent") kv(dl, "Gateways", s.type === "stratum" ? (s.gateways ? `${s.gateways} tags seen (rare)` : "none: every template is the pool's") : n(s.gateways));
  kv(dl, "Last block", `${n(s.last.height)} · ${ago(s.last.ts)}`);
  policyRows(dl, s.policy, s.type === "datum" ? "Template policy (all its blocks)" : "Template policy");
  box.append(dl);
}
let tipTarget = null;
function showTip(obj, x, y) {
  const key = obj ? obj.sys + "|" + (obj.tag || "") : null;
  if (key !== tipTarget) {
    tipTarget = key;
    if (!obj) { tip.hidden = true; return; }
    const s = systemData(obj.sys);
    if (!s) { tip.hidden = true; return; }
    if (obj.tag && s.type !== "independent") { const pd = planetData(obj.sys, obj.tag); if (pd && pd[1]) fillPlanet(tip, pd[0], pd[1]); else fillSystem(tip, s); }
    else if (s.type === "independent") fillPlanet(tip, s, s.planets[0] || { tag: s.name, blocks: s.blocks, blocks7d: s.blocks7d, last: s.last, policy: s.policy });
    else fillSystem(tip, s);
    tip.hidden = false;
  }
  if (!obj) return;
  const w = tip.offsetWidth, h = tip.offsetHeight;
  tip.style.left = clamp(x + 18, 8, innerWidth - w - 8) + "px";
  tip.style.top = clamp(y + 18, 8, innerHeight - h - 50) + "px";
}

// ---------- pinned panel ----------
function showPanel(rec, tag) {
  const s = rec.data, panel = $("panel");
  panel.hidden = false;
  panel.className = "panel " + (s.type === "stratum" ? "empire" : s.type === "independent" ? "rim" : "");
  $("panel-kind").textContent = s.type === "stratum" ? "Imperial stronghold" : s.type === "datum" ? "Star system" : "Independent world";
  $("panel-name").textContent = s.name;
  $("panel-sub").textContent = `${TYPE_LABEL[s.type][0]}: ${TYPE_LABEL[s.type][1]}`;
  const dl = $("panel-stats"); dl.replaceChildren();
  kv(dl, "Blocks", `${n(s.blocks)} · ${n(s.blocks7d)} this week`);
  kv(dl, "Share (7d)", `${s.share7d}%`);
  if (s.estHashrate) kv(dl, "Est. hashrate", fmtHash(s.estHashrate));
  kv(dl, "First block", n(s.firstHeight));
  kv(dl, "Last block", `${n(s.last.height)} · ${ago(s.last.ts)}`);
  policyRows(dl, s.policy);
  const list = $("panel-planets"); list.replaceChildren();
  const planets = s.type === "independent" ? [] : s.planets;
  $("panel-planets-h").textContent = s.type === "stratum" ? `Worlds under Imperial rule (${planets.length})` : `Gateways (${planets.length})`;
  $("panel-planets-h").hidden = !planets.length;
  for (const p of planets.slice(0, 250)) {
    const li = el("li"); li.tabIndex = 0;
    const d = el("i", "d" + (p.live && p.live.online ? " on" : "")); d.style.background = "#" + (p.house ? new THREE.Color(0xffcf5c) : planetColor(p.tag)).getHexString();
    li.append(d, el("span", "n", p.tag), el("span", "c", p.blocks ? `${n(p.blocks)} blk` : p.live ? "forming" : "–"));
    if (p.tag === tag) li.style.background = "rgba(255,204,102,.14)";
    const go = () => { const r = systemsById.get(s.id); const pl = r && r.planets.find((x) => x.tag === p.tag); if (pl) focusSystem(s.id, p.tag); };
    li.addEventListener("click", go); li.addEventListener("keydown", (e) => { if (e.key === "Enter") go(); });
    list.append(li);
  }
  const foot = $("panel-link"); foot.replaceChildren();
  const link = safeLink(s.link);
  if (link) { const a = el("a", null, link.replace(/^https:\/\//, "")); a.href = link; a.target = "_blank"; a.rel = "noopener noreferrer"; foot.append("Pool site: ", a, " · "); }
  const ex = el("a", null, "last block on the explorer"); ex.href = `https://mempool.lazarus-xbt.xyz/block/${encodeURIComponent(s.last.id)}`; ex.target = "_blank"; ex.rel = "noopener"; foot.append(ex);
}
function hidePanel() { $("panel").hidden = true; }
$("panel-close").addEventListener("click", unfocus);
addEventListener("keydown", (e) => { if (e.key === "Escape") { if (focused) unfocus(); } });

// ---------- star chart ----------
function renderChart(filter = "") {
  const ol = $("systems"); ol.replaceChildren();
  const q = filter.trim().toLowerCase();
  const color = (s) => (s.type === "stratum" ? "#ff2a2a" : s.type === "independent" ? "#7fe0b0" : "#ffcf5c");
  const add = (s, label, count, tag) => {
    const li = el("li"); li.tabIndex = 0; if (tag) li.classList.add("hit");
    const d = el("i", "d"); d.style.background = color(s); li.append(d);
    const nm = el("span", "n", label); li.append(nm); li.append(el("span", "c", count));
    const go = () => focusSystem(s.id, tag);
    li.addEventListener("click", go); li.addEventListener("keydown", (e) => { if (e.key === "Enter") go(); });
    ol.append(li);
  };
  const sep = (t) => ol.append(el("li", "sep", t));
  if (q) {
    let shown = 0;
    for (const s of DATA.systems) {
      if (shown > 80) break;
      if (s.name.toLowerCase().includes(q)) { add(s, s.name, `${n(s.blocks)} blk`); shown++; }
      if (s.type === "independent") continue;
      for (const p of s.planets) if (!p.house && p.tag.toLowerCase().includes(q)) { add(s, `${p.tag} · ${s.name}`, p.blocks ? `${n(p.blocks)} blk` : "forming", p.tag); shown++; }
    }
    if (!shown) ol.append(el("li", "sub", "No contact. Try another name."));
    return;
  }
  const byType = (t) => DATA.systems.filter((s) => s.type === t).sort((a, b) => b.blocks7d - a.blocks7d || b.blocks - a.blocks);
  sep(`Rebel Alliance · ${DATA.counts.datum}`); byType("datum").forEach((s) => add(s, s.name, `${s.share7d}%`));
  sep(`Galactic Empire · ${DATA.counts.stratum}`); byType("stratum").forEach((s) => add(s, s.name, `${s.share7d}%`));
  const rim = byType("independent");
  sep(`Outer Rim · ${DATA.counts.independent}`); rim.slice(0, 60).forEach((s) => add(s, s.name, s.blocks7d ? `${s.share7d}%` : `${n(s.blocks)} blk`));
  if (rim.length > 60) ol.append(el("li", "sub", `…and ${rim.length - 60} more. Search to find one.`));
}
$("search").addEventListener("input", (e) => renderChart(e.target.value));
$("chart-toggle").addEventListener("click", () => {
  const c = $("chart"), closed = c.classList.toggle("closed");
  $("chart-toggle").setAttribute("aria-expanded", String(!closed)); store.set("xbtg-chart", closed ? "closed" : "open");
});
if (store.get("xbtg-chart") === "closed" || narrow) { $("chart").classList.add("closed"); $("chart-toggle").setAttribute("aria-expanded", "false"); }
document.querySelectorAll(".leg").forEach((b) => b.addEventListener("click", () => {
  const on = b.classList.toggle("on"); typeGroups[b.dataset.type].visible = on;
}));

// ---------- header + relay ----------
function renderHud() {
  const gw = DATA.systems.reduce((a, s) => a + (s.type === "datum" ? s.planets.filter((p) => !p.house).length : 0), 0);
  $("nethash").textContent = fmtHash(DATA.networkHashrate);
  $("syscount").textContent = n(DATA.systems.length);
  $("gwcount").textContent = n(gw);
}
function stardate() {
  if (!DATA || !DATA.tip) return;
  const since = Math.max(0, Date.now() / 1000 - DATA.tip.ts);
  $("stardate").textContent = (DATA.tip.height + Math.min(0.99, since / 420)).toFixed(2);
}
let freshHeights = new Set();
function renderRelay() {
  const box = $("relay"); box.replaceChildren();
  const make = () => DATA.recent.slice(0, 24).map((b) => {
    const s = el("span"); if (freshHeights.has(b.height)) s.className = "fresh";
    const cls = b.systemType === "stratum" ? "e" : b.systemType === "independent" ? "o" : "r";
    s.append(el("b", null, `#${n(b.height)}`), " forged by ", el("span", cls, b.planet ? `${b.planet} · ${b.system}` : b.system), ` · ${ago(b.ts)}`);
    return s;
  });
  box.append(...make(), ...make());
}

// ---------- pointer ----------
const ray = new THREE.Raycaster(), pointer = new THREE.Vector2();
let pointerXY = null, downAt = null;
renderer.domElement.addEventListener("pointermove", (e) => { pointerXY = [e.clientX, e.clientY]; });
renderer.domElement.addEventListener("pointerleave", () => { pointerXY = null; showTip(null); });
renderer.domElement.addEventListener("pointerdown", (e) => { downAt = [e.clientX, e.clientY, performance.now()]; });
renderer.domElement.addEventListener("pointerup", (e) => {
  if (!downAt) return;
  const moved = Math.hypot(e.clientX - downAt[0], e.clientY - downAt[1]), quick = performance.now() - downAt[2] < 400;
  downAt = null;
  if (moved > 6 || !quick) return;
  const hit = pick(e.clientX, e.clientY);
  if (hit) focusSystem(hit.sys, hit.tag && systemData(hit.sys).type !== "independent" ? hit.tag : null);
  if (hit && e.pointerType !== "mouse") showTip(hit, e.clientX, e.clientY);
});
function pick(x, y) {
  pointer.set((x / innerWidth) * 2 - 1, -(y / innerHeight) * 2 + 1);
  ray.setFromCamera(pointer, camera);
  const vis = pickables.filter((m) => { let o = m; while (o) { if (!o.visible) return false; o = o.parent; } return true; });
  const hits = ray.intersectObjects(vis, false);
  let best = null;
  for (const h of hits) {
    const u = h.object.userData;
    if (u.kind === "planet") { best = { sys: u.sys, tag: u.tag }; break; }
    if (!best) best = { sys: u.sys };
  }
  if (!best && rimPoints && typeGroups.independent.visible) {
    ray.params.Points.threshold = clamp(camera.position.distanceTo(controls.target) / 110, 2, 26);
    const rh = ray.intersectObject(rimPoints, false);
    if (rh.length) best = { sys: rimIndex[rh[0].index] };
  }
  return best;
}

// ---------- crawl ----------
function endCrawl() {
  const c = $("crawl"); if (!c || c.classList.contains("gone")) return;
  c.classList.add("gone"); store.set("xbtg-crawl", "seen"); setTimeout(() => c.remove(), 1400);
  introFlight();
}
$("engage").addEventListener("click", endCrawl);
$("skip").addEventListener("click", endCrawl);
const showCrawl = !reduced && (store.get("xbtg-crawl") !== "seen" || new URLSearchParams(location.search).has("crawl"));
if (!showCrawl) $("crawl").remove(); else setTimeout(endCrawl, 50000);
let introDone = false;
function introFlight() {
  if (introDone) return; introDone = true;
  camera.position.set(0, 2600, 5200); controls.target.set(0, 0, 0);
  flyTo(new THREE.Vector3(0, 0, 0), 1750, 4.2);
  // the latest real blocks light up as we arrive
  DATA.recent.slice(0, 3).reverse().forEach((b, k) => setTimeout(() => blockFlash(sysIdFor(b), b.planet), 3600 + k * 2200));
}
const sysIdFor = (b) => { const s = DATA.systems.find((x) => x.name === b.system && (b.systemType ? x.type === b.systemType : true)); return s ? s.id : null; };

// ---------- data ----------
function fail(msg) { const e = $("error"); e.textContent = msg; e.hidden = false; $("loading").classList.add("done"); }
async function load() {
  const r = await fetch(DATA_URL, { cache: "no-store" });
  if (!r.ok) throw new Error(`HTTP ${r.status}`);
  return r.json();
}
async function poll() {
  try {
    const d = await load();
    const lastSeen = DATA.tip ? DATA.tip.height : 0;
    const fresh = d.recent.filter((b) => b.height > lastSeen).sort((a, b) => a.height - b.height);
    DATA = d;
    fresh.forEach((b, k) => { freshHeights.add(b.height); setTimeout(() => blockFlash(sysIdFor(b), b.planet), k * 1600); });
    renderHud(); renderRelay();
    if ($("search").value === "") renderChart();
  } catch { /* keep the last picture */ }
}

// ---------- loop ----------
const clock = new THREE.Clock();
let nextComet = 6;
function tick() {
  const dt = Math.min(clock.getDelta(), 0.1), t = clock.elapsedTime;
  planetUniformsShared.uTime.value = t;
  for (const m of timeMats) m.uniforms.uTime.value = t;
  galaxy.rotation.y += 0.004 * dt * MOTION;
  for (const c of galaxy.children) if (c.userData.spin) c.material.rotation += c.userData.spin * dt;
  typeGroups.independent.rotation.y += 0.0012 * dt * MOTION;
  for (const f of animated) f(dt, t);
  for (let i = effects.length - 1; i >= 0; i--) {
    const fx = effects[i]; fx.age += dt; const p = fx.age / fx.life;
    if (p >= 1) { fx.done(); effects.splice(i, 1); } else fx.step(p);
  }
  if (t > nextComet && !reduced) { shootingStar(); nextComet = t + 5 + Math.random() * 9; }
  if (flight) {
    flight.t += dt / flight.dur; const k = ease(clamp(flight.t, 0, 1));
    camera.position.lerpVectors(flight.fromPos, flight.toPos, k);
    controls.target.lerpVectors(flight.fromTarget, flight.toTarget, k);
    if (flight.t >= 1) flight = null;
  } else if (focused) {
    // keep a focused planet in the middle as it orbits
    const rec = systemsById.get(focused.id), pl = focused.tag && rec && rec.planets.find((p) => p.tag === focused.tag);
    if (pl) { const w = new THREE.Vector3(); pl.mesh.getWorldPosition(w); const d = w.sub(controls.target); controls.target.add(d); camera.position.add(d); }
  }
  controls.update();
  // labels fade with distance so the far galaxy stays clean
  const camD = camera.position.distanceTo(controls.target);
  for (const l of labelled) l.element.style.opacity = camD > 3600 ? "0" : camD > 2400 ? "0.6" : "1";
  const wp = new THREE.Vector3();
  for (const b of beacons) { b.getWorldPosition(wp); const d = camera.position.distanceTo(wp); b.material.opacity = clamp((d - 500) / 900, 0, 0.9); }
  if (FAST && window.__xbtg && window.__xbtg.follow) pointerXY = window.__xbtg.where(...window.__xbtg.follow);
  if (pointerXY && !flight) {
    const hit = pick(pointerXY[0], pointerXY[1]);
    showTip(hit, pointerXY[0], pointerXY[1]);
    for (const rec of systemsById.values()) for (const p of rec.planets) if (p.mesh.material.uniforms) p.mesh.material.uniforms.uHover.value = hit && hit.sys === rec.data.id && (hit.tag === p.tag || rec.type === "independent") ? 1 : 0;
    renderer.domElement.style.cursor = hit ? "pointer" : "grab";
  }
  if (composer) composer.render(); else renderer.render(scene, camera);
  labels.render(scene, camera);
}
addEventListener("resize", () => {
  camera.aspect = innerWidth / innerHeight; camera.updateProjectionMatrix();
  renderer.setSize(innerWidth, innerHeight); labels.setSize(innerWidth, innerHeight);
  if (composer) composer.setSize(innerWidth, innerHeight);
});

(async function start() {
  try { DATA = await load(); } catch (e) { fail(`Long-range sensors are offline (${e.message}). Try again in a minute.`); return; }
  buildAll(DATA);
  renderHud(); renderRelay(); renderChart(); stardate();
  setInterval(stardate, 1000); setInterval(renderRelay, 30_000); setInterval(poll, POLL_MS);
  renderer.setAnimationLoop(tick);
  if (FAST) window.__xbtg = { // test hook: where a system or planet is on screen
    where(sys, tag) { const r = systemsById.get(sys); if (!r) return null; const pl = tag && r.planets.find((x) => x.tag === tag); const w = new THREE.Vector3(); (pl ? pl.mesh : r.star || r.planets[0].mesh).getWorldPosition(w); w.project(camera); return [Math.round((w.x + 1) / 2 * innerWidth), Math.round((1 - w.y) / 2 * innerHeight)]; },
    ids: () => [...systemsById.keys()],
    follow: null, // [sys, tag]: keep the pointer on it (hover test)
  };
  $("loading").classList.add("done"); setTimeout(() => $("loading").remove(), 900);
  if (!document.getElementById("crawl")) introFlight();
})();
