// BLAKE2b-256, 80-byte work (one compression). Matches libsodium/RFC 7693.
#define B2B_IV0 0x6a09e667f3bcc908UL
#define B2B_IV1 0xbb67ae8584caa73bUL
#define B2B_IV2 0x3c6ef372fe94f82bUL
#define B2B_IV3 0xa54ff53a5f1d36f1UL
#define B2B_IV4 0x510e527fade682d1UL
#define B2B_IV5 0x9b05688c2b3e6c1fUL
#define B2B_IV6 0x1f83d9abfb41bd6bUL
#define B2B_IV7 0x5be0cd19137e2179UL

static inline ulong rotr64(ulong x, uint n) { return (x >> n) | (x << (64 - n)); }

#define G(a,b,c,d,x,y) \
    do { \
        a = a + b + (x); \
        d = rotr64(d ^ a, 32); \
        c = c + d; \
        b = rotr64(b ^ c, 24); \
        a = a + b + (y); \
        d = rotr64(d ^ a, 16); \
        c = c + d; \
        b = rotr64(b ^ c, 63); \
    } while (0)

__constant uchar SIGMA[12][16] = {
    { 0, 1, 2, 3, 4, 5, 6, 7, 8, 9,10,11,12,13,14,15 },
    {14,10, 4, 8, 9,15,13, 6, 1,12, 0, 2,11, 7, 5, 3 },
    {11, 8,12, 0, 5, 2,15,13,10,14, 3, 6, 7, 1, 9, 4 },
    { 7, 9, 3, 1,13,12,11,14, 2, 6, 5,10, 4, 0,15, 8 },
    { 9, 0, 5, 7, 2, 4,10,15,14, 1,11,12, 6, 8, 3,13 },
    { 2,12, 6,10, 0,11, 8, 3, 4,13, 7, 5,15,14, 1, 9 },
    {12, 5, 1,15,14,13, 4,10, 0, 7, 6, 3, 9, 2, 8,11 },
    {13,11, 7,14,12, 1, 3, 9, 5, 0,15, 4, 8, 6, 2,10 },
    { 6,15,14, 9,11, 3, 0, 8,12, 2,13, 7, 1, 4,10, 5 },
    {10, 2, 8, 4, 7, 6, 1, 5,15,11, 9,14, 3,12,13, 0 },
    { 0, 1, 2, 3, 4, 5, 6, 7, 8, 9,10,11,12,13,14,15 },
    {14,10, 4, 8, 9,15,13, 6, 1,12, 0, 2,11, 7, 5, 3 }
};

static void blake2b_80(const uchar *msg, uchar *out32) {
    ulong h[8] = { B2B_IV0 ^ 0x01010020UL, B2B_IV1, B2B_IV2, B2B_IV3,
                   B2B_IV4, B2B_IV5, B2B_IV6, B2B_IV7 };
    ulong m[16];
    ulong v[16];
    #pragma unroll
    for (int i = 0; i < 10; i++) {
        ulong x = 0;
        #pragma unroll
        for (int b = 0; b < 8; b++) x |= ((ulong)msg[i * 8 + b]) << (8 * b);
        m[i] = x;
    }
    m[10] = 0; m[11] = 0; m[12] = 0; m[13] = 0; m[14] = 0; m[15] = 0;

    #pragma unroll
    for (int i = 0; i < 8; i++) v[i] = h[i];
    v[8] = B2B_IV0; v[9] = B2B_IV1; v[10] = B2B_IV2; v[11] = B2B_IV3;
    v[12] = B2B_IV4 ^ 80UL; // t0 = 80
    v[13] = B2B_IV5;
    v[14] = B2B_IV6 ^ 0xFFFFFFFFFFFFFFFFUL; // last block
    v[15] = B2B_IV7;

    #pragma unroll
    for (int r = 0; r < 12; r++) {
        __constant uchar *s = SIGMA[r];
        G(v[0], v[4], v[8],  v[12], m[s[0]],  m[s[1]]);
        G(v[1], v[5], v[9],  v[13], m[s[2]],  m[s[3]]);
        G(v[2], v[6], v[10], v[14], m[s[4]],  m[s[5]]);
        G(v[3], v[7], v[11], v[15], m[s[6]],  m[s[7]]);
        G(v[0], v[5], v[10], v[15], m[s[8]],  m[s[9]]);
        G(v[1], v[6], v[11], v[12], m[s[10]], m[s[11]]);
        G(v[2], v[7], v[8],  v[13], m[s[12]], m[s[13]]);
        G(v[3], v[4], v[9],  v[14], m[s[14]], m[s[15]]);
    }
    #pragma unroll
    for (int i = 0; i < 8; i++) h[i] ^= v[i] ^ v[i + 8];
    #pragma unroll
    for (int i = 0; i < 4; i++) {
        ulong x = h[i];
        #pragma unroll
        for (int b = 0; b < 8; b++) out32[i * 8 + b] = (uchar)(x >> (8 * b));
    }
}

__kernel void mine(
    __global const uchar *work_template, // 80 bytes, nonce at [32:40)
    __global const uchar *target,        // 32-byte LE target (MSB at [31])
    const ulong nonce_base,
    const uint  max_hits,
    __global ulong *hits,                // [0]=count, then nonce values
    __global ulong *hashes_done
) {
    ulong gid = (ulong)get_global_id(0);
    uchar work[80];
    uchar hash[32];
    #pragma unroll
    for (int i = 0; i < 80; i++) work[i] = work_template[i];
    ulong nonce = nonce_base + gid;
    #pragma unroll
    for (int b = 0; b < 8; b++) work[32 + b] = (uchar)(nonce >> (8 * b));
    blake2b_80(work, hash);

    // hash_le[i] = hash[31-i]; compare from byte 31 downward vs target
    int ok = 1;
    #pragma unroll
    for (int i = 31; i >= 0; i--) {
        uchar hv = hash[31 - i];
        uchar tv = target[i];
        if (hv < tv) { ok = 1; break; }
        if (hv > tv) { ok = 0; break; }
    }
    if (ok) {
        uint slot = (uint)atomic_inc((__global volatile uint *)hits);
        if (slot < max_hits) hits[1 + slot] = nonce;
    }
    if (gid == 0) hashes_done[0] = nonce_base + (ulong)get_global_size(0);
}
