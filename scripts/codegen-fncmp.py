#!/usr/bin/env python3
"""Per-function comparison of two objdump -d --no-show-raw-insn dumps.

Functions pair by symbol name.  Each pair is one of:

  exact         instruction text identical once addresses and <symbol>
                targets are stripped
  reloc-only    identical once hex/decimal immediates are masked too, with
                an AArch64 offset-0 operand `[xN]` read as `[xN, #I]`
  padding-only  identical once trailing alignment padding (nop, int3,
                `xchg %ax,%ax`) is dropped as well: the function moved, and
                the gap to the next aligned symbol changed
  erratum-stub  GNU ld's Cortex-A53 erratum 843419 workaround: an `adrp`
                at page offset 0xff8/0xffc became `adr` of the same page,
                or a load or store 1-3 instructions after it became a
                branch into the stub; the stub after the body holds only
                padding, a branch past the stubs, and displaced loads and
                stores each followed by a branch back to the instruction
                after the original, at the address the body branches to
  changed       anything else

A function whose name is new pairs first with a baseline function whose
name differs only in the crate hash or impl disambiguator (the demangled
path without `[hash]`, when c++filt is there; that pair is classified as
above), then with an unpaired baseline function of the same instructions
(exact, then reloc-only) as a rename.  Changed counts the changed
functions plus the baseline functions left with no counterpart.  The
report prints one line per function that is neither exact nor reloc-only
under its own name, and the full masked diff of every changed one.
"""
import re, sys, difflib, shutil, subprocess
from collections import Counter

HDR = re.compile(r'^[0-9a-f]+ <(.*)>:$')
INS = re.compile(r'^ *([0-9a-f]+):\t(.*)$')
PAD = re.compile(r'^((data16|cs|ds) )*(nop[a-z]*( .*)?|xchg %ax,%ax|int3)$')
BRANCH = re.compile(r'^b ([0-9a-f]+) <sym>$')
ERRATUM_OFFSETS = (0xff8, 0xffc)

def parse(path):
    """name -> [(address, text)], in dump order; a repeated name gets #2, #3."""
    fns, seen = {}, Counter()
    cur = None
    for line in open(path, errors='replace'):
        line = line.rstrip('\n')
        m = HDR.match(line)
        if m:
            name = m.group(1); seen[name] += 1
            if seen[name] > 1:
                name = f'{name}#{seen[name]}'
            cur = fns[name] = []
            continue
        m = INS.match(line)
        if m and cur is not None:
            t = re.sub(r'<[^>]*>', '<sym>', m.group(2))
            t = re.sub(r'\s+', ' ', t).strip()
            cur.append((int(m.group(1), 16), t))
    return fns

def mask1(t):
    t = re.sub(r'0x[0-9a-f]+', '0xI', t)
    t = re.sub(r'#-?[0-9]+', '#I', t)
    # AArch64 offset 0: objdump prints `[x9]` where any other offset
    # prints `[x9, #16]`, so a :lo12: relocation that resolves to 0
    # drops the immediate. Spell it `[x9, #I]` like the others. A
    # following `!` or `,` (pre- or post-index) keeps the bare form.
    t = re.sub(r'\[(x[0-9]+|sp)\](?!\s*[!,])', r'[\1, #I]', t)
    t = re.sub(r'\b[0-9a-f]{3,} <sym>', 'A <sym>', t)
    t = re.sub(r'// #-?[0-9]+', '// #I', t)
    return t

def texts(f): return tuple(t for _, t in f)
def mask(f): return tuple(mask1(t) for _, t in f)

def strip_pad(f):
    n = len(f)
    while n and PAD.match(f[n - 1][1]):
        n -= 1
    return f[:n]

