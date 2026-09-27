#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Mike Moore (AwokenLazarus)
# Published for reference: it exports each suite from its own demo repo (git archive), and only
# some of those repos are in demos/ here (sov-004, sov-008, sov-015). See ../README.md.
# SOV-010: every Sovereign TIDES S0 verify on the merged branch rnd/sov-s0
# (a checkout of it, $WT), one suite after another, all in ports
# SOV010_PORT_BASE..+99 (default 32300-32399), no miners, nothing pushed.
#
# SOV-012: the branch under test can be any descendant of rnd/sov-s0 d5278a2. WT defaults to the
# Bitcoin worktree you run this from when that is on a rnd/sov-* branch, else rnd-sov-s0. Datadirs
# go under ./run for rnd/sov-s0 and under $RND/sov-NNN/run/verify-all for rnd/sov-NNN
# (SOV010_RUN overrides).
#
# The demo repos are never edited. Each demo is exported (git archive) from the commit its task was
# accepted at into <run>/suites/<id>/, and the copy is re-based into the port range: by its
# port-base env where it has one, else by shifting its port literals. Demos written before sov-006's
# sovereignty-mode switch get the same two changes regress.sh already makes for SOV-004: the mode
# on and the ingest token on their POSTs. Every change is listed in README.md.
#
#   ./verify-all.sh                  # one PASS/FAIL line per suite, then RESULT PASS|FAIL
#   ONLY="enrol nodes-file" ./verify-all.sh
#   WT=/path/to/checkout ./verify-all.sh
#   SOV010_PORT_BASE=32400 ./verify-all.sh    # ports 32400-32499
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
S0_BASE=d5278a2  # rnd/sov-s0 as SOV-010 left it
if [[ -z "${WT:-}" ]]; then
  WT=
  here_wt=$(git rev-parse --show-toplevel 2>/dev/null) || here_wt=
  if [[ -n "$here_wt" && "$(git -C "$here_wt" rev-parse --abbrev-ref HEAD)" == rnd/sov-* ]]; then WT=$here_wt; fi
fi
RND=${RND:?set RND to the directory holding the sov-NNN demo repos}
[[ -n "$WT" ]] || { echo "set WT to a checkout on a rnd/sov-* branch" >&2; exit 1; }
LO=${SOV010_PORT_BASE:-32300}
[[ "$LO" =~ ^[0-9]+$ && $LO -ge 30000 && $LO -le 39900 ]] || { echo "SOV010_PORT_BASE must be 30000-39900" >&2; exit 1; }
HI=$((LO + 99))
BRANCH=$(git -C "$WT" rev-parse --abbrev-ref HEAD)
if [[ -n "${SOV010_RUN:-}" ]]; then RUNDIR=$SOV010_RUN
elif [[ "$BRANCH" =~ ^rnd/(sov-[0-9]+)$ ]]; then RUNDIR=$RND/${BASH_REMATCH[1]}/run/verify-all
else RUNDIR=$HERE/run; fi
PICK=${ONLY:-}
unset ONLY  # regress.sh reads its own ONLY
STAMP=$(date +%Y%m%dT%H%M%S)
OUT=$RUNDIR/verify-$STAMP
SUITES=$RUNDIR/suites
mkdir -p "$OUT" "$SUITES"
export CARGO_BUILD_JOBS=4

# the commits SOV-006/002/004/008/009/007 were accepted at
declare -A PIN=([sov-006]=59f7d51 [sov-002]=e57a828 [sov-004]=9c6741f [sov-008]=5737c30 [sov-009]=4e596f8 [sov-007]=545bb65)

busy() {  # ports in [$1,$2] something listens on
  ss -Hltn | awk '{print $4}' | sed 's/.*://' | awk -v lo="$1" -v hi="$2" '$1>=lo && $1<=hi' | sort -u | tr '\n' ' '
}

