//! Deterministic native checks through real canvas handlers and GPU readback.
use super::*;

#[derive(Default)]
pub(super) struct SurfaceTest {
    frame: u32,
    index: usize,
    before_positions: Vec<Vec3>,
    after_positions: Vec<Vec3>,
    before_mask: Vec<u8>,
    after_mask: Vec<u8>,
}

impl App {
    pub(super) fn selftest_surface_frame(&mut self, t: &mut SurfaceTest) -> bool {
        let tools = [
            BrushKind::MaskPen,
            BrushKind::MaskSoft,
            BrushKind::MaskRect,
            BrushKind::MaskLasso,
            BrushKind::MaskCircle,
            BrushKind::MaskLine,
            BrushKind::ClipLine,
        ];
        let kind = tools[t.index];
        let frame = t.frame;
        t.frame += 1;
        let centre = self.project((self.doc.bounds().min + self.doc.bounds().max) * 0.5);
        let (start, end) = if kind == BrushKind::ClipLine {
            (
                centre + Vec2::new(self.viewport().x * 0.12, -self.viewport().y * 0.4),
                centre + Vec2::new(self.viewport().x * 0.12, self.viewport().y * 0.4),
            )
        } else if matches!(kind, BrushKind::MaskPen | BrushKind::MaskSoft) {
            (centre - Vec2::new(60.0, 0.0), centre + Vec2::new(60.0, 0.0))
        } else {
            (
                centre - Vec2::new(80.0, 60.0),
                centre + Vec2::new(80.0, 60.0),
            )
        };
        let matches_mask = |a: &[u8], b: &[u8], n: usize| {
            (0..n).all(|i| a.get(i).copied().unwrap_or(0) == b.get(i).copied().unwrap_or(0))
        };
        let check_gpu = |app: &App| {
            let gpu = app
                .gfx
                .as_ref()
                .and_then(Gfx::read_positions)
                .unwrap_or_default();
            assert_eq!(gpu.len(), app.mesh().vertex_count());
            assert!(
                gpu.iter()
                    .zip(&app.mesh().positions)
                    .all(|(a, b)| a.distance(*b) < 1e-5),
                "{kind:?} GPU positions"
            );
            let gpu = app
                .gfx
                .as_ref()
                .and_then(Gfx::read_mask)
                .unwrap_or_default();
            assert!(gpu.len() >= app.mesh().vertex_count());
            assert!(
                matches_mask(&gpu, &app.mesh().mask, app.mesh().vertex_count()),
                "{kind:?} GPU mask"
            );
        };
        match frame {
            20 => {
                t.before_positions = self.mesh().positions.clone();
                t.before_mask = self.mesh().mask.clone();
                self.workspace.select_brush(&mut self.brush, kind);
                self.brush.strength = 1.0;
                self.brush.radius = 0.35;
                self.on_key(KeyCode::KeyQ);
                self.cursor_moved(start, false);
                self.left_press(false, false);
            }
            21..=27 => {
                let p = if kind == BrushKind::MaskLasso {
                    let points = [
                        Vec2::new(end.x, start.y),
                        end,
                        Vec2::new(start.x, end.y),
                        start,
                        start,
                        start,
                        start,
                    ];
                    points[(frame - 21) as usize]
                } else {
                    start.lerp(end, (frame - 20) as f32 / 7.0)
                };
                self.cursor_moved(p, false);
            }
            28 => self.left_release(),
            34 => {
                t.after_positions = self.mesh().positions.clone();
                t.after_mask = self.mesh().mask.clone();
                if kind.is_mask() {
                    assert!(t.after_mask.iter().any(|&m| m > 0), "{kind:?} painted");
                    assert!(
                        t.after_positions == t.before_positions,
                        "{kind:?} must not sculpt"
                    );
                } else {
                    assert!(
                        t.after_positions != t.before_positions,
                        "Clip Line must move geometry"
                    );
                }
                check_gpu(self);
                self.history.undo(&mut self.doc);
                self.show_active_level();
            }
            40 => {
                assert!(
                    self.mesh().positions == t.before_positions,
                    "{kind:?} undo positions"
                );
                assert!(
                    matches_mask(
                        &self.mesh().mask,
                        &t.before_mask,
                        self.mesh().vertex_count()
                    ),
                    "{kind:?} undo mask"
                );
                check_gpu(self);
                self.history.redo(&mut self.doc);
                self.show_active_level();
            }
            46 => {
                assert!(
                    self.mesh()
                        .positions
                        .iter()
                        .zip(&t.after_positions)
                        .all(|(a, b)| a.distance(*b) < 1e-6),
                    "{kind:?} redo positions"
                );
                assert!(
                    matches_mask(&self.mesh().mask, &t.after_mask, self.mesh().vertex_count()),
                    "{kind:?} redo mask"
                );
                check_gpu(self);
                self.history.undo(&mut self.doc);
                self.show_active_level();
                log::info!(
                    "selftest-surface: {} input, GPU, undo and redo (ok)",
                    kind.name()
                );
                t.index += 1;
                if t.index == tools.len() {
                    log::info!("selftest-surface: all passed");
                    return true;
                }
                t.frame = 18;
            }
            _ => {}
        }
        false
    }
}

