#!/usr/bin/env bash
# Build one of the Pages sites from the running node and deploy it.
#
#   ./deploy.sh pool      [preview-branch]     # -> lazarus-pool.pages.dev    / pool.lazarus-xbt.xyz
#   ./deploy.sh mempool   [preview-branch]     # -> lazarus-mempool.pages.dev / mempool.lazarus-xbt.xyz
#
# With no branch the deploy goes to production ("main"). Give a branch name to get a preview at
# <branch>.<project>.pages.dev without touching the live site. Every deploy is verified against
# the node before this script exits; a production deploy that fails verification is reported, and
# the previous one can be restored from the Pages dashboard (Deployments > Rollback).
#
# The pool build snapshots :8889, the read-only dashboard processes, never the :8888 writer.
#
# Auth is wrangler's own login (`wrangler login`). The REST token in ~/.config/cloudflare/env has
# no Pages scope, so it is kept out of wrangler's environment here.
set -euo pipefail
SITE="${1:?usage: deploy.sh pool|mempool [branch]}"
BRANCH="${2:-main}"
HERE="$(cd "$(dirname "$0")" && pwd)"
NODE="${LAZARUS_NODE:?set LAZARUS_NODE to the host running server.py (use an ssh -L forward for the hub)}"
WRANGLER="${WRANGLER:-$HOME/.local/bin/wrangler}"
# account id from ~/.config/cloudflare/env (CLOUDFLARE_ACCOUNT_ID=...)
export CLOUDFLARE_ACCOUNT_ID="${CLOUDFLARE_ACCOUNT_ID:-$(grep -E "^CLOUDFLARE_ACCOUNT_ID=" "$HOME/.config/cloudflare/env" 2>/dev/null | cut -d= -f2)}"
: "${CLOUDFLARE_ACCOUNT_ID:?CLOUDFLARE_ACCOUNT_ID not set}"
unset CF_API_TOKEN CLOUDFLARE_API_TOKEN

# Production is built from the node's running copy of pool/, so a node that has not pulled main
# ships whatever it has instead (this is how production and GitHub drifted apart before
# 2026-09-23). Refuse a production deploy from an out-of-date or dirty tree here, and after the
# build refuse one whose static files differ from this tree's (the node is running other code).
if [ "$BRANCH" = main ] && [ "${ALLOW_DRIFT:-}" != 1 ]; then
  git -C "$HERE" fetch -q origin main
  if [ -n "$(git -C "$HERE" status --porcelain -- ../pool ../cloudflare)" ] || [ "$(git -C "$HERE" rev-parse HEAD)" != "$(git -C "$HERE" rev-parse origin/main)" ]; then
    echo "refusing a production deploy: this tree is not a clean copy of origin/main (pull, or ALLOW_DRIFT=1)" >&2
    exit 3
  fi
fi

case "$SITE" in
  pool)    PROJECT=lazarus-pool;    ORIGIN="http://$NODE:8889"; python3 "$HERE/pool-site/build.py" --origin "$ORIGIN"; VERIFY=verify-pool.py ;;
  mempool) PROJECT=lazarus-mempool; ORIGIN="http://$NODE:3006"; "$HERE/mempool-site/build.sh" "$ORIGIN"; VERIFY=verify-mempool.py ;;
  *) echo "unknown site: $SITE" >&2; exit 2 ;;
esac

if [ "$SITE" = pool ] && [ "$BRANCH" = main ] && [ "${ALLOW_DRIFT:-}" != 1 ]; then
  if ! diff -rq "$HERE/pool-site/dist/static" "$HERE/../pool/static" >/dev/null; then
    echo "refusing a production deploy: the node's /static differs from pool/static in this tree:" >&2
    diff -rq "$HERE/pool-site/dist/static" "$HERE/../pool/static" >&2 || true
    echo "pull main on the node and restart server.py (or ALLOW_DRIFT=1)" >&2
    exit 3
  fi
fi

cd "$HERE/$SITE-site"
"$WRANGLER" pages deploy dist --project-name "$PROJECT" --branch "$BRANCH" --commit-dirty=true

if [ "$BRANCH" = main ]; then URL="https://$PROJECT.pages.dev"; else URL="https://$BRANCH.$PROJECT.pages.dev"; fi
sleep 10
python3 "$HERE/$VERIFY" "$ORIGIN" "$URL"