def erratum(o, n):
    """A note if n is o plus the erratum 843419 rewrite and stub, else None.
    o and n have their trailing padding stripped."""
    k = len(o)
    if len(n) < k:
        return None
    body, tail = n[:k], n[k:]
    end = (tail or body)[-1][0] + 4 if n else 0
    def erratum_adrp(j):  # the adrp/adr at page offset 0xff8/0xffc 1-3 before j
        return next((a for a, t in reversed(body[max(0, j - 3):j])
                     if re.match(r'adrp? ', t) and a & 0xfff in ERRATUM_OFFSETS), None)
    sites = []
    # (masked displaced load or store, return address) -> the stub address
    # the body branches to, or None when the instruction stayed in place
    # (adr form) and the stub's copy is unreachable.
    stubbed = {}
    for i, ((_, ot), (na, nt)) in enumerate(zip(o, body)):
        if mask1(ot) == mask1(nt):
            if erratum_adrp(i) is not None and re.match(r'(ld|st)', nt):
                stubbed.setdefault((mask1(nt), na + 4), None)
            continue
        a = re.match(r'adrp (x[0-9]+), ([0-9a-f]+) <sym>$', ot)
        b = re.match(r'adr (x[0-9]+), ([0-9a-f]+) <sym>$', nt)
        if (a and b and a.group(1) == b.group(1) and na & 0xfff in ERRATUM_OFFSETS
                and int(b.group(2), 16) & 0xfff == 0):
            sites.append(f'adrp {a.group(1)} at {na:#x} (page offset {na & 0xfff:#x}) became adr')
            continue
        b, p = BRANCH.match(nt), erratum_adrp(i)
        if (b and p is not None and re.match(r'(ld|st)', ot) and tail
                and tail[0][0] <= int(b.group(1), 16) <= tail[-1][0]):
            stubbed[(mask1(ot), na + 4)] = int(b.group(1), 16)
            sites.append(f'`{ot}` at {na:#x} became a branch to the stub '
                         f'(adrp at {p:#x}, page offset {p & 0xfff:#x})')
            continue
        return None
    if not sites:
        return None
    # The stub: padding, a branch past the stubs, and each displaced load or
    # store followed by a branch back to the instruction after it, at the
    # address the body branches to.
    found = set()
    j = 0
    while j < len(tail):
        a, t = tail[j]
        b = BRANCH.match(tail[j + 1][1]) if j + 1 < len(tail) else None
        pair = (mask1(t), int(b.group(1), 16)) if b else None
        if PAD.match(t) or (BRANCH.match(t) and int(BRANCH.match(t).group(1), 16) >= end):
            j += 1
        elif pair in stubbed and stubbed[pair] in (None, a):
            found.add(pair)
            j += 2
        else:
            return None
    if any(at is not None and pair not in found for pair, at in stubbed.items()):
        return None
    note = '; '.join(sites)
    if tail:
        note += f'; {len(tail)}-instruction stub after the body: ' + '; '.join(t for _, t in tail)
    return note

def classify(o, n):
    """(kind, note) for baseline function o and new function n."""
    if texts(o) == texts(n):
        return 'exact', ''
    if mask(o) == mask(n):
        return 'reloc-only', ''
    so, sn = strip_pad(o), strip_pad(n)
    if mask(so) == mask(sn):
        return 'padding-only', (f'same {len(so)} instructions, trailing padding '
                                f'{len(o) - len(so)} -> {len(n) - len(sn)}')
    note = erratum(so, sn)
    if note:
        return 'erratum-stub', 'GNU ld erratum 843419 workaround: ' + note
    note = erratum(sn, so)
    if note:
        return 'erratum-stub', 'GNU ld erratum 843419 workaround in the baseline: ' + note
    return 'changed', ''

def demangle(names):
    tool = shutil.which('c++filt')
    if not tool or not names:
        return {}
    out = subprocess.run([tool], input='\n'.join(names) + '\n', capture_output=True,
                         text=True).stdout.splitlines()
    return {n: d for n, d in zip(names, out) if d != n}

old, new = parse(sys.argv[1]), parse(sys.argv[2])
kinds = Counter()
lines = []  # (kind, new name, baseline name or None, note)
for n, v in new.items():
    if n in old:
        k, note = classify(old[n], v)
        kinds[k] += 1
        if k not in ('exact', 'reloc-only'):
            lines.append((k, n, n, note))
