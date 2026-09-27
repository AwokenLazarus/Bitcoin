# Results

Only regtest runs and public chain data.

| Path | What |
|---|---|
| `score-7d.md` / `.json`, `score-1d.md` / `.json` | sovereignty score over the real XBT chain, blocks 972,713–974,019 (7 days) and the last 24 h, from the public explorer on 2026-09-25 |
| `demo-full.log` | the full `tools/demo.sh` run quoted in [design.md](../design.md) |
| `entropy_result.md` / `.json` | a later 180 s detector run (within the ranges the design quotes) |
| `rdts_opreturn_result.json`, `rdts_opreturn_result-rdtsexpiry.json` | the RDTS `OP_RETURN` size test with RDTS off and on |
| `sov-004/` | hybrid-proxy canary demo, 3 × 120 s: `verify-evidence.log` and each run's JSON |
| `sov-015/` | foreign-canary demo, 3 × 420 s, and the relay-race probe: `verify-evidence.log`, `race-full-evidence.log`, JSON |
| `sov-010/VERIFIED.md` | the last full S0 verify (10 suites, `RESULT PASS`) |