b=$(busy $LO $HI)
[[ -z "$b" ]] || { echo "ports in $LO-$HI already in use: $b; refusing to start" >&2; exit 1; }
git -C "$WT" merge-base --is-ancestor $S0_BASE HEAD 2>/dev/null || { echo "$WT ($BRANCH) does not contain rnd/sov-s0 $S0_BASE" >&2; exit 1; }
echo "== $BRANCH $(git -C "$WT" rev-parse --short HEAD)$(git -C "$WT" diff --quiet HEAD || echo ' (dirty)'), ports $LO-$HI, logs $OUT"

# stage <id>: a fresh copy of the demo repo at its pinned commit (tracked files overwritten, run/ kept)
stage() {
  local id=$1 dst=$SUITES/$1
  mkdir -p "$dst"
  git -C "$RND/$id" archive "${PIN[$id]}" | tar -x -C "$dst"
  echo "$dst"
}

# shift_ports <dir> <lo> <hi>: every 5-digit literal in [lo,hi] in the copy's .sh/.py moves to LO + (n - lo)
shift_ports() {
  local dir=$1 lo=$2 hi=$3
  find "$dir" -maxdepth 2 \( -name '*.sh' -o -name '*.py' \) -not -path "$dir/run/*" -print0 |
    xargs -0 perl -pi -e "s/\\b(3\\d{4})\\b/(\$1>=$lo && \$1<=$hi) ? \$1-$lo+$LO : \$1/ge"
}

# must <file> <text>...: the copy was patched as intended
must() {
  local f=$1 w; shift
  for w in "$@"; do grep -qF -- "$w" "$f" || { echo "could not patch $f ($w)" >&2; return 1; }; done
}

# saw <log> <line>...: the suite printed each of these (a zero exit alone is not a pass)
saw() {
  local f=$1 w; shift
  for w in "$@"; do grep -qF -- "$w" "$f" || { echo "missing from the output: $w"; return 1; }; done
}

# a POST helper that carries primed's ingest token (the same sed regress.sh uses)
with_token() {  # with_token <file.py>
  sed -i 's|Request(STATS + path, json.dumps(body).encode(), {"Content-Type": "application/json"})|Request(STATS + path, json.dumps(body).encode(), {"Content-Type": "application/json", "X-Sovereignty-Token": os.environ["SOV_TOKEN"]})|' "$1"
  must "$1" X-Sovereignty-Token
}

# sov012 <demo copy> <log>: the before/after lines add-resolved.py made its harnesses print (SOV-012),
# from this run's logs only
sov012() {
  find "$1/run" -name '*.log' -newer "$OUT/.start" -print0 2>/dev/null | xargs -0 -r grep -h '^SOV012' | sort | uniq \
    | sed 's/^/  /' | tee -a "$OUT/sov012.txt"
}

# ---------------------------------------------------------------------------------------- suites
suite_cargo_prime() {
  (cd "$WT/prime" && nice -n 10 cargo test -j4 --release 2>&1) | tee "$1.full" | grep -E '^test result|FAILED|panicked'
  local rc=${PIPESTATUS[0]}
  awk '/^test result/ {p+=$4; f+=$6} END {print "prime: " p " passed, " f " failed"}' "$1.full"
  return "$rc"
}

suite_cargo_lazarus() {
  (cd "$WT/lazarus" && nice -n 10 cargo test -j4 --release 2>&1) | tee "$1.full" | grep -E '^test result|FAILED|panicked'
  local rc=${PIPESTATUS[0]}
  awk '/^test result/ {p+=$4; f+=$6} END {print "lazarus: " p " passed, " f " failed"}' "$1.full"
  [[ $rc == 0 ]] || return "$rc"
  (cd "$WT/lazarus/canary" && python3 -B -m unittest test_lazarus_canary 2>&1)
}

suite_bench() {
  local d; d=$(stage sov-006)
  PRIME=$WT/prime bash "$d/bench.sh" || return
  saw "$1" 'BENCH PASS'
}

