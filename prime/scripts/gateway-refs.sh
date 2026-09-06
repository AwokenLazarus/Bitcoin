#!/bin/bash
# Report whether the gateway builds this Prime supports have moved upstream.
#
#   scripts/gateway-refs.sh [stats-url]
#
# For each supported lineage it prints the current upstream head, and for the StartOS package
# the gateway commit its newest published release pins. Gateways send their git hash as the
# user agent, so when a stats URL is reachable this also prints the builds actually connected
# and marks the ones that are not a current upstream head — the ones worth testing next.
#
#   stats-url   default http://127.0.0.1:28916/stats.json (the pool's primed)
set -euo pipefail

STATS="${1:-http://127.0.0.1:28916/stats.json}"

gh_api() { curl -sSf --max-time 20 -H 'Accept: application/vnd.github+json' "https://api.github.com/$1"; }

head_of() { # repo ref -> sha
  gh_api "repos/$1/commits/$2" | python3 -c 'import json,sys; print(json.load(sys.stdin)["sha"])'
}

# The StartOS package pins the gateway as a submodule, so a release names a commit.
startos_newest_release() {
  gh_api repos/Retropex/datum-gateway-startos/releases | python3 -c '
import json, sys
rs = [r for r in json.load(sys.stdin) if r["tag_name"].startswith("pow_")]
print(rs[0]["tag_name"] if rs else "pow")'
}
startos_submodule() { # ref -> sha
  gh_api "repos/Retropex/datum-gateway-startos/git/trees/$1" | python3 -c '
import json, sys
for t in json.load(sys.stdin).get("tree") or []:
    if t["type"] == "commit" and t["path"] == "datum_gateway":
        print(t["sha"]); break'
}

TAG=$(startos_newest_release)
# name<TAB>upstream<TAB>ref<TAB>sha, so the connected-builds pass can name each hash.
REFS=$(
  printf 'convoy\tCONVOYMining/datum_gateway\tmaster\t%s\n' "$(head_of CONVOYMining/datum_gateway master)"
  printf 'fte\tFlyTheElephant1/datum_gateway\tmaster\t%s\n' "$(head_of FlyTheElephant1/datum_gateway master)"
  printf 'iohzrd\tiohzrd/datum_gateway\tmaster\t%s\n' "$(head_of iohzrd/datum_gateway master)"
  printf 'startos\tRetropex/datum-gateway-startos\t%s\t%s\n' "$TAG" "$(startos_submodule "$TAG")"
  printf 'startos-convoy\tRetropex/datum-gateway-startos\tpow-convoy\t%s\n' "$(startos_submodule pow-convoy)"
)

printf '%-15s %-31s %-16s %s\n' LINEAGE UPSTREAM REF HEAD
while IFS=$'\t' read -r name repo ref sha; do
  printf '%-15s %-31s %-16s %s\n' "$name" "$repo" "$ref" "${sha:0:12}"
done <<<"$REFS"

echo
if ! CLIENTS=$(curl -sSf --max-time 6 "$STATS" 2>/dev/null); then
  echo "no stats at $STATS; skipping connected builds"
  exit 0
fi
echo "connected builds:"
# A user agent looks like "v0.4.1-beta[+local]/<git hash>[+]"; the hash identifies the build.
printf '%s' "$CLIENTS" | REFS="$REFS" python3 -c '
import json, os, re, sys

known = {}
for line in os.environ["REFS"].splitlines():
    parts = line.split("\t")
    if len(parts) == 4 and parts[3]:
        known.setdefault(parts[3], []).append(parts[0])

counts = {}
for c in json.load(sys.stdin).get("clients") or []:
    key = (c.get("user_agent") or "?", c.get("generation") or "?")
    counts[key] = counts.get(key, 0) + 1

for (ua, gen), n in sorted(counts.items(), key=lambda kv: -kv[1]):
    m = re.search(r"[0-9a-f]{40}", ua)
    if not m:
        tag = "no git hash"
    else:
        tag = "/".join(known.get(m.group(0), [])) or "untracked"
    print(f"  {n:>3}x  {gen:<7} {ua:<64} {tag}")
'
