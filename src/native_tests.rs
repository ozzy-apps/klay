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
