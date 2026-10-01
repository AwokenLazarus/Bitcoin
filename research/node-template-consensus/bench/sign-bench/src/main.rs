// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Mike Moore (AwokenLazarus)

//! Time `NtaAttestationHash` + BIP340 sign the way the gateway does.
//!
//! `Signer::sign` is `attestation_hash` (the Knots `NtaAttestationHash`) plus
//! `sign_schnorr_no_aux_rand`, including the `Secp256k1::signing_only()` the
//! gateway's `Signer::attest` pays on every tip. Aux randomness is zero, matching
//! the gateway and the Knots test signer.
//!
//! `NTA_BENCH_RUNS` (default 5) is the run count. `bench.sh --quick` sets it to 1.

use std::hint::black_box;
use std::time::Instant;

use lazarus_protocol::nta::Signer;

fn bench_runs() -> usize {
    let Ok(raw) = std::env::var("NTA_BENCH_RUNS") else {
        return 5;
    };
    let n: usize = raw.parse().expect("NTA_BENCH_RUNS must be an integer");
    assert!(n >= 1, "NTA_BENCH_RUNS must be >= 1");
    n
}

fn main() {
    let secret = [0x11u8; 32];
    let signer = Signer::from_secret(&secret).expect("constant 32-byte test key");
    let prev = [0x22u8; 32];
    let nbits = 0x207f_ffffu32;

    for i in 0..500u32 {
        black_box(signer.sign(black_box(120 + i), black_box(nbits), black_box(&prev)));
    }

    let runs = bench_runs();
    let iters = 2000u32;
    println!("{{");
    println!("  \"op\": \"Signer::sign (NtaAttestationHash + BIP340, zero aux)\",");
    println!("  \"crate\": \"lazarus-protocol\",\n  \"iters_per_run\": {iters},");
    println!("  \"runs\": [");
    for r in 0..runs {
        let t0 = Instant::now();
        for i in 0..iters {
            black_box(signer.sign(
                black_box(10_000 + u32::try_from(r).expect("run index") * iters + i),
                black_box(nbits),
                black_box(&prev),
            ));
        }
        let elapsed = t0.elapsed().as_nanos();
        let ns = elapsed as f64 / f64::from(iters);
        let comma = if r + 1 == runs { "" } else { "," };
        println!("    {{\"run\": {r}, \"elapsed_ns\": {elapsed}, \"ns_per_op\": {ns:.1}}}{comma}");
    }
    println!("  ]");
    println!("}}");
}
