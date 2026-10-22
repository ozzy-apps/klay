#!/usr/bin/env python3
"""Install Klay's launcher/icon and attach the icon to the local ELF executable."""
import os
from pathlib import Path
import shutil
import subprocess

root=Path(__file__).resolve().parents[1]
binary=root/'target/release/klay'
if not binary.is_file():raise SystemExit('Build Klay with cargo build --release first')
data=Path(os.environ.get('XDG_DATA_HOME',str(Path.home()/'.local/share')))
icons=data/'icons/hicolor/scalable/apps';icons.mkdir(parents=True,exist_ok=True)
icon=icons/'klay.svg';shutil.copy2(root/'assets/icon/klay.svg',icon)
for size in [32,64,128,256,512,1024]:
    folder=data/f'icons/hicolor/{size}x{size}/apps';folder.mkdir(parents=True,exist_ok=True)
    shutil.copy2(root/f'assets/icon/klay-{size}.png',folder/'klay.png')
apps=data/'applications';apps.mkdir(parents=True,exist_ok=True)
desktop=apps/'klay.desktop'
def quoted(s):return '"'+str(s).replace('\\','\\\\').replace('"','\\"').replace('`','\\`').replace('$','\\$')+'"'
text=(root/'packaging/klay.desktop').read_text().replace('Exec=klay','Exec='+quoted(binary)).replace('Icon=klay','Icon='+str(icon))
version=subprocess.check_output([str(binary),'--version'],text=True).strip().split()[-1]
text+='X-Klay-Version='+version+'\n'
desktop.write_text(text);desktop.chmod(0o755)
print(f'Installed {desktop}')
if shutil.which('update-desktop-database'):subprocess.run(['update-desktop-database',str(apps)],check=False)
if shutil.which('gio'):
    result=subprocess.run(['gio','set',str(binary),'metadata::custom-icon',icon.as_uri()],capture_output=True,text=True)
    if result.returncode==0:
        print('Executable file icon set to Coral Fold')
    else:
        print('Launcher icon installed; this desktop does not support per-file icon metadata: '+result.stderr.strip())
