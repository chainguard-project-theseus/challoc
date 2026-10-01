"""object_graph_bench.py: an object-graph builder (the python rows of bench/rows.toml).

ROUNDS times builds a tree of NODES class instances (each with a name, a
payload list, a children list and a parent reference), adds random cross
edges, walks it breadth-first summing payloads, detaches a third of the
subtrees and rebuilds them, then drops the whole graph. Instances and their
inline attribute storage, the small lists and the deque are pymalloc
objects unless PYTHONMALLOC=malloc; the children lists of wide nodes and
the BFS queue grow past 512 bytes and reach malloc either way.
Deterministic. Usage: object_graph_bench.py [ROUNDS] [NODES].
"""
import random
import sys
import time
from collections import deque

ROUNDS = int(sys.argv[1]) if len(sys.argv) > 1 else 2
NODES = int(sys.argv[2]) if len(sys.argv) > 2 else 150000

rng = random.Random(4141)


class Node:
    def __init__(self, ident, parent):
        self.ident = ident
        self.name = f"node-{ident}"
        self.parent = parent
        self.children = []
        self.links = []
        self.payload = [ident & 0xFF] * (1 + ident % 7)


def build(n):
    root = Node(0, None)
    nodes = [root]
    for i in range(1, n):
        parent = nodes[rng.randrange(max(1, len(nodes) // 8), len(nodes)) if len(nodes) > 8 else 0]
        node = Node(i, parent)
        parent.children.append(node)
        nodes.append(node)
    for _ in range(n // 10):
        a = nodes[rng.randrange(n)]
        b = nodes[rng.randrange(n)]
        a.links.append(b)
    return root, nodes


def walk(root):
    total = 0
    seen = 0
    q = deque([root])
    while q:
        node = q.popleft()
        seen += 1
        total += sum(node.payload)
        q.extend(node.children)
    return total, seen


t0 = time.perf_counter()
acc = 0
for _ in range(ROUNDS):
    root, nodes = build(NODES)
    total, seen = walk(root)
    if seen != NODES:
        sys.exit(f"walk saw {seen} of {NODES} nodes")
    # detach a third of the root's subtrees and rebuild them
    victims = root.children[: len(root.children) // 3]
    del root.children[: len(root.children) // 3]
    for v in victims:
        v.parent = None
    del victims
    for i in range(NODES // 3):
        node = Node(NODES + i, root)
        root.children.append(node)
    acc += total + walk(root)[1]
    del root, nodes
dt = time.perf_counter() - t0
print(f"object_graph rounds={ROUNDS} nodes={NODES} acc={acc} secs={dt:.3f}")
