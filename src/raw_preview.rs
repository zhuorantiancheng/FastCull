//! Local GPU tone/color controls and scene-referred floating-point previews.
//! Owned CPU scene pixels are released after their first successful GL upload.
use eframe::{egui, egui_glow, glow};
use glow::HasContext;
use std::sync::{Arc, Mutex, Weak};

/// DNG table axes are H/S/V, with saturation the fastest varying element.
#[derive(Clone, Debug)]
pub struct HsvLookup {
    pub dims: [usize; 3],
    pub data: Vec<[f32; 3]>,
    pub encoding: u32,
}

unsafe fn texture_parameters(gl: &glow::Context, target: u32, filter: u32) {
    unsafe {
        gl.tex_parameter_i32(target, glow::TEXTURE_MIN_FILTER, filter as i32);
        gl.tex_parameter_i32(target, glow::TEXTURE_MAG_FILTER, filter as i32);
        gl.tex_parameter_i32(target, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
        gl.tex_parameter_i32(target, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
    }
}
fn valid_curve(curve: &[f32]) -> bool {
    curve.len() >= 2 && curve.len() <= 65536 && curve.iter().all(|v| v.is_finite())
}
fn valid_lookup(map: &HsvLookup) -> bool {
    lookup_dimensions_valid(map) && map.data.iter().flatten().all(|v| v.is_finite())
}
fn lookup_dimensions_valid(map: &HsvLookup) -> bool {
    map.dims.into_iter().all(|v| v > 0 && v <= 256)
        && map
            .dims
            .into_iter()
            .try_fold(1_usize, |a, b| a.checked_mul(b))
            == Some(map.data.len())
}
fn encode(v: f32) -> f32 {
    if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.max(0.).powf(1. / 2.4) - 0.055
    }
}
fn decode(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}
fn rgb_hsv(rgb: [f32; 3]) -> [f32; 3] {
    let lo = rgb.into_iter().fold(f32::INFINITY, f32::min);
    let hi = rgb.into_iter().fold(f32::NEG_INFINITY, f32::max);
    let delta = hi - lo;
    let hue = if delta <= 1e-12 {
        0.
    } else if hi == rgb[0] {
        ((rgb[1] - rgb[2]) / delta).rem_euclid(6.)
    } else if hi == rgb[1] {
        (rgb[2] - rgb[0]) / delta + 2.
    } else {
        (rgb[0] - rgb[1]) / delta + 4.
    };
    [hue, if hi > 1e-12 { delta / hi } else { 0. }, hi]
}
fn hsv_rgb(hsv: [f32; 3]) -> [f32; 3] {
    let [h, s, v] = hsv;
    let h = h.rem_euclid(6.);
    let c = v * s;
    let x = c * (1. - ((h % 2.) - 1.).abs());
    let a = match h.floor() as u32 {
        0 => [c, x, 0.],
        1 => [x, c, 0.],
        2 => [0., c, x],
        3 => [0., x, c],
        4 => [x, 0., c],
        _ => [c, 0., x],
    };
    a.map(|a| a + v - c)
}
fn look_up(map: &HsvLookup, rgb: [f32; 3]) -> [f32; 3] {
    if !lookup_dimensions_valid(map) {
        return rgb;
    }
    let mut hsv = rgb_hsv(rgb.map(|v| v.max(0.)));
    let encoded = map.encoding == 1 && map.dims[2] > 1;
    let value = if encoded { encode(hsv[2]) } else { hsv[2] };
    let [nh, ns, nv] = map.dims;
    let h = hsv[0] * nh as f32 / 6.;
    let s = hsv[1].clamp(0., 1.) * ns.saturating_sub(1) as f32;
    let v = value.clamp(0., 1.) * nv.saturating_sub(1) as f32;
    let base = [
        h.floor() as usize % nh,
        s.floor() as usize,
        v.floor() as usize,
    ];
    let next = [
        (base[0] + 1) % nh,
        (base[1] + 1).min(ns - 1),
        (base[2] + 1).min(nv - 1),
    ];
    let frac = [h.fract(), s.fract(), v.fract()];
    let mut result = [0.; 3];
    for dv in 0..2 {
        for dh in 0..2 {
            for ds in 0..2 {
                let index = [
                    if dh == 0 { base[0] } else { next[0] },
                    if ds == 0 { base[1] } else { next[1] },
                    if dv == 0 { base[2] } else { next[2] },
                ];
                let weight = (if dh == 0 { 1. - frac[0] } else { frac[0] })
                    * (if ds == 0 { 1. - frac[1] } else { frac[1] })
                    * (if dv == 0 { 1. - frac[2] } else { frac[2] });
                let entry = map.data[(index[2] * nh + index[0]) * ns + index[1]];
                for c in 0..3 {
                    result[c] += entry[c] * weight;
                }
            }
        }
    }
    hsv[0] = (hsv[0] + result[0] / 60.).rem_euclid(6.);
    hsv[1] = (hsv[1] * result[1]).clamp(0., 1.);
    hsv[2] = if encoded {
        decode((value * result[2]).max(0.))
    } else {
        (value * result[2]).max(0.)
    };
    hsv_rgb(hsv)
}
fn curve_value(v: f32, curve: &[f32]) -> f32 {
    let v = if v > 0.8 {
        0.8 + 0.2 * (1. - (-(v - 0.8) / 0.2).exp())
    } else {
        v.max(0.)
    };
    if curve.len() < 2 {
        return v;
    }
    let x = v.clamp(0., 1.) * (curve.len() - 1) as f32;
    let i = (x as usize).min(curve.len() - 1);
    let j = (i + 1).min(curve.len() - 1);
    curve[i] + (curve[j] - curve[i]) * x.fract()
}
/// Numerically mirrors SCENE_FRAGMENT for histogram and headless validation.
/// The input is linear ProPhoto/D50; exposure is absolute, not a display delta.
pub fn evaluate_scene(rgb: [f32; 3], settings: Settings, recipe: &Recipe) -> [f32; 3] {
    let gain = 2f32.powf((settings.exposure + recipe.baseline_exposure).clamp(-20., 20.));
    let mut rgb = std::array::from_fn(|c| (rgb[c] * gain * settings.wb[c]).max(0.));
    let b = settings.basic;
    let mut y = rgb[0] * 0.2880402 + rgb[1] * 0.7118741 + rgb[2] * 0.0000857;
    if b[0] != 0. && y > 1e-9 {
        let next = 0.18 * (y / 0.18).powf(2f32.powf(b[0] * 0.8));
        rgb = rgb.map(|v| v * next / y);
        y = next;
    }
    let zone = y / (y + 0.18);
    let shift = b[2] * (1. - zone).powi(2) * 1.5
        + b[1] * zone * zone * 1.5
        + b[3] * zone.powi(4)
        + b[4] * (1. - zone).powi(4);
    rgb = rgb.map(|v| v * 2f32.powf(shift));
    if let Some(map) = &recipe.look {
        rgb = look_up(map, rgb);
    }
    let lo = rgb.into_iter().fold(f32::INFINITY, f32::min);
    let hi = rgb.into_iter().fold(f32::NEG_INFINITY, f32::max);
    let low = curve_value(lo, &recipe.curve);
    let high = curve_value(hi, &recipe.curve);
    rgb = if hi - lo > 1e-9 {
        rgb.map(|v| low + (v - lo) * (high - low) / (hi - lo))
    } else {
        [high; 3]
    };
    y = rgb[0] * 0.2880402 + rgb[1] * 0.7118741 + rgb[2] * 0.0000857;
    let saturation = if high > 1e-9 { (high - low) / high } else { 0. };
    let gain = (1. + b[5] * (1. - saturation)).max(0.) * (1. + b[6]).max(0.);
    rgb = rgb.map(|v| v + (v - y) * (gain - 1.));
    // ProPhoto/D50 -> Bradford D65 -> linear sRGB. Row sums are one.
    [
        2.034075 * rgb[0] - 0.727334 * rgb[1] - 0.306741 * rgb[2],
        -0.228813 * rgb[0] + 1.231_73 * rgb[1] - 0.002917 * rgb[2],
        -0.008570 * rgb[0] - 0.153287 * rgb[1] + 1.161857 * rgb[2],
    ]
    .map(|v| encode(v.clamp(0., 1.)).clamp(0., 1.))
}

const SCENE_FRAGMENT: &str = r#"#version 330
uniform sampler2D image; uniform sampler2D curve; uniform sampler3D look_table;
uniform float gain; uniform vec4 tone; uniform vec3 chroma; uniform vec3 wb;
uniform bool look_encoded; in vec2 uv; out vec4 color;
float encode_s(float x) {return x<=0.0031308 ? 12.92*x : 1.055*pow(max(x,0.0),1.0/2.4)-0.055;}
float decode_s(float x) {return x<=0.04045 ? x/12.92 : pow((x+0.055)/1.055,2.4);}
vec3 rgb_hsv(vec3 x) {
    float lo=min(x.r,min(x.g,x.b)),hi=max(x.r,max(x.g,x.b)),d=hi-lo,h=0.0;
    if(d>1e-12) {if(hi==x.r)h=mod((x.g-x.b)/d,6.0);else if(hi==x.g)h=(x.b-x.r)/d+2.0;else h=(x.r-x.g)/d+4.0;}
    return vec3(h,hi>1e-12?d/hi:0.0,hi);
}
vec3 hsv_rgb(vec3 hsv) {
    float h=mod(hsv.x,6.0),c=hsv.z*hsv.y,x=c*(1.0-abs(mod(h,2.0)-1.0));vec3 a;
    if(h<1.0)a=vec3(c,x,0);else if(h<2.0)a=vec3(x,c,0);else if(h<3.0)a=vec3(0,c,x);
    else if(h<4.0)a=vec3(0,x,c);else if(h<5.0)a=vec3(x,0,c);else a=vec3(c,0,x);
    return a+vec3(hsv.z-c);
}
vec3 apply_look(vec3 rgb) {
    vec3 hsv=rgb_hsv(max(rgb,vec3(0)));ivec3 sz=textureSize(look_table,0);
    float value=look_encoded?encode_s(hsv.z):hsv.z;
    vec3 c=vec3(hsv.x*float(sz.y)/6.0,clamp(hsv.y,0.0,1.0)*float(sz.x-1),clamp(value,0.0,1.0)*float(sz.z-1));
    ivec3 a=ivec3(floor(c)),b=ivec3((a.x+1)%sz.y,min(a.y+1,sz.x-1),min(a.z+1,sz.z-1));
    a.x=a.x%sz.y;vec3 f=fract(c),entry=vec3(0);
    for(int z=0;z<2;z++)for(int h=0;h<2;h++)for(int s=0;s<2;s++) {
        ivec3 p=ivec3(s==0?a.y:b.y,h==0?a.x:b.x,z==0?a.z:b.z);
        float w=(h==0?1.0-f.x:f.x)*(s==0?1.0-f.y:f.y)*(z==0?1.0-f.z:f.z);
        entry+=texelFetch(look_table,p,0).rgb*w;
    }
    hsv.x=mod(hsv.x+entry.x/60.0,6.0);hsv.y=clamp(hsv.y*entry.y,0.0,1.0);
    hsv.z=look_encoded?decode_s(max(value*entry.z,0.0)):max(value*entry.z,0.0);
    return hsv_rgb(hsv);
}
float curve_value(float x) {
    if(x>0.8)x=0.8+0.2*(1.0-exp(-(x-0.8)/0.2));else x=max(x,0.0);
    int n=textureSize(curve,0).x;float q=clamp(x,0.0,1.0)*float(n-1);int i=min(int(q),n-1),j=min(i+1,n-1);
    return mix(texelFetch(curve,ivec2(i,0),0).r,texelFetch(curve,ivec2(j,0),0).r,fract(q));
}
void main() {
    if(any(lessThan(uv,vec2(0)))||any(greaterThan(uv,vec2(1)))){color=vec4(0,0,0,1);return;}
    vec3 x=max(texture(image,uv).rgb*gain*wb,vec3(0));float y=dot(x,vec3(.2880402,.7118741,.0000857));
    if(tone.x!=0.0&&y>1e-9){float n=.18*pow(y/.18,exp2(tone.x*.8));x*=n/y;y=n;}
    float z=y/(y+.18),shift=tone.z*pow(1.0-z,2.0)*1.5+tone.y*z*z*1.5+tone.w*pow(z,4.0)+chroma.x*pow(1.0-z,4.0);
    x*=exp2(shift);x=apply_look(x);
    float lo=min(x.r,min(x.g,x.b)),hi=max(x.r,max(x.g,x.b)),low=curve_value(lo),high=curve_value(hi);
    x=hi-lo>1e-9?vec3(low)+(x-vec3(lo))*(high-low)/(hi-lo):vec3(high);
    y=dot(x,vec3(.2880402,.7118741,.0000857));float sat=high>1e-9?(high-low)/high:0.0;
    float g=max(0.0,1.0+chroma.y*(1.0-sat))*max(0.0,1.0+chroma.z);x+=(x-vec3(y))*(g-1.0);
    x=vec3(dot(x,vec3(2.034075,-.727334,-.306741)),dot(x,vec3(-.228813,1.231730,-.002917)),dot(x,vec3(-.008570,-.153287,1.161857)));
    x=clamp(x,0.0,1.0);color=vec4(clamp(vec3(encode_s(x.r),encode_s(x.g),encode_s(x.b)),0.0,1.0),1);
}
"#;

#[cfg(test)]
mod scene_tests {
    use super::*;
    fn close(a: [f32; 3], b: [f32; 3]) {
        for c in 0..3 {
            assert!((a[c] - b[c]).abs() < 0.00002, "{a:?} != {b:?}");
        }
    }
    #[test]
    fn neutral_axis_and_identity_curve_preserve_middle_gray() {
        let recipe = Recipe::default();
        let rgb = evaluate_scene([0.18; 3], Settings::default(), &recipe);
        close(rgb, [encode(0.18); 3]);
        assert!((curve_value(0.25, &[0., 0.5, 1.]) - 0.25).abs() < 1e-7);
        assert!(curve_value(1.1, &[0., 1.]) < curve_value(2., &[0., 1.]));
    }
    #[test]
    fn exposure_is_applied_once_and_hdr_detail_can_be_recovered() {
        let recipe = Recipe::default();
        let settings = Settings {
            exposure: 1.,
            ..Default::default()
        };
        close(
            evaluate_scene([0.04; 3], settings, &recipe),
            evaluate_scene([0.08; 3], Settings::default(), &recipe),
        );
        let settings = Settings {
            exposure: -2.,
            ..Default::default()
        };
        let hdr = evaluate_scene([2.4, 1.2, 0.6], settings, &recipe);
        close(
            hdr,
            evaluate_scene([0.6, 0.3, 0.15], Settings::default(), &recipe),
        );
        let clipped = evaluate_scene([1., 1., 0.6], settings, &recipe);
        assert!((hdr[0] - clipped[0]).abs() > 0.1);
    }
    #[test]
    fn minus_hundred_saturation_is_achromatic_even_with_vibrance() {
        let mut settings = Settings::default();
        settings.basic[5] = 1.;
        settings.basic[6] = -1.;
        let rgb = evaluate_scene([0.45, 0.17, 0.05], settings, &Recipe::default());
        assert!((rgb[0] - rgb[1]).abs() < 0.00002);
        assert!((rgb[1] - rgb[2]).abs() < 0.00002);
    }
    #[test]
    fn lookup_wraps_hue_and_encodes_only_value_when_required() {
        let map = HsvLookup {
            dims: [4, 2, 2],
            data: (0..16)
                .map(|i| [if (i / 2) % 4 == 0 { 10. } else { -4. }, 1., 1.])
                .collect(),
            encoding: 0,
        };
        let a = look_up(&map, hsv_rgb([0.00001, 0.7, 0.3]));
        let b = look_up(&map, hsv_rgb([5.99999, 0.7, 0.3]));
        close(a, b);
        let mut map = HsvLookup {
            dims: [2, 2, 2],
            data: vec![[0., 1., 1.5]; 8],
            encoding: 1,
        };
        let encoded = look_up(&map, [0.2; 3]);
        close(encoded, [decode(encode(0.2) * 1.5); 3]);
        map.encoding = 0;
        close(look_up(&map, [0.2; 3]), [0.3; 3]);
        map.dims[2] = 1;
        map.data.truncate(4);
        map.encoding = 1;
        close(look_up(&map, [0.2; 3]), [0.3; 3]);
    }
    #[test]
    fn exposure_and_controls_remain_finite_across_working_headroom() {
        for exposure in [-5., 0., 5.] {
            for value in [-1., 0., 1.] {
                let settings = Settings {
                    exposure,
                    basic: [value; 7],
                    ..Default::default()
                };
                for input in [[0.; 3], [-0.1, 0.2, 10.], [32., 8., 2.]] {
                    let result = evaluate_scene(input, settings, &Recipe::default());
                    assert!(
                        result
                            .into_iter()
                            .all(|v| v.is_finite() && (0.0..=1.0).contains(&v))
                    );
                }
            }
        }
    }
}
#[derive(Clone, Debug)]
pub struct Recipe {
    pub look: Option<HsvLookup>,
    pub curve: Vec<f32>,
    pub baseline_exposure: f32,
}
impl Default for Recipe {
    fn default() -> Self {
        Self {
            look: None,
            curve: vec![0., 1.],
            baseline_exposure: 0.,
        }
    }
}
/// Scene-referred, linear ProPhoto/D50. Values above one retain sensor headroom.
pub struct LinearImage {
    pub size: [usize; 2],
    pub pixels: Vec<[f32; 4]>,
    pub recipe: Recipe,
}
struct GpuImage {
    image: glow::Texture,
    curve: glow::Texture,
    look: glow::Texture,
}
struct LinearState {
    pixels: Option<Vec<[f32; 4]>>,
    gpu: Option<GpuImage>,
}
pub struct LinearTexture {
    size: [usize; 2],
    recipe: Recipe,
    state: Mutex<LinearState>,
    garbage: Arc<Mutex<Vec<GpuImage>>>,
}
impl LinearTexture {
    pub fn size_vec2(&self) -> egui::Vec2 {
        egui::vec2(self.size[0] as f32, self.size[1] as f32)
    }
}
impl Drop for LinearTexture {
    fn drop(&mut self) {
        if let Some(gpu) = self.state.get_mut().unwrap().gpu.take() {
            // Drop can occur without a current context. Delete next paint/exit.
            self.garbage.lock().unwrap().push(gpu);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub exposure: f32,
    pub basic: [f32; 7],
    pub wb: [f32; 3],
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            exposure: 0.,
            basic: [0.; 7],
            wb: [1.; 3],
        }
    }
}
pub struct Preview {
    gl: Arc<glow::Context>,
    shader: Mutex<Option<Result<(glow::Program, glow::VertexArray), String>>>,
    linear_shader: Mutex<Option<Result<(glow::Program, glow::VertexArray), String>>>,
    linear_error: Mutex<Option<String>>,
    frames: Mutex<Vec<Weak<LinearTexture>>>,
    garbage: Arc<Mutex<Vec<GpuImage>>>,
}
impl Preview {
    pub fn new(gl: Arc<glow::Context>) -> Arc<Self> {
        Arc::new(Self {
            gl,
            shader: Mutex::new(None),
            linear_shader: Mutex::new(None),
            linear_error: Mutex::new(None),
            frames: Mutex::new(Vec::new()),
            garbage: Arc::new(Mutex::new(Vec::new())),
        })
    }
    unsafe fn compile(&self, linear: bool) -> Result<(glow::Program, glow::VertexArray), String> {
        unsafe {
            let gl = &self.gl;
            let program = gl.create_program()?;
            let sources = [
                (
                    glow::VERTEX_SHADER,
                    r#"#version 330
uniform vec4 bounds; uniform vec2 uvs[4]; out vec2 uv;
void main() {
    int indices[6] = int[6](0,1,2,0,2,3); int i = indices[gl_VertexID];
    vec2 p[4] = vec2[4](bounds.xy, vec2(bounds.z,bounds.y), bounds.zw, vec2(bounds.x,bounds.w));
    gl_Position = vec4(p[i],0,1); uv = uvs[i];
}"#,
                ),
                (
                    glow::FRAGMENT_SHADER,
                    if linear {
                        SCENE_FRAGMENT
                    } else {
                        r#"#version 330
uniform sampler2D image; uniform float gain; uniform vec4 tone; uniform vec3 chroma; uniform vec3 wb; in vec2 uv; out vec4 color;
void main() {
    if (any(lessThan(uv,vec2(0))) || any(greaterThan(uv,vec2(1)))) { color=vec4(0,0,0,1); return; }
    vec3 s = texture(image,uv).rgb;
    vec3 linear = mix(s/12.92, pow((s+0.055)/1.055,vec3(2.4)), greaterThan(s,vec3(0.04045)));
    linear = clamp(linear*gain*wb,0.0,1.0);
    vec3 out_s = mix(linear*12.92, 1.055*pow(linear,vec3(1.0/2.4))-0.055, greaterThan(linear,vec3(0.0031308)));
    float y=dot(out_s,vec3(.2126,.7152,.0722));
    float lift=tone.z*pow(1.0-y,2.0)*.35 + tone.y*y*y*.35 + tone.w*pow(y,4.0)*.25 + chroma.x*pow(1.0-y,4.0)*.25;
    out_s=clamp((out_s-.5)*(1.0+tone.x*.8)+.5+lift,0.0,1.0);
    y=dot(out_s,vec3(.2126,.7152,.0722));
    float saturation=max(out_s.r,max(out_s.g,out_s.b))-min(out_s.r,min(out_s.g,out_s.b));
    float vibrance_gain=max(0.0,1.0+chroma.y*(1.0-saturation));
    out_s=mix(vec3(y),out_s,vibrance_gain*max(0.0,1.0+chroma.z));
    color = vec4(clamp(out_s,0.0,1.0),1);
}"#
                    },
                ),
            ];
            for (kind, source) in sources {
                let shader = gl.create_shader(kind)?;
                gl.shader_source(shader, source);
                gl.compile_shader(shader);
                if !gl.get_shader_compile_status(shader) {
                    let error = gl.get_shader_info_log(shader);
                    gl.delete_shader(shader);
                    gl.delete_program(program);
                    return Err(error);
                }
                gl.attach_shader(program, shader);
                gl.delete_shader(shader);
            }
            gl.link_program(program);
            if !gl.get_program_link_status(program) {
                let error = gl.get_program_info_log(program);
                gl.delete_program(program);
                return Err(error);
            }
            match gl.create_vertex_array() {
                Ok(vao) => Ok((program, vao)),
                Err(e) => {
                    gl.delete_program(program);
                    Err(e)
                }
            }
        }
    }
    pub fn paint(
        self: &Arc<Self>,
        painter: &egui::Painter,
        canvas: egui::Rect,
        rect: egui::Rect,
        texture: egui::TextureId,
        uvs: [egui::Pos2; 4],
        delta: Settings,
    ) {
        let preview = self.clone();
        let bounds = [
            2. * (rect.left() - canvas.left()) / canvas.width() - 1.,
            1. - 2. * (rect.top() - canvas.top()) / canvas.height(),
            2. * (rect.right() - canvas.left()) / canvas.width() - 1.,
            1. - 2. * (rect.bottom() - canvas.top()) / canvas.height(),
        ];
        painter.add(egui::PaintCallback {
            rect: canvas,
            callback: Arc::new(egui_glow::CallbackFn::new(move |_, painter| {
                let mut shader = preview.shader.lock().unwrap();
                if shader.is_none() {
                    *shader = Some(unsafe { preview.compile(false) });
                    if let Some(Err(e)) = &*shader {
                        log::error!("exposure shader: {e}");
                    }
                }
                if let Some(Ok((program, vao))) = *shader
                    && let Some(texture) = painter.texture(texture)
                {
                    unsafe {
                        let gl = painter.gl();
                        gl.use_program(Some(program));
                        gl.bind_vertex_array(Some(vao));
                        gl.active_texture(glow::TEXTURE0);
                        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
                        gl.uniform_1_i32(gl.get_uniform_location(program, "image").as_ref(), 0);
                        gl.uniform_1_f32(
                            gl.get_uniform_location(program, "gain").as_ref(),
                            2f32.powf(delta.exposure),
                        );
                        gl.uniform_4_f32_slice(
                            gl.get_uniform_location(program, "tone").as_ref(),
                            &delta.basic[..4],
                        );
                        gl.uniform_3_f32_slice(
                            gl.get_uniform_location(program, "chroma").as_ref(),
                            &delta.basic[4..],
                        );
                        gl.uniform_3_f32_slice(
                            gl.get_uniform_location(program, "wb").as_ref(),
                            &delta.wb,
                        );
                        gl.uniform_4_f32_slice(
                            gl.get_uniform_location(program, "bounds").as_ref(),
                            &bounds,
                        );
                        let uv: Vec<f32> = uvs.iter().flat_map(|p| [p.x, p.y]).collect();
                        gl.uniform_2_f32_slice(
                            gl.get_uniform_location(program, "uvs[0]").as_ref(),
                            &uv,
                        );
                        gl.draw_arrays(glow::TRIANGLES, 0, 6);
                    }
                }
            })),
        });
    }
    /// Only queues owned CPU pixels. OpenGL work occurs in a paint callback.
    pub fn upload(self: &Arc<Self>, image: LinearImage) -> Arc<LinearTexture> {
        *self.linear_error.lock().unwrap() = None;
        let frame = Arc::new(LinearTexture {
            size: image.size,
            recipe: image.recipe,
            state: Mutex::new(LinearState {
                pixels: Some(image.pixels),
                gpu: None,
            }),
            garbage: self.garbage.clone(),
        });
        let mut frames = self.frames.lock().unwrap();
        frames.retain(|frame| frame.strong_count() > 0);
        frames.push(Arc::downgrade(&frame));
        frame
    }
    unsafe fn delete_image(&self, image: GpuImage) {
        unsafe {
            self.gl.delete_texture(image.image);
            self.gl.delete_texture(image.curve);
            self.gl.delete_texture(image.look);
        }
    }
    unsafe fn collect_garbage(&self) {
        for image in self.garbage.lock().unwrap().drain(..) {
            unsafe { self.delete_image(image) };
        }
    }
    unsafe fn upload_frame(
        &self,
        frame: &LinearTexture,
        pixels: &[[f32; 4]],
    ) -> Result<GpuImage, String> {
        unsafe {
            let gl = &self.gl;
            let [w, h] = frame.size;
            let limit = gl.get_parameter_i32(glow::MAX_TEXTURE_SIZE) as usize;
            if w == 0 || h == 0 || w > limit || h > limit || w.checked_mul(h) != Some(pixels.len())
            {
                return Err("浮点 RAW 预览尺寸无效或超过显卡纹理上限".into());
            }
            if pixels.iter().flatten().any(|v| !v.is_finite()) {
                return Err("浮点 RAW 预览含无效数值".into());
            }
            if !valid_curve(&frame.recipe.curve) || !frame.recipe.baseline_exposure.is_finite() {
                return Err("RAW 色调曲线或曝光基线无效".into());
            }
            if frame
                .recipe
                .look
                .as_ref()
                .is_some_and(|map| !valid_lookup(map) || map.encoding > 1)
            {
                return Err("RAW 色彩查找表尺寸、数值或编码无效".into());
            }
            let image = gl.create_texture()?;
            let curve = match gl.create_texture() {
                Ok(value) => value,
                Err(error) => {
                    gl.delete_texture(image);
                    return Err(error);
                }
            };
            let look = match gl.create_texture() {
                Ok(value) => value,
                Err(error) => {
                    gl.delete_texture(image);
                    gl.delete_texture(curve);
                    return Err(error);
                }
            };
            let result = GpuImage { image, curve, look };
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
            gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 0);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(image));
            texture_parameters(gl, glow::TEXTURE_2D, glow::LINEAR);
            let bytes = std::slice::from_raw_parts(
                pixels.as_ptr().cast::<u8>(),
                std::mem::size_of_val(pixels),
            );
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA32F as i32,
                w as i32,
                h as i32,
                0,
                glow::RGBA,
                glow::FLOAT,
                glow::PixelUnpackData::Slice(Some(bytes)),
            );
            let identity = [0., 1.];
            let curve_data: &[f32] = if valid_curve(&frame.recipe.curve) {
                &frame.recipe.curve
            } else {
                &identity
            };
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, Some(curve));
            texture_parameters(gl, glow::TEXTURE_2D, glow::NEAREST);
            let bytes = std::slice::from_raw_parts(
                curve_data.as_ptr().cast::<u8>(),
                std::mem::size_of_val(curve_data),
            );
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::R32F as i32,
                curve_data.len() as i32,
                1,
                0,
                glow::RED,
                glow::FLOAT,
                glow::PixelUnpackData::Slice(Some(bytes)),
            );
            let identity = HsvLookup {
                dims: [1, 1, 1],
                data: vec![[0., 1., 1.]],
                encoding: 0,
            };
            let lookup = frame
                .recipe
                .look
                .as_ref()
                .filter(|v| valid_lookup(v))
                .unwrap_or(&identity);
            let [nh, ns, nv] = lookup.dims;
            let lookup_limit = gl.get_parameter_i32(glow::MAX_3D_TEXTURE_SIZE) as usize;
            if lookup.dims.into_iter().any(|n| n > lookup_limit) {
                self.delete_image(result);
                return Err("RAW 色彩查找表超过显卡上限".into());
            }
            gl.active_texture(glow::TEXTURE2);
            gl.bind_texture(glow::TEXTURE_3D, Some(look));
            texture_parameters(gl, glow::TEXTURE_3D, glow::NEAREST);
            gl.tex_parameter_i32(
                glow::TEXTURE_3D,
                glow::TEXTURE_WRAP_R,
                glow::CLAMP_TO_EDGE as i32,
            );
            let bytes = std::slice::from_raw_parts(
                lookup.data.as_ptr().cast::<u8>(),
                std::mem::size_of_val(lookup.data.as_slice()),
            );
            gl.tex_image_3d(
                glow::TEXTURE_3D,
                0,
                glow::RGB32F as i32,
                ns as i32,
                nh as i32,
                nv as i32,
                0,
                glow::RGB,
                glow::FLOAT,
                glow::PixelUnpackData::Slice(Some(bytes)),
            );
            gl.active_texture(glow::TEXTURE0);
            let status = gl.get_error();
            if status != glow::NO_ERROR {
                self.delete_image(result);
                return Err(format!("浮点 RAW 纹理上传失败（OpenGL {status:#x}）"));
            }
            Ok(result)
        }
    }
    pub fn paint_linear(
        self: &Arc<Self>,
        painter: &egui::Painter,
        canvas: egui::Rect,
        rect: egui::Rect,
        texture: Arc<LinearTexture>,
        uvs: [egui::Pos2; 4],
        settings: Settings,
    ) {
        let preview = self.clone();
        let bounds = [
            2. * (rect.left() - canvas.left()) / canvas.width() - 1.,
            1. - 2. * (rect.top() - canvas.top()) / canvas.height(),
            2. * (rect.right() - canvas.left()) / canvas.width() - 1.,
            1. - 2. * (rect.bottom() - canvas.top()) / canvas.height(),
        ];
        painter.add(egui::PaintCallback {
            rect: canvas,
            callback: Arc::new(egui_glow::CallbackFn::new(move |_, painter| {
                unsafe {
                    preview.collect_garbage();
                }
                let mut shader = preview.linear_shader.lock().unwrap();
                if shader.is_none() {
                    *shader = Some(unsafe { preview.compile(true) });
                }
                let Some(Ok((program, vao))) = *shader else {
                    return;
                };
                let mut state = texture.state.lock().unwrap();
                if let Some(pixels) = state.pixels.take() {
                    match unsafe { preview.upload_frame(&texture, &pixels) } {
                        Ok(gpu) => state.gpu = Some(gpu),
                        Err(error) => {
                            log::error!("linear RAW upload: {error}");
                            *preview.linear_error.lock().unwrap() = Some(error);
                        }
                    }
                }
                let Some(gpu) = &state.gpu else {
                    return;
                };
                unsafe {
                    let gl = painter.gl();
                    gl.use_program(Some(program));
                    gl.bind_vertex_array(Some(vao));
                    for (unit, target, image, uniform) in [
                        (0, glow::TEXTURE_2D, gpu.image, "image"),
                        (1, glow::TEXTURE_2D, gpu.curve, "curve"),
                        (2, glow::TEXTURE_3D, gpu.look, "look_table"),
                    ] {
                        gl.active_texture(glow::TEXTURE0 + unit);
                        gl.bind_texture(target, Some(image));
                        gl.uniform_1_i32(
                            gl.get_uniform_location(program, uniform).as_ref(),
                            unit as i32,
                        );
                    }
                    let encoded = texture
                        .recipe
                        .look
                        .as_ref()
                        .is_some_and(|v| v.encoding == 1 && v.dims[2] > 1);
                    gl.uniform_1_i32(
                        gl.get_uniform_location(program, "look_encoded").as_ref(),
                        i32::from(encoded),
                    );
                    gl.uniform_1_f32(
                        gl.get_uniform_location(program, "gain").as_ref(),
                        2f32.powf(
                            (settings.exposure + texture.recipe.baseline_exposure).clamp(-20., 20.),
                        ),
                    );
                    gl.uniform_4_f32_slice(
                        gl.get_uniform_location(program, "tone").as_ref(),
                        &settings.basic[..4],
                    );
                    gl.uniform_3_f32_slice(
                        gl.get_uniform_location(program, "chroma").as_ref(),
                        &settings.basic[4..],
                    );
                    gl.uniform_3_f32_slice(
                        gl.get_uniform_location(program, "wb").as_ref(),
                        &settings.wb,
                    );
                    gl.uniform_4_f32_slice(
                        gl.get_uniform_location(program, "bounds").as_ref(),
                        &bounds,
                    );
                    let uv: Vec<f32> = uvs.iter().flat_map(|p| [p.x, p.y]).collect();
                    gl.uniform_2_f32_slice(
                        gl.get_uniform_location(program, "uvs[0]").as_ref(),
                        &uv,
                    );
                    gl.draw_arrays(glow::TRIANGLES, 0, 6);
                    gl.active_texture(glow::TEXTURE0);
                }
            })),
        });
    }
    pub fn error(&self) -> Option<String> {
        let upload_error = self.linear_error.lock().unwrap().clone();
        upload_error
            .or_else(|| {
                self.linear_shader
                    .lock()
                    .unwrap()
                    .as_ref()
                    .and_then(|s| s.as_ref().err().cloned())
            })
            .or_else(|| {
                self.shader
                    .lock()
                    .unwrap()
                    .as_ref()
                    .and_then(|s| s.as_ref().err().cloned())
            })
    }
    pub fn destroy(&self) {
        for frame in self
            .frames
            .lock()
            .unwrap()
            .drain(..)
            .filter_map(|v| v.upgrade())
        {
            if let Some(gpu) = frame.state.lock().unwrap().gpu.take() {
                unsafe {
                    self.delete_image(gpu);
                }
            }
        }
        unsafe {
            self.collect_garbage();
        }
        if let Some(Ok((program, vao))) = self.linear_shader.lock().unwrap().take() {
            unsafe {
                self.gl.delete_program(program);
                self.gl.delete_vertex_array(vao);
            }
        }
        if let Some(Ok((program, vao))) = self.shader.lock().unwrap().take() {
            unsafe {
                self.gl.delete_program(program);
                self.gl.delete_vertex_array(vao);
            }
        }
    }
}
