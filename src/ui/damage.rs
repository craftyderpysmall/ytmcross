//! Which parts of the window changed since the last frame.
//!
//! Every painted shape is hashed together with the pixels it can touch.
//! Shapes that appeared or disappeared since the previous frame mark their
//! area as damaged, and only that area is rasterized and copied to the
//! window: a hover redraws one row, the playing clock a few dozen pixels,
//! and a frame in which nothing visible changed draws nothing at all.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hash, Hasher};

use egui::epaint::{ClippedPrimitive, ClippedShape, ColorMode, Mesh, PathStroke, Primitive, Shape};
use egui::{Pos2, Rect, pos2};

/// FxHash: fast, and plenty for telling a frame's shapes apart.
#[derive(Default)]
struct Fx(u64);

impl Hasher for Fx {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        let (chunks, rest) = bytes.as_chunks::<8>();
        for c in chunks {
            self.write_u64(u64::from_le_bytes(*c));
        }
        if !rest.is_empty() {
            let mut last = [0u8; 8];
            last[..rest.len()].copy_from_slice(rest);
            self.write_u64(u64::from_le_bytes(last) ^ rest.len() as u64);
        }
    }

    fn write_u64(&mut self, v: u64) {
        self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

/// For keys that are hashes already.
#[derive(Default)]
struct Same(u64);

impl Hasher for Same {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, _: &[u8]) {
        unreachable!("only u64 keys")
    }

    fn write_u64(&mut self, v: u64) {
        self.0 = v;
    }
}

/// A rectangle of pixels, max exclusive.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Region {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

impl Region {
    pub fn full(w: u32, h: u32) -> Region {
        Region { x0: 0, y0: 0, x1: w, y1: h }
    }

    pub fn width(&self) -> u32 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> u32 {
        self.y1 - self.y0
    }

    fn area(&self) -> u64 {
        u64::from(self.width()) * u64::from(self.height())
    }

    fn union(&self, o: &Region) -> Region {
        Region { x0: self.x0.min(o.x0), y0: self.y0.min(o.y0), x1: self.x1.max(o.x1), y1: self.y1.max(o.y1) }
    }

    /// Overlapping, or closer than `gap`: cheaper to draw as one.
    fn near(&self, o: &Region, gap: u32) -> bool {
        self.x0 <= o.x1 + gap && o.x0 <= self.x1 + gap && self.y0 <= o.y1 + gap && o.y0 <= self.y1 + gap
    }

    fn meets(&self, r: &Rect) -> bool {
        r.min.x < self.x1 as f32 && r.max.x > self.x0 as f32 && r.min.y < self.y1 as f32 && r.max.y > self.y0 as f32
    }
}

/// What a frame painted: per shape, a hash and the pixels it can touch.
#[derive(Default)]
pub struct Scene {
    shapes: Vec<(u64, Rect)>,
}

impl Scene {
    /// Hashes the visible shapes of a frame. Returns them too, flattened
    /// and in step with the scene.
    pub fn new(shapes: Vec<ClippedShape>, pixels_per_point: f32) -> (Scene, Vec<ClippedShape>) {
        let mut scene = Scene { shapes: Vec::with_capacity(shapes.len()) };
        let mut visible = Vec::with_capacity(shapes.len());
        for s in shapes {
            scene.add(s.clip_rect, s.shape, pixels_per_point, &mut visible);
        }
        (scene, visible)
    }

    fn add(&mut self, clip: Rect, shape: Shape, ppp: f32, visible: &mut Vec<ClippedShape>) {
        match shape {
            Shape::Noop => {}
            Shape::Vec(list) => list.into_iter().for_each(|s| self.add(clip, s, ppp, visible)),
            shape => {
                let bounds = shape.visual_bounding_rect().intersect(clip);
                if !bounds.is_positive() {
                    return;
                }
                let mut h = Fx::default();
                rect(&mut h, clip);
                hash_shape(&mut h, &shape);
                // Anti-aliasing feathers about a pixel beyond the geometry.
                self.shapes.push((h.finish(), (bounds * ppp).expand(2.0)));
                visible.push(ClippedShape { clip_rect: clip, shape });
            }
        }
    }

    /// Of this scene's `shapes`, the ones that can touch `regions`: the rest
    /// needn't even be tessellated.
    pub fn touching(&self, shapes: Vec<ClippedShape>, regions: &[Region]) -> Vec<ClippedShape> {
        shapes.into_iter().zip(&self.shapes).filter(|(_, (_, r))| regions.iter().any(|g| g.meets(r))).map(|(s, _)| s).collect()
    }
}

