import sys, math
exec(open(sys.argv[0].replace('patchbad.py','badrows.py')).read().split('res={}')[0])
fn=sys.argv[1]; cx,cy,cz,r=map(float,sys.argv[2:6])
V=[];G={};g=None
for l in open(fn):
    if l.startswith('v '): V.append(tuple(map(float,l.split()[1:4])))
    elif l.startswith('g '): g=l.split()[1]; G.setdefault(g,[])
    elif l.startswith('f '): G[g].append([int(x)-1 for x in l.split()[1:]])
rows=[]
for g,F in G.items():
    near=0; bad_=0; n=0
    for f in F:
        p=[V[i] for i in f]; c=[sum(q[k] for q in p)/len(p) for k in range(3)]
        n+=1
        if (c[0]-cx)**2+(c[1]-cy)**2+(c[2]-cz)**2 < r*r:
            near+=1
            if len(f)!=4 or bad(p): bad_+=1
    if near: rows.append((bad_,near,n,g))
for r_ in sorted(rows,reverse=True)[:12]: print(r_)
