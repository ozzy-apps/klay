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
def bad(p):
    L=[norm(sub(p[(k+1)%4],p[k])) for k in range(4)]
    if min(L)<=0 or max(L)/min(L)>3: return True
    for k in range(4):
        a=sub(p[(k+3)%4],p[k]); b=sub(p[(k+1)%4],p[k])
        ang=math.degrees(math.acos(max(-1,min(1,dot(a,b)/(norm(a)*norm(b))))))
        if ang<45 or ang>135: return True
    return False
res={}
for fn in sys.argv[1:]:
    V,F=load(fn); t={}; b={}
    for f in F:
        if len(f)!=4: continue
        p=[V[i] for i in f]; c=[sum(q[k] for q in p)/4 for k in range(3)]
        if abs(c[0])>0.22 and c[1]>0.05: continue
        key=(round(c[1]*10)/10, 'L' if c[0]<0 else 'R')
        t[key]=t.get(key,0)+1
        if bad(p): b[key]=b.get(key,0)+1
    res[fn]=(t,b)
keys=sorted(set(k for fn in res for k in res[fn][0]))
print('y side  ' + '  '.join(f.split('/')[-2] for f in sys.argv[1:]))
for k in keys:
    print(f"{k[0]:5.1f} {k[1]}  " + '  '.join(f"{res[f][1].get(k,0):4d}/{res[f][0].get(k,0):4d}" for f in sys.argv[1:]))
