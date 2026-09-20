#!/usr/bin/env python3
"""Lazarus hub monitor. Runs on AgentLaz from cron every 2 minutes and emails Mike via Proton SMTP.

External vantage on purpose: a dead hub cannot alert about itself. Checks the hub over Tailscale
SSH plus the public explorer and Electrum endpoints. Alerts once on a transition to bad, a reminder
every 6 h while still bad, and a recovery mail when it clears. State in ~/.local/state/hub-monitor/.

Usage: hub-monitor.py            normal run
       hub-monitor.py --test     send one test alert to prove the mail path, no checks
"""
import json, os, socket, ssl, subprocess, sys, time, urllib.request

HUB = "lazarus-hub"
STATE_DIR = os.path.expanduser("~/.local/state/hub-monitor")
STATE = os.path.join(STATE_DIR, "state.json")
REMIND_S = 6 * 3600
PROTONCTL = os.path.expanduser("~/proton-mail/protonctl.py")
LOG = os.path.join(STATE_DIR, "monitor.log")

# --- one SSH round trip collects everything from the hub -------------------------------------------
HUB_PROBE = r'''
set +e
M="sudo -u btc-mining /opt/knots/current/bin/bitcoin-cli -datadir=/srv/node-mining/bitcoin -rpcport=9332"
echo "MOUNTED=$(mountpoint -q /srv/pool && echo 1 || echo 0)"
for u in lazarus-prime lazarus-gateway-asic lazarus-gateway-gpu lazarus-gateway-solo lazarus-gateway-solo-gpu lazarus-pool-writer lazarus-pool-reader@1 lazarus-pool-reader@2 bitcoind-mining bitcoind-services electrs mempool-backend cloudflared hub-autounlock fee-wallet.timer; do echo "SVC_$u=$(systemctl is-active $u)"; done
echo "HEIGHT=$($M getblockcount 2>/dev/null)"
echo "TIPAGE=$(( $(date +%s) - $($M getblockheader $($M getbestblockhash 2>/dev/null) 2>/dev/null | python3 -c 'import sys,json;print(json.load(sys.stdin)["time"])' 2>/dev/null || echo 0) ))"
echo "PEERS=$($M getconnectioncount 2>/dev/null)"
G=$(curl -s -m 5 http://127.0.0.1:7152/); echo "GW_PUB=$(echo "$G" | grep -oE 'Published height: [0-9]+' | grep -oE '[0-9]+')"
echo "GW_ACC=$(echo "$G" | grep -oE 'Local Shares Accepted: [0-9]+' | grep -oE '[0-9]+')"; echo "GW_REJ=$(echo "$G" | grep -oE 'Local Shares Rejected: [0-9]+' | grep -oE '[0-9]+')"
P=$(curl -s -m 5 http://127.0.0.1:28916/stats.json); echo "PRIME_GW=$(echo "$P" | python3 -c 'import sys,json;print(json.load(sys.stdin)["gateways"])' 2>/dev/null)"
echo "PRIME_GHS=$(echo "$P" | python3 -c 'import sys,json;print(int(json.load(sys.stdin)["hashrate"]["pool_ghs"]))' 2>/dev/null)"
echo "FEEWALLET_LAST=$(systemctl show fee-wallet.service -p ExecMainStatus --value 2>/dev/null)"
echo "FEEWALLET_AGE=$(( $(date +%s) - $(date -d "$(systemctl show fee-wallet.service -p ExecMainExitTimestamp --value 2>/dev/null)" +%s 2>/dev/null || echo 0) ))"
echo "SITE_LOCAL=$(curl -s -m 5 -o /dev/null -w '%{http_code}' http://127.0.0.1:8889/api/pool)"
echo "RAID_BAD=$(grep -c -E '\[U_\]|\[_U\]|\[_\]' /proc/mdstat)"
echo "DISK_MAX=$(df --output=pcent /srv/pool /srv/node-mining /srv/node-services / 2>/dev/null | tail -n +2 | tr -dc '0-9\n' | sort -n | tail -1)"
echo "SMART_BAD=$(for d in /dev/nvme[0-7]n1; do sudo smartctl -H $d 2>/dev/null | grep -c -i 'FAILED'; done | paste -sd+ | bc)"
'''

def sh(cmd, timeout=60):
    try:
        return subprocess.run(cmd, shell=True, capture_output=True, text=True, timeout=timeout).stdout
    except Exception:
        return ""

def hub_probe():
    out = sh(f"ssh -o BatchMode=yes -o ConnectTimeout=15 {HUB} 'bash -s' <<'EOF'\n{HUB_PROBE}\nEOF", timeout=90)
    kv = {}
    for line in out.splitlines():
        if "=" in line:
            k, v = line.split("=", 1); kv[k.strip()] = v.strip()
    return kv

def home_height():
    out = sh("""/home/mike/lazarus-ops/pool-ssh -c 'D=~/umbrel/app-data/bitcoin-knots/data/bitcoin; curl -s --user "$(cat $D/.cookie)" -H "content-type:text/plain" --data-binary "{\\"jsonrpc\\":\\"1.0\\",\\"id\\":1,\\"method\\":\\"getblockcount\\",\\"params\\":[]}" http://127.0.0.1:9332/' 2>/dev/null""", 40)
    try: return int(json.loads(out)["result"])
    except Exception: return None