suite_regress() {
  local d; d=$(stage sov-006)
  shift_ports "$d" 31900 31999
  must "$d/regress.sh" "busy $LO $HI" "local BASE=$LO" "STATS_PORT=$((LO + 66))" "rpc = \"http://127.0.0.1:$((LO + 98))\"" || return 1
  # its harness copies (XBT-071, SOV-004) also post each snapshot as a template check (add-checks.py),
  # and post canary settlements with a before/after read (add-resolved.py, SOV-012)
  perl -pi -e 's|^(  grep -q X-Sovereignty-Token "\$2")|  python3 "\$ADD_CHECKS" "\$2" \|\| return 1\n  python3 "\$ADD_RESOLVED" "\$2" \|\| return 1\n$1|' "$d/regress.sh"
  must "$d/regress.sh" 'python3 "$ADD_CHECKS" "$2"' 'python3 "$ADD_RESOLVED" "$2"' || return 1
  export ADD_CHECKS=$HERE/add-checks.py ADD_RESOLVED=$HERE/add-resolved.py
  # blocks 30-50 s apart for W8's 12 s canary due point (see add-checks.py), and runs long enough
  # to hold ~10 due canaries per gateway
  XBT071_GAP_MIN=30 XBT071_GAP_MAX=50 HYBRID_SECS=${HYBRID_SECS:-300} \
    PRIME=$WT/prime HYBRID_RUNS=${HYBRID_RUNS:-1} bash "$d/regress.sh"
  local rc=$?
  sov012 "$d" "$1"
  [[ $rc == 0 ]] || return $rc
  saw "$1" '   off: PASS' '   xbt071: PASS' '   hybrid: PASS' 'RESULT PASS'
}

suite_nta() {
  local d; d=$(stage sov-002)
  # sovereignty-mode on (credit is an ingest POST) and the token on its POSTs
  perl -0pi -e 's|sovereignty-demo = true\n|sovereignty-mode = "observe"\nsovereignty-demo = true\n|' "$d/sov002_demo.py"
  perl -0pi -e 's|\{"Content-Type": "application/json"\}\)|{"Content-Type": "application/json", "X-Sovereignty-Token": open(os.path.join(RUN, "prime", "sovereignty.token")).read().strip()})|' "$d/sov002_demo.py"
  must "$d/sov002_demo.py" 'sovereignty-mode = "observe"' 'X-Sovereignty-Token' || return 1
  WT=$WT SOV002_PORT_BASE=$LO bash "$d/nta-signing-demo.sh" || return
  saw "$1" 'RESULT PASS' 'demo OK:'
}

suite_hybrid() {
  local d; d=$(stage sov-004)
  shift_ports "$d" 31700 31799
  with_token "$d/hybrid_knots.py" || return 1
  python3 "$HERE/add-checks.py" "$d/hybrid_knots.py" || return 1
  python3 "$HERE/add-resolved.py" "$d/hybrid_knots.py" || return 1
  # as regress.sh does: mode publish, the token exported once primed wrote it
  sed -i -e 's|^sovereignty-bonus = true|sovereignty-mode = "publish"\nsovereignty-bonus = true|' \
      -e 's|PRIMED_PID=\$!|PRIMED_PID=$!; for _ in $(seq 1 50); do [[ -s "$dir/sovereignty.token" ]] \&\& break; sleep 0.1; done; export SOV_TOKEN=$(cat "$dir/sovereignty.token")|' \
      -e 's|headline = "SOV-004 rnd/sov-004"|headline = "SOV-010 verify-all: SOV-004 hybrid"|' "$d/hybrid-canary-demo.sh"
  must "$d/hybrid-canary-demo.sh" "STATS_PORT=$((LO + 16))" "LISTEN_PORT=$((LO + 15))" "rpc = \"http://127.0.0.1:$((LO + 98))\"" \
    "\"\$KNOTS_SECS\" $((LO + 20))" 'sovereignty-mode = "publish"' 'SOV_TOKEN' || return 1
  PRIME=$WT/prime RUNS=${HYBRID_STANDALONE_RUNS:-2} KNOTS_SECS=${HYBRID_SECS:-300} bash "$d/hybrid-canary-demo.sh"
  local rc=$?
  sov012 "$d" "$1"
  [[ $rc == 0 ]] || return $rc
  saw "$1" 'RESULT PASS'
}

