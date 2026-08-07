#!/usr/bin/env python3
"""Build a candidate glyph pool for the dense encoding from the cleanest
single-token Qwen3 vocab blocks. We need ~187 distinct symbols that are:
  - exactly 1 token
  - not ASCII (so they can't be confused with text-mode operators)
  - not combining marks / control chars / variation selectors
  - not whitespace
Output: a proposed 187-glyph pool with their token ids, sorted by Unicode block."""
from transformers import AutoTokenizer
import re

tok = AutoTokenizer.from_pretrained("Qwen/Qwen3-0.6B")
N = tok.vocab_size

# Build set of single-token chars
single = {}
for i in range(N):
    try:
        s = tok.decode([i])
    except Exception:
        continue
    if len(s) == 1 and tok.encode(s, add_special_tokens=False) == [i]:
        cp = ord(s)
        single[cp] = i

# Candidate blocks (ranges), exclude combining marks (0x0300-0x036F),
# variation selectors (0xFE00-0xFE0F), control chars (<0x20), spaces.
RANGES = [
    ("Arrows",                    0x2190, 0x21FF),
    ("Math Operators",            0x2200, 0x22FF),
    ("Misc Technical",            0x2300, 0x237F),
    ("Enclosed Alphanumerics",    0x2460, 0x24FF),
    ("Box Drawing",               0x2500, 0x257F),
    ("Block Elements",            0x2580, 0x259F),
    ("Geometric Shapes",          0x25A0, 0x25FF),
    ("Misc Symbols",              0x2600, 0x26FF),
    ("Dingbats",                  0x2700, 0x27BF),
    ("Misc Symbols and Arrows",   0x2B00, 0x2BFF),
    ("Supplemental Arrows-B",     0x2900, 0x297F),
    ("Supplemental Arrows-A",     0x27F0, 0x27FF),
    ("Misc Mathematical Symbols-A",0x27C0, 0x27EF),
    ("Misc Mathematical Symbols-B",0x2980, 0x29FF),
    ("Supplemental Math Operators",0x2A00, 0x2AFF),
]

need = 187
pool = []  # (block, cp, id, char)
for name, lo, hi in RANGES:
    for cp in range(lo, hi+1):
        if cp in single:
            pool.append((name, cp, single[cp], chr(cp)))

print(f"Candidate pool from math/arrow/shape blocks: {len(pool)} symbols")
by_block = {}
for name, cp, i, c in pool:
    by_block.setdefault(name, []).append((cp, i, c))
for name in [r[0] for r in RANGES]:
    items = by_block.get(name, [])
    if items:
        print(f"  {name:32s} {len(items):3d}  {''.join(c for _,_,c in items[:30])}")

# Build a 187-pool: prefer blocks in this order until we hit 187
chosen = []
order = ["Box Drawing","Block Elements","Geometric Shapes","Misc Symbols",
         "Dingbats","Misc Symbols and Arrows","Arrows","Math Operators",
         "Misc Technical","Supplemental Arrows-B","Supplemental Arrows-A",
         "Misc Mathematical Symbols-B","Supplemental Math Operators",
         "Misc Mathematical Symbols-A","Enclosed Alphanumerics"]
for name in order:
    for cp, i, c in by_block.get(name, []):
        if len(chosen) < need:
            chosen.append((name, cp, i, c))
        else:
            break
    if len(chosen) >= need:
        break

print(f"\nChosen pool: {len(chosen)} symbols (need {need})")
# Save to file
with open("/tmp/proposed_glyph_pool.txt","w") as f:
    for j,(name,cp,i,c) in enumerate(chosen):
        f.write(f"{j:3d}\tU+{cp:04X}\tid={i:6d}\t{c}\t{name}\n")
print("Written to /tmp/proposed_glyph_pool.txt")
