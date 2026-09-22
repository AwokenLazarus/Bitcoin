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
import { mergeGeometries } from "three/addons/utils/BufferGeometryUtils.js";

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
  datum: ["Rebel Alliance", "pays miners in the coinbase and DATUM gateways build blocks on it"],
  stratum: ["Galactic Empire", "never seen paying miners in the coinbase, or never seen taking DATUM"],
  independent: ["Outer Rim", "one operator, own node, whole blocks for themselves"],
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
// Outer Rim worlds: a lit disc with a ring, kept between 6 and 26 px so they read at any distance.
const rimVS = `
  attribute float size; attribute vec3 color; attribute float phase;
  uniform float uTime; uniform float uScale; varying vec3 vColor; varying float vPulse;
  void main() {
    vColor = color; vPulse = 0.75 + 0.25 * sin(uTime * 1.3 + phase * 6.2831);
    vec4 mv = modelViewMatrix * vec4(position, 1.0);
    gl_PointSize = clamp(size * uScale * (700.0 / -mv.z), 6.0 * uScale, 26.0 * uScale);
    gl_Position = projectionMatrix * mv;
  }`;
const rimFS = `
  varying vec3 vColor; varying float vPulse;
  void main() {
    vec2 d = gl_PointCoord - 0.5; float r = length(d);
    if (r > 0.5) discard;
    float core = smoothstep(0.2, 0.0, r);
    float ring = smoothstep(0.05, 0.0, abs(r - 0.36)) * 0.8;
    float halo = smoothstep(0.5, 0.2, r) * 0.25;
    float a = (core + ring + halo) * vPulse;
    gl_FragColor = vec4(mix(vColor, vec3(1.0), core * 0.5) * a, a);
  }`;
function rimMaterial() {
  return new THREE.ShaderMaterial({ uniforms: { uTime: { value: 0 }, uScale: { value: renderer.getPixelRatio() } }, vertexShader: rimVS, fragmentShader: rimFS, transparent: true, depthWrite: false, blending: THREE.AdditiveBlending });
}
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

// ---- size: close to hashrate, compressed so small pools stay visible ----
// Radius grows with the square root of the pool's 7-day block share, so apparent area is roughly
// proportional to hashrate, between a floor (so a small pool is still a world) and a ceiling.
let MAX_SHARE = 1;
const R_MIN = 3.5, R_MAX = 22;
function sizeFor(share) { return R_MIN + (R_MAX - R_MIN) * Math.sqrt(clamp((share || 0) / MAX_SHARE, 0, 1)); }
const dormant = (s) => !s.blocks7d;

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
function beacon(root, color, radius, sys) {
  const m = new THREE.SpriteMaterial({ map: TEX.ring, color, sizeAttenuation: false, transparent: true, opacity: 0.9, blending: THREE.AdditiveBlending, depthWrite: false });
  const b = new THREE.Sprite(m); b.scale.setScalar(0.014 + 0.0033 * radius); b.userData = { kind: "system", sys };
  root.add(b); beacons.push(b); pickables.push(b); return b;
}

