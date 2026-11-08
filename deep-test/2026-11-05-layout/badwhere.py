"""Badly shaped quads per T-pose region: badwhere.py OBJ [OBJ...] [--face]

Regions by quad centroid on the 2-unit T-pose (Y up, +Z front): feet y < -0.85; legs
y < -0.1; hands |x| > 0.62; arms |x| > 0.22 and y > 0.05; head y > 0.55; else torso.
--face splits the head into sub-regions, taken in this order so that each quad lands in
exactly one and the counts sum to the head total: cranium (y > 0.86 or z < 0.03), eyes
(0.78 < y < 0.86, |x| < 0.25), nose (|x| < 0.06, 0.70 < y < 0.80), mouth (0.72 < y < 0.78,
|x| < 0.15), cheek/jaw (0.60 < y < 0.78, 0.06 < |x| < 0.3), and 'other' for the rest of
the head (the neck front below y 0.60, the sides past |x| 0.3).
"""
import sys, math

def load(fn):
    V=[];F=[]
    for l in open(fn):
        if l.startswith('v '): V.append(tuple(map(float,l.split()[1:4])))
        elif l.startswith('f '): F.append([int(x)-1 for x in l.split()[1:]])
    return V,F
def sub(a,b): return (a[0]-b[0],a[1]-b[1],a[2]-b[2])
def dot(a,b): return a[0]*b[0]+a[1]*b[1]+a[2]*b[2]
def norm(a): return math.sqrt(dot(a,a))
def region(c):
    x,y,z=c
    if y < -0.85: return 'feet'
    if y < -0.1: return 'legs'
    if abs(x) > 0.62: return 'hands'
    if abs(x) > 0.22 and y > 0.05: return 'arms'
    if y > 0.55: return 'head'
    return 'torso'
def face_region(c):
    x,y,z=c
    if y > 0.86 or z < 0.03: return 'cranium'
    if 0.78 < y < 0.86 and abs(x) < 0.25: return 'eyes'
    if abs(x) < 0.06 and 0.70 < y < 0.80: return 'nose'
    if 0.72 < y < 0.78 and abs(x) < 0.15: return 'mouth'
    if 0.60 < y < 0.78 and 0.06 < abs(x) < 0.3: return 'cheek/jaw'
    return 'other'
FACE_ORDER = ['cranium', 'eyes', 'nose', 'mouth', 'cheek/jaw', 'other']
args = sys.argv[1:]
face = '--face' in args
args = [a for a in args if a != '--face']
for fn in args:
    V,F=load(fn); tot={}; bad={}; why={}; ftot={}; fbad={}
    for f in F:
        if len(f)!=4: continue
        p=[V[i] for i in f]; c=tuple(sum(q[k] for q in p)/4 for k in range(3)); r=region(c)
        tot[r]=tot.get(r,0)+1
        L=[norm(sub(p[(k+1)%4],p[k])) for k in range(4)]
        reason=None
        if min(L)<=0 or max(L)/min(L)>3: reason='ratio'
        else:
            for k in range(4):
                a=sub(p[(k+3)%4],p[k]); b=sub(p[(k+1)%4],p[k])
                ang=math.degrees(math.acos(max(-1,min(1,dot(a,b)/(norm(a)*norm(b))))))
                if ang<45 or ang>135: reason='angle'; break
        if reason:
            bad[r]=bad.get(r,0)+1; why[(r,reason)]=why.get((r,reason),0)+1
        if r == 'head':
            fr = face_region(c)
            ftot[fr]=ftot.get(fr,0)+1
            if reason: fbad[fr]=fbad.get(fr,0)+1
    print(fn.split('/')[-2])
    for r in sorted(tot): print(f"  {r:6s} {tot[r]:6d} quads, {bad.get(r,0):5d} bad ({100*bad.get(r,0)/tot[r]:.1f}%)  ratio {why.get((r,'ratio'),0)} angle {why.get((r,'angle'),0)}")
    if face:
        print(f"  head by sub-region ({sum(ftot.values())} quads, {sum(fbad.values())} bad):")
        for r in FACE_ORDER:
            if r in ftot: print(f"    {r:9s} {ftot[r]:6d} quads, {fbad.get(r,0):5d} bad ({100*fbad.get(r,0)/ftot[r]:.1f}%)")
