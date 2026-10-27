#!/usr/bin/env python3
"""Every change of gRandomState between two vblanks is one or more
xorshift32 steps (better-rand's generator, with 0 mapped to its seed)
from the previous value, or from the game's reseed to 1. JSON from
`psxw mod test` on stdin."""
import json
import sys

M = 0xFFFFFFFF


def step(x):
    if x == 0:
        x = 0x2545F491
    x ^= (x << 13) & M
    x ^= x >> 17
    x ^= (x << 5) & M
    return x & M


def reaches(a, b, n=100000):
    for _ in range(n + 1):
        if a == b:
            return True
        a = step(a)
    return False


trace = json.load(sys.stdin)["trace"]["gRandomState"]
prev, changes = 0, 0
for frame, v in enumerate(trace, 1):
    if v != prev:
        changes += 1
        if not (reaches(prev, v) or reaches(1, v)):
            print(f"frame {frame}: 0x{prev:08X} -> 0x{v:08X} is no xorshift step")
            sys.exit(1)
    prev = v
print(f"{changes} changes, all xorshift steps")
sys.exit(0 if changes else 1)