/// State of `--selftest-materials`.
#[derive(Default)]
pub(super) struct MaterialTest {
    frame: u32,
    /// Probe pixels (physical) and the view-space normal the shader sees there.
    probes: Vec<(Vec2, Vec3)>,
    /// What each material drew at the probes.
    drawn: Vec<Vec<[u8; 4]>>,
    ok: bool,
}

/// Frames between materials.
const MATERIAL_STEP: u32 = 6;

impl App {
    /// `--selftest-materials`: draw the model in every material, read the frame back at a few
    /// probe points and compare each MatCap with the CPU's own lookup of its images; then
    /// paint with the Paint brush under a MatCap and check the polypaint tints it.
    pub(super) fn selftest_materials_frame(&mut self, t: &mut MaterialTest) -> bool {
        let count = crate::workspace::MATERIALS.len() as u32;
        let frame = t.frame;
        t.frame += 1;
        let check = |t: &mut MaterialTest, what: String, pass: bool| {
            t.ok &= pass;
            log::info!("selftest-materials: {what} ({})", if pass { "ok" } else { "FAIL" });
        };
        if frame == 18 {
            t.ok = true;
            self.workspace.wireframe = false;
            self.hover = None;
            // Points on the sphere facing the camera, a little off centre (Chrome's horizon
            // runs through the centre) and toward each corner of its MatCap.
            let bounds = self.doc.bounds();
            let (c, r) = ((bounds.min + bounds.max) * 0.5, (bounds.max.x - bounds.min.x) * 0.5);
            let (f, right, up, eye) = (self.camera.forward(), self.camera.right(), self.camera.up(), self.camera.eye());
            t.probes = [(0.1, 0.25), (-0.5, 0.5), (0.55, -0.35), (-0.3, -0.6), (0.45, 0.4)]
                .iter()
                .map(|&(a, b)| {
                    let n = (-f * 0.9 + right * a + up * b).normalize();
                    let p = c + n * r;
                    let v = (eye - p).normalize();
                    let x = up.cross(v).normalize();
                    let y = v.cross(x);
                    (self.project(p), Vec3::new(n.dot(x), n.dot(y), n.dot(v)))
                })
                .collect();
        }
        let encode = |c: Vec3| c.to_array().map(|x| (x.clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0).round() as i32);
        let step = frame.saturating_sub(20);
        if frame >= 20 && step < count * MATERIAL_STEP {
            let material = (step / MATERIAL_STEP) as usize;
            match step % MATERIAL_STEP {
                0 => self.workspace.material = material,
                3 => {
                    if let Some(g) = self.gfx.as_mut() {
                        g.capture = t.probes.iter().map(|(p, _)| [p.x.round() as u32, p.y.round() as u32]).collect();
                    }
                }
                4 => {
                    let drawn = self.gfx.as_mut().map(|g| std::mem::take(&mut g.captured)).unwrap_or_default();
                    if drawn.len() != t.probes.len() {
                        log::warn!("selftest-materials: this surface cannot be read back; skipped");
                        return true;
                    }
                    let name = crate::workspace::MATERIALS[material].1;
                    if let Some(look) = crate::matcap::of_material(material) {
                        let error = t
                            .probes
                            .iter()
                            .zip(&drawn)
                            .map(|((_, n), px)| {
                                let want = encode(crate::matcap::shade(look, look.base(), *n));
                                (0..3).map(|k| (want[k] - px[k] as i32).abs()).max().unwrap()
                            })
                            .max()
                            .unwrap();
                        check(t, format!("{name} matches its MatCap images, max error {error}/255"), error <= 14);
                    } else {
                        check(t, format!("{name} draws"), drawn.iter().all(|p| p[..3] != [24, 25, 28]));
                    }
                    t.drawn.push(drawn);
                }
                _ => {}
            }
            return false;
        }
        let p = count * MATERIAL_STEP + 20;
        let probe = t.probes.first().map(|p| p.0).unwrap_or_default();
        match frame {
            f if f == p => {
                let distinct = (0..t.drawn.len()).all(|a| (0..a).all(|b| t.drawn[a] != t.drawn[b]));
                check(t, format!("all {count} materials look different"), distinct);
                // Polypaint under a MatCap: a red stroke over the first probe.
                self.workspace.material = crate::matcap::FIRST + 6;
                self.workspace.select_brush(&mut self.brush, BrushKind::Paint);
                self.brush.color = [230, 20, 20];
                self.brush.radius = 0.5;
                self.on_key(KeyCode::KeyQ);
                self.cursor_moved(probe, false);
                self.left_press(false, false);
            }
            f if f > p && f < p + 12 => {
                // Scrub back and forth across the probe: paint builds up with each dab.
                let x = if (f - p) % 2 == 0 { -40.0 } else { 40.0 };
                self.cursor_moved(probe + Vec2::new(x, (f - p) as f32 * 3.0 - 18.0), false);
            }
            f if f == p + 12 => self.left_release(),
            f if f == p + 13 => {
                // Off the model, so the brush ring is not drawn over the probe.
                self.cursor_moved(self.canvas_origin() + Vec2::splat(4.0), false);
                self.hover = None;
            }
            f if f == p + 16 => {
                if let Some(g) = self.gfx.as_mut() {
                    g.capture = vec![[probe.x.round() as u32, probe.y.round() as u32]];
                }
            }
            f if f == p + 17 => {
                let px = self.gfx.as_mut().map(|g| std::mem::take(&mut g.captured)).unwrap_or_default();
                let plain = t.drawn[crate::matcap::FIRST + 6][0];
                let painted = px.first().copied().unwrap_or(plain);
                check(
                    t,
                    format!("red polypaint tints MatCap Toy Plastic: {plain:?} -> {painted:?}"),
                    painted[0] as i32 > plain[0] as i32 + 60 && (painted[2] as i32) < plain[2] as i32 - 60,
                );
                self.history.undo(&mut self.doc);
                self.show_active_level();
                self.workspace.material = 0;
                assert!(t.ok, "material self-test failed");
                log::info!("selftest-materials: all passed");
                return true;
            }
            _ => {}
        }
        false
    }
}