suite_sidecar() {
  local d; d=$(stage sov-008)
  shift_ports "$d" 32000 32099
  must "$d/canary-sidecar-demo.sh" "LISTEN_PORT=$((LO + 15))" "STATS_PORT=$((LO + 16))" "BASE=$((LO + 20))" "\$1>=$LO && \$1<=$HI" || return 1
  # gwsim is built against the wire crate of the branch under test, not rnd/sov-008's
  sed -i "s|^datum-wire = { path = .*|datum-wire = { path = \"$WT/prime/wire\" }|" "$d/gwsim/Cargo.toml"
  must "$d/gwsim/Cargo.toml" "path = \"$WT/prime/wire\"" || return 1
  # gateways enrol (v1, with their nodes) and the sidecar reads sovereignty-nodes.json (sov008-enrol.py)
  # SOV-012: a sidecar that knows nodes_source reads them from primed's GET /sovereignty/nodes only
  local src=
  grep -q nodes_source "$WT/lazarus/canary/lazarus_canary.py" && src=prime
  python3 "$HERE/sov008-enrol.py" "$d" $src || return 1
  [[ -z "$src" ]] || must "$d/sidecar_knots.py" 'nodes_source = "prime"' || return 1
  WT=$WT bash "$d/canary-sidecar-demo.sh" || return
  saw "$1" 'RESULT PASS' 'sovereignty-nodes.json holds their nodes'
}

suite_enrol() {
  local d; d=$(stage sov-009)
  shift_ports "$d" 32100 32199
  must "$d/enrol_demo.py" "KNOTS_RPC, KNOTS_P2P = $LO, $((LO + 1))" "DEAD = $HI" || return 1
  must "$d/enrol-demo.sh" "busy $LO $HI" || return 1
  PRIME=$WT/prime bash "$d/enrol-demo.sh" || return
  saw "$1" 'RESULT PASS'
}

# The glue (SOV-010 §2): the sidecar reads what the enrol demo's primed wrote to
# sovereignty-nodes.json, through its own gateways(), and gets exactly the active keys' nodes.
suite_nodes_file() {
  local data; data=$(dirname "$(find "$SUITES/sov-009/run/enrol" -name sovereignty-nodes.json | head -1)")
  [[ -n "$data" && -f "$data/sovereignty-nodes.json" ]] || { echo "no sovereignty-nodes.json from the enrol suite"; return 1; }
  python3 -B - "$WT/lazarus/canary" "$data" <<'P'
import json, os, stat, sys
sys.path.insert(0, sys.argv[1])
import lazarus_canary as lc
data = sys.argv[2]
nf = os.path.join(data, "sovereignty-nodes.json")
mode = stat.S_IMODE(os.stat(nf).st_mode)
nodes = json.load(open(nf))
# sovereignty-keys.json: a list of key records, status "active" | "superseded" | "revoked" | "refused"
status = {k["g"].lower(): k.get("status", "active") for k in json.load(open(os.path.join(data, "sovereignty-keys.json")))}
s = lc.Canary.__new__(lc.Canary)
s.cfg = lc.merged({"nodes_file": nf, "nodes_source": "file"})
gws = s.gateways()
want = {g.lower(): v["node"] for g, v in nodes.items() if v["node"] != "none"}
got = {g: v["node"] for g, v in gws.items()}
inactive = sorted(g for g, st in status.items() if st != "active")
checks = [
    ("file mode 0600", mode == 0o600, oct(mode)),
    ("sidecar gateways == sovereignty-nodes.json", got == want, f"{len(got)} gateways"),
    ("at least one gateway to canary", len(got) >= 1, len(got)),
    ("every node belongs to an active key", all(status.get(g) == "active" for g in got), {g[:12]: status.get(g) for g in got}),
    ("superseded/revoked/refused keys are not canaried", bool(inactive) and not set(inactive) & set(got),
     {g[:12]: status[g] for g in inactive}),
]
ok = True
for name, good, detail in checks:
    print(("ok   " if good else "FAIL ") + name + f" ({detail})")
    ok &= bool(good)
sys.exit(0 if ok else 1)
P
}

