#!/usr/bin/env python3
"""Three deliberately non-Material UI compositions for native Klay."""
import importlib.util
import json
from pathlib import Path
import xml.etree.ElementTree as ET

OUT = Path(__file__).resolve().parent
BASE_PATH = OUT.parent / '2026-10-22' / 'generate_mockups.py'
spec = importlib.util.spec_from_file_location('klay_drawing', BASE_PATH)
base = importlib.util.module_from_spec(spec)
spec.loader.exec_module(base)
VIZ = Path('/home/ozzy/.codex/visualizations/2026/10/21/01a1609b-0a53-7680-81b9-8fcceabf7bb0/klay-non-material.html')

THEMES = [
    dict(slug='01-classic',name='Classic',base=13,
         bg='#202020',surface='#292929',raised='#383838',field='#1c1c1c',line='#555555',
         text='#f4f0e9',secondary='#c5c1ba',subtle='#a8a49e',accent='#edac71',
         selected='#edac71',onselected='#24180d',canvas='#191b1d',radius=0),
    dict(slug='02-precision',name='Precision',base=14,
         bg='#111719',surface='#192124',raised='#232e32',field='#11191c',line='#48585d',
         text='#edf4f3',secondary='#bccdce',subtle='#9bafb3',accent='#90d7ca',
         selected='#25483f',onselected='#d6fff5',canvas='#191b1d',radius=0),
    dict(slug='03-atelier',name='Atelier',base=15,
         bg='#292623',surface='#302d29',raised='#3b3630',field='#24211e',line='#655d53',
         text='#f7efe4',secondary='#d0c3b3',subtle='#b7a997',accent='#efb68e',
         selected='#4c3a2e',onselected='#ffe0c7',canvas='#191b1d',radius=0),
]

BRUSHES = [('Draw',11,142),('Smooth',92,142),('Grab',174,142),('Flatten',12,244),('Inflate',94,244)]

