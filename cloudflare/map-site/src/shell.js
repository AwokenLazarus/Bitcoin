// The hub's page frame and the small formatters, for the pages the Worker renders itself.
// Every value that came from a coinbase or a pool's API goes through esc().

export const esc = (s) => String(s == null ? "" : s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
export const q = (s) => encodeURIComponent(String(s));
export const n = (v) => Number(v || 0).toLocaleString("en-US");
export const hr = (ghs) => !ghs ? "—" : ghs >= 1e6 ? `${(ghs / 1e6).toFixed(2)} PH/s` : ghs >= 1000 ? `${(ghs / 1000).toFixed(2)} TH/s` : `${Math.round(ghs)} GH/s`;
export const when = (ts) => ts ? `${new Date(ts * 1000).toISOString().slice(0, 16).replace("T", " ")} UTC` : "—";
export const xbt = (sats) => sats == null ? "—" : `${(sats / 1e8).toLocaleString("en-US", { maximumFractionDigits: 8 })} XBT`;
export const plural = (count, one, many) => `${n(count)} ${count === 1 ? one : many}`;
export const safeUrl = (u) => /^https:\/\//.test(String(u || "")) ? esc(u) : "#";

const NAV = [["/learn/", "The chain"], ["/ecosystem/", "Ecosystem"], ["/map/", "Galaxy map"], ["/naughtylist/", "Naughty list"], ["/key/", "Lazarus Key"], ["/faq/", "FAQ"]];

export const HEAD = (title, description, canonical, current = "", css = "") => `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<title>${esc(title)}</title>
<meta name="description" content="${esc(description)}">
<link rel="canonical" href="${esc(canonical)}">
<meta property="og:type" content="website">
<meta property="og:site_name" content="Lazarus · Bitcoin BLAKE2b">
<meta property="og:title" content="${esc(title)}">
<meta property="og:description" content="${esc(description)}">
<meta property="og:url" content="${esc(canonical)}">
<meta property="og:image" content="https://lazarus-xbt.xyz/assets/og.png">
<meta name="twitter:card" content="summary_large_image">
<meta name="twitter:site" content="@LazarusPoolXBT">
<meta name="theme-color" content="#1a1712">
<link rel="icon" href="/favicon.ico" sizes="32x32">
<link rel="icon" href="/favicon.svg" type="image/svg+xml">
<link rel="apple-touch-icon" href="/apple-touch-icon.png">
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=IBM+Plex+Mono:wght@400;500&family=IBM+Plex+Sans:wght@400;500;600&family=Newsreader:ital,wght@0,400;0,500;1,400&display=swap">
<link rel="stylesheet" href="/assets/hub.css?v=7">
${css ? `<link rel="stylesheet" href="${esc(css)}">` : ""}
<script src="/assets/js.js"></script>
</head>
<body>
<a class="skip" href="#main">Skip to content</a>
<header class="site-header">
  <div class="topbar wrap">
    <a class="wordmark" href="/"><svg class="chi" viewBox="0 0 20.36 32" aria-hidden="true"><path d="M15.96 4.16C15.96 2.91 15.56 2.02 14.61 1.56C14.14 1.33 13.51 1.21 12.71 1.21H9.61L9.69 0H13.65C14.77 0 15.67 0.18 16.36 0.55C17.04 0.91 17.54 1.4 17.86 2.01C18.17 2.63 18.33 3.32 18.33 4.07C18.33 4.84 18.17 5.55 17.84 6.2C17.52 6.84 17.01 7.36 16.32 7.75C15.63 8.13 15.3 8.33 14.01 8.33H9.69L9.61 7.12H13.4C14.39 7.12 15.19 6.66 15.63 5.78C15.85 5.35 15.96 4.81 15.96 4.16ZM11.57 0 11.57 10.83V30.94L13.18 31.55V32H7.2V31.55L8.8 30.94V10.83V1.06L7.2 0.45V0ZM9.8 20.31 3.89 27.07 6.01 27.79V28.32H0V27.79L2.08 27.07L9.02 19.39L9.97 18.7L15.18 12.61L13.05 11.89V11.36H19.07V11.89L16.99 12.61L10.77 19.61ZM14.05 27.07 9.04 20.79 8.6 20.39 2.34 12.63 0.3 11.89V11.36H8.93V11.89L6.61 12.61L10.91 18.05L11.38 18.5L18.32 27.04L20.36 27.79V28.32H11.73V27.79Z"/></svg>Lazarus <em>XBT</em></a>
    <button class="nav-toggle" type="button" aria-expanded="false" aria-controls="site-nav">Menu</button>
    <nav class="site-nav" id="site-nav" aria-label="Main">
      ${NAV.map(([href, label]) => `<a href="${href}"${href === current ? ' aria-current="page"' : ""}>${label}</a>`).join("")}
      <a class="btn small" href="https://pool.lazarus-xbt.xyz" rel="noopener">Lazarus Pool</a>
    </nav>
  </div>
</header>
<main id="main">
<section class="section">
  <div class="wrap">
`;

export const FOOT = `
  </div>
</section>
</main>
<script src="/assets/hub.js?v=7" defer></script>
</body>
</html>
`;