pub enum Damage {
    None,
    Full,
    Regions(Vec<Region>),
}

/// Drawing more separate regions than this costs more than it saves.
const MAX_REGIONS: usize = 6;

/// What must be redrawn to go from `old` to `new` on a `w` x `h` window.
pub fn diff(old: &Scene, new: &Scene, w: u32, h: u32) -> Damage {
    if old.shapes.len() == new.shapes.len() && old.shapes.iter().zip(&new.shapes).all(|(a, b)| a.0 == b.0) {
        return Damage::None;
    }
    // Shapes only in one of the frames (counted, as identical shapes can repeat).
    let mut count: HashMap<u64, i32, BuildHasherDefault<Same>> =
        HashMap::with_capacity_and_hasher(old.shapes.len() + new.shapes.len(), Default::default());
    old.shapes.iter().for_each(|(k, _)| *count.entry(*k).or_default() += 1);
    new.shapes.iter().for_each(|(k, _)| *count.entry(*k).or_default() -= 1);
    let common = |s: &&(u64, Rect)| count[&s.0] == 0;
    // The shapes both frames share must still stack in the same order.
    if !old.shapes.iter().filter(common).map(|s| s.0).eq(new.shapes.iter().filter(common).map(|s| s.0)) {
        return Damage::Full;
    }
    let mut regions: Vec<Region> = Vec::new();
    for (_, r) in old.shapes.iter().chain(&new.shapes).filter(|s| count[&s.0] != 0) {
        let x0 = r.min.x.floor().clamp(0.0, w as f32) as u32;
        let y0 = r.min.y.floor().clamp(0.0, h as f32) as u32;
        let x1 = r.max.x.ceil().clamp(0.0, w as f32) as u32;
        let y1 = r.max.y.ceil().clamp(0.0, h as f32) as u32;
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        let mut region = Region { x0, y0, x1, y1 };
        while let Some(i) = regions.iter().position(|q| q.near(&region, 16)) {
            region = region.union(&regions.swap_remove(i));
        }
        regions.push(region);
    }
    let area: u64 = regions.iter().map(Region::area).sum();
    if regions.len() > MAX_REGIONS || area * 2 > u64::from(w) * u64::from(h) {
        Damage::Full
    } else if regions.is_empty() {
        Damage::None
    } else {
        Damage::Regions(regions)
    }
}

/// Ends each mesh with a degenerate triangle. The rasterizer only treats a
/// quad as a rectangle when more indices follow it; sealed, every quad is
/// treated alike whichever part of the frame its mesh was cut from.
pub fn seal(primitives: &mut [ClippedPrimitive]) {
    for p in primitives {
        if let Primitive::Mesh(m) = &mut p.primitive
            && let Some(&k) = m.indices.first()
        {
            m.indices.extend([k, k, k]);
        }
    }
}

/// The parts of `primitives` that can touch `region`: only those triangles
/// are rasterized. Vertices keep their positions, so the pixels come out
/// exactly as in a full frame (once both are sealed).
pub fn clip(primitives: &[ClippedPrimitive], region: Region, ppp: f32) -> Vec<ClippedPrimitive> {
    let area = Rect::from_min_max(pos2(region.x0 as f32, region.y0 as f32), pos2(region.x1 as f32, region.y1 as f32)) / ppp;
    // Triangles touching the edge feather into it.
    let near = area.expand(1.0 / ppp);
    primitives
        .iter()
        .filter_map(|p| {
            let Primitive::Mesh(mesh) = &p.primitive else { return None };
            let clip_rect = p.clip_rect.intersect(area);
            if !clip_rect.is_positive() {
                return None;
            }
            let mesh = mesh_within(mesh, near)?;
            Some(ClippedPrimitive { clip_rect, primitive: Primitive::Mesh(mesh) })
        })
        .collect()
}

