"""Closed edge loops round a point: loops.py OBJ x,y,z [--also x,y,z] [--max 8]

From the vertex nearest the point, every edge within --max median edges of it is followed
as an edge loop the way Blender's loop select does: at a valence-4 vertex the loop carries
on through the edge opposite, across the two quads that share the edge just walked; it
stops at a pole, at a border, at a face that is not a quad, or after 4x the quad count
steps. A loop that comes back to its first edge is closed. A closed loop cuts a closed
surface in two; it encloses the point when the point's face (the nearest face round the
nearest vertex) is on the smaller side, counted in faces, that side is disc-like (no more
than n^2 / 2 pi faces for a loop of n edges: a neck ring has the head on its small side
and is no loop round the mouth), and the whole loop lies within 2x --max median edges of
the point. Projecting the loop onto a plane and taking a winding number was tried first and
counted the mouth's loop as a loop round the eye. Prints the number of distinct closed
enclosing loops, each with its edge count and, with --also, whether it also encloses the
second point (the mouth's outer loop should enclose the nose).
"""
import sys, math, statistics

def load(fn):
    V = []; F = []
    for l in open(fn):
        if l.startswith('v '): V.append(tuple(map(float, l.split()[1:4])))
        elif l.startswith('f '): F.append([int(x.split('/')[0]) - 1 for x in l.split()[1:]])
    return V, F

def sub(a, b): return (a[0] - b[0], a[1] - b[1], a[2] - b[2])
def dot(a, b): return a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
def cross(a, b): return (a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0])
def norm(a): return math.sqrt(dot(a, a))

def build(V, F):
    """Edge -> faces, vertex -> edges, and the edges of every face, over faces with no
    repeated index."""
    faces = []
    for f in F:
        g = [v for k, v in enumerate(f) if v != f[(k + 1) % len(f)]]
        if len(g) >= 3 and len(set(g)) == len(g): faces.append(g)
    efaces = {}; vedges = {}
    for fi, f in enumerate(faces):
        for k in range(len(f)):
            a, b = f[k], f[(k + 1) % len(f)]
            e = (min(a, b), max(a, b))
            efaces.setdefault(e, []).append(fi)
    for (a, b) in efaces:
        vedges.setdefault(a, []).append((a, b)); vedges.setdefault(b, []).append((a, b))
    return faces, efaces, vedges

def next_edge(faces, efaces, vedges, e, v):
    """The edge the loop continues along past vertex v after edge e, or None at a pole, a
    border or a non-quad face."""
    if len(vedges.get(v, ())) != 4: return None
    fs = efaces[e]
    if len(fs) != 2: return None
    used = set()
    for fi in fs:
        f = faces[fi]
        if len(f) != 4: return None
        for k in range(4):
            a, b = f[k], f[(k + 1) % 4]
            if v in (a, b): used.add((min(a, b), max(a, b)))
    rest = [x for x in vedges[v] if x not in used]
    return rest[0] if len(rest) == 1 else None

def follow(faces, efaces, vedges, e0, limit):
    """The loop through e0 as an ordered list of edges, and whether it closed."""
    walks = []
    for v in (e0[1], e0[0]):
        # Walk away from e0 past one end, then past the other.
        walk = []; e = e0
        for _ in range(limit):
            n = next_edge(faces, efaces, vedges, e, v)
            if n is None: break
            if n == e0: return [e0] + walk, True
            walk.append(n)
            v = n[1] if n[0] == v else n[0]
            e = n
        walks.append(walk)
    return walks[1][::-1] + [e0] + walks[0], False

def loop_vertices(edges):
    """The vertices of an edge loop in order."""
    first, second = edges[0], edges[1]
    v = first[0] if first[0] in second else first[1]
    verts = [first[1] if v == first[0] else first[0], v]
    for e in edges[1:]:
        v = e[1] if e[0] == v else e[0]
        verts.append(v)
    if verts[-1] == verts[0]: verts.pop()
    return verts

def face_near(V, faces, vedges, efaces, p):
    """The face whose centroid is nearest p among the faces round the vertex nearest p."""
    v0 = min(range(len(V)), key=lambda v: math.dist(V[v], p) if v in vedges else float('inf'))
    around = {fi for e in vedges[v0] for fi in efaces[e]}
    cen = lambda fi: tuple(sum(V[v][k] for v in faces[fi]) / len(faces[fi]) for k in range(3))
    return v0, min(around, key=lambda fi: math.dist(cen(fi), p))

def side(faces, efaces, edges, f0):
    """The faces reachable from f0 without crossing the loop `edges`."""
    block = set(edges)
    seen = {f0}; stack = [f0]
    while stack:
        fi = stack.pop()
        f = faces[fi]
        for k in range(len(f)):
            a, b = f[k], f[(k + 1) % len(f)]
            e = (min(a, b), max(a, b))
            if e in block: continue
            for g in efaces[e]:
                if g not in seen:
                    seen.add(g); stack.append(g)
    return seen

def encloses(faces, efaces, edges, f0):
    """Whether the closed loop `edges` encloses face f0 on the surface: the loop splits a
    closed surface in two, f0 is on the smaller side, and that side is disc-like (it holds
    at most twice the quads a round disc with that perimeter holds, n^2 / 2 pi; a ring round
    the neck has the whole head on its small side and is not a loop round the mouth)."""
    s = side(faces, efaces, edges, f0)
    n = len(edges)
    return 2 * len(s) < len(faces) and len(s) <= n * n / (2 * math.pi), s

def main():
    args = sys.argv[1:]
    also = None; maxe = 8.0
    if '--also' in args:
        i = args.index('--also'); also = tuple(map(float, args[i + 1].split(','))); del args[i:i + 2]
    if '--max' in args:
        i = args.index('--max'); maxe = float(args[i + 1]); del args[i:i + 2]
    fn = args[0]; p = tuple(map(float, args[1].split(',')))
    V, F = load(fn)
    faces, efaces, vedges = build(V, F)
    med = statistics.median(math.dist(V[a], V[b]) for a, b in efaces)
    quads = sum(1 for f in faces if len(f) == 4)
    v0, f0 = face_near(V, faces, vedges, efaces, p)
    f_also = face_near(V, faces, vedges, efaces, also)[1] if also is not None else None
    reach = maxe * med
    near = [e for e in efaces if math.dist(V[e[0]], V[v0]) < reach and math.dist(V[e[1]], V[v0]) < reach]
    seen = set(); found = []
    for e in sorted(near):
        if e in seen: continue
        edges, closed = follow(faces, efaces, vedges, e, 4 * quads)
        key = frozenset(edges)
        seen.update(edges)
        if not closed or len(edges) < 3: continue
        verts = loop_vertices(edges)
        if max(math.dist(V[v], p) for v in verts) > 2 * reach: continue
        inside, region = encloses(faces, efaces, edges, f0)
        if not inside: continue
        found.append((len(edges), f_also in region))
    found.sort()
    print(f"{'/'.join(fn.split('/')[-2:])}: {len(found)} closed loops round [{p[0]}, {p[1]}, {p[2]}] (nearest vertex {['%.3f' % c for c in V[v0]]}, valence {len(vedges.get(v0, ()))}, {len(near)} edges within {maxe:g} median edges)")
    for n, a in found:
        print(f"  loop of {n} edges" + (f", encloses the --also point: {'yes' if a else 'no'}" if also is not None else ""))

if __name__ == '__main__':
    main()
