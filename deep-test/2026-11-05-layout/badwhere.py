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
for fn in sys.argv[1:]:
    V,F=load(fn); tot={}; bad={}; why={}
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
    print(fn.split('/')[-2])
    for r in sorted(tot): print(f"  {r:6s} {tot[r]:6d} quads, {bad.get(r,0):5d} bad ({100*bad.get(r,0)/tot[r]:.1f}%)  ratio {why.get((r,'ratio'),0)} angle {why.get((r,'angle'),0)}")
