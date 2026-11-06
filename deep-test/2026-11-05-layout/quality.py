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
for fn in sys.argv[1:]:
    V,F=load(fn); bad=0; n=0
    for f in F:
        if len(f)!=4: continue
        n+=1; p=[V[i] for i in f]; ok=True
        L=[norm(sub(p[(k+1)%4],p[k])) for k in range(4)]
        if min(L)<=0 or max(L)/min(L)>3: ok=False
        for k in range(4):
            a=sub(p[(k+3)%4],p[k]); b=sub(p[(k+1)%4],p[k])
            na,nb=norm(a),norm(b)
            if na*nb==0: ok=False; break
            ang=math.degrees(math.acos(max(-1,min(1,dot(a,b)/(na*nb)))))
            if ang<45 or ang>135: ok=False
        if not ok: bad+=1
    print(f"{fn.split('/')[-1]}: {n} quads, {bad} badly shaped ({100*bad/max(n,1):.1f}%)")
