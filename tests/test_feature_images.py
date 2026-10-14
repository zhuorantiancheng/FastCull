"""The native pixel QA must exclude UI and independently inspect each comparison pane."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
from PIL import Image

spec=importlib.util.spec_from_file_location('feature_pixels',Path(__file__).resolve().parents[1]/'scripts/verify_feature_images.py')
module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)

class NativePixelTests(unittest.TestCase):
    def fixture(self,root,second=True):
        def bright(v):
            s=v/255;linear=s/12.92 if s<=.04045 else ((s+.055)/1.055)**2.4
            linear=min(1,linear*2)
            return round(255*(12.92*linear if linear<=.0031308 else 1.055*linear**(1/2.4)-.055))
        def base():
            im=Image.new('RGB',(640,480),(43,43,43));im.paste((30,30,30),(242,22,640,300));return im
        before=base();after=base()
        for y in range(26,296):
            for x in range(350,530):
                rgb=(30+x%120,40+y%130,50+(x+y)%110)
                before.putpixel((x,y),rgb);after.putpixel((x,y),tuple(bright(v) for v in rgb))
        before.save(root/'05.png');after.save(root/'06.png')
        for name in ['08','09']:
            im=base()
            panes=[(252,430)]+([(452,628)] if second else [])
            for left,right in panes:
                for y in range(74,296):
                    for x in range(left,right): im.putpixel((x,y),(30+x%120,40+y%130,50+(x+y)%110))
            im.save(root/f'{name}.png')

    def test_sidebar_is_excluded_without_relaxing_exposure_tolerance(self):
        with tempfile.TemporaryDirectory() as name:
            root=Path(name);self.fixture(root)
            report=module.verify(root)
            self.assertEqual(report['photo_canvas_left'],242)
            self.assertLess(report['exposure_mean_error'],2)
            self.assertGreater(report['exposure_channel_samples'],100)

    def test_first_photo_cannot_hide_a_missing_second_comparison_pane(self):
        with tempfile.TemporaryDirectory() as name:
            root=Path(name);self.fixture(root,second=False)
            with self.assertRaisesRegex(AssertionError,'comparison canvas'): module.verify(root)
