#!/usr/bin/env python3
"""Dump every Qwen3-0.6B token that decodes to a single lowercase-ASCII word
(leading space + [a-z]+). These are the candidate replacement mnemonics:
each is exactly one token when it appears mid-document."""
from transformers import AutoTokenizer
import re, json

tok = AutoTokenizer.from_pretrained("Qwen/Qwen3-0.6B")

# Pattern: leading space then lowercase letters only, whole thing.
pat = re.compile(r"^ [a-z]+$")

words = []   # (word without space, id)
for i in range(tok.vocab_size):
    try:
        s = tok.decode([i])
    except Exception:
        continue
    if pat.match(s):
        w = s[1:]            # strip leading space
        # Sanity: re-encoding " "+w must give exactly [i]
        if tok.encode(" " + w, add_special_tokens=False) == [i]:
            words.append((w, i))

words.sort(key=lambda x: x[0])
with open("/tmp/qwen_single_words.txt", "w") as f:
    for w, i in words:
        f.write(f"{w}\t{i}\n")
print(f"{len(words)} single-token lowercase words written to /tmp/qwen_single_words.txt")
print("sample:", [w for w, _ in words[:12]])
