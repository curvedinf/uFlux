#!/usr/bin/env python3
"""Scan Qwen3-0.6B vocab for single-token entries suitable as dense op glyphs.
A suitable glyph is a token id that:
  - decodes to some string s
  - re-encodes to exactly [id] (truly single-token, no special-casing)
  - is NOT ascii letters/digits/space (those are common / ambiguous as ops)
We categorize by what s looks like to surface 'rare' candidates."""
from transformers import AutoTokenizer
import re, json

tok = AutoTokenizer.from_pretrained("Qwen/Qwen3-0.6B")
N = tok.vocab_size

single = []  # (id, s, category)
for i in range(N):
    try:
        s = tok.decode([i])
    except Exception:
        continue
    if tok.encode(s, add_special_tokens=False) != [i]:
        continue
    # Categorize
    if s == "" or s == " ":
        cat = "space"
    elif re.fullmatch(r'[A-Za-z]', s):
        cat = "ascii_letter"
    elif re.fullmatch(r'[0-9]', s):
        cat = "ascii_digit"
    elif re.fullmatch(r'[ -/:-@\[-`{-~]', s) and len(s) == 1:
        cat = "ascii_punct"
    elif re.fullmatch(r'\s+', s):
        cat = "whitespace"
    elif len(s) == 1:
        cp = ord(s)
        if 0x1F000 <= cp <= 0x1FAFF:
            cat = "emoji_pict"
        elif 0x2000 <= cp <= 0x2BFF:
            cat = "symbol_punct"
        elif 0x3000 <= cp <= 0x303F:
            cat = "cjk_punct"
        else:
            cat = "other_singlechar"
    elif re.fullmatch(r'[ -~]+', s):
        cat = "ascii_multichar"
    elif len(s) <= 4:
        cat = "short_nonascii"
    else:
        cat = "long"

    single.append((i, s, cat))

# Count categories
from collections import Counter
cats = Counter(c for _, _, c in single)
print("=== Category counts ===")
for c, n in cats.most_common():
    print(f"  {c:20s} {n}")

# We want: single characters that are NOT ascii letters/digits/space/whitespace.
# These are unambiguous as space-delimited op glyphs.
good = [(i, s) for i, s, c in single
        if c in ("symbol_punct", "emoji_pict", "other_singlechar", "cjk_punct")
        and s.strip() != ""]
print(f"\n=== {len(good)} single-char non-ascii symbol candidates ===")
print("(each is exactly 1 token, and re-encodes cleanly)")

# Show distribution by unicode block
from collections import defaultdict
blocks = defaultdict(list)
for i, s in good:
    cp = ord(s)
    if 0x2000 <= cp <= 0x206F: blk = "General Punctuation"
    elif 0x2070 <= cp <= 0x209F: blk = "Superscripts/Subscripts"
    elif 0x20A0 <= cp <= 0x20CF: blk = "Currency"
    elif 0x20D0 <= cp <= 0x20FF: blk = "Combining Diacritical Marks for Symbols"
    elif 0x2100 <= cp <= 0x214F: blk = "Letterlike Symbols"
    elif 0x2150 <= cp <= 0x218F: blk = "Number Forms"
    elif 0x2190 <= cp <= 0x21FF: blk = "Arrows"
    elif 0x2200 <= cp <= 0x22FF: blk = "Math Operators"
    elif 0x2300 <= cp <= 0x23FF: blk = "Misc Technical"
    elif 0x2400 <= cp <= 0x243F: blk = "Control Pictures"
    elif 0x2440 <= cp <= 0x245F: blk = "OCR"
    elif 0x2460 <= cp <= 0x24FF: blk = "Enclosed Alphanumerics"
    elif 0x2500 <= cp <= 0x257F: blk = "Box Drawing"
    elif 0x2580 <= cp <= 0x259F: blk = "Block Elements"
    elif 0x25A0 <= cp <= 0x25FF: blk = "Geometric Shapes"
    elif 0x2600 <= cp <= 0x26FF: blk = "Misc Symbols"
    elif 0x2700 <= cp <= 0x27BF: blk = "Dingbats"
    elif 0x2B00 <= cp <= 0x2BFF: blk = "Misc Symbols and Arrows"
    elif 0x3000 <= cp <= 0x303F: blk = "CJK Symbols and Punctuation"
    elif 0x1F000 <= cp <= 0x1F0FF: blk = "Mahjong/Playing Cards"
    elif 0x1F100 <= cp <= 0x1F1FF: blk = "Enclosed Alphanumeric Supplement"
    elif 0x1F300 <= cp <= 0x1F5FF: blk = "Misc Symbols and Pictographs"
    elif 0x1F600 <= cp <= 0x1F64F: blk = "Emoticons"
    elif 0x1F680 <= cp <= 0x1F6FF: blk = "Transport and Map"
    elif 0x1F900 <= cp <= 0x1F9FF: blk = "Supplemental Symbols and Pictographs"
    elif 0x1FA00 <= cp <= 0x1FA6F: blk = "Chess Symbols"
    elif 0x1FA70 <= cp <= 0x1FAFF: blk = "Symbols and Pictographs Extended-A"
    else: blk = f"Other (U+{cp:04X})"
    blocks[blk].append((i, s))

print("\n=== By Unicode block (candidate glyphs) ===")
for blk in sorted(blocks, key=lambda b: -len(blocks[b])):
    items = blocks[blk]
    print(f"  {blk:45s} {len(items):4d}  e.g. {''.join(s for _,s in items[:12])}")

# Write full list to file
with open("/tmp/qwen_single_symbols.txt", "w") as f:
    for i, s in good:
        f.write(f"U+{ord(s):04X}\t{i}\t{s}\n")
print(f"\nFull list -> /tmp/qwen_single_symbols.txt ({len(good)} entries)")
