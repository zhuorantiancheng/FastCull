"""Render crop UI in the native GPU window using only private photo copies.

Run with --scratch .agent-temp/<date>-crop/<run>. Requires Pillow for the grid.
The resulting PNGs still need human/model visual inspection; this is not ACR QA.
"""
import argparse
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
from PIL import Image, ImageDraw

p = argparse.ArgumentParser()
p.add_argument('--exe', type=Path, default=Path('lrplugins/FastCull.lrplugin/fastcull.exe'))
p.add_argument('--scratch', required=True, type=Path)
p.add_argument('--sample', type=Path, help='Optional source photo, copied before editing')
p.add_argument('--size', default='1200x800')
p.add_argument('--orientation', type=int, default=1, choices=range(1, 9))
args = p.parse_args()
scratch = args.scratch.resolve()
scratch.mkdir(parents=True, exist_ok=False)
photos = scratch / 'photos'
photos.mkdir()
if args.sample:
    source_hash = hashlib.sha256(args.sample.read_bytes()).digest()
    shutil.copyfile(args.sample, photos / args.sample.name)
else:
    image = Image.new('RGB', (1800, 1200), '#31495c')
    draw = ImageDraw.Draw(image)
    for x in range(0, 1800, 100):
        draw.line((x, 0, x, 1200), fill='#7792a5', width=2)
    for y in range(0, 1200, 100):
        draw.line((0, y, 1800, y), fill='#7792a5', width=2)
    for x, y, color, text in [(100,100,'#e87354','TOP LEFT'),(1400,100,'#edd96b','TOP RIGHT'),
                               (100,900,'#76c5a3','BOTTOM LEFT'),(1400,900,'#769fe0','BOTTOM RIGHT')]:
        draw.rectangle((x,y,x+280,y+180),fill=color)
        draw.text((x+10,y+70),text,fill='black',font_size=26)
    draw.ellipse((600,300,1200,900),outline='white',width=8)
    draw.line((900,0,900,1200),fill='#e87354',width=5)
    draw.line((0,600,1800,600),fill='#edd96b',width=5)
    exif=Image.Exif()
    exif[274]=args.orientation
    image.save(photos/'crop-grid.jpg',quality=95,exif=exif)

env=os.environ.copy()
env.update(FASTCULL_CROP_SMOKE='1',FASTCULL_SMOKE_DIR=str(scratch/'screenshots'),
           FASTCULL_LOG_DIR=str(scratch/'logs'),FASTCULL_SETTINGS_DIR=str(scratch/'settings'),
           FASTCULL_SMOKE_SIZE=args.size,TEMP=str(scratch),TMP=str(scratch))
for k in ['FASTCULL_FEATURE_SMOKE','FASTCULL_BRIDGE_MOVE_SMOKE','FASTCULL_TREE_SMOKE','FASTCULL_FOLDER_SMOKE']:
    env.pop(k,None)
result=subprocess.run([str(args.exe.resolve()),str(photos)],env=env,timeout=75,
                      stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.PIPE,
                      creationflags=subprocess.CREATE_NO_WINDOW)
assert result.returncode==0,(result.returncode,result.stderr)
assert (scratch/'screenshots/crop-ok.txt').is_file(), 'Native crop smoke did not finish'
assert len(list((scratch/'screenshots').glob('*.png')))==8
if args.sample:
    assert hashlib.sha256(args.sample.read_bytes()).digest()==source_hash
print(f'Native crop smoke passed: {scratch / "screenshots"}')