/// State of `--selftest-document`.
#[derive(Default)]
pub(super) struct DocumentTest {
    frame: u32,
    ok: bool,
    probes: Vec<(Vec2, [f32; 3])>,
    snapshot: Option<std::path::PathBuf>,
}

impl App {
    /// `--selftest-document`: Document › Back (gradient) and Border drawn on the GPU match
    /// `DocumentLook`; zoomed into the model, the border stays empty canvas; Snapshot writes
    /// the canvas inside the frame as a PNG.
    pub(super) fn selftest_document_frame(&mut self, t: &mut DocumentTest) -> bool {
        let frame = t.frame;
        t.frame += 1;
        let check = |t: &mut DocumentTest, what: String, pass: bool| {
            t.ok &= pass;
            log::info!("selftest-document: {what} ({})", if pass { "ok" } else { "FAIL" });
        };
        let inset = self.workspace.layout.document.inset * self.scale;
        let read = |app: &mut App, at: &[Vec2]| {
            if let Some(g) = app.gfx.as_mut() {
                g.capture = at.iter().map(|p| [p.x.round() as u32, p.y.round() as u32]).collect();
            }
        };
        match frame {
            18 => {
                t.ok = true;
                self.workspace.grid = false;
                self.hover = None;
                self.workspace.layout.document = crate::workspace::DocumentLook {
                    back: [90, 60, 40],
                    range: 1.0,
                    center: 0.5,
                    rate: 1.0,
                    border: [10, 120, 200],
                    ..Default::default()
                };
            }
            22 => {
                // Off the model: a border pixel, and near the top and bottom of the frame.
                let r = self.canvas_rect;
                let (top, bottom) = (r.min.y + inset, r.max.y - inset);
                // Above and below the model, clear of the view label in the top left corner.
                let x = r.center().x;
                let d = self.workspace.layout.document;
                let back = |y: f32| d.back_at((y - top) / (bottom - top));
                let border = d.border.map(|c| c as f32 / 255.0);
                t.probes = vec![
                    (Vec2::new(r.min.x + inset * 0.5, r.center().y), border),
                    (Vec2::new(x, top + 6.0), back(top + 6.0)),
                    (Vec2::new(x, bottom - 7.0), back(bottom - 7.0)),
                ];
                let at: Vec<Vec2> = t.probes.iter().map(|p| p.0).collect();
                read(self, &at);
            }
            23 => {
                let px = self.gfx.as_mut().map(|g| std::mem::take(&mut g.captured)).unwrap_or_default();
                if px.len() != t.probes.len() {
                    log::warn!("selftest-document: this surface cannot be read back; skipped");
                    return true;
                }
                let error = t
                    .probes
                    .iter()
                    .zip(&px)
                    .map(|((_, want), got)| (0..3).map(|k| (want[k] * 255.0 - got[k] as f32).abs()).fold(0.0, f32::max))
                    .fold(0.0, f32::max);
                check(t, format!("Border and the Back gradient match, max error {error:.1}/255"), error <= 2.0);
                check(t, format!("gradient runs light to dark: top {:?}, bottom {:?}", px[1], px[2]), px[1][0] > px[2][0] + 60);
                // Zoom right into the model so it covers the whole canvas.
                self.camera.distance *= 0.45;
            }
            27 => {
                let r = self.canvas_rect;
                let border = Vec2::new(r.min.x + inset * 0.5, r.center().y);
                let inside = Vec2::new(r.center().x, r.center().y);
                t.probes = vec![(border, [0.0; 3]), (inside, [0.0; 3])];
                read(self, &[border, inside]);
            }
            28 => {
                let px = self.gfx.as_mut().map(|g| std::mem::take(&mut g.captured)).unwrap_or_default();
                let want = self.workspace.layout.document.border;
                check(t, format!("zoomed in, the border stays empty canvas: {:?}", px[0]), px[0][..3] == want);
                let back = self.workspace.layout.document.back_at(0.5).map(|c| (c * 255.0).round() as u8);
                check(t, format!("and the model fills the frame: {:?}", px[1]), px[1][..3] != want && px[1][..3] != back);
                self.request_snapshot();
            }
            30 => {
                let stem = std::path::Path::new(&self.project_path).file_stem().unwrap().to_string_lossy().to_string();
                let dir = std::path::Path::new(&self.project_path).parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(std::path::Path::new(".")).to_path_buf();
                let path = dir.join(format!("{stem} snapshot.png"));
                let size = image::image_dimensions(&path).ok();
                let r = self.canvas_rect.shrink(inset);
                let want = (r.width().round() as u32, r.height().round() as u32);
                check(t, format!("Snapshot wrote {} at {size:?}", path.display()), size == Some(want));
                t.snapshot = Some(path);
            }
            31 => {
                if let Some(p) = t.snapshot.take() {
                    let _ = std::fs::remove_file(p);
                }
                self.workspace.layout.document = crate::workspace::DocumentLook::default();
                assert!(t.ok, "document self-test failed");
                log::info!("selftest-document: all passed");
                // --hold: stay open, zoomed in, to look at the empty border.
                return !self.selftest_hold;
            }
            _ => {}
        }
        false
    }
}

