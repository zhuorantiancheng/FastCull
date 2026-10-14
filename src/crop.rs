//! Camera Raw crop coordinates refer to the un-oriented source pixels.
//! Left/Top and Right/Bottom are the rotated diagonal endpoints, NOT an AABB.
use eframe::egui::{Pos2, Rect, Vec2};

const UNIT: f32 = 1_000_000.;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Crop {
    pub enabled: bool,
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub angle: i32,
}
impl Default for Crop {
    fn default() -> Self {
        Self {
            enabled: false,
            left: 0,
            top: 0,
            right: 1_000_000,
            bottom: 1_000_000,
            angle: 0,
        }
    }
}

pub fn order(o: u16) -> [usize; 4] {
    match o {
        2 => [1, 0, 3, 2],
        3 => [2, 3, 0, 1],
        4 => [3, 2, 1, 0],
        5 => [0, 3, 2, 1],
        6 => [3, 0, 1, 2],
        7 => [2, 1, 0, 3],
        8 => [1, 2, 3, 0],
        _ => [0, 1, 2, 3],
    }
}
pub fn oriented_size(size: Vec2, o: u16) -> Vec2 {
    if o >= 5 {
        Vec2::new(size.y, size.x)
    } else {
        size
    }
}
fn mirrored(o: u16) -> f32 {
    if matches!(o, 2 | 4 | 5 | 7) { -1. } else { 1. }
}
pub fn rotate(v: Vec2, degrees: f32) -> Vec2 {
    let (s, c) = degrees.to_radians().sin_cos();
    Vec2::new(c * v.x - s * v.y, s * v.x + c * v.y)
}
fn orient(p: Pos2, o: u16) -> Pos2 {
    match o {
        2 => Pos2::new(1. - p.x, p.y),
        3 => Pos2::new(1. - p.x, 1. - p.y),
        4 => Pos2::new(p.x, 1. - p.y),
        5 => Pos2::new(p.y, p.x),
        6 => Pos2::new(1. - p.y, p.x),
        7 => Pos2::new(1. - p.y, 1. - p.x),
        8 => Pos2::new(p.y, 1. - p.x),
        _ => p,
    }
}
fn inverse(p: Pos2, o: u16) -> Pos2 {
    orient(
        p,
        match o {
            6 => 8,
            8 => 6,
            _ => o,
        },
    )
}
pub fn corners(r: Rect) -> [Pos2; 4] {
    [
        r.left_top(),
        r.right_top(),
        r.right_bottom(),
        r.left_bottom(),
    ]
}

/// Axis-aligned crop in the leveled, visually oriented photo plane, in pixels.
#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    pub rect: Rect,
    pub angle: f32,
}
impl Geometry {
    pub fn full(size: Vec2) -> Self {
        Self {
            rect: Rect::from_min_size(Pos2::ZERO, size),
            angle: 0.,
        }
    }
    /// UVs for any rectangle in the leveled plane (also used for the edit backdrop).
    pub fn uvs(self, size: Vec2, o: u16) -> [Pos2; 4] {
        let display = oriented_size(size, o);
        corners(self.rect).map(|p| {
            let source =
                (rotate(p.to_vec2() - display * 0.5, self.angle) + display * 0.5) / display;
            inverse(source.to_pos2(), o)
        })
    }
    /// Fit all four corners inside the source. Preserve aspect ratio and avoid black edges.
    pub fn constrain(&mut self, display: Vec2) {
        let (s, c) = self.angle.to_radians().sin_cos();
        let mut half = self.rect.size().max(Vec2::splat(1.)) * 0.5;
        let extent = Vec2::new(
            c.abs() * half.x + s.abs() * half.y,
            s.abs() * half.x + c.abs() * half.y,
        );
        let scale = (display.x / (2. * extent.x))
            .min(display.y / (2. * extent.y))
            .min(1.);
        half *= scale;
        let extent = Vec2::new(
            c.abs() * half.x + s.abs() * half.y,
            s.abs() * half.x + c.abs() * half.y,
        );
        let mut center =
            rotate(self.rect.center().to_vec2() - display * 0.5, self.angle) + display * 0.5;
        center.x = center.x.clamp(
            extent.x.min(display.x * 0.5),
            (display.x - extent.x).max(display.x * 0.5),
        );
        center.y = center.y.clamp(
            extent.y.min(display.y * 0.5),
            (display.y - extent.y).max(display.y * 0.5),
        );
        center = rotate(center - display * 0.5, -self.angle) + display * 0.5;
        self.rect = Rect::from_center_size(center.to_pos2(), half * 2.);
    }
    pub fn set_ratio(&mut self, ratio: f32, display: Vec2) {
        let mut size = self.rect.size();
        if size.x / size.y > ratio {
            size.x = size.y * ratio;
        } else {
            size.y = size.x / ratio;
        }
        self.rect = Rect::from_center_size(self.rect.center(), size);
        self.constrain(display);
    }
}
impl Crop {
    pub fn valid(self) -> bool {
        [self.left, self.top, self.right, self.bottom]
            .iter()
            .all(|v| (0..=1_000_000).contains(v))
            && (-45_000_000..=45_000_000).contains(&self.angle)
            && (!self.enabled || (self.left != self.right || self.top != self.bottom))
    }
    pub fn raw_uvs(self, size: Vec2) -> [Pos2; 4] {
        if !self.enabled {
            return corners(Rect::from_min_max(Pos2::ZERO, Pos2::new(1., 1.)));
        }
        let a = Vec2::new(self.left as f32, self.top as f32) / UNIT * size;
        let b = Vec2::new(self.right as f32, self.bottom as f32) / UNIT * size;
        let center = (a + b) * 0.5;
        let half = rotate((b - a) * 0.5, -self.angle as f32 / UNIT);
        [
            Vec2::new(-half.x, -half.y),
            Vec2::new(half.x, -half.y),
            half,
            Vec2::new(-half.x, half.y),
        ]
        .map(|v| ((center + rotate(v, self.angle as f32 / UNIT)) / size).to_pos2())
    }
    pub fn geometry(self, size: Vec2, o: u16) -> Geometry {
        let display = oriented_size(size, o);
        if !self.enabled {
            return Geometry::full(display);
        }
        let angle = self.angle as f32 / UNIT * mirrored(o);
        let uv = self.raw_uvs(size);
        let pts = order(o).map(|i| {
            (rotate(orient(uv[i], o).to_vec2() * display - display * 0.5, -angle) + display * 0.5)
                .to_pos2()
        });
        Geometry {
            rect: Rect::from_min_max(pts[0], pts[2]),
            angle,
        }
    }
    pub fn from_geometry(g: Geometry, size: Vec2, o: u16) -> Self {
        let uv = g.uvs(size, o);
        let indices = order(o);
        let a = uv[indices.iter().position(|i| *i == 0).unwrap()];
        let b = uv[indices.iter().position(|i| *i == 2).unwrap()];
        Self {
            enabled: true,
            left: (a.x * UNIT).round() as i32,
            top: (a.y * UNIT).round() as i32,
            right: (b.x * UNIT).round() as i32,
            bottom: (b.y * UNIT).round() as i32,
            angle: (g.angle * mirrored(o) * UNIT).round() as i32,
        }
    }
}