/// The triangles of `mesh` whose bounds meet `area`. A quad's two triangles
/// stay together, so the rasterizer pairs them into rectangles exactly as
/// it does for the whole mesh.
fn mesh_within(mesh: &Mesh, area: Rect) -> Option<Mesh> {
    let (vertices, indices) = (&mesh.vertices, &mesh.indices);
    let mut out = Mesh::with_texture(mesh.texture_id);
    let mut remap = vec![u32::MAX; vertices.len()];
    let mut i = 0;
    while i + 3 <= indices.len() {
        let quad = i + 6 <= indices.len() && {
            let (a, b) = (&indices[i..i + 3], &indices[i + 3..i + 6]);
            b.iter().filter(|k| a.contains(k)).count() >= 2
        };
        let unit = &indices[i..i + if quad { 6 } else { 3 }];
        i += unit.len();
        let mut bounds = Rect::NOTHING;
        unit.iter().for_each(|&k| bounds.extend_with(vertices[k as usize].pos));
        if !bounds.intersects(area) {
            continue;
        }
        for &k in unit {
            let slot = &mut remap[k as usize];
            if *slot == u32::MAX {
                *slot = out.vertices.len() as u32;
                out.vertices.push(vertices[k as usize]);
            }
            out.indices.push(*slot);
        }
    }
    (!out.indices.is_empty()).then_some(out)
}

fn f(h: &mut Fx, v: f32) {
    v.to_bits().hash(h);
}

fn point(h: &mut Fx, p: Pos2) {
    f(h, p.x);
    f(h, p.y);
}

fn rect(h: &mut Fx, r: Rect) {
    point(h, r.min);
    point(h, r.max);
}

fn path_stroke(h: &mut Fx, s: &PathStroke) {
    f(h, s.width);
    (s.kind as u8).hash(h);
    match &s.color {
        ColorMode::Solid(c) => c.hash(h),
        // A color callback can't be compared: always redraw it.
        ColorMode::UV(_) => std::time::Instant::now().hash(h),
    }
}