/// State of `--selftest-symmetry`.
#[derive(Default)]
pub(super) struct SymmetryTest {
    frame: u32,
    index: usize,
    original: Vec<Vec3>,
    edited: Vec<Vec3>,
}

impl App {
    /// `--selftest-symmetry`: Draw, Grab and Snake Hook (GPU) strokes through the canvas
    /// with Y mirror and Radial Z 6 on: every copy moves, the GPU copy matches the CPU,
    /// undo and redo are exact.
    pub(super) fn selftest_symmetry_frame(&mut self, t: &mut SymmetryTest) -> bool {
        let kinds = [BrushKind::Draw, BrushKind::Grab, BrushKind::SnakeHook, BrushKind::Inflate];
        let Some(&kind) = kinds.get(t.index) else {
            log::info!("selftest-symmetry: all passed");
            return true;
        };
        let frame = t.frame;
        t.frame += 1;
        let sym = crate::symmetry::Symmetry { y: true, radial: Some((crate::symmetry::Axis::Z, 6)), ..Default::default() };
        let start = Vec3::new(0.55, 0.2, 0.81).normalize();
        let check_gpu = |app: &App| {
            let gpu = app.gfx.as_ref().and_then(Gfx::read_positions).unwrap_or_default();
            let error = gpu.iter().zip(&app.mesh().positions).map(|(a, b)| a.distance(*b)).fold(0.0f32, f32::max);
            assert!(gpu.len() == app.mesh().vertex_count() && error < 1e-5, "{kind:?} CPU/GPU error {error}");
        };
        match frame {
            20 => {
                self.workspace.symmetry = false;
                self.workspace.symmetry_y = sym.y;
                self.workspace.radial = true;
                self.workspace.radial_axis = crate::symmetry::Axis::Z;
                self.workspace.radial_count = 6.0;
                assert_eq!(self.workspace.sym(), sym);
                t.original = self.mesh().positions.clone();
                self.workspace.select_brush(&mut self.brush, kind);
                self.brush.radius = 0.2;
                self.brush.strength = 0.8;
                self.on_key(KeyCode::KeyQ);
                let p = self.project(start);
                self.cursor_moved(p, false);
                self.left_press(false, false);
            }
            21..=27 => {
                let p = self.project(start) + Vec2::new((frame - 20) as f32 * 6.0, -((frame - 20) as f32) * 3.0);
                self.cursor_moved(p, false);
            }
            28 => self.left_release(),
            34 => {
                t.edited = self.mesh().positions.clone();
                let images = sym.images();
                let moved = |q: Vec3| {
                    let v = (0..t.original.len()).min_by(|&a, &b| t.original[a].distance(q).total_cmp(&t.original[b].distance(q))).unwrap();
                    t.edited[v].distance(t.original[v])
                };
                let least = (0..images.len()).map(|k| moved(images.map(k, start))).fold(f32::INFINITY, f32::min);
                assert!(least > 1e-4, "{kind:?}: every one of the {} copies moves (least {least})", images.len());
                check_gpu(self);
                self.history.undo(&mut self.doc);
                self.show_active_level();
            }
            40 => {
                assert!(self.mesh().positions == t.original, "{kind:?} undo");
                check_gpu(self);
                self.history.redo(&mut self.doc);
                self.show_active_level();
            }
            46 => {
                assert!(self.mesh().positions.iter().zip(&t.edited).all(|(a, b)| a.distance(*b) < 1e-6), "{kind:?} redo");
                check_gpu(self);
                self.history.undo(&mut self.doc);
                self.show_active_level();
                log::info!("selftest-symmetry: {} with Y and Radial Z 6 ({} copies): canvas, GPU, undo and redo (ok)", kind.name(), sym.images().len());
                t.index += 1;
                t.frame = 18;
            }
            _ => {}
        }
        false
    }
}

