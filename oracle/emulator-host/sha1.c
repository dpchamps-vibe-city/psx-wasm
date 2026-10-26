/* Minimal SHA-1 (FIPS 180-4) for hashing the BIOS image into provenance. */

#include "sha1.h"

#include <string.h>

static uint32_t rol(uint32_t x, int n) { return (x << n) | (x >> (32 - n)); }

static void block(uint32_t h[5], const uint8_t *p) {
    uint32_t w[80];
    for (int i = 0; i < 16; i++)
        w[i] = (uint32_t)p[4 * i] << 24 | (uint32_t)p[4 * i + 1] << 16 | (uint32_t)p[4 * i + 2] << 8 | p[4 * i + 3];
    for (int i = 16; i < 80; i++) w[i] = rol(w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16], 1);
    uint32_t a = h[0], b = h[1], c = h[2], d = h[3], e = h[4];
    for (int i = 0; i < 80; i++) {
        uint32_t f, k;
        if (i < 20) {
            f = (b & c) | (~b & d);
            k = 0x5A827999;
        } else if (i < 40) {
            f = b ^ c ^ d;
            k = 0x6ED9EBA1;
        } else if (i < 60) {
            f = (b & c) | (b & d) | (c & d);
            k = 0x8F1BBCDC;
        } else {
            f = b ^ c ^ d;
            k = 0xCA62C1D6;
        }
        uint32_t t = rol(a, 5) + f + e + k + w[i];
        e = d;
        d = c;
        c = rol(b, 30);
        b = a;
        a = t;
    }
    h[0] += a;
    h[1] += b;
    h[2] += c;
    h[3] += d;
    h[4] += e;
}

void sha1_hex(const uint8_t *data, size_t len, char out[41]) {
    uint32_t h[5] = {0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0};
    size_t i = 0;
    for (; i + 64 <= len; i += 64) block(h, data + i);
    uint8_t tail[128];
    size_t rem = len - i;
    memset(tail, 0, sizeof tail);
    memcpy(tail, data + i, rem);
    tail[rem] = 0x80;
    size_t tlen = rem + 1 + 8 <= 64 ? 64 : 128;
    uint64_t bits = (uint64_t)len * 8;
    for (int j = 0; j < 8; j++) tail[tlen - 1 - j] = (uint8_t)(bits >> (8 * j));
    block(h, tail);
    if (tlen == 128) block(h, tail + 64);
    static const char hex[] = "0123456789abcdef";
    for (int j = 0; j < 20; j++) {
        uint8_t b = (uint8_t)(h[j / 4] >> (24 - 8 * (j % 4)));
        out[2 * j] = hex[b >> 4];
        out[2 * j + 1] = hex[b & 15];
    }
    out[40] = 0;
}