suite_lzt1() {
  local d; d=$(stage sov-007)
  # Since sov-009 the old `LZT1 register G=…` route is demo-only and this demo runs with
  # sovereignty-demo off. gw-A registers the way SOV-009 left it: `lazarus-gateway enrol --json`
  # prints a G-signed v1 message from its persisted identity key, posted to /sovereignty/enrol.
  python3 - "$d/sov007_demo.py" <<'P' || return 1
import sys
p = sys.argv[1]
s = open(p).read()
old = """    r = post("/sovereignty/register", {"g": G_A, "payout": ADDR["A"], "house": False,
                                       "sig": G_A_KEY.sign(msg).hex()}, token)
"""
new = """    enrol = json.loads(subprocess.check_output([GATEWAY, "--config", os.path.join(RUN, "gw-A", "gateway.json"),
                                                "enrol", "--json", "--version", "v1", "--node", "none"], text=True))
    r = post("/sovereignty/enrol", enrol, token)
    print("enrol v1:", json.dumps(r)[:200])
"""
if s.count(old) != 1:
    sys.exit("could not patch the sov-007 registration")
open(p, "w").write(s.replace(old, new))
P
  WT=$WT SOV007_PORT_BASE=$LO bash "$d/lzt1-datum-demo.sh" || return
  saw "$1" 'RESULT PASS'
}

# ------------------------------------------------------------------------------------------ run
NAMES=(cargo-prime cargo-lazarus bench regress nta hybrid sidecar enrol nodes-file lzt1)
declare -A FN=([cargo-prime]=suite_cargo_prime [cargo-lazarus]=suite_cargo_lazarus [bench]=suite_bench
  [regress]=suite_regress [nta]=suite_nta [hybrid]=suite_hybrid [sidecar]=suite_sidecar [enrol]=suite_enrol
  [nodes-file]=suite_nodes_file [lzt1]=suite_lzt1)
declare -A WHAT=([cargo-prime]="cargo test prime" [cargo-lazarus]="cargo test lazarus + canary unittest"
  [bench]="sov-006 bench.sh" [regress]="sov-006 regress.sh (off, XBT-071, hybrid)" [nta]="sov-002 nta-signing-demo.sh"
  [hybrid]="sov-004 hybrid-canary-demo.sh" [sidecar]="sov-008 canary-sidecar-demo.sh" [enrol]="sov-009 enrol-demo.sh"
  [nodes-file]="sidecar reads sovereignty-nodes.json" [lzt1]="sov-007 lzt1-datum-demo.sh")
T0=$(date +%s)
touch "$OUT/.start"
FAILED=()
LINES=()
for n in "${NAMES[@]}"; do
  [[ -z "$PICK" || " $PICK " == *" $n "* ]] || continue
  log=$OUT/$n.log
  t0=$(date +%s)
  echo "-- $n: ${WHAT[$n]} (log $log)"
  "${FN[$n]}" "$log" > "$log" 2>&1
  rc=$?
  secs=$(( $(date +%s) - t0 ))
  left=$(busy $LO $HI)
  if [[ -n "$left" ]]; then echo "left listening after $n: $left" | tee -a "$log"; rc=97; fi
  if [[ $rc == 0 ]]; then v=PASS; else v=FAIL; FAILED+=("$n"); tail -15 "$log" | sed 's/^/   | /'; fi
  line=$(printf '%-4s %-12s %5ss  %s' "$v" "$n" "$secs" "${WHAT[$n]}")
  [[ $rc == 0 ]] || line+=" (rc=$rc)"
  echo "$line"
  LINES+=("$line")
done
total=$(( $(date +%s) - T0 ))
{
  echo "$BRANCH $(git -C "$WT" rev-parse HEAD), ports $LO-$HI"
  printf '%s\n' "${LINES[@]}"
  echo "total ${total}s"
} > "$OUT/summary.txt"
echo "== summary ($OUT/summary.txt)"
printf '%s\n' "${LINES[@]}"
echo "total ${total}s"
[[ -s "$OUT/sov012.txt" ]] && { echo "== SOV-012 own hit rates, before -> after fix 1"; cat "$OUT/sov012.txt"; }
if [[ ${#FAILED[@]} -eq 0 ]]; then echo "RESULT PASS"; else echo "RESULT FAIL: ${FAILED[*]}"; exit 1; fi