fn hash_shape(h: &mut Fx, shape: &Shape) {
    std::mem::discriminant(shape).hash(h);
    match shape {
        Shape::Noop | Shape::Vec(_) => {}
        Shape::Circle(c) => {
            point(h, c.center);
            f(h, c.radius);
            c.fill.hash(h);
            c.stroke.hash(h);
        }
        Shape::Ellipse(e) => {
            point(h, e.center);
            f(h, e.radius.x);
            f(h, e.radius.y);
            e.fill.hash(h);
            e.stroke.hash(h);
            f(h, e.angle);
        }
        Shape::LineSegment { points, stroke } => {
            points.iter().for_each(|p| point(h, *p));
            stroke.hash(h);
        }
        Shape::Path(p) => {
            p.points.iter().for_each(|q| point(h, *q));
            p.closed.hash(h);
            p.fill.hash(h);
            path_stroke(h, &p.stroke);
        }
        Shape::Rect(r) => {
            rect(h, r.rect);
            r.corner_radius.hash(h);
            r.fill.hash(h);
            r.stroke.hash(h);
            (r.stroke_kind as u8).hash(h);
            r.round_to_pixels.hash(h);
            f(h, r.blur_width);
            if let Some(b) = &r.brush {
                b.fill_texture_id.hash(h);
                rect(h, b.uv);
            }
            f(h, r.angle);
        }
        Shape::Text(t) => {
            point(h, t.pos);
            // What the galley shows: its layout job and the result's extent.
            let g = &t.galley;
            g.job.hash(h);
            rect(h, g.rect);
            rect(h, g.mesh_bounds);
            (g.num_vertices, g.num_indices).hash(h);
            // A text selection is painted into the rows themselves: a
            // rectangle appended after the glyphs, which are recolored under
            // it (egui's text_selection). The rectangles pin it all down.
            for row in &g.rows {
                let v = &row.visuals;
                v.mesh.vertices.len().hash(h);
                for vertex in v.mesh.vertices.get(v.glyph_vertex_range.end..).unwrap_or_default() {
                    point(h, vertex.pos);
                    vertex.color.hash(h);
                }
            }
            t.underline.hash(h);
            t.fallback_color.hash(h);
            t.override_text_color.hash(h);
            f(h, t.opacity_factor);
            f(h, t.angle);
        }
        Shape::Mesh(m) => {
            m.texture_id.hash(h);
            for v in &m.vertices {
                point(h, v.pos);
                point(h, v.uv);
                v.color.hash(h);
            }
            m.indices.hash(h);
        }
        Shape::QuadraticBezier(b) => {
            b.points.iter().for_each(|p| point(h, *p));
            b.closed.hash(h);
            b.fill.hash(h);
            path_stroke(h, &b.stroke);
        }
        Shape::CubicBezier(b) => {
            b.points.iter().for_each(|p| point(h, *p));
            b.closed.hash(h);
            b.fill.hash(h);
            path_stroke(h, &b.stroke);
        }
        // Painted by a backend we don't have; always redraw it.
        Shape::Callback(_) => std::time::Instant::now().hash(h),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Color32, vec2};

    fn scene(shapes: Vec<Shape>) -> Scene {
        let clip = Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 1000.0));
        let clipped: Vec<ClippedShape> = shapes.into_iter().map(|shape| ClippedShape { clip_rect: clip, shape }).collect();
        Scene::new(clipped, 1.0).0
    }

    fn rect_shape(x: f32, color: Color32) -> Shape {
        Shape::rect_filled(Rect::from_min_size(pos2(x, 10.0), vec2(20.0, 20.0)), 0.0, color)
    }

    #[test]
    fn unchanged_frames_draw_nothing() {
        let a = scene(vec![rect_shape(10.0, Color32::RED), rect_shape(100.0, Color32::BLUE)]);
        let b = scene(vec![rect_shape(10.0, Color32::RED), rect_shape(100.0, Color32::BLUE)]);
        assert!(matches!(diff(&a, &b, 1000, 1000), Damage::None));
    }

    #[test]
    fn a_changed_shape_damages_only_its_area() {
        let a = scene(vec![rect_shape(10.0, Color32::RED), rect_shape(500.0, Color32::BLUE)]);
        let b = scene(vec![rect_shape(10.0, Color32::RED), rect_shape(500.0, Color32::GREEN)]);
        let Damage::Regions(r) = diff(&a, &b, 1000, 1000) else { panic!("expected regions") };
        assert_eq!(r, vec![Region { x0: 498, y0: 8, x1: 522, y1: 32 }]);
    }

    #[test]
    fn a_text_selection_is_damage() {
        use egui::epaint::TextShape;
        let ctx = egui::Context::default();
        let mut galley = None;
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            galley = Some(ui.painter().layout_no_wrap("selected".into(), egui::FontId::default(), Color32::WHITE));
        });
        let galley = galley.unwrap();
        let plain = Shape::Text(TextShape::new(pos2(10.0, 10.0), galley.clone(), Color32::WHITE));
        let mut selected = galley;
        let visuals = egui::Visuals::dark();
        let range = egui::text_selection::CCursorRange::two(egui::text::CCursor::new(1), egui::text::CCursor::new(4));
        egui::text_selection::visuals::paint_text_selection(&mut selected, &visuals, &range, None);
        let shown = Shape::Text(TextShape::new(pos2(10.0, 10.0), selected, Color32::WHITE));
        assert!(!matches!(diff(&scene(vec![plain]), &scene(vec![shown]), 1000, 1000), Damage::None));
    }

    #[test]
    fn reordering_shared_shapes_redraws_everything() {
        let a = scene(vec![rect_shape(10.0, Color32::RED), rect_shape(15.0, Color32::BLUE)]);
        let b = scene(vec![rect_shape(15.0, Color32::BLUE), rect_shape(10.0, Color32::RED)]);
        assert!(matches!(diff(&a, &b, 1000, 1000), Damage::Full));
    }

    #[test]
    fn clipped_mesh_keeps_quads_whole() {
        let mut mesh = Mesh::default();
        mesh.add_colored_rect(Rect::from_min_size(pos2(0.0, 0.0), vec2(10.0, 10.0)), Color32::RED);
        mesh.add_colored_rect(Rect::from_min_size(pos2(100.0, 0.0), vec2(10.0, 10.0)), Color32::RED);
        mesh.add_colored_rect(Rect::from_min_size(pos2(200.0, 0.0), vec2(10.0, 10.0)), Color32::RED);
        let inner = mesh_within(&mesh, Rect::from_min_size(pos2(95.0, 0.0), vec2(20.0, 20.0))).unwrap();
        assert_eq!((inner.vertices.len(), inner.indices.len()), (4, 6));
        assert!(mesh_within(&mesh, Rect::from_min_size(pos2(50.0, 50.0), vec2(20.0, 20.0))).is_none());
    }

    #[test]
    fn only_shapes_near_damage_are_kept() {
        let clip = Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 1000.0));
        let shapes = vec![rect_shape(10.0, Color32::RED), rect_shape(500.0, Color32::BLUE)];
        let clipped = shapes.into_iter().map(|shape| ClippedShape { clip_rect: clip, shape }).collect();
        let (scene, visible) = Scene::new(clipped, 1.0);
        let kept = scene.touching(visible, &[Region { x0: 490, y0: 0, x1: 600, y1: 100 }]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].shape.visual_bounding_rect().min.x, 500.0);
    }
}