// ---- ships of the Empire ----
let SD_GEO = null, TIE_GEO = null;
function prep(g) { const n = g.index ? g.toNonIndexed() : g; n.deleteAttribute("uv"); return n; }
function starDestroyerGeometry() {
  if (SD_GEO) return SD_GEO;
  // hull: a dagger with a raised spine, nose along +z; length about 8 units
  const L = 4.2, R = 2.6, W = 2.3, H = 0.55, D = 0.3;
  const tip = [0, 0, L], rl = [-W, 0, -R], rr = [W, 0, -R], top = [0, H, -R * 0.92], bot = [0, -D, -R * 0.92];
  const tri = (a, b, c) => [...a, ...b, ...c];
  const pos = new Float32Array([
    ...tri(tip, top, rl), ...tri(tip, rr, top),             // upper faces
    ...tri(tip, rl, bot), ...tri(tip, bot, rr),             // lower faces
    ...tri(rl, top, rr), ...tri(rl, rr, bot),               // stern
  ]);
  const hull = new THREE.BufferGeometry(); hull.setAttribute("position", new THREE.BufferAttribute(pos, 3)); hull.computeVertexNormals();
  const parts = [hull];
  const box = (w, h, d, x, y, z) => { const b = new THREE.BoxGeometry(w, h, d); b.translate(x, y, z); parts.push(prep(b)); };
  // stepped superstructure toward the stern
  box(1.9, 0.22, 1.5, 0, H * 0.72, -1.55);
  box(1.3, 0.24, 1.1, 0, H * 0.72 + 0.22, -1.85);
  box(0.8, 0.26, 0.8, 0, H * 0.72 + 0.46, -2.05);
  // command tower and bridge
  box(0.22, 0.55, 0.3, 0, H * 0.72 + 0.84, -2.15);
  box(1.05, 0.14, 0.34, 0, H * 0.72 + 1.15, -2.15);
  for (const x of [-0.4, 0.4]) { const d = new THREE.SphereGeometry(0.13, 10, 6); d.translate(x, H * 0.72 + 1.36, -2.15); parts.push(prep(d)); }
  // engine block
  box(2.2, 0.5, 0.35, 0, 0.12, -2.5);
  for (const x of [-0.6, 0, 0.6]) { const c = new THREE.CylinderGeometry(0.2, 0.24, 0.35, 10); c.rotateX(Math.PI / 2); c.translate(x, 0.12, -2.72); parts.push(prep(c)); }
  // greebles along the spine
  const r = rng(424242);
  for (let k = 0; k < 26; k++) { const z = 3.2 - r() * 4.6, half = (W * (L - z)) / (L + R) * 0.55, x = (r() * 2 - 1) * half; box(0.12 + r() * 0.18, 0.06, 0.12 + r() * 0.25, x, H * (1 - (Math.abs(x) / W)) * ((L - z) / (L + R)) * 0.9 + 0.03, z); }
  SD_GEO = mergeGeometries(parts.map((g) => (g.attributes.normal ? g : (g.computeVertexNormals(), g))));
  return SD_GEO;
}
const SD_MAT = new THREE.MeshPhongMaterial({ color: 0x9aa0a8, emissive: 0x0d0f14, specular: 0x333333, shininess: 28, flatShading: true });
function starDestroyer(scale = 1) {
  const g = new THREE.Group();
  const hull = new THREE.Mesh(starDestroyerGeometry(), SD_MAT); g.add(hull);
  for (const x of [-0.6, 0, 0.6]) { const e = sprite(TEX.glow, 0x7fc4ff, 0.9, 0.95); e.position.set(x, 0.12, -2.95); g.add(e); }
  const nav = sprite(TEX.soft, 0xff3b3b, 0.5, 0.9); nav.position.set(0, 0.05, 4.1); g.add(nav);
  g.scale.setScalar(scale); return g;
}
function tieGeometry() {
  if (TIE_GEO) return TIE_GEO;
  const parts = [];
  const cock = new THREE.SphereGeometry(0.34, 12, 8); parts.push(prep(cock));
  const strut = new THREE.CylinderGeometry(0.07, 0.07, 1.3, 6); strut.rotateZ(Math.PI / 2); parts.push(prep(strut));
  for (const x of [-0.68, 0.68]) { const w = new THREE.CylinderGeometry(0.72, 0.72, 0.05, 6); w.rotateZ(Math.PI / 2); w.translate(x, 0, 0); parts.push(prep(w)); }
  TIE_GEO = mergeGeometries(parts); return TIE_GEO;
}
const TIE_MAT = new THREE.MeshPhongMaterial({ color: 0x5b606a, emissive: 0x080a0e, shininess: 40, flatShading: true });
function tieSwarm(root, R, seed) {
  const r = rng(seed), n = lowPower ? 6 : 12;
  for (let k = 0; k < n; k++) {
    const pivot = new THREE.Object3D(); pivot.rotation.set((r() - 0.5) * 1.4, r() * Math.PI * 2, (r() - 0.5) * 1.0); root.add(pivot);
    const tie = new THREE.Mesh(tieGeometry(), TIE_MAT); const sc = 0.45 + R * 0.025; tie.scale.setScalar(sc);
    const orbit = R * (1.35 + r() * 0.9); tie.position.set(orbit, 0, 0); pivot.add(tie);
    const sp = ((1.8 + r() * 1.4) / Math.sqrt(orbit)) * MOTION;
    animated.push((dt, t) => { pivot.rotateY(sp * dt); tie.rotation.z = Math.sin(t * 2 + k) * 0.4; });
  }
}
function patrol(root, R, count, seed) {
  const r = rng(seed);
  for (let k = 0; k < count; k++) {
    const pivot = new THREE.Object3D(); pivot.rotation.set((r() - 0.5) * 0.7, r() * Math.PI * 2, (r() - 0.5) * 0.5); root.add(pivot);
    const sd = starDestroyer(0.55 + R * 0.05); const orbit = R * (2.3 + k * 0.6); sd.position.set(orbit, 0, 0); pivot.add(sd);
    const dir = r() < 0.5 ? 1 : -1; sd.rotation.y = dir > 0 ? Math.PI : 0;
    const sp = (0.32 / Math.sqrt(orbit)) * dir * MOTION;
    animated.push((dt, t) => { pivot.rotateY(sp * dt); sd.position.y = Math.sin(t * 0.5 + k) * 0.6; });
  }
}