left = [n for n in old if n not in new]
fresh = [n for n in new if n not in old]
# A v0 symbol's crate hash (Cs..._) and impl disambiguator (Ms<n>_) change
# when the crate metadata changes or an impl is added next to it; the
# demangled path without [hash] stays. Pair leftovers whose key is unique
# on both sides, and classify them like same-name pairs. Without c++filt
# the key is the mangled name with those two dropped and every backref
# (B<base-62>_, a byte offset into the symbol, which moves when an earlier
# hash or disambiguator changes length) spelled B_. That is coarser than
# the demangled path: two names that differ only in what a backref points
# at share a key. A key shared within one side pairs nothing; a pair made
# across the sides is still classified by its instructions.
dm = demangle(left + fresh)
def key(name):
    if name in dm:
        return re.sub(r'\[[0-9a-f]+\]', '', dm[name])
    name = re.sub(r'(Cs[0-9A-Za-z]+_|Ms[0-9A-Za-z]*_)', '', name)
    return re.sub(r'B[0-9A-Za-z]*_', 'B_', name)
ko, kn = Counter(map(key, left)), Counter(map(key, fresh))
by_key = {key(o): o for o in left if ko[key(o)] == 1}
for n in list(fresh):
    m = by_key.get(key(n))
    if m is None or kn[key(n)] != 1:
        continue
    k, note = classify(old[m], new[n])
    kinds[k] += 1
    lines.append((k, n, m, f'renamed from {m}' + (f'; {note}' if note else '')))
    left.remove(m); fresh.remove(n)
for n in fresh:
    v = new[n]
    k, m = 'exact', next((o for o in left if texts(old[o]) == texts(v)), None)
    if m is None:
        k, m = 'reloc-only', next((o for o in left if mask(old[o]) == mask(v)), None)
    if m is None:
        # A renamed and changed function diffs against its closest leftover.
        k, ratio = 'changed', {o: difflib.SequenceMatcher(None, mask(old[o]), mask(v)).ratio()
                               for o in left}
        m = max(ratio, key=ratio.get, default=None)
        m = m if m is not None and ratio[m] >= 0.5 else None
    if m is not None:
        left.remove(m)
    kinds[k] += 1
    lines.append((k, n, m, f'renamed from {m}' if k != 'changed' else ''))

dm.update(demangle(sorted({x for l in lines for x in l[1:3] if x} - set(dm))))
def show(name):
    return f'{name} = {dm[name]}' if name in dm else name

# A baseline function with no counterpart is a change too: its code is
# gone, or went into a caller.
changed = [l for l in lines if l[0] == 'changed']
print(f'functions old {len(old)} new {len(new)}: exact {kinds["exact"]}, '
      f'reloc-only {kinds["reloc-only"]}, padding-only {kinds["padding-only"]}, '
      f'erratum-stub {kinds["erratum-stub"]}, changed {len(changed) + len(left)}')
print(f'insns old {sum(map(len, old.values()))} new {sum(map(len, new.values()))}')
for k, n, m, note in lines:
    if k == 'changed':
        base = f'baseline {m} ({len(old[m])} insns)' if m else 'no baseline counterpart'
        print(f'### changed: {n} ({len(new[n])} insns) vs {base}')
    else:
        print(f'### {k}: {n} ({len(old[m])} -> {len(new[n])} insns): {note}')
    if n in dm:
        print(f'    = {dm[n]}')
if left:
    print("\nbaseline functions with no counterpart (counted as changed):")
    for n in left:
        print(f'   {show(n)} ({len(old[n])} insns)')
for k, n, m, note in changed:
    against = f' against baseline {m}' if m and m != n else ''
    print(f'\n--- diff of {n}{against}, immediates masked')
    for l in difflib.unified_diff(list(mask(old[m])) if m else [], list(mask(new[n])),
                                  'old', 'new', n=2, lineterm=''):
        print('   ' + l)