def site_ok():
    for _ in range(2):
        try:
            return urllib.request.urlopen(urllib.request.Request("https://pool.awokenlazarus.xyz/api/pool", headers={"User-Agent": "lazarus-hub-monitor"}), timeout=20).status == 200
        except Exception: pass
    return False

def stratum_ok():
    import socket
    for _ in range(2):
        try:
            with socket.create_connection(("103.219.170.129", 23334), timeout=8): return True
        except Exception: pass
    return False

def explorer_tip():
    # two attempts: one slow edge response must not count as an outage
    for _ in range(2):
        try:
            return int(urllib.request.urlopen(urllib.request.Request("https://mempool.awokenlazarus.xyz/api/v1/blocks/tip/height", headers={"User-Agent": "lazarus-hub-monitor/1.0"}), timeout=15).read())
        except Exception: time.sleep(5)
    return None

def electrum_ok():
    for _ in range(2):
        if _electrum_once(): return True
        time.sleep(5)
    return False

def _electrum_once():
    try:
        ctx = ssl.create_default_context()
        s = ctx.wrap_socket(socket.create_connection(("electrum.awokenlazarus.xyz", 50002), timeout=12), server_hostname="electrum.awokenlazarus.xyz")
        f = s.makefile("rwb"); f.write(b'{"id":0,"method":"server.version","params":["hub-monitor","1.8"]}\n'); f.flush()
        return "electrs" in f.readline().decode()
    except Exception: return False

def checks():
    """Return {check_name: (ok: bool, detail: str)}."""
    r = {}
    kv = hub_probe()
    if not kv:
        r["hub_reachable"] = (False, "no answer over Tailscale SSH")
        # still check the public faces so the mail says what visitors see
        r["explorer_public"] = (explorer_tip() is not None, "public explorer API")
        r["electrum_public"] = (electrum_ok(), "public Electrum TLS")
        return r
    r["hub_reachable"] = (True, "")
    r["pool_volume_mounted"] = (kv.get("MOUNTED") == "1", "/srv/pool not mounted (Tang unlock failed?)")
    for u in ["lazarus-prime", "lazarus-gateway-asic", "lazarus-gateway-gpu", "lazarus-gateway-solo", "lazarus-gateway-solo-gpu", "lazarus-pool-writer", "lazarus-pool-reader@1", "lazarus-pool-reader@2", "bitcoind-mining", "bitcoind-services", "electrs", "mempool-backend", "cloudflared", "fee-wallet.timer"]:
        st = kv.get(f"SVC_{u}", "?"); r[f"svc_{u}"] = (st == "active", f"{u} is {st}")
    try: tipage = int(kv.get("TIPAGE", "0"))
    except ValueError: tipage = 0
    r["tip_age"] = (0 < tipage < 1800, f"node tip is {tipage//60} min old (chain stalled or node stuck?)")
    try: peers = int(kv.get("PEERS", "0"))
    except ValueError: peers = 0
    r["peers"] = (peers >= 8, f"only {peers} peers on the mining node")
    try:
        h, pub = int(kv.get("HEIGHT", "0")), int(kv.get("GW_PUB", "0"))
        r["template_fresh"] = (pub >= h + 1 or pub == 0, f"gateway template at {pub} but node at {h} (stale template)")
    except ValueError:
        r["template_fresh"] = (False, "could not read heights")
    try:
        acc, rej = int(kv.get("GW_ACC", "0")), int(kv.get("GW_REJ", "0"))
        prev = load_state().get("counters", {})
        dacc, drej = acc - prev.get("acc", acc), rej - prev.get("rej", rej)
        rate = drej / (dacc + drej) if (dacc + drej) > 20 else 0.0
        r["reject_rate"] = (rate < 0.05, f"reject rate {rate*100:.1f}% over the last interval ({drej}/{dacc+drej})")
        save_counters({"acc": acc, "rej": rej})
    except ValueError: pass
    try: gws = int(kv.get("PRIME_GW", "0"))
    except ValueError: gws = 0
    r["prime_gateways"] = (gws >= 120, f"only {gws} DATUM gateways on prime (was ~240)")
    try: ghs = int(kv.get("PRIME_GHS", "0"))
    except ValueError: ghs = 0
    r["pool_hashrate"] = (ghs >= 4_000_000, f"pool hashrate {ghs/1e6:.2f} PH/s (was ~10)")
    try:
        fw_age = int(kv.get("FEEWALLET_AGE", "0")); fw_st = kv.get("FEEWALLET_LAST", "0")
        r["fee_wallet_runs"] = (fw_st == "0" and fw_age < 900, f"fee-wallet last run status {fw_st}, {fw_age//60} min ago")
    except ValueError: pass
    r["site_backend"] = (kv.get("SITE_LOCAL") == "200", f"pool site reader answered {kv.get('SITE_LOCAL')}")
    r["site_public"] = (site_ok(), "public pool site API not answering")
    r["stratum_public"] = (stratum_ok(), "public stratum port 23334 not accepting connections")
    r["raid"] = (kv.get("RAID_BAD", "0") == "0", "a RAID mirror is degraded")
    try: r["disk"] = (int(kv.get("DISK_MAX", "0")) < 85, f"a filesystem is {kv.get('DISK_MAX')}% full")
    except ValueError: pass
    r["smart"] = (kv.get("SMART_BAD", "0") in ("0", ""), "a drive reports SMART FAILED")
    hh = home_height()
    try:
        if hh is not None: r["height_vs_home"] = (abs(int(kv.get("HEIGHT", "0")) - hh) <= 2, f"hub at {kv.get('HEIGHT')} vs home at {hh} (fork or stall)")
    except ValueError: pass
    et = explorer_tip(); r["explorer_public"] = (et is not None, "public explorer API not answering")
    try:
        if et is not None: r["explorer_current"] = (abs(et - int(kv.get("HEIGHT", "0"))) <= 2, f"explorer shows {et}, node at {kv.get('HEIGHT')}")
    except ValueError: pass
    r["electrum_public"] = (electrum_ok(), "public Electrum TLS not answering")
    return r

