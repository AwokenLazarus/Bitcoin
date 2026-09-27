# Last full verify

```
rnd/sov-s0 d5278a208d60b3b2c84096b34227d0064c08ba17
PASS cargo-prime     16s  cargo test prime
PASS cargo-lazarus     2s  cargo test lazarus + canary unittest
PASS bench           10s  sov-006 bench.sh
PASS regress        810s  sov-006 regress.sh (off, XBT-071, hybrid)
PASS nta            660s  sov-002 nta-signing-demo.sh
PASS hybrid         613s  sov-004 hybrid-canary-demo.sh
PASS sidecar        482s  sov-008 canary-sidecar-demo.sh
PASS enrol           13s  sov-009 enrol-demo.sh
PASS nodes-file       0s  sidecar reads sovereignty-nodes.json
PASS lzt1           181s  sov-007 lzt1-datum-demo.sh
total 2787s
RESULT PASS
miners_left 32300-32399: none
```
