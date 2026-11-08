"""Spike check for remesh output: edges over 4x the median length, folded quads (the
quad's two triangle normals pointing more than 90 degrees apart) and collapsed faces
(under 1% of the median face area; Blender's wireframe shoots spikes out of them)."""
import sys, math
def sub(a,b): return (a[0]-b[0],a[1]-b[1],a[2]-b[2])
def cross(a,b): return (a[1]*b[2]-a[2]*b[1],a[2]*b[0]-a[0]*b[2],a[0]*b[1]-a[1]*b[0])
def dot(a,b): return a[0]*b[0]+a[1]*b[1]+a[2]*b[2]
for fn in sys.argv[1:]:
    V=[];F=[]
    for l in open(fn):
        if l.startswith('v '): V.append(tuple(map(float,l.split()[1:4])))
        elif l.startswith('f '): F.append([int(x)-1 for x in l.split()[1:]])
    E={}
    for f in F:
        for k in range(len(f)):
            a,b=f[k],f[(k+1)%len(f)]; E[(min(a,b),max(a,b))]=math.dist(V[a],V[b])
    L=sorted(E.values()); med=L[len(L)//2]
    long=[e for e,d in E.items() if d>4*med]
    folds=0
    def area(f):
        p=[V[i] for i in f]
        a=0.0
        for k in range(1,len(p)-1):
            c=cross(sub(p[k],p[0]),sub(p[k+1],p[0])); a+=math.sqrt(dot(c,c))*0.5
        return a
    A=sorted(area(f) for f in F); amed=A[len(A)//2]
    tiny=sum(1 for a in A if a<amed*0.01)
    for f in F:
        if len(f)!=4 or len(set(f))<4: continue
        p=[V[i] for i in f]
        n1=cross(sub(p[1],p[0]),sub(p[2],p[0])); n2=cross(sub(p[2],p[0]),sub(p[3],p[0]))
        if dot(n1,n2)<0: folds+=1
    print(f"{'/'.join(fn.split('/')[-2:])}: {len(F)} faces, {len(long)} edges over 4x median (longest {L[-1]/med:.1f}x), {folds} folded quads, {tiny} collapsed faces")