/// State of `--selftest-boolean`.
#[derive(Default)]
pub(super) struct BooleanTest {
    frame: u32,
    probes: Vec<Vec2>,
    /// Pixels: the bite drawn as it is, with Live Boolean, and the made boolean mesh.
    plain: Vec<[u8; 4]>,
    live: Vec<[u8; 4]>,
    ok: bool,
}

impl App {
    /// `--selftest-boolean`: a ball subtracted from the front of the model. The frame is
    /// read back at probes across the bite with Live Boolean off and on, then after Make
    /// Boolean Mesh: the live view must match the made mesh and differ from the plain one.
    pub(super) fn selftest_boolean_frame(&mut self, t: &mut BooleanTest) -> bool {
        let frame = t.frame;
        t.frame += 1;
        let check = |t: &mut BooleanTest, what: String, pass: bool| {
            t.ok &= pass;
            log::info!("selftest-boolean: {what} ({})", if pass { "ok" } else { "FAIL" });
        };
        let capture = |app: &mut App, probes: &[Vec2]| {
            if let Some(g) = app.gfx.as_mut() {
                g.capture = probes.iter().map(|p| [p.x.round() as u32, p.y.round() as u32]).collect();
            }
        };
        let take = |app: &mut App| app.gfx.as_mut().map(|g| std::mem::take(&mut g.captured)).unwrap_or_default();
        let diff = |a: &[u8; 4], b: &[u8; 4]| (0..3).map(|k| (a[k] as i32 - b[k] as i32).abs()).max().unwrap();
        match frame {
            18 => {
                t.ok = true;
                self.workspace.wireframe = false;
                let b = self.doc.bounds();
                let (c, r) = ((b.min + b.max) * 0.5, (b.max - b.min).x * 0.5);
                let f = self.camera.forward();
                let (right, up) = (self.camera.right(), self.camera.up());
                let front = c - f * r;
                let (p, i) = cube_sphere(24, r * 0.4);
                let bite = SculptMesh::from_raw(p.into_iter().map(|q| q + front).collect(), i);
                self.insert_after_active(vec![("Bite".into(), bite)]);
                self.subtools[self.active_tool + 1].boolean = boolean::BoolOp::Subtract;
                // Probes in the bite (away from its rim) and on the model around it.
                t.probes = [(0.0, 0.0), (0.1, 0.06), (-0.08, -0.1), (0.15, -0.05), (0.75, 0.0), (-0.7, 0.3), (0.1, -0.78)]
                    .iter()
                    .map(|&(x, y)| {
                        let q = right * x + up * y;
                        let z = (1.0f32 - x * x - y * y).max(0.0).sqrt();
                        self.project(c + (q - f * z) * r)
                    })
                    .collect();
                self.cursor_moved(self.canvas_origin() + Vec2::splat(4.0), false);
                self.hover = None;
            }
            22 => capture(self, &t.probes.clone()),
            23 => {
                t.plain = take(self);
                if t.plain.len() != t.probes.len() {
                    log::warn!("selftest-boolean: this surface cannot be read back; skipped");
                    return true;
                }
                self.workspace.live_boolean = true;
            }
            28 => capture(self, &t.probes.clone()),
            29 => {
                t.live = take(self);
                let changed = (0..4).map(|k| diff(&t.live[k], &t.plain[k])).max().unwrap();
                check(t, format!("Live Boolean carves the bite: it changed by up to {changed}/255"), changed > 20);
                let outside = (4..7).map(|k| diff(&t.live[k], &t.plain[k])).max().unwrap();
                check(t, format!("the model around it is untouched ({outside}/255)"), outside <= 3);
                let start = Instant::now();
                self.make_boolean_mesh();
                log::info!("selftest-boolean: {}", self.status);
                check(t, format!("Make Boolean Mesh in {:.0} ms", start.elapsed().as_secs_f32() * 1000.0), self.subtools.last().is_some_and(|s| s.name.starts_with("Boolean")));
                self.hover = None;
            }
            36 => capture(self, &t.probes.clone()),
            37 => {
                let made = take(self);
                let errors: Vec<i32> = (0..t.probes.len()).map(|k| diff(&made[k], &t.live[k])).collect();
                let worst = *errors.iter().max().unwrap();
                let mean = errors.iter().sum::<i32>() as f32 / errors.len() as f32;
                check(t, format!("the live view matches the made mesh: worst {worst}/255, mean {mean:.1}"), worst <= 40 && mean <= 15.0);
                log::info!("selftest-boolean: {}", if t.ok { "all passed" } else { "FAILED" });
                return true;
            }
            _ => {}
        }
        false
    }
}
