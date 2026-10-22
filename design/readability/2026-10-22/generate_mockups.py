#!/usr/bin/env python3
"""Editable UI proposals. Reuse the native render; draw new interface in SVG."""
import base64
import html
import json
from pathlib import Path
import xml.etree.ElementTree as ET

import cairo
import gi
gi.require_version("Rsvg", "2.0")
from gi.repository import Rsvg

OUT = Path(__file__).resolve().parent
PROJECT = OUT.parents[2]
VIZ = Path("/home/ozzy/.codex/visualizations/2026/10/21/01a1609b-0a53-7680-81b9-8fcceabf7bb0/klay-readability.html")

THEMES = [
    dict(slug="01-graphite", name="Graphite", base=13, left=258, right=276,
         bg="#1c1e21", surface="#24262a", raised="#303238", field="#36393f",
         line="#494c53", text="#f4f0ed", secondary="#c2bec2", subtle="#a5a3ab",
         accent="#ffb59d", selected="#604237", onselected="#ffdbc9", canvas="#191b1d", radius=5),
    dict(slug="02-slate", name="Slate", base=14, left=280, right=296,
         bg="#1b2028", surface="#252c36", raised="#303a47", field="#364252",
         line="#536072", text="#f1f3f7", secondary="#c1cbd9", subtle="#a8b4c5",
         accent="#ffbea7", selected="#62483e", onselected="#ffe0d2", canvas="#191b1d", radius=10),
    dict(slug="03-light-studio", name="Light Studio", base=14, left=280, right=296,
         bg="#e6e4e1", surface="#f6f3ef", raised="#e9e5e0", field="#ffffff",
         line="#c6bfb8", text="#282525", secondary="#57514d", subtle="#68615c",
         accent="#8b402e", selected="#ffdbce", onselected="#542013", canvas="#191b1d", radius=8),
]

def shared_defs():
    native = base64.b64encode((PROJECT / "ui-native.png").read_bytes()).decode()
    brand = base64.b64encode((PROJECT / "assets/icon/klay-64.png").read_bytes()).decode()
    return f'''<defs>
      <image id="klay-native" href="data:image/png;base64,{native}" width="1600" height="1000"/>
      <image id="klay-brand" href="data:image/png;base64,{brand}" width="64" height="64"/>
      <linearGradient id="klay-color" x1="0" x2="1"><stop stop-color="#fff"/><stop offset="1" stop-color="#e99059"/></linearGradient>
      <linearGradient id="klay-dark" x2="0" y2="1"><stop stop-color="#000" stop-opacity="0"/><stop offset="1" stop-color="#000"/></linearGradient>
    </defs>'''

