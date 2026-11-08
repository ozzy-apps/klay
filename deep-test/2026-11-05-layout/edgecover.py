"""Hard-edge coverage: edgecover.py OBJ LINES.obj

LINES.obj is the feature-line dump of a zremesh run (KLAY_ZR_FEATLINES=/abs/file.obj: `v`
records and one `l` polyline per detected hard edge or crease). Every line is sampled
every 0.1 quad (quad = the median edge length of the output OBJ) and the distance from
each sample to the nearest output mesh edge is taken through a hash grid of edge segments.
Prints the fraction of line length within 0.15 quad of an output edge (a crease that is a
mesh edge loop scores near 100%), the largest distance in quads, and the number of poles
(valence != 4) within 2 quads of any line (HARD-10 wants none).
"""
import sys, math, statistics

def load(fn):
    V = []; F = []; L = []
    for l in open(fn):
        if l.startswith('v '): V.append(tuple(map(float, l.split()[1:4])))
        elif l.startswith('f '): F.append([int(x.split('/')[0]) - 1 for x in l.split()[1:]])
        elif l.startswith('l '): L.append([int(x) - 1 for x in l.split()[1:]])
    return V, F, L

def sub(a, b): return (a[0] - b[0], a[1] - b[1], a[2] - b[2])
def dot(a, b): return a[0] * b[0] + a[1] * b[1] + a[2] * b[2]

def seg_dist(p, a, b):
    """Distance from p to segment ab."""
    d = sub(b, a); l2 = dot(d, d)
    t = 0.0 if l2 == 0 else max(0.0, min(1.0, dot(sub(p, a), d) / l2))
    q = (a[0] + d[0] * t, a[1] + d[1] * t, a[2] + d[2] * t)
    return math.dist(p, q)

class Grid:
    """Segments binned by the cells their bounding boxes cover."""
    def __init__(self, cell):
        self.cell = cell; self.cells = {}
    def key(self, p): return (math.floor(p[0] / self.cell), math.floor(p[1] / self.cell), math.floor(p[2] / self.cell))
    def add(self, a, b):
        ka, kb = self.key(a), self.key(b)
        for i in range(min(ka[0], kb[0]), max(ka[0], kb[0]) + 1):
            for j in range(min(ka[1], kb[1]), max(ka[1], kb[1]) + 1):
                for k in range(min(ka[2], kb[2]), max(ka[2], kb[2]) + 1):
                    self.cells.setdefault((i, j, k), []).append((a, b))
    def nearest(self, p, reach):
        """The nearest segment distance from p, searching cells within `reach`."""
        r = math.ceil(reach / self.cell)
        c = self.key(p); best = float('inf')
        for i in range(c[0] - r, c[0] + r + 1):
            for j in range(c[1] - r, c[1] + r + 1):
                for k in range(c[2] - r, c[2] + r + 1):
                    for a, b in self.cells.get((i, j, k), ()):
                        d = seg_dist(p, a, b)
                        if d < best: best = d
        return best

def main():
    fn, lines_fn = sys.argv[1], sys.argv[2]
    V, F, _ = load(fn)
    LV, _, LL = load(lines_fn)
    edges = {}
    for f in F:
        g = [v for k, v in enumerate(f) if v != f[(k + 1) % len(f)]]
        for k in range(len(g)):
            a, b = g[k], g[(k + 1) % len(g)]
            e = (min(a, b), max(a, b)); edges[e] = edges.get(e, 0) + 1
    quad = statistics.median(math.dist(V[a], V[b]) for a, b in edges)
    val = [0] * len(V); border = [False] * len(V)
    for (a, b), n in edges.items():
        val[a] += 1; val[b] += 1
        if n == 1: border[a] = border[b] = True
    mesh = Grid(quad)
    for a, b in edges: mesh.add(V[a], V[b])
    # Samples along every line, 0.1 quad apart.
    step = 0.1 * quad
    samples = []; total = 0.0
    feat = Grid(2 * quad)
    for line in LL:
        for a, b in zip(line, line[1:]):
            pa, pb = LV[a], LV[b]
            feat.add(pa, pb)
            d = math.dist(pa, pb); total += d
            n = max(1, math.ceil(d / step))
            for i in range(n):
                t = (i + 0.5) / n
                samples.append(((pa[0] + (pb[0] - pa[0]) * t, pa[1] + (pb[1] - pa[1]) * t, pa[2] + (pb[2] - pa[2]) * t), d / n))
    covered = 0.0; worst = 0.0
    for p, w in samples:
        d = mesh.nearest(p, 0.15 * quad)
        if d <= 0.15 * quad: covered += w
        if d == float('inf'): d = mesh.nearest(p, 4 * quad)
        worst = max(worst, d)
    near_poles = 0
    for v in range(len(V)):
        if val[v] == 0 or border[v] or val[v] == 4: continue
        if feat.nearest(V[v], 2 * quad) <= 2 * quad: near_poles += 1
    print(f"{'/'.join(fn.split('/')[-2:])}: {len(LL)} feature lines, {total / quad:.0f} quads of line, {len(samples)} samples; {100 * covered / max(total, 1e-12):.1f}% within 0.15 quad of a mesh edge, max distance {worst / quad:.2f} quads{' (over 4)' if worst == float('inf') else ''}, {near_poles} poles within 2 quads of a line")

if __name__ == '__main__':
    main()
