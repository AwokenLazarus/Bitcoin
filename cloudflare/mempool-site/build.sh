#!/usr/bin/env bash
# Build the mempool explorer's static surface for Cloudflare Pages.
#
# The files are the ones the Umbrel app serves: mempool/frontend at the pinned digest, with the
# pieces nginx or the image entrypoint generate at runtime taken from the running site so the
# result matches it exactly:
#   - every locale's index.html (nginx sub_filter injects the Lazarus theme tags)
#   - resources/config.js, resources/customize.js (written by the entrypoint from the app's env)
#   - /lazarus/* theme assets, the Lazarus pool logo and the favicons (nginx aliases)
# _worker.js then does what nginx-mempool.conf does: pick the locale, fall back to the app
# shell, and proxy /api and the websocket to the node.
#
# The node is only ever read. What the public site says differently from it (name, canonical
# URL, colours, config.js keys, theme cache-busting) is applied to the copy by postprocess.py.
#
#   ./build.sh http://<node>:3006
set -euo pipefail
ORIGIN="${1:?usage: build.sh <running mempool web origin>}"
IMAGE="mempool/frontend:v3.3.1@sha256:0a162e7e0d26a01e9686ddf69c96c4beae5fe10b0daa1020f3d392e033c058f1"
HERE="$(cd "$(dirname "$0")" && pwd)"
THEME="$HERE/../../node/umbrel/mempool-theme/www"
DIST="$HERE/dist"

rm -rf "$DIST" && mkdir -p "$DIST"
cid="$(docker create "$IMAGE")"
trap 'docker rm "$cid" >/dev/null' EXIT
docker cp "$cid:/var/www/mempool/browser/." "$DIST/"

get() { curl -fsS --max-time 30 -H 'Accept-Encoding: identity' "$ORIGIN$1" -o "$2"; }

for dir in "$DIST"/*/; do
  lang="$(basename "$dir")"
  [ -f "$dir/index.html" ] || continue
  get "/$lang/index.html" "$dir/index.html"
  grep -q '/lazarus/theme.js' "$dir/index.html" || { echo "$lang/index.html: theme tags missing" >&2; exit 1; }
done
get /resources/config.js "$DIST/resources/config.js"
get /resources/customize.js "$DIST/resources/customize.js" || rm -f "$DIST/resources/customize.js"

mkdir -p "$DIST/lazarus" "$DIST/resources/mining-pools" "$DIST/resources/favicons"
for f in "$THEME"/*; do
  name="$(basename "$f")"
  case "$name" in *.bak*) continue ;; esac
  # LICENSE, NOTICE and COPYING.md ship too: mempool is AGPL-3.0 and the footer links to them.
  # If the node's nginx does not serve one of them, the checkout's copy is the same file.
  case "$name" in
    # The checkout is the source of truth for theme assets: a theme change ships with a Pages deploy,
    # without first having to be copied onto the node that serves the capture.
    *) cp "$f" "$DIST/lazarus/$name" ;;
  esac
done
get /lazarus/pool-tags.json "$DIST/lazarus/pool-tags.json" || true
get /resources/mining-pools/lazarus.svg "$DIST/resources/mining-pools/lazarus.svg"
for f in favicon.ico favicon-16x16.png favicon-32x32.png apple-touch-icon.png; do
  get "/resources/favicons/$f" "$DIST/resources/favicons/$f"
done

python3 "$HERE/postprocess.py" "$DIST"
for f in LICENSE NOTICE; do
  [ -s "$DIST/lazarus/$f" ] || { echo "lazarus/$f missing: the footer links to it" >&2; exit 1; }
done

cp "$HERE/_worker.js" "$HERE/_routes.json" "$HERE/_headers" "$DIST/"
echo "built $(find "$DIST" -type f | wc -l) files, $(du -sh "$DIST" | cut -f1)"