// ---- a DATUM pool's own public stratum: an Imperial outpost inside a rebel system ----
function outpost(root, rec, p, orbit, starWorld, rr) {
  const pivot = new THREE.Object3D(); pivot.rotation.set((rr() - 0.5) * 0.2, rr() * Math.PI * 2, 0); root.add(pivot);
  const pr = 1.2 + 4.5 * Math.sqrt(clamp((p.blocks7d / Math.max(1, rec.data.blocks7d)) * (rec.data.share7d || 0) / MAX_SHARE, 0, 1));
  const used = p.blocks7d > 0 || (p.liveStratum && p.liveStratum.hashrate > 0);
  const mat = planetMaterial(new THREE.Color(used ? 0xb3261e : 0x5a2a2a), starWorld, 0.77, 1);
  const mesh = new THREE.Mesh(new THREE.SphereGeometry(pr, 28, 18), mat); mesh.position.x = orbit; pivot.add(mesh);
  mesh.add(sprite(TEX.glow, 0xff2020, pr * 6, used ? 0.55 : 0.2));
  const ring = new THREE.Mesh(new THREE.RingGeometry(pr * 1.6, pr * 1.68, 64), new THREE.MeshBasicMaterial({ color: 0xff3030, transparent: true, opacity: used ? 0.6 : 0.2, side: THREE.DoubleSide, blending: THREE.AdditiveBlending, depthWrite: false }));
  ring.rotation.x = Math.PI / 2; mesh.add(ring);
  if (used) {
    const esc = new THREE.Object3D(); mesh.add(esc);
    const sd = starDestroyer(0.18 + pr * 0.05); sd.position.set(pr * 2.4, 0, 0); sd.rotation.y = Math.PI; esc.add(sd);
    animated.push((dt) => esc.rotateY(0.6 * dt * MOTION));
    animated.push((dt, t) => { ring.scale.setScalar(1 + ((t * 0.35) % 1) * 0.8); ring.material.opacity = 0.6 * (1 - ((t * 0.35) % 1)); });
  }
  const sp = (0.9 / Math.sqrt(orbit)) * 0.7 * MOTION; animated.push((dt) => pivot.rotateY(sp * dt));
  mesh.userData = { kind: "planet", sys: rec.data.id, tag: p.tag }; pickables.push(mesh);
  rec.planets.push({ tag: p.tag, mesh, radius: pr, outpost: true });
  return pr;
}