class UI:
    def __init__(self, theme):
        self.t = theme
        self.p = []
        self.slug = theme["slug"]
        self.clipcount = 0

    def add(self, value): self.p.append(value)

    def rect(self, x, y, w, h, fill, radius=0, stroke=None, sw=1):
        self.add(f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{radius}" fill="{self.t.get(fill, fill)}"' + (f' stroke="{self.t.get(stroke,stroke)}" stroke-width="{sw}"' if stroke else '') + '/>')

    def text(self, x, y, s, size=None, color="text", weight=400, anchor=None, mono=False):
        family = "JetBrains Mono, monospace" if mono else "Inter, DejaVu Sans, sans-serif"
        self.add(f'<text x="{x}" y="{y}" fill="{self.t.get(color,color)}" font-family="{family}" font-size="{size or self.t["base"]}" font-weight="{weight}"' + (f' text-anchor="{anchor}"' if anchor else '') + f'>{html.escape(str(s))}</text>')

    def line(self, x1, y1, x2, y2, color="line", width=1):
        self.add(f'<path d="M{x1} {y1}L{x2} {y2}" fill="none" stroke="{self.t.get(color,color)}" stroke-width="{width}"/>')

    def icon(self, x, y, name, color="secondary", size=20):
        paths = {
            "sculpt": 'M3 18C7 18 8 13 10 4C11 2 12 3 13 8C14 14 17 17 21 17',
            "move": 'M12 2V22M2 12H22M8 6L12 2L16 6M8 18L12 22L16 18M6 8L2 12L6 16M18 8L22 12L18 16',
            "scale": 'M5 9V20H16M9 15L21 3M14 3H21V10',
            "rotate": 'M19 7A8 8 0 1 0 21 14M19 2V7H14',
            "frame": 'M3 8V3H8M16 3H21V8M21 16V21H16M8 21H3V16M8 8H16V16H8Z',
            "persp": 'M6 3H18L22 21H2ZM10 3L8 21M14 3L16 21',
            "floor": 'M5 5H19L23 19H1ZM3 13H21M12 5V19',
            "sym": 'M12 2V22M8 6L2 12L8 18ZM16 6L22 12L16 18Z',
            "local": 'M12 3A9 9 0 1 0 12 21A9 9 0 1 0 12 3M9 9H15V15H9Z',
            "detail": 'M2 8Q6 2 10 8T18 8T24 8M2 17Q6 11 10 17T18 17T24 17',
            "hide": 'M2 12Q12 1 22 12Q12 23 2 12M3 22L21 2',
            "undo": 'M8 5L3 10L8 15M3 10H15Q23 10 20 19',
            "redo": 'M16 5L21 10L16 15M21 10H9Q1 10 4 19',
            "freehand": 'M2 14C5 22 7 2 11 11S18 20 22 10',
            "alpha": 'M3 3H21V21H3ZM7 17L17 7M7 10H7.1M14 17H14.1',
            "texture": 'M3 3H21V21H3ZM9 3V21M15 3V21M3 9H21M3 15H21',
            "chevron": 'M5 9L12 16L19 9',
            "check": 'M4 12L9 17L20 6',
            "eye": 'M2 12Q12 1 22 12Q12 23 2 12M12 8A4 4 0 1 0 12 16A4 4 0 1 0 12 8',
            "save": 'M4 3H17L21 7V21H3V3ZM7 3V10H17V3M7 21V14H17V21',
            "cube": 'M12 2L22 7V18L12 23L2 18V7ZM2 7L12 12L22 7M12 12V23',
        }
        self.add(f'<g transform="translate({x},{y}) scale({size/24})" stroke="{self.t.get(color,color)}" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" fill="none"><path d="{paths.get(name, paths["cube"])}"/></g>')

    def dot(self, x, y, r=2, color="secondary"):
        self.add(f'<circle cx="{x}" cy="{y}" r="{r}" fill="{self.t.get(color,color)}"/>')

    def more(self, x, y):
        for dx in [-5,0,5]: self.dot(x+dx,y,1.3)

    def crop(self, x, y, w, h, vx, vy, vw, vh):
        self.add(f'<svg x="{x}" y="{y}" width="{w}" height="{h}" viewBox="{vx} {vy} {vw} {vh}" preserveAspectRatio="xMidYMid slice" overflow="hidden"><use href="#klay-native"/></svg>')

    def sphere_crop(self, x, y, size, vx, vy, source_size):
        self.clipcount += 1
        clip_id = f'{self.slug}-sphere-{self.clipcount}'
        self.add(f'<defs><clipPath id="{clip_id}"><circle cx="{x+size/2}" cy="{y+size/2}" r="{size/2}"/></clipPath></defs><g clip-path="url(#{clip_id})">')
        self.crop(x,y,size,size,vx,vy,source_size,source_size)
        self.add('</g>')

    def button(self, x, y, w, h, label, active=False, icon=None, disabled=False):
        self.rect(x,y,w,h,"selected" if active else "field",self.t["radius"],"accent" if active else "line",1)
        if icon:
            self.icon(x+12,y+(h-18)/2,icon,"onselected" if active else "subtle" if disabled else "secondary",18)
        self.text(x+(w/2 if not icon else 38),y+h/2+4.5,label,color="onselected" if active else "subtle" if disabled else "text",weight=600 if active else 400,anchor="middle" if not icon else None)

    def header(self, x, y, w, label, opened=True):
        self.rect(x,y,w,38,"raised")
        self.icon(x+10,y+12,"chevron",size=13)
        self.text(x+32,y+24,label,weight=600)
        self.more(x+w-18,y+19)

    def subhead(self, x, y, label, suffix=None):
        self.text(x,y,label,weight=600)
        if suffix: self.text(x+210,y,suffix,11,"subtle",anchor="end")

    def slider(self, x, y, w, label, value, fraction, unit=""):
        self.text(x,y+12,label,color="secondary")
        self.rect(x+w-62,y-5,62,28,"field",5,"line")
        self.text(x+w-8,y+13,f'{value}{unit}',12,color="text",anchor="end",mono=True)
        self.rect(x,y+34,w,4,"line",2)
        self.rect(x,y+34,w*fraction,4,"accent",2)
        self.dot(x+w*fraction,y+36,5,"accent")

    def checkbox(self, x, y, label, checked=False, disabled=False):
        self.rect(x,y,16,16,"selected" if checked else "field",3,"accent" if checked else "line")
        if checked: self.icon(x+1,y+1,"check","onselected",14)
        self.text(x+26,y+13,label,color="subtle" if disabled else "text")

    def kv(self, x, y, w, label, value):
        self.text(x,y,label,color="secondary")
        self.text(x+w,y,value,12,color="text",anchor="end",mono=True)

    def brush(self,x,y,w,h,name,index,active=False):
        self.rect(x,y,w,h,"selected" if active else "field",self.t["radius"],"accent" if active else "line")
        coords = {"Draw":(11,142),"Smooth":(92,142),"Grab":(174,142),"Flatten":(12,244),"Inflate":(94,244)}
        vx,vy=coords[name]
        s=min(w-12,h-30)
        self.sphere_crop(x+(w-s)/2,y+5,s,vx,vy,68)
        self.text(x+w-8,y+17,index,10,"onselected" if active else "secondary",anchor="end",mono=True)
        self.text(x+w/2,y+h-10,name,12,"onselected" if active else "text",600 if active else 400,"middle")

    def draw(self):
        t=self.t
        roomy=t["slug"]!="01-graphite"
        left=t["left"]; right=t["right"]; shelf=92; nav=64
        start=124; bottom=972
        cx=left+shelf+8; rx=1600-right; nx=rx-nav-8; cw=nx-cx-8
        self.rect(0,0,1600,1000,"bg")
        # Functional menu bar, with the approved branding.
        self.rect(0,0,1600,40,"surface")
        self.add('<use href="#klay-brand" transform="translate(12,6) scale(.4375)"/>')
        self.text(48,26,"Klay",16,weight=600)
        x=105
        for name in ["Alpha","Brush","Document","Edit","Masking","Material","Mesh","Performance","Preferences","Stroke","Symmetry","Transform","View","Workspace"]:
            self.text(x,25,name,12 if not roomy else 12.5,color="text")
            x += len(name)*6.6+20
        self.text(1582,25,"Untitled · Sphere",12,"secondary",anchor="end")
        self.line(0,39,1600,39)
        # Top shelf: labels and numbers remain outside of the slider tracks.
        self.rect(0,40,1600,80,"surface")
        for i,(name,ic) in enumerate([("Sculpt","sculpt"),("Move","move"),("Scale","scale"),("Rotate","rotate")]):
            xx=12+i*62
            self.rect(xx,50,56,60,"selected" if i==0 else "raised",t["radius"],"accent" if i==0 else None)
            self.icon(xx+18,59,ic,"onselected" if i==0 else "secondary",20)
            self.text(xx+28,100,name,11.5,"onselected" if i==0 else "secondary",600 if i==0 else 400,"middle")
        self.line(272,52,272,108)
        self.text(292,69,"Draw",14,weight=600)
        self.text(292,95,"Freehand",12,"secondary")
        self.button(388,54,64,48,"Zadd",True)
        self.button(458,54,64,48,"Zsub")
        self.line(542,52,542,108)
        self.slider(562,61,210,"Draw size","0.25",.35)
        self.slider(800,61,210,"Z intensity","0.50",.5)
        self.button(1034,54,86,48,"Dynamic",True)
        self.line(1142,52,1142,108)
        self.button(1162,54,94,48,"Undo",icon="undo")
        self.button(1264,54,94,48,"Redo",icon="redo")
        self.button(1444,54,140,48,"Save project",icon="save")
        self.line(0,119,1600,119)
        # Left docking tray.
        self.rect(0,start,left,bottom-start,"surface")
        self.header(0,start,left,"Brush")
        if not roomy:
            for i,n in enumerate(["Draw","Smooth","Grab","Flatten","Inflate"]):
                self.brush(12+(i%3)*79,174+(i//3)*101,74,94,n,i+1,i==0)
            sy=406
        else:
            for i,n in enumerate(["Draw","Smooth","Grab","Flatten","Inflate"]):
                self.brush(14+(i%2)*129,176+(i//2)*106,119,98,n,i+1,i==0)
            sy=524
        self.text(14,sy-10,"Ctrl · Invert   /   Shift · Smooth",11,"secondary")
        self.line(14,sy+6,left-14,sy+6)
        self.subhead(14,sy+32,"Modifiers")
        self.slider(14,sy+54,left-28,"Radius","0.25",.35)
        self.slider(14,sy+120,left-28,"Strength","0.50",.5)
        self.button(14,sy+182,(left-34)/2,34,"Add",True)
        self.button(left/2+3,sy+182,(left-34)/2,34,"Subtract")
        self.line(14,sy+237,left-14,sy+237)
        self.text(14,sy+265,"Falloff",color="secondary")
        self.text(left-14,sy+265,"0.50",12,"secondary",anchor="end",mono=True)
        # Collapsed unsupported palettes stay readable, with state in text.
        fy=sy+293
        for j,label in enumerate(["Auto masking","Symmetry"]):
            self.rect(0,fy+j*44,left,42,"raised")
            self.text(16,fy+j*44+26,label,weight=500)
            self.text(left-14,fy+j*44+26,"Unavailable",11,"subtle",anchor="end")
        self.line(left,start,left,bottom)
        # Fixed ZBrush-style shelf.
        sx=left+6
        self.rect(sx,start,shelf,bottom-start,"surface")
        tiles=[("Draw",None), ("Freehand","freehand"), ("Alpha off","alpha"), ("Texture off","texture"), ("Warm clay",None)]
        for j,(label,ic) in enumerate(tiles):
            yy=start+10+j*123
            self.rect(sx+6,yy,80,88,"raised",t["radius"],"accent" if j==0 else "line")
            if j==0: self.sphere_crop(sx+12,yy+9,68,11,142,68)
            elif j==4: self.sphere_crop(sx+12,yy+9,68,282,511,68)
            else: self.icon(sx+29,yy+30,ic,"secondary",34)
            self.text(sx+46,yy+107,label,11.5,"text",anchor="middle")
        color_y=start+635
        self.rect(sx+6,color_y,80,80,"field",5,"line")
        self.rect(sx+7,color_y+1,78,78,"url(#klay-color)",4)
        self.rect(sx+7,color_y+1,78,78,"url(#klay-dark)",4)
        self.add(f'<circle cx="{sx+62}" cy="{color_y+19}" r="5" stroke="#fff" stroke-width="1.5" fill="none"/>')
        self.rect(sx+8,color_y+91,22,22,"#f1c7a9",3,"line")
        self.text(sx+46,color_y+132,"#F1C7A9",11,"secondary",anchor="middle",mono=True)
        self.button(sx+6,color_y+146,80,30,"Swap")
        # Identical real native viewport in all three proposals.
        self.rect(cx,start,cw,bottom-start,"canvas")
        self.crop(cx,start,cw,bottom-start,370,112,902,832)
        self.rect(cx+cw-99,start,99,64,"canvas")
        self.rect(cx,bottom-3,cw,3,"canvas")
        self.rect(cx+14,start+14,121,30,"#2b2e32",6,"#55585e")
        self.text(cx+25,start+34,"Perspective",12,"#e3e1de")
        self.text(cx+148,start+34,"/ Sphere",12,"#bcbec3")
        # Viewport orientation gizmo, clear axis labels.
        gx=cx+cw-49; gy=start+48
        self.line(gx,gy,gx,gy-22,"#c4c7cb",1.4)
        self.line(gx,gy,gx+21,gy+12,"#efaa8b",1.4)
        self.line(gx,gy,gx-20,gy+15,"#97bde1",1.4)
        self.text(gx-4,gy-29,"Y",11,"#d5d8dc")
        self.text(gx+24,gy+18,"X",11,"#efaa8b")
        self.text(gx-31,gy+21,"Z",11,"#97bde1")
        self.rect(cx+14,bottom-45,202,29,"#26292d",5)
        self.text(cx+24,bottom-26,"Draw  ·  R 0.25  ·  S 0.50",12,"#d4d4d8")
        # Fixed navigation shelf.
        self.rect(nx,start,nav,bottom-start,"surface")
        navs=[("Frame","frame",False), ("Persp","persp",True), ("Floor","floor",True), ("Sym","sym",False), ("Local","local",False), ("Detail","detail",False), ("Hide","hide",False)]
        for j,(name,ic,on) in enumerate(navs):
            yy=start+10+j*73
            self.rect(nx+5,yy,54,65,"selected" if on else "raised",t["radius"],"accent" if on else None)
            self.icon(nx+21,yy+10,ic,"onselected" if on else "secondary",21)
            self.text(nx+32,yy+51,name,11,"onselected" if on else "secondary",anchor="middle")
        # Right tray: consistent hierarchy and aligned numeric values.
        self.rect(rx,start,right,bottom-start,"surface")
        self.line(rx,start,rx,bottom)
        self.header(rx,start,right,"Subtools")
        xx=rx+14; ww=right-28
        self.rect(xx,start+52,ww,45,"selected",t["radius"])
        self.icon(xx+11,start+65,"cube","onselected",18)
        self.text(xx+40,start+80,"Sphere",weight=600,color="onselected")
        self.icon(rx+right-43,start+66,"eye","onselected",18)
        self.kv(xx,start+124,ww,"Active points","240 002")
        self.button(xx,start+144,(ww-8)/2,32,"Append")
        self.button(xx+(ww+8)/2,start+144,(ww-8)/2,32,"Duplicate")
        self.header(rx,start+195,right,"Material")
        for j,(vx,vy) in enumerate([(1350,288),(1394,288),(1437,288),(1480,288)]):
            bx=xx+j*57
            self.rect(bx,start+246,50,50,"selected" if j==0 else "raised",7,"accent" if j==0 else "line")
            self.sphere_crop(bx+7,start+253,36,vx,vy,30)
        self.text(xx,start+319,"Warm clay",color="text",weight=500)
        self.checkbox(xx,start+341,"Surface detail shading")
        self.checkbox(xx,start+372,"Floor grid",True)
        self.header(rx,start+416,right,"Mesh")
        for j,(k,v) in enumerate([("Object","Sphere"),("Vertices","240 002"),("Triangles","480 000"),("Chunks","64")]):
            self.kv(xx,start+483+j*29,ww,k,v)
        self.line(xx,start+590,rx+right-14,start+590)
        self.subhead(xx,start+618,"Rebuild")
        self.slider(xx,start+641,ww,"Segments","200",.5)
        self.button(xx,start+704,ww,36,"Rebuild sphere")
        self.header(rx,start+762,right,"History",False)
        self.text(xx,start+824,"Undo available",12,"secondary")
        # Status uses only application state and working shortcuts.
        self.rect(0,bottom,1600,28,"raised")
        self.dot(16,bottom+14,3,"#8bc788" if self.slug!="03-light-studio" else "#356b32")
        self.text(28,bottom+18,"Ready",11,"secondary")
        self.text(94,bottom+18,"240 002 vertices  ·  480 000 triangles",11,"secondary",mono=True)
        self.text(935,bottom+18,"Ctrl  Invert   ·   Shift  Smooth   ·   Tab  Focus",11,"secondary")
        self.text(1582,bottom+18,"5.9 ms  ·  170 fps",11,"secondary",anchor="end",mono=True)
        return '\n'.join(self.p)

def render(svg, target, width=1600, height=1000):
    handle=Rsvg.Handle.new_from_data(svg.encode())
    surface=cairo.ImageSurface(cairo.FORMAT_ARGB32,width,height)
    vp=Rsvg.Rectangle(); vp.x=vp.y=0; vp.width=width; vp.height=height
    assert handle.render_document(cairo.Context(surface),vp)
    surface.write_to_png(str(target))

def contrast(a,b):
    def lum(c):
        v=[int(c[i:i+2],16)/255 for i in (1,3,5)]
        v=[c/12.92 if c<=.04045 else ((c+.055)/1.055)**2.4 for c in v]
        return sum(c*w for c,w in zip(v,[.2126,.7152,.0722]))
    l1,l2=sorted((lum(a),lum(b)))
    return round((l2+.05)/(l1+.05),2)

def main():
    defs=shared_defs(); mockups=[]; metrics=[]
    for t in THEMES:
        body=UI(t).draw()
        title=f'Klay — {t["name"]} readability mockup'
        svg=f'<svg xmlns="http://www.w3.org/2000/svg" width="1600" height="1000" viewBox="0 0 1600 1000" role="img" aria-label="{title}"><title>{title}</title>{defs}{body}</svg>'
        ET.fromstring(svg)
        (OUT/f'{t["slug"]}.svg').write_text(svg)
        render(svg,OUT/f'{t["slug"]}.png')
        # Control detail shows the actual label size at 1:1.
        detail=f'<svg xmlns="http://www.w3.org/2000/svg" width="372" height="848" viewBox="0 124 372 848">{defs}{body}</svg>'
        render(detail,OUT/f'{t["slug"]}-controls.png',372,848)
        mockups.append(f'<svg xmlns="http://www.w3.org/2000/svg" class="klay-design" data-concept="{t["slug"]}" viewBox="0 0 1600 1000" role="img" aria-label="{title}"'+(' hidden="hidden"' if len(mockups) else '')+f'><title>{title}</title>{body}</svg>')
        metrics.append(dict(concept=t['name'],body_px=t['base'],primary=contrast(t['text'],t['surface']),secondary=contrast(t['secondary'],t['surface']),secondary_on_control=contrast(t['secondary'],t['field']),selected=contrast(t['onselected'],t['selected'])))
    (OUT/'contrast.json').write_text(json.dumps(metrics,indent=2)+'\n')
    template=(OUT/'comparison.fragment.html').read_text()
    inline=template.replace('<!--KLAY_SHARED_DEFS-->',f'<svg width="0" height="0" aria-hidden="true" style="position:absolute;overflow:hidden">{defs}</svg>').replace('<!--KLAY_MOCKUPS-->','\n'.join(mockups))
    assert '<!--KLAY_' not in inline
    assert len(inline.encode())<1_000_000
    VIZ.write_text(inline)
    # A static, simultaneously visible overview for the handoff.
    overview=['<svg xmlns="http://www.w3.org/2000/svg" width="1680" height="3250" viewBox="0 0 1680 3250">',defs,'<rect width="1680" height="3250" fill="#121418"/>']
    for i,t in enumerate(THEMES):
        y=i*1080+45
        overview.append(f'<text x="40" y="{y}" font-family="Inter, sans-serif" font-size="22" font-weight="600" fill="#f2eee9">{i+1:02d}  {t["name"]}</text>')
        overview.append(f'<svg x="40" y="{y+20}" width="1600" height="1000" viewBox="0 0 1600 1000">{UI(t).draw()}</svg>')
    overview.append('</svg>')
    render(''.join(overview),OUT/'comparison.png',1680,3250)
    print(json.dumps(dict(mockups=3,inline_bytes=len(inline.encode()),contrast=metrics),indent=2))

if __name__=='__main__': main()