def load_state():
    try: return json.load(open(STATE))
    except Exception: return {}

def save_state(s):
    os.makedirs(STATE_DIR, exist_ok=True); json.dump(s, open(STATE, "w"))

def save_counters(c):
    s = load_state(); s["counters"] = c; save_state(s)

def send(subject, body):
    env = {}
    for line in open(os.path.expanduser("~/.config/proton-mail/env")):
        line = line.strip()
        if line and not line.startswith("#") and "=" in line:
            k, v = line.split("=", 1); env[k] = v
    to = env.get("PROTON_DEFAULT_TO", "")
    if not to: return False
    r = subprocess.run([sys.executable, PROTONCTL, "send", "--to", to, "--subject", subject, "--body", body], capture_output=True, text=True, timeout=60)
    with open(LOG, "a") as f: f.write(f"{time.strftime('%FT%TZ', time.gmtime())} mail rc={r.returncode} {subject}\n")
    return r.returncode == 0

def main():
    os.makedirs(STATE_DIR, exist_ok=True)
    if "--test" in sys.argv:
        ok = send("[lazarus-hub] TEST alert", "This is a test from hub-monitor on AgentLaz.\nIf you can read this, hub alerts reach you.\n\nChecks: hub reachable, /srv/pool unlocked, 8 services, tip age, peers, template freshness, reject rate, RAID, disk, SMART, hub vs home height, public explorer, public Electrum.")
        print("test mail sent" if ok else "test mail FAILED"); return
    res = checks(); st = load_state(); bad = st.get("bad", {}); now = int(time.time())
    newly_bad, still_bad, recovered = [], [], []
    for name, (ok, detail) in res.items():
        if not ok:
            if name not in bad: bad[name] = {"since": now, "last_mail": 0, "detail": detail}; newly_bad.append((name, detail))
            elif now - bad[name]["last_mail"] >= REMIND_S: still_bad.append((name, detail, bad[name]["since"]))
        elif name in bad:
            recovered.append((name, bad[name]["since"])); del bad[name]
    if newly_bad or still_bad:
        lines = [f"Hub check at {time.strftime('%FT%TZ', time.gmtime())}", ""]
        if newly_bad: lines += ["NEW PROBLEMS:"] + [f"  - {n}: {d}" for n, d in newly_bad] + [""]
        if still_bad: lines += ["STILL BAD (reminder):"] + [f"  - {n}: {d} (since {time.strftime('%FT%TZ', time.gmtime(s))})" for n, d, s in still_bad] + [""]
        lines += ["All current problems: " + ", ".join(sorted(bad)) if bad else "", "", "Manual unlock if needed: cat ~/.config/lazarus-hub/luks-pool.key | ssh lazarus-hub sudo hub-unlock"]
        subj = "[lazarus-hub] " + ("ALERT: " + ", ".join(n for n, _ in newly_bad) if newly_bad else "still bad: " + ", ".join(n for n, _, _ in still_bad))
        if send(subj[:120], "\n".join(lines)):
            for n, _ in newly_bad: bad[n]["last_mail"] = now
            for n, _, _ in still_bad: bad[n]["last_mail"] = now
    if recovered:
        send("[lazarus-hub] recovered: " + ", ".join(n for n, _ in recovered)[:100], "Cleared at " + time.strftime("%FT%TZ", time.gmtime()) + ":\n" + "\n".join(f"  - {n} (was bad since {time.strftime('%FT%TZ', time.gmtime(s))})" for n, s in recovered) + ("\n\nStill bad: " + ", ".join(sorted(bad)) if bad else "\n\nAll checks green."))
    st["bad"] = bad; st["last_run"] = now; st["last_result"] = {k: v[0] for k, v in res.items()}; save_state(st)
    with open(LOG, "a") as f: f.write(f"{time.strftime('%FT%TZ', time.gmtime())} ok={sum(1 for v in res.values() if v[0])}/{len(res)} bad={sorted(bad)}\n")

if __name__ == "__main__":
    main()