function buildRebel(s, pos, i) {
  const root = new THREE.Group(); root.position.copy(pos); typeGroups.datum.add(root);
  const rr = rng(hash(s.id)), starR = sizeFor(s.share7d) * 0.85, sleepy = dormant(s);
  const color = new THREE.Color(s.name === "Lazarus" ? 0xffcf5c : REBEL[i % REBEL.length]);
  if (sleepy) color.multiplyScalar(0.55);
  const star = new THREE.Mesh(new THREE.SphereGeometry(starR, 32, 16), new THREE.MeshBasicMaterial({ color: color.clone().multiplyScalar(1.6) }));
  root.add(star);
  const halo = sprite(TEX.glow, color, starR * 9, sleepy ? 0.4 : 0.85), corona = sprite(TEX.soft, color, starR * 16, sleepy ? 0.1 : 0.25);
  root.add(halo, corona);
  animated.push((dt, t) => { corona.scale.setScalar(starR * (15 + Math.sin(t * 0.8 + i) * 1.6)); });
  const rec = { data: s, root, pos, star, planets: [], starR, extent: starR * 3, type: "datum" };
  const starWorld = pos.clone();
  const planets = s.planets.slice().sort((a, b) => (b.blocks - a.blocks) || ((b.live ? 1 : 0) - (a.live ? 1 : 0)));
  let k = 0;
  const house = planets.find((p) => p.house);
  if (house) { const o = starR * 2.6 + 6; const pr = outpost(root, rec, house, o, starWorld, rr); rec.extent = Math.max(rec.extent, o + pr); }
  for (const p of planets) {
    if (p.house) continue;
    const orbit = starR * 2.6 + 12 + 3.1 * Math.sqrt(k + 1) * (1 + (k % 3) * 0.08);
    const pr = p.blocks ? 0.7 + 1.35 * Math.log10(p.blocks + 1) : 0.45;
    const pivot = new THREE.Object3D();
    pivot.rotation.set((rr() - 0.5) * 0.35, rr() * Math.PI * 2, (rr() - 0.5) * 0.2);
    root.add(pivot);
    const c = planetColor(p.tag);
    const mat = planetMaterial(c, starWorld, (hash(p.tag) % 1000) / 1000, p.blocks ? 1 : 0.55);
    const mesh = new THREE.Mesh(new THREE.SphereGeometry(pr, 20, 14), mat);
    mesh.position.x = orbit; pivot.add(mesh);
    if (p.live && p.live.online) {
      mat.uniforms.uLive.value = 1;
      const b = sprite(TEX.soft, 0x7dffea, pr * 4.5, 0.5); mesh.add(b);
      const kk = k; animated.push((dt, t) => { b.material.opacity = 0.25 + 0.35 * (0.5 + 0.5 * Math.sin(t * 2.2 + kk)); });
    }
    if (k < 36) {
      const pts = []; for (let a = 0; a <= 96; a++) pts.push(new THREE.Vector3(Math.cos((a / 96) * Math.PI * 2) * orbit, 0, Math.sin((a / 96) * Math.PI * 2) * orbit));
      const line = new THREE.Line(new THREE.BufferGeometry().setFromPoints(pts), new THREE.LineBasicMaterial({ color: c, transparent: true, opacity: 0.09, depthWrite: false }));
      line.quaternion.copy(pivot.quaternion); root.add(line);
    }
    const speed = (0.9 / Math.sqrt(orbit)) * (0.6 + rr() * 0.5) * MOTION;
    animated.push((dt) => { pivot.rotateY(speed * dt); });
    mesh.userData = { kind: "planet", sys: s.id, tag: p.tag };
    pickables.push(mesh);
    rec.planets.push({ tag: p.tag, mesh, radius: pr });
    rec.extent = Math.max(rec.extent, orbit + pr);
    k++;
  }
  const hit = new THREE.Mesh(new THREE.SphereGeometry(Math.max(starR * 2.2, 10), 12, 8), new THREE.MeshBasicMaterial({ visible: false }));
  hit.userData = { kind: "system", sys: s.id }; root.add(hit); pickables.push(hit);
  const label = makeLabel(s.name); label.position.set(0, starR * 2 + 4, 0); root.add(label); labelled.push(label);
  beacon(root, color, starR / 0.85, s.id);
  return rec;
}

function buildEmpire(s, pos, i) {
  const root = new THREE.Group(); root.position.copy(pos); typeGroups.stratum.add(root);
  const R = sizeFor(s.share7d) * 0.95, rr = rng(hash(s.id)), sleepy = dormant(s);
  const ds = new THREE.Mesh(new THREE.SphereGeometry(R, 56, 36), new THREE.MeshPhongMaterial({ map: deathStarTexture(hash(s.id)), emissive: 0x220000, shininess: 12 }));
  ds.rotation.z = 0.25; root.add(ds);
  const glow = sprite(TEX.glow, 0xff2020, R * 8, sleepy ? 0.25 : 0.55), corona = sprite(TEX.soft, 0xff1a1a, R * 14, sleepy ? 0.08 : 0.2);
  root.add(glow, corona);
  const neb = sprite(TEX.nebula, 0xb00010, R * 30, 0.1); neb.position.y = -2; root.add(neb);
  const scan = new THREE.Mesh(new THREE.RingGeometry(R * 1.7, R * 1.78, 96), new THREE.MeshBasicMaterial({ color: 0xff3030, transparent: true, opacity: 0.5, side: THREE.DoubleSide, blending: THREE.AdditiveBlending, depthWrite: false }));
  scan.rotation.x = Math.PI / 2; root.add(scan);
  animated.push((dt, t) => {
    ds.rotation.y += 0.05 * dt * MOTION;
    const q = (t * 0.18 + i * 0.3) % 1; scan.scale.setScalar(1 + q * 2.4); scan.material.opacity = (sleepy ? 0.2 : 0.5) * (1 - q);
    corona.scale.setScalar(R * (13 + Math.sin(t * 0.7 + i) * 1.2));
  });
  patrol(root, R, clamp(Math.round(1 + Math.sqrt(s.share7d || 0) * 0.9), 1, 6), hash(s.id) + 7);
  if (!sleepy) tieSwarm(root, R, hash(s.id) + 11);
  const rec = { data: s, root, pos, star: ds, planets: [], starR: R, extent: R * 4.5, type: "stratum", ds };
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
  beacon(root, 0xff2a2a, R / 0.95, s.id);
  return rec;
}

