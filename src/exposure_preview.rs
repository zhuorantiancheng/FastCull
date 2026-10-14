//! Lazy GPU approximation of an exposure delta on the embedded JPEG.
//! No CPU image copies or additional full-size textures are retained.
use eframe::{egui, egui_glow, glow};
use glow::HasContext;
use std::sync::{Arc, Mutex};

pub struct Preview {
    gl: Arc<glow::Context>,
    shader: Mutex<Option<Result<(glow::Program, glow::VertexArray), String>>>,
}
impl Preview {
    pub fn new(gl: Arc<glow::Context>) -> Arc<Self> {
        Arc::new(Self {
            gl,
            shader: Mutex::new(None),
        })
    }
    unsafe fn compile(&self) -> Result<(glow::Program, glow::VertexArray), String> {
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
                    r#"#version 330
uniform sampler2D image; uniform float gain; in vec2 uv; out vec4 color;
void main() {
    if (any(lessThan(uv,vec2(0))) || any(greaterThan(uv,vec2(1)))) { color=vec4(0,0,0,1); return; }
    vec3 s = texture(image,uv).rgb;
    vec3 linear = mix(s/12.92, pow((s+0.055)/1.055,vec3(2.4)), greaterThan(s,vec3(0.04045)));
    linear = clamp(linear*gain,0.0,1.0);
    vec3 out_s = mix(linear*12.92, 1.055*pow(linear,vec3(1.0/2.4))-0.055, greaterThan(linear,vec3(0.0031308)));
    color = vec4(out_s,1);
}"#,
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
        delta: f32,
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
                    *shader = Some(unsafe { preview.compile() });
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
                            2f32.powf(delta),
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
    pub fn error(&self) -> Option<String> {
        self.shader
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|s| s.as_ref().err().cloned())
    }
    pub fn destroy(&self) {
        if let Some(Ok((program, vao))) = self.shader.lock().unwrap().take() {
            unsafe {
                self.gl.delete_program(program);
                self.gl.delete_vertex_array(vao);
            }
        }
    }
}
