#!/usr/bin/env python3
"""Per-function comparison of two objdump -d --no-show-raw-insn dumps.

Exact: instruction text identical once addresses and <symbol> targets are
stripped.  Reloc-only: identical once hex/decimal immediates are masked too,
with an AArch64 offset-0 operand `[xN]` read as `[xN, #I]`.
Everything else is listed with the nearest baseline function by name (or by
size) and a unified diff.
"""
import re, sys, difflib
from collections import Counter

HDR = re.compile(r'^[0-9a-f]+ <(.*)>:$')
INS = re.compile(r'^ *[0-9a-f]+:\t(.*)$')

def parse(path):
    fns = {}
    name = None
    for line in open(path, errors='replace'):
        line = line.rstrip('\n')
        m = HDR.match(line)
        if m:
            name = m.group(1); fns[name] = []
            continue
        m = INS.match(line)
        if m and name is not None:
            t = re.sub(r'<[^>]*>', '<sym>', m.group(1))
            t = re.sub(r'\s+', ' ', t).strip()
            fns[name].append(t)
    return fns

def mask(insns):
    out = []
    for t in insns:
        t = re.sub(r'0x[0-9a-f]+', '0xI', t)
        t = re.sub(r'#-?[0-9]+', '#I', t)
        # AArch64 offset 0: objdump prints `[x9]` where any other offset
        # prints `[x9, #16]`, so a :lo12: relocation that resolves to 0
        # drops the immediate. Spell it `[x9, #I]` like the others. A
        # following `!` or `,` (pre- or post-index) keeps the bare form.
        t = re.sub(r'\[(x[0-9]+|sp)\](?!\s*[!,])', r'[\1, #I]', t)
        t = re.sub(r'\b[0-9a-f]{3,} <sym>', 'A <sym>', t)
        t = re.sub(r'// #-?[0-9]+', '// #I', t)
        out.append(t)
    return tuple(out)

old, new = parse(sys.argv[1]), parse(sys.argv[2])
old_ex = Counter(tuple(v) for v in old.values())
old_ma = Counter(mask(v) for v in old.values())
exact = reloc = 0
changed = []
for n, v in new.items():
    if old_ex[tuple(v)] > 0:
        old_ex[tuple(v)] -= 1; old_ma[mask(v)] -= 1; exact += 1
    elif old_ma[mask(v)] > 0:
        old_ma[mask(v)] -= 1; reloc += 1
    else:
        changed.append(n)
print(f'functions old {len(old)} new {len(new)}: exact {exact}, reloc-only {reloc}, changed {len(changed)}')
print(f'insns old {sum(map(len, old.values()))} new {sum(map(len, new.values()))}')
# baseline functions not consumed
used = Counter(tuple(v) for v in new.values())
left = [n for n, v in old.items() if old_ma[mask(v)] > 0]
for n in changed:
    v = new[n]
    cand = n if n in old else min(old, key=lambda o: abs(len(old[o]) - len(v)) + (0 if o in left else 1000))
    print(f'\n### changed: {n} ({len(v)} insns) vs baseline {cand} ({len(old[cand])} insns)')
    for l in difflib.unified_diff(list(mask(old[cand])), list(mask(v)), 'old', 'new', n=2, lineterm=''):
        print('   ' + l)
if left:
    print('\nbaseline functions with no counterpart:')
    for n in left:
        print(f'   {n} ({len(old[n])} insns)')
