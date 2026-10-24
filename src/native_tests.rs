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