class Draft(base.UI):
    # All interface geometry is square. Circles remain only in sphere imagery.
    def rect(self,x,y,w,h,fill,radius=0,stroke=None,sw=1):
        super().rect(x,y,w,h,fill,0,stroke,sw)

    def button(self,x,y,w,h,label,active=False,icon=None,disabled=False):
        self.rect(x,y,w,h,'selected' if active else 'raised',stroke='accent' if active else 'line')
        if self.slug=='01-classic':
            self.line(x+1,y+1,x+w-1,y+1,'#ffd5a2' if active else '#646464')
            self.line(x+1,y+h-1,x+w-1,y+h-1,'#191919')
        if icon: self.icon(x+10,y+(h-18)/2,icon,'onselected' if active else 'secondary',18)
        self.text(x+(34 if icon else w/2),y+h/2+4.5,label,13,'onselected' if active else 'subtle' if disabled else 'text',600 if active else 400,None if icon else 'middle')

    def menu(self,height=36):
        self.rect(0,0,1600,height,'surface')
        self.add(f'<use href="#klay-brand" transform="translate(12,{(height-26)/2}) scale(.40625)"/>')
        self.text(47,height/2+5,'Klay',16,weight=600)
        x=112
        for label in ['Alpha','Brush','Document','Edit','Masking','Material','Mesh','Performance','Preferences','Stroke','Symmetry','Transform','View','Workspace']:
            self.text(x,height/2+4.5,label,12.5,'secondary')
            x+=len(label)*6.7+19
        self.text(1580,height/2+4.5,'Untitled · Sphere',12,'secondary',anchor='end')
        self.line(0,height-1,1600,height-1)

    def status(self):
        self.rect(0,974,1600,26,'surface')
        self.line(0,974,1600,974)
        self.rect(12,985,4,4,'accent')
        self.text(24,991,'Ready',11,'secondary')
        self.text(100,991,'240 002 vertices  /  480 000 triangles',11,'secondary',mono=True)
        self.text(1000,991,'Ctrl  Invert     Shift  Smooth     Tab  Focus',11,'secondary')
        self.text(1580,991,'5.9 ms / 170 fps',11,'secondary',anchor='end',mono=True)

    def scene(self,x,y,w,h,overlay=True):
        self.rect(x,y,w,h,'canvas')
        scale=min(w/862,h/790)
        iw=862*scale; ih=790*scale; ix=x+(w-iw)/2; iy=y+(h-ih)/2
        self.crop(ix,iy,iw,ih,384,146,862,790)
        self.rect(ix,iy,3,ih,'canvas')
        self.rect(ix+iw-3,iy,3,ih,'canvas')
        self.rect(ix+iw-100*scale,iy,100*scale,64*scale,'canvas')
        self.rect(ix,iy+ih-3*scale,iw,3*scale,'canvas')
        if overlay:
            self.text(x+18,y+28,'Perspective  /  Sphere',12,'#d2d3d4')
        gx=x+w-44; gy=y+47
        self.line(gx,gy,gx,gy-23,'#c4c7cb',1.2)
        self.line(gx,gy,gx+20,gy+12,'#efaa8b',1.2)
        self.line(gx,gy,gx-21,gy+14,'#97bde1',1.2)
        self.text(gx-4,gy-29,'Y',11,'#d5d8dc')
        self.text(gx+22,gy+20,'X',11,'#efaa8b')
        self.text(gx-30,gy+22,'Z',11,'#97bde1')

    def heading(self,x,y,w,label):
        self.rect(x,y,w,31,'raised')
        self.text(x+13,y+21,label,13,weight=600)
        self.text(x+w-14,y+20,'−',14,'secondary',anchor='end')
        self.line(x,y+30,x+w,y+30)

    def bar(self,x,y,w,label,value,fraction):
        self.text(x,y+12,label,13,'secondary')
        self.rect(x,y+23,w,25,'field',stroke='line')
        self.rect(x+1,y+24,(w-61)*fraction,23,'#51443b')
        self.line(x+w-60,y+24,x+w-60,y+47)
        self.text(x+w-8,y+40,value,12,'text',anchor='end',mono=True)
        px=x+(w-61)*fraction
        self.line(px,y+27,px,y+44,'accent',2)

    def tick_slider(self,x,y,w,label,value,fraction,open_style=False):
        self.text(x,y+12,label,15 if open_style else 14,'secondary')
        if not open_style:
            self.rect(x+w-78,y-8,78,32,'field',stroke='line')
        self.text(x+w-10,y+13,value,15,'text',anchor='end',mono=True)
        if open_style:
            self.line(x,y+34,x+w,y+34,'line',2)
            self.line(x,y+34,x+w*fraction,y+34,'accent',2)
            self.rect(x+w*fraction-3,y+29,6,10,'accent')
        else:
            for i in range(21):
                xx=x+i*w/20
                self.line(xx,y+37,xx,y+43+(5 if i%5==0 else 0),'line')
            self.line(x,y+37,x+w,y+37,'line')
            self.rect(x+w*fraction-3,y+32,6,10,'accent')

    def check(self,x,y,label,checked=False,size=13):
        self.rect(x,y,14,14,'accent' if checked else 'field',stroke='accent' if checked else 'line')
        if checked: self.icon(x+1,y+1,'check','onselected' if self.slug=='01-classic' else 'bg',12)
        self.text(x+24,y+12,label,size)

    def sphere(self,x,y,s,name):
        _,vx,vy=next(b for b in BRUSHES if b[0]==name)
        self.sphere_crop(x,y,s,vx,vy,68)

    def brush_tile(self,x,y,w,h,name,index,active=False):
        self.rect(x,y,w,h,'field',stroke='accent' if active else 'line')
        s=min(w-12,h-28)
        self.sphere(x+(w-s)/2,y+5,s,name)
        self.text(x+w-6,y+14,index,10,'secondary',anchor='end',mono=True)
        self.rect(x+1,y+h-23,w-2,22,'selected' if active else 'raised')
        self.text(x+w/2,y+h-7,name,12,'onselected' if active else 'text',600 if active else 400,'middle')

    def material(self,x,y,size,index=0):
        vx=[1350,1394,1437,1480][index]
        self.sphere_crop(x,y,size,vx,288,30)

    def classic(self):
        self.rect(0,0,1600,1000,'bg'); self.menu()
        self.rect(0,36,1600,68,'surface')
        for i,(label,icon) in enumerate([('Sculpt','sculpt'),('Move','move'),('Scale','scale'),('Rotate','rotate')]):
            xx=10+i*58
            self.button(xx,44,52,51,'',i==0)
            self.icon(xx+17,50,icon,'onselected' if i==0 else 'secondary',18)
            self.text(xx+26,86,label,11,'onselected' if i==0 else 'secondary',anchor='middle')
        self.line(253,45,253,95)
        self.button(270,47,63,42,'Zadd',True); self.button(338,47,63,42,'Zsub')
        self.text(421,61,'Draw',14,weight=600); self.text(421,84,'Freehand',12,'secondary')
        self.bar(555,44,230,'Draw size','0.25',.35)
        self.bar(810,44,230,'Z intensity','0.50',.5)
        self.button(1064,47,90,42,'Dynamic',True)
        self.button(1330,47,117,42,'Undo',icon='undo'); self.button(1455,47,131,42,'Redo',icon='redo')
        self.line(0,103,1600,103)
        # Familiar left tray, auxiliary shelf, canvas, view shelf, right tray.
        self.rect(0,108,244,866,'surface'); self.heading(0,108,244,'Brush')
        for i,(name,_,_) in enumerate(BRUSHES): self.brush_tile(10+i%3*77,151+i//3*96,70,88,name,i+1,i==0)
        self.heading(0,354,244,'Modifiers')
        self.bar(12,401,220,'Radius','0.25',.35); self.bar(12,466,220,'Strength','0.50',.5)
        self.button(12,533,106,30,'Add',True); self.button(124,533,108,30,'Subtract')
        self.bar(12,589,220,'Falloff','0.50',.5)
        self.heading(0,674,244,'Symmetry')
        self.text(12,730,'Unavailable',12,'subtle')
        self.heading(0,765,244,'Masking'); self.text(12,821,'Unavailable',12,'subtle')
        self.line(243,108,243,974)
        self.rect(250,108,84,866,'surface')
        for j,(label,ic) in enumerate([('Draw',None),('Freehand','freehand'),('Alpha off','alpha'),('Texture off','texture'),('Warm clay',None)]):
            yy=119+j*127
            self.rect(257,yy,70,89,'field',stroke='accent' if j==0 else 'line')
            if j==0:self.sphere(260,yy+11,64,'Draw')
            elif j==4:self.sphere_crop(260,yy+11,64,282,511,68)
            else:self.icon(275,yy+26,ic,'secondary',33)
            self.text(292,yy+109,label,11.5,'text',anchor='middle')
        self.rect(260,773,64,64,'url(#klay-color)'); self.rect(260,773,64,64,'url(#klay-dark)')
        self.text(292,858,'#F1C7A9',11,'secondary',anchor='middle',mono=True)
        self.button(257,879,70,29,'Swap')
        self.scene(340,108,908,866)
        self.rect(1254,108,60,866,'surface')
        for j,(label,ic,on) in enumerate([('Frame','frame',False),('Persp','persp',True),('Floor','floor',True),('Local','local',False),('Detail','detail',False),('Hide','hide',False)]):
            yy=118+j*70
            self.button(1260,yy,48,59,'',on)
            self.icon(1274,yy+9,ic,'onselected' if on else 'secondary',20)
            self.text(1284,yy+46,label,11,'onselected' if on else 'secondary',anchor='middle')
        self.rect(1320,108,280,866,'surface'); self.line(1320,108,1320,974)
        self.heading(1320,108,280,'Subtools')
        self.rect(1332,152,256,35,'raised',stroke='line')
        self.rect(1332,152,3,35,'accent'); self.icon(1343,160,'cube','secondary',18)
        self.text(1372,175,'Sphere',14); self.icon(1560,160,'eye','secondary',18)
        self.button(1332,201,124,30,'Append'); self.button(1464,201,124,30,'Duplicate')
        self.heading(1320,256,280,'Material')
        for j in range(4):
            self.rect(1332+j*61,301,52,52,'field',stroke='accent' if j==0 else 'line')
            self.material(1338+j*61,307,40,j)
        self.text(1332,379,'Warm clay',13)
        self.check(1332,403,'Surface detail shading'); self.check(1332,434,'Floor grid',True)
        self.heading(1320,485,280,'Mesh')
        for j,(k,v) in enumerate([('Object','Sphere'),('Vertices','240 002'),('Triangles','480 000'),('Chunks','64')]):self.kv(1332,542+j*31,256,k,v)
        self.line(1332,660,1588,660)
        self.bar(1332,685,256,'Segments','200',.5)
        self.button(1332,751,256,34,'Rebuild sphere')
        self.heading(1320,819,280,'History'); self.text(1332,879,'Undo available',13,'secondary')
        self.status(); return '\n'.join(self.p)

    def precision(self):
        self.rect(0,0,1600,1000,'bg'); self.menu(34)
        self.rect(0,34,1600,46,'surface'); self.line(0,79,1600,79)
        self.text(20,63,'SCULPT',12,'accent',600)
        self.line(95,44,95,69)
        self.text(115,63,'Draw / Freehand',14)
        self.button(303,42,72,30,'Zadd',True); self.button(380,42,72,30,'Zsub')
        self.text(484,63,'Radius',13,'secondary'); self.text(551,63,'0.25',13,mono=True)
        self.text(635,63,'Strength',13,'secondary'); self.text(717,63,'0.50',13,mono=True)
        self.line(800,44,800,69)
        self.text(821,63,'Dynamic radius',13,'accent')
        self.button(1336,42,117,30,'Undo',icon='undo'); self.button(1461,42,123,30,'Redo',icon='redo')
        self.rect(0,80,72,894,'surface'); self.line(71,80,71,974)
        for j,(label,ic) in enumerate([('Sculpt','sculpt'),('Move','move'),('Scale','scale'),('Rotate','rotate')]):
            yy=94+j*80
            if j==0:self.rect(0,yy,3,64,'accent');self.rect(4,yy,64,64,'raised')
            self.icon(25,yy+9,ic,'accent' if j==0 else 'secondary',23)
            self.text(36,yy+53,label,11,'text',anchor='middle')
        for j,(label,ic) in enumerate([('Frame','frame'),('Persp','persp'),('Floor','floor')]):
            yy=607+j*78; self.icon(26,yy,ic,'accent' if j else 'secondary',21)
            self.text(36,yy+42,label,11,'secondary',anchor='middle')
        self.scene(80,80,1176,772)
        self.rect(1264,80,336,894,'surface'); self.line(1264,80,1264,974)
        for j,label in enumerate(['Brush','Stroke','Mesh']):
            self.text(1290+j*106,105,label,13,'text' if j==0 else 'secondary',600 if j==0 else 400)
        self.line(1264,117,1600,117); self.line(1278,117,1368,117,'accent',2)
        self.text(1284,158,'BRUSH',11,'secondary',600)
        self.text(1284,196,'Draw',26,weight=500); self.text(1579,193,'01',15,'accent',anchor='end',mono=True)
        self.line(1284,218,1580,218)
        self.tick_slider(1284,250,296,'Radius','0.25',.35)
        self.tick_slider(1284,343,296,'Strength','0.50',.5)
        self.tick_slider(1284,436,296,'Falloff','0.50',.5)
        self.button(1284,516,144,34,'Add',True); self.button(1436,516,144,34,'Subtract')
        self.line(1284,578,1580,578); self.text(1284,605,'STROKE',11,'secondary',600)
        for j,(label,value) in enumerate([('Type','Freehand'),('Alpha','Off'),('Texture','Off')]):
            self.text(1284,642+j*43,label,14,'secondary'); self.text(1580,642+j*43,value,14,anchor='end')
            self.line(1284,656+j*43,1580,656+j*43)
        self.text(1284,794,'SYMMETRY',11,'secondary',600); self.text(1580,794,'Unavailable',12,'subtle',anchor='end')
        self.line(1284,817,1580,817)
        self.text(1284,854,'Active points',14,'secondary');self.text(1580,854,'240 002',14,anchor='end',mono=True)
        self.text(1284,893,'Triangles',14,'secondary');self.text(1580,893,'480 000',14,anchor='end',mono=True)
        # Brush library is a bottom rack, not a second vertical tray.
        self.rect(80,858,1176,116,'surface'); self.line(80,858,1256,858)
        self.text(96,887,'BRUSHES',11,'secondary',600)
        for j,(label,_,_) in enumerate(BRUSHES):
            xx=191+j*139
            if j==0:self.rect(xx-7,869,131,94,'raised');self.line(xx-7,963,xx+124,963,'accent',2)
            self.sphere(xx+7,875,52,label); self.text(xx+82,897,j+1,11,'secondary',mono=True)
            self.text(xx+7,946,label,13,'text',600 if j==0 else 400)
        self.line(915,876,915,957)
        self.text(938,886,'MATERIAL',11,'secondary',600)
        self.material(939,904,42); self.text(995,930,'Warm clay',13)
        self.status(); return '\n'.join(self.p)

    def atelier(self):
        self.rect(0,0,1600,1000,'bg'); self.menu(44)
        self.rect(0,44,1600,50,'surface');self.line(0,93,1600,93)
        for j,label in enumerate(['Sculpt','Move','Scale','Rotate']):
            xx=18+j*87; self.text(xx,75,label,14,'text' if j==0 else 'secondary',500 if j==0 else 400)
            if j==0:self.line(xx,93,xx+52,93,'accent',2)
        self.line(381,58,381,80)
        self.text(409,75,'Radius',14,'secondary');self.text(533,75,'0.25',15,mono=True)
        self.line(524,83,584,83)
        self.text(627,75,'Strength',14,'secondary');self.text(753,75,'0.50',15,mono=True)
        self.line(745,83,808,83)
        self.text(853,75,'Zadd',14,'accent',600);self.text(929,75,'Zsub',14,'secondary')
        self.text(1362,75,'Undo',14,'secondary'); self.text(1456,75,'Redo',14,'secondary')
        # Open brush rows. Selection is a line and typography, not a tile system.
        self.rect(0,94,186,880,'surface');self.line(185,94,185,974)
        self.text(18,131,'Brushes',16,weight=500)
        for j,(label,_,_) in enumerate(BRUSHES):
            yy=155+j*80
            if j==0:self.rect(0,yy-5,3,65,'accent');self.rect(7,yy-5,171,65,'raised')
            self.sphere(15,yy+3,43,label)
            self.text(70,yy+25,label,14,'accent' if j==0 else 'text',500 if j==0 else 400)
            self.text(167,yy+25,j+1,10,'secondary',anchor='end',mono=True)
        self.line(18,591,168,591)
        self.text(18,628,'Material',15,weight=500)
        self.sphere_crop(18,648,55,282,511,68); self.text(18,730,'Warm clay',14)
        for j in range(4):
            self.material(18+j*39,751,28,j)
            if j==0:self.line(18,789,46,789,'accent',2)
        self.rect(18,831,20,20,'#f1c7a9');self.text(49,846,'#F1C7A9',11,'secondary',mono=True)
        self.scene(193,94,1047,880,False)
        self.text(214,126,'Sphere / Perspective',12,'#d2d3d4')
        for j,label in enumerate(['Frame','Persp','Floor']):
            xx=210+j*80;self.text(xx,945,label,13,'#f0d0b8' if j else '#d2d3d4')
            if j:self.line(xx,954,xx+40,954,'#e0ad89')
        # A single quiet inspector, organized with spacing and rules.
        self.rect(1248,94,352,880,'surface'); self.line(1248,94,1248,974)
        self.text(1273,135,'Brush',17,weight=500)
        self.text(1273,184,'Draw',28,weight=400);self.text(1574,182,'01',13,'secondary',anchor='end',mono=True)
        self.text(1273,217,'Freehand',14,'secondary');self.line(1273,240,1574,240)
        self.tick_slider(1273,271,301,'Radius','0.25',.35,True)
        self.tick_slider(1273,356,301,'Strength','0.50',.5,True)
        self.text(1273,454,'Add',15,'accent',500);self.line(1273,468,1390,468,'accent',2)
        self.text(1446,454,'Subtract',15,'secondary');self.line(1410,468,1574,468)
        self.line(1273,504,1574,504)
        for j,(label,value) in enumerate([('Stroke','Freehand'),('Alpha','Off'),('Texture','Off')]):
            yy=541+j*47;self.text(1273,yy,label,15,'secondary');self.text(1574,yy,value,15,anchor='end')
        self.line(1273,662,1574,662)
        self.text(1273,705,'Subtools',17,weight=500)
        self.rect(1273,727,301,41,'raised');self.line(1273,727,1273,768,'accent',2)
        self.text(1287,754,'Sphere',15);self.icon(1544,739,'eye','secondary',18)
        self.text(1273,808,'Vertices',14,'secondary');self.text(1574,808,'240 002',13,anchor='end',mono=True)
        self.text(1273,845,'Triangles',14,'secondary');self.text(1574,845,'480 000',13,anchor='end',mono=True)
        self.line(1273,873,1574,873)
        self.text(1273,915,'Append',14);self.text(1435,915,'Duplicate',14)
        self.status(); return '\n'.join(self.p)

def main():
    defs=base.shared_defs(); designs=[]; metrics=[]; bodies=[]
    for t,method in zip(THEMES,['classic','precision','atelier']):
        body=getattr(Draft(t),method)();bodies.append(body)
        title=f'Klay — {t["name"]} — non-Material readability mockup'
        svg=f'<svg xmlns="http://www.w3.org/2000/svg" width="1600" height="1000" viewBox="0 0 1600 1000" role="img" aria-label="{title}"><title>{title}</title>{defs}{body}</svg>'
        tree=ET.fromstring(svg)
        assert all(float(e.attrib.get('rx',0))==0 for e in tree.iter('{http://www.w3.org/2000/svg}rect'))
        (OUT/f'{t["slug"]}.svg').write_text(svg)
        base.render(svg,OUT/f'{t["slug"]}.png')
        dx,dy,dw,dh = {'classic':(0,108,244,866),'precision':(1264,80,336,894),'atelier':(1248,94,352,880)}[method]
        detail=f'<svg xmlns="http://www.w3.org/2000/svg" width="{dw}" height="{dh}" viewBox="{dx} {dy} {dw} {dh}">{defs}{body}</svg>'
        base.render(detail,OUT/f'{t["slug"]}-controls.png',dw,dh)
        designs.append(f'<svg xmlns="http://www.w3.org/2000/svg" class="klay-nm-design" data-concept="{t["slug"]}" viewBox="0 0 1600 1000" role="img" aria-label="{title}"'+(' hidden="hidden"' if len(designs) else '')+f'><title>{title}</title>{body}</svg>')
        metrics.append(dict(concept=t['name'],primary=base.contrast(t['text'],t['surface']),secondary=base.contrast(t['secondary'],t['surface']),selected=base.contrast(t['onselected'],t['selected'])))
    fragment=(OUT/'comparison.fragment.html').read_text()
    inline=fragment.replace('<!--KLAY_SHARED_DEFS-->',f'<svg width="0" height="0" aria-hidden="true" style="position:absolute;overflow:hidden">{defs}</svg>').replace('<!--KLAY_MOCKUPS-->','\n'.join(designs))
    assert len(inline.encode())<1_000_000 and '<!--KLAY_' not in inline
    VIZ.write_text(inline)
    (OUT/'contrast.json').write_text(json.dumps(metrics,indent=2)+'\n')
    sheet=['<svg xmlns="http://www.w3.org/2000/svg" width="1680" height="3250" viewBox="0 0 1680 3250">',defs,'<rect width="1680" height="3250" fill="#171717"/>']
    for i,(t,body) in enumerate(zip(THEMES,bodies)):
        y=i*1080+42
        sheet.append(f'<text x="40" y="{y}" font-family="Inter, sans-serif" font-size="22" fill="#eee8df">{i+1:02d}  {t["name"]}</text><svg x="40" y="{y+20}" width="1600" height="1000" viewBox="0 0 1600 1000">{body}</svg>')
    sheet.append('</svg>')
    base.render(''.join(sheet),OUT/'comparison.png',1680,3250)
    print(json.dumps(dict(mockups=3,inline_bytes=len(inline.encode()),contrast=metrics),indent=2))

if __name__=='__main__':main()
