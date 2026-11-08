"""Pole count for remesh output: poles.py OBJ [OBJ...] [--plane X]

A pole is a vertex whose valence (edges meeting at it) is not 4. The edge set comes from
every face of the OBJ (a quad with a repeated index counts as the triangle it is); the quad
count for 'per 1000 quads' takes real quads only. Closed meshes have no border, so every
vertex is interior; border vertices (on an edge used by one face) are left out and counted
separately. Prints the total, poles per 1000 quads, poles on the symmetry plane
(|x - X| < 0.1 median edge, X = 0 by default), poles per region and 'reduction rings'.

The regions are the T-pose regions of badwhere.py (the model is 2 units tall, Y up, feet
at y = -1, head over y = 0.55, hands past |x| = 0.62) plus two that overlap them: the
shoulder band (0.35 < y < 0.6, |x| < 0.35) and the face (head quads with z > 0.03). They
only mean something on the T-pose at that size; on any other model read just the totals.

A reduction ring is a row of poles at one height where the modeller changed the quad
count: the poles of a region are binned by y in bins of 0.6 median edge, a run of
adjacent non-empty bins is one ring, and a ring of 6 poles or more whose x spread
exceeds 4 median edges is reported as 'ring at y=lo..hi (N poles)'. Limb regions are
split by side first, so each ankle and each wrist gets its own line.
"""
import sys, math, statistics

def load(fn):
    V = []; F = []
    for l in open(fn):
        if l.startswith('v '): V.append(tuple(map(float, l.split()[1:4])))
        elif l.startswith('f '): F.append([int(x.split('/')[0]) - 1 for x in l.split()[1:]])
    return V, F

def region(c):
    x, y, z = c
    if y < -0.85: return 'feet'
    if y < -0.1: return 'legs'
    if abs(x) > 0.62: return 'hands'
    if abs(x) > 0.22 and y > 0.05: return 'arms'
    if y > 0.55: return 'head'
    return 'torso'

ORDER = ['torso', 'head', 'arms', 'legs', 'hands', 'feet', 'shoulder band', 'face']

def analyse(V, F, plane_x=0.0):
    """Valence per vertex, poles, the median edge length and the quad count."""
    edges = {}
    quads = 0
    for f in F:
        g = [v for k, v in enumerate(f) if v != f[(k + 1) % len(f)]]
        if len(g) == 4 and len(set(g)) == 4: quads += 1
        for k in range(len(g)):
            a, b = g[k], g[(k + 1) % len(g)]
            if a == b: continue
            e = (min(a, b), max(a, b))
            edges[e] = edges.get(e, 0) + 1
    val = [0] * len(V); border = [False] * len(V)
    for (a, b), n in edges.items():
        val[a] += 1; val[b] += 1
        if n == 1: border[a] = border[b] = True
    med = statistics.median(math.dist(V[a], V[b]) for a, b in edges) if edges else 1.0
    poles = [v for v in range(len(V)) if val[v] > 0 and not border[v] and val[v] != 4]
    nborder = sum(1 for v in range(len(V)) if val[v] > 0 and border[v])
    return val, poles, med, quads, nborder

LIMBS = ('arms', 'hands', 'legs', 'feet')

def rings(V, poles, med, min_poles=6, spread_edges=4.0):
    """Reduction rings per region: the poles are binned by y in bins of 0.6 median edge,
    runs of adjacent non-empty bins are one ring, and a ring is reported when it holds
    >= min_poles poles over an x spread wider than spread_edges edges. Limb regions are
    split by side (x sign) first, so each ankle gets its own ring. Returns (y low, y high,
    region, poles, x spread) tuples, top down."""
    out = []
    by_region = {}
    for v in poles:
        r = region(V[v])
        if r in LIMBS: r += ' left' if V[v][0] > 0 else ' right'
        by_region.setdefault(r, []).append(v)
    h = 0.6 * med
    for r, vs in by_region.items():
        bins = {}
        for v in vs:
            bins.setdefault(math.floor(V[v][1] / h), []).append(v)
        run = []
        last = None
        for b in sorted(bins) + [None]:
            if b is not None and (last is None or b == last + 1):
                run.extend(bins[b])
            else:
                xs = [V[v][0] for v in run]
                ys = [V[v][1] for v in run]
                if len(run) >= min_poles and max(xs) - min(xs) > spread_edges * med:
                    out.append((min(ys), max(ys), r, len(run), max(xs) - min(xs)))
                run = list(bins[b]) if b is not None else []
            last = b
    out.sort(key=lambda t: (-t[1], t[2]))
    return out

def main():
    args = sys.argv[1:]
    plane_x = 0.0
    if '--plane' in args:
        i = args.index('--plane'); plane_x = float(args[i + 1]); del args[i:i + 2]
    for fn in args:
        V, F = load(fn)
        val, poles, med, quads, nborder = analyse(V, F, plane_x)
        on_plane = sum(1 for v in poles if abs(V[v][0] - plane_x) < 0.1 * med)
        print(f"{'/'.join(fn.split('/')[-2:])}: {quads} quads, {len(poles)} poles ({1000 * len(poles) / max(quads, 1):.1f} per 1000 quads), {on_plane} on the plane, median edge {med:.4f}, {nborder} border vertices")
        valences = {}
        for v in poles: valences[val[v]] = valences.get(val[v], 0) + 1
        print("  valences: " + ", ".join(f"{k}: {valences[k]}" for k in sorted(valences)))
        per = {}; face = 0; shoulder = 0
        for v in poles:
            x, y, z = V[v]
            r = region(V[v]); per[r] = per.get(r, 0) + 1
            if r == 'head' and z > 0.03: face += 1
            if 0.35 < y < 0.6 and abs(x) < 0.35: shoulder += 1
        per['shoulder band'] = shoulder; per['face'] = face
        for r in ORDER:
            if r in per: print(f"  {r:14s} {per[r]:5d} poles")
        for y0, y1, r, n, spread in rings(V, poles, med):
            print(f"  ring at y={y0:+.3f}..{y1:+.3f} ({n} poles, {r}, x spread {spread / med:.1f} edges)")

if __name__ == '__main__':
    main()
