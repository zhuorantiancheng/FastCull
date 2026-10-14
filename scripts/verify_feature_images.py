"""Pixel checks on app-owned native captures; no Lightroom/catalog access."""
from pathlib import Path
from PIL import Image

def verify(directory):
    directory=Path(directory)
    before=Image.open(directory/'05.png').convert('RGB')
    after=Image.open(directory/'06.png').convert('RGB')
    w,h=before.size
    # Infer the current theme's canvas color from the right fit-view margin.
    # Hard-coding the former (30,30,30) also sampled the unchanged sidebar.
    background=before.getpixel((w-2,int(h*.4)))
    assert max(background)<64,'Cannot locate the dark photo canvas margin'
    canvas_top=next((y for y in range(int(h*.1),int(h*.5))
        if all(before.getpixel((w-2,y+d))==background for d in range(3))),int(h*.1))
    # Stay inside the photo canvas even when the toolbar wraps at 640x480.
    canvas_bottom=next((y for y in range(int(h*.2),int(h*.9)-3)
        if all(before.getpixel((w-2,y+d))!=background for d in range(3))),int(h*.65))
    # A visible folder/filter panel has a different color from the canvas.
    # Fixed screen percentages can include that unchanged UI at the minimum width.
    probe_y=int(h*.4)
    canvas_left=next((x for x in range(0,int(w*.9)-5)
        if all(before.getpixel((x+dx,probe_y+dy))==background
               for dx in range(5) for dy in range(3))),0)
    crop=(max(int(w*.32),canvas_left+8),max(int(h*.1),canvas_top+8),int(w*.68),min(int(h*.65),canvas_bottom-8))
    def pixels(image):
        return image.get_flattened_data() if hasattr(image,'get_flattened_data') else image.getdata()
    a=list(pixels(before.crop(crop)));b=list(pixels(after.crop(crop)))
    assert len(set(a))>1000,'Photo canvas is missing or hidden by controls'
    def brighter(v):
        s=v/255.;linear=s/12.92 if s<=.04045 else ((s+.055)/1.055)**2.4
        linear=min(1.,linear*2)
        return 255*(linear*12.92 if linear<=.0031308 else 1.055*linear**(1/2.4)-.055)
    errors=[];lift=[]
    for old,new in zip(a[::97],b[::97]):
        if old!=background and all(20 < v < 190 for v in old):
            errors.extend(abs(brighter(v)-n) for v,n in zip(old,new));lift.append(sum(new)-sum(old))
    mean=sum(errors)/len(errors) if errors else float('inf')
    assert len(errors)>100 and mean<2.,f'Exposure GPU +1 EV mismatch: n={len(errors)}, error={mean:.3f}'
    assert sum(lift)/len(lift)>10,'Exposure did not brighten the displayed image'
    for name in ['08','09']:
        picture=Image.open(directory/f'{name}.png').convert('RGB')
        middle=(canvas_left+w)//2
        for left,right in [(canvas_left+8,middle-8),(middle+8,w-8)]:
            assert len(set(pixels(picture.crop((left,int(h*.2),right,min(int(h*.65),canvas_bottom-8))))))>1000,'One comparison canvas is missing'
    return dict(width=w,height=h,photo_canvas_left=canvas_left,exposure_channel_samples=len(errors),exposure_mean_error=round(mean,3),both_comparison_canvases_visible=True)

if __name__=='__main__':
    import argparse,json
    parser=argparse.ArgumentParser();parser.add_argument('directory',type=Path);args=parser.parse_args()
    print(json.dumps(verify(args.directory),indent=2))