let rimPoints = null, rimIndex = [];
function buildRim(systems, positions) {
  const N = systems.length, pos = new Float32Array(N * 3), col = new Float32Array(N * 3), size = new Float32Array(N), ph = new Float32Array(N);
  systems.forEach((s, i) => {
    const p = positions.get(s.id); pos.set([p.x, p.y, p.z], i * 3);
    const c = planetColor(s.name).lerp(new THREE.Color(0x7fe0b0), 0.6); col.set([c.r, c.g, c.b], i * 3);
    size[i] = 5 + 7 * Math.log10(s.blocks + 1); ph[i] = (hash(s.id) % 1000) / 1000;
    const rec = { data: s, root: null, pos: p, star: null, planets: [], starR: 2, extent: 12, type: "independent", rimIndex: i };
    systemsById.set(s.id, rec);
  });
  const g = new THREE.BufferGeometry();
  g.setAttribute("position", new THREE.BufferAttribute(pos, 3)); g.setAttribute("color", new THREE.BufferAttribute(col, 3));
  g.setAttribute("size", new THREE.BufferAttribute(size, 1)); g.setAttribute("phase", new THREE.BufferAttribute(ph, 1));
  const m = rimMaterial(); timeMats.push(m);
  rimPoints = new THREE.Points(g, m); rimPoints.userData = { kind: "rim" };
  typeGroups.independent.add(rimPoints);
  rimIndex = systems.map((s) => s.id);
  // every independent is also a small lit world, so there is a planet to arrive at up close
  const unit = new THREE.SphereGeometry(1, 16, 12);
  systems.forEach((s) => {
    const p = positions.get(s.id), pr = 0.8 + 1.1 * Math.log10(s.blocks + 1);
    const mesh = new THREE.Mesh(unit, planetMaterial(planetColor(s.name), new THREE.Vector3(0, 0, 0), (hash(s.id) % 1000) / 1000));
    mesh.scale.setScalar(pr); mesh.position.copy(p); mesh.userData = { kind: "system", sys: s.id }; typeGroups.independent.add(mesh); pickables.push(mesh);
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
  MAX_SHARE = Math.max(1, ...data.systems.filter((x) => x.type !== "independent").map((x) => x.share7d || 0));
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
  liveTarget(sysId, planet ? tag : null, at);
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
// Where a system or planet is this frame. The Outer Rim turns slowly around the core and planets
// orbit, so a position taken at load time goes stale.
function liveTarget(id, tag, out = new THREE.Vector3()) {
  const rec = systemsById.get(id);
  if (!rec) return null;
  const planet = tag && rec.planets.find((p) => p.tag === tag);
  if (planet) return planet.mesh.getWorldPosition(out);
  if (rec.type === "independent") { typeGroups.independent.updateMatrixWorld(); return out.copy(rec.pos).applyMatrix4(typeGroups.independent.matrixWorld); }
  return out.copy(rec.pos);
}
function flyTo(target, distance, dur = 1.8, track = null) {
  const fromPos = camera.position.clone(), fromTarget = controls.target.clone();
  const dir = camera.position.clone().sub(controls.target).normalize();
  if (Math.abs(dir.y) > 0.92) dir.set(0.3, 0.6, 0.74).normalize();
  const toPos = target.clone().addScaledVector(dir.lerp(new THREE.Vector3(0, 0.45, 1).normalize(), 0.35).normalize(), distance);
  flight = { t: 0, dur: FAST ? 0.05 : dur / Math.max(MOTION, 0.5), fromPos, fromTarget, toPos, toTarget: target.clone(), track };
  if (fromPos.distanceTo(toPos) > 400 && !reduced) warp();
}
function focusSystem(id, tag) {
  const rec = systemsById.get(id);
  if (!rec) return;
  const planet = tag && rec.planets.find((p) => p.tag === tag);
  // follow anything that moves: a planet on its orbit, or an Outer Rim world drifting round the core
  focused = { id, tag: planet ? tag : null, follow: !!planet || rec.type === "independent" };
  const target = liveTarget(id, focused.tag);
  const dist = planet ? Math.max(10, planet.radius * 14) : rec.type === "independent" ? 45 : rec.extent * 2.6 + 30;
  flyTo(target, dist, 1.8, focused.follow ? () => liveTarget(id, focused.tag) : null);
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
  const outpostPlanet = p.house && s.type === "datum";
  box.className = "tip " + (s.type === "stratum" || outpostPlanet ? "empire" : s.type === "independent" ? "rim" : "");
  box.replaceChildren();
  box.append(el("div", "t-kind", s.type === "independent" ? "Independent world" : outpostPlanet ? "Imperial outpost · public stratum" : p.house ? "Imperial core" : p.blocks ? "DATUM gateway" : "DATUM gateway · forming"));
  box.append(el("div", "t-name", p.tag));
  box.append(el("div", "t-sub", outpostPlanet ? `Inside the rebel ${s.name} system. Miners here hash on the pool's own template, not their own.` : `${s.name} system · ${TYPE_LABEL[s.type][0]}`));
  const dl = el("dl", "kv");
  if (p.liveStratum) { sec(dl, "Live · stratum endpoint"); kv(dl, "Hashrate", fmtHash(p.liveStratum.hashrate)); kv(dl, "Miners", n(p.liveStratum.miners)); sec(dl, "Blocks"); }
  if (outpostPlanet && !p.blocks7d && !(p.liveStratum && p.liveStratum.hashrate)) kv(dl, "Status", "no blocks through it this week");
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
  factionRows(dl, s);
  policyRows(dl, s.policy, s.type === "datum" ? "Template policy (all its blocks)" : "Template policy");
  box.append(dl);
}
function factionRows(dl, s) {
  const f = s.faction;
  if (!f) return;
  sec(dl, "Allegiance test");
  kv(dl, "Pays miners", f.paysMinersEver ? "yes: in the coinbase (TIDES split or straight to the finder)" : "not seen: blocks pay the pool's own wallet", f.paysMinersEver ? "live-on" : "live-off");
  kv(dl, "Takes DATUM", f.datumEver ? "yes: miners' gateways build blocks here" : "not seen", f.datumEver ? "live-on" : "live-off");
  kv(dl, f.window === "7d" ? "Last 7 days" : "All blocks", `${f.datumPct}% built by DATUM gateways · ${f.coinbasePaidPct}% paid to miners in the coinbase`);
  if (s.type === "datum" && f.poolBuiltPct > 0) kv(dl, "Imperial outpost", `${f.poolBuiltPct}% of blocks come from the pool's own templates`);
  if (f.operatorNote) kv(dl, "Verdict", f.operatorNote, "live-off");
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
  factionRows(dl, s);
  policyRows(dl, s.policy);
  const list = $("panel-planets"); list.replaceChildren();
  const planets = s.type === "independent" ? [] : s.planets;
  $("panel-planets-h").textContent = s.type === "stratum" ? `Worlds under Imperial rule (${planets.length})` : `Gateways (${planets.length})`;
  $("panel-planets-h").hidden = !planets.length;
  for (const p of planets.slice(0, 250)) {
    const li = el("li"); li.tabIndex = 0;
    const d = el("i", "d" + (p.live && p.live.online ? " on" : "")); d.style.background = "#" + (p.house ? new THREE.Color(0xff2a2a) : planetColor(p.tag)).getHexString();
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
    const cls = b.systemType === "stratum" || b.viaStratum ? "e" : b.systemType === "independent" ? "o" : "r";
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
    if (flight.track) { const now = flight.track(); if (now) { const moved = now.clone().sub(flight.toTarget); flight.toTarget.add(moved); flight.toPos.add(moved); } }
    flight.t += dt / flight.dur; const k = ease(clamp(flight.t, 0, 1));
    camera.position.lerpVectors(flight.fromPos, flight.toPos, k);
    controls.target.lerpVectors(flight.fromTarget, flight.toTarget, k);
    if (flight.t >= 1) flight = null;
  } else if (focused && focused.follow) {
    // keep a moving target in the middle; the camera keeps whatever angle and zoom the user gives it
    const w = liveTarget(focused.id, focused.tag);
    if (w) { const d = w.sub(controls.target); controls.target.add(d); camera.position.add(d); }
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
    zoom(d, dy = 0) { const t = controls.target.clone(); t.y += dy; flyTo(t, d); },
  };
  $("loading").classList.add("done"); setTimeout(() => $("loading").remove(), 900);
  if (!document.getElementById("crawl")) introFlight();
})();
