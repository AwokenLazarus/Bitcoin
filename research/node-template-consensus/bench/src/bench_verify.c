/* SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Mike Moore (AwokenLazarus)
 *
 * Time secp256k1_schnorrsig_verify from a Knots build.
 *
 * Links libsecp256k1 (MIT). src/bench has no Schnorr/BIP340 bench, and
 * BUILD_BENCH is off on the tree this was written against, so this calls the
 * same static library bitcoind links, the way ContextualCheckBlock does
 * (32-byte message).
 *
 * Prints one JSON object on stdout. The accumulator is printed so the verifies
 * cannot be deleted. Signing is outside the timed loops.
 * NTA_BENCH_RUNS (default 5) is the run count. bench.sh --quick sets it to 1.
 */
#include <secp256k1.h>
#include <secp256k1_extrakeys.h>
#include <secp256k1_schnorrsig.h>

#include <stdlib.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <time.h>

static int bench_runs(void)
{
    const char *e = getenv("NTA_BENCH_RUNS");
    if (e == NULL || e[0] == '\0') return 5;
    int n = atoi(e);
    if (n < 1) {
        fprintf(stderr, "bench_verify: NTA_BENCH_RUNS must be >= 1\n");
        exit(1);
    }
    return n;
}

static uint64_t nsec(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ull + (uint64_t)ts.tv_nsec;
}

static void batch(const secp256k1_context *ctx, const unsigned char *sig,
                  const unsigned char *msg, const secp256k1_xonly_pubkey *pk,
                  int n, int runs, int *acc)
{
    printf("  \"batch_%d\": [\n", n);
    for (int r = 0; r < runs; r++) {
        uint64_t t0 = nsec();
        for (int i = 0; i < n; i++) {
            *acc += secp256k1_schnorrsig_verify(ctx, sig, msg, 32, pk);
        }
        uint64_t dt = nsec() - t0;
        printf("    {\"run\": %d, \"elapsed_ns\": %llu, \"ns_per_op\": %.1f}%s\n",
               r, (unsigned long long)dt, (double)dt / (double)n,
               r + 1 == runs ? "" : ",");
    }
    printf("  ]");
}

int main(void)
{
    const int runs = bench_runs();
    const int iters = 4000;
    const int warmup = 2000;

    secp256k1_context *ctx = secp256k1_context_create(SECP256K1_CONTEXT_SIGN | SECP256K1_CONTEXT_VERIFY);
    if (!ctx) return 1;

    unsigned char sk[32];
    memset(sk, 0x11, sizeof(sk));
    sk[0] = 1;
    secp256k1_keypair kp;
    if (!secp256k1_keypair_create(ctx, &kp, sk)) return 1;

    secp256k1_xonly_pubkey pk;
    if (!secp256k1_keypair_xonly_pub(ctx, &pk, NULL, &kp)) return 1;

    unsigned char msg[32];
    memset(msg, 0x42, sizeof(msg));
    unsigned char sig[64];
    if (!secp256k1_schnorrsig_sign32(ctx, sig, msg, &kp, NULL)) return 1;
    if (!secp256k1_schnorrsig_verify(ctx, sig, msg, 32, &pk)) return 1;

    int acc = 0;
    for (int i = 0; i < warmup; i++) {
        acc += secp256k1_schnorrsig_verify(ctx, sig, msg, 32, &pk);
    }

    printf("{\n");
    printf("  \"op\": \"secp256k1_schnorrsig_verify\",\n");
    printf("  \"message_len\": 32,\n");
    printf("  \"iters_per_run\": %d,\n", iters);
    printf("  \"warmup\": %d,\n", warmup);
    printf("  \"runs\": [\n");
    for (int r = 0; r < runs; r++) {
        uint64_t t0 = nsec();
        for (int i = 0; i < iters; i++) {
            acc += secp256k1_schnorrsig_verify(ctx, sig, msg, 32, &pk);
        }
        uint64_t dt = nsec() - t0;
        printf("    {\"run\": %d, \"elapsed_ns\": %llu, \"ns_per_op\": %.1f}%s\n",
               r, (unsigned long long)dt, (double)dt / (double)iters,
               r + 1 == runs ? "" : ",");
    }
    printf("  ],\n");
    /* The old "~0.4 s" and "~26 ms" lines were 8000 and 512 times one verify. */
    batch(ctx, sig, msg, &pk, 512, runs, &acc);
    printf(",\n");
    batch(ctx, sig, msg, &pk, 8000, runs, &acc);
    printf(",\n");
    printf("  \"acc\": %d\n", acc);
    printf("}\n");

    secp256k1_context_destroy(ctx);
    return acc > 0 ? 0 : 2;
}
