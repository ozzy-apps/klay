mod camera;
mod mesh;
mod render;
mod sculpt;
mod workspace;

use camera::Camera;
use glam::{Vec2, Vec3};
use mesh::{SculptMesh, cube_sphere};
use render::{Gfx, Uniforms};
use sculpt::{Brush, BrushKind, History, Stroke};
use workspace::{Mode, QuickPopup};
use std::sync::Arc;
use std::time::Instant;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::window::{Window, WindowId};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum NavMode {
    Orbit,
    Pan,
    Zoom,
    Roll,
}

/// A camera drag in progress. The mode is not stored: it is derived from the button and
/// the current modifiers on every move, so modifiers can be changed mid-drag.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct NavDrag {
    button: MouseButton,
    /// Alt has been held at some point during a left-button drag. From then on the drag
    /// pans while Alt is down and zooms once Alt is released (ZBrush: Alt + click,
    /// release Alt, drag = zoom). Before Alt is ever used a left drag orbits.
    alt_used: bool,
    /// Shift was held when the left button went down: releasing Shift then turns the
    /// drag into a roll about the view axis (ZBrush: Shift, click, release Shift, drag).
    shift_at_press: bool,
}

impl NavDrag {
    fn mode(&self, m: ModifiersState) -> NavMode {
        match self.button {
            MouseButton::Left => {
                if m.alt_key() {
                    NavMode::Pan
                } else if self.alt_used {
                    NavMode::Zoom
                } else if self.shift_at_press && !m.shift_key() {
                    NavMode::Roll
                } else {
                    NavMode::Orbit
                }
            }
            // Right/middle (ZBrush right-click navigation): orbit; Alt pans; Ctrl zooms.
            _ => {
                if m.control_key() {
                    NavMode::Zoom
                } else if m.alt_key() {
                    NavMode::Pan
                } else {
                    NavMode::Orbit
                }
            }
        }
    }
}

struct App {
    window: Option<Arc<Window>>,
    gfx: Option<Gfx>,
    egui_ctx: egui::Context,
    egui_state: Option<egui_winit::State>,

    mesh: SculptMesh,
    camera: Camera,
    brush: Brush,
    history: History,
    stroke: Option<Stroke>,
    stroke_brush: Brush,

    cursor: Vec2,
    last_cursor: Vec2,
    /// egui points per physical pixel, for placing popups at the cursor.
    scale: f32,
    modifiers: ModifiersState,
    nav: Option<NavDrag>,
    hover: Option<Vec3>,

    // UI state.
    workspace: workspace::Workspace,
    canvas_rect: egui::Rect,
    segments: u32,
    export_path: String,
    status: String,
    pending_rebuild: bool,
    pending_export: bool,
    pending_undo: bool,
    pending_redo: bool,

    // Stats.
    build_ms: f32,
    last_frame: Instant,
    frame_ms: f32,
    fps: f32,
    stroke_ms: f32,
    /// A grab update is waiting for the next frame (grab is applied once per frame).
    stroke_pending: bool,
    last_dab_count: usize,
    demo: bool,
    vsync: bool,
    /// Frames left in `--stress` mode (a large grab moved every frame), plus accumulators.
    stress: Option<(u32, f32, f32)>,
    upload_ms: f32,
    render_ms: f32,
    stress_upload: f32,
    stress_render: f32,
}

impl App {
    fn new(segments: u32, demo: bool) -> Self {
        let t = Instant::now();
        let (p, i) = cube_sphere(segments, 1.0);
        let mesh = SculptMesh::from_raw(p, i);
        let build_ms = t.elapsed().as_secs_f32() * 1000.0;
        Self {
            window: None,
            gfx: None,
            egui_ctx: egui::Context::default(),
            egui_state: None,
            mesh,
            camera: Camera::new(),
            brush: Brush::default(),
            history: History::new(32),
            stroke: None,
            stroke_brush: Brush::default(),
            cursor: Vec2::ZERO,
            last_cursor: Vec2::ZERO,
            scale: 1.0,
            modifiers: ModifiersState::empty(),
            nav: None,
            hover: None,
            workspace: workspace::Workspace::new(),
            canvas_rect: egui::Rect::NOTHING,
            segments,
            export_path: "klay_export.obj".to_string(),
            status: String::new(),
            pending_rebuild: false,
            pending_export: false,
            pending_undo: false,
            pending_redo: false,
            build_ms,
            last_frame: Instant::now(),
            frame_ms: 0.0,
            fps: 0.0,
            stroke_ms: 0.0,
            stroke_pending: false,
            last_dab_count: 0,
            demo,
            vsync: true,
            stress: None,
            upload_ms: 0.0,
            render_ms: 0.0,
            stress_upload: 0.0,
            stress_render: 0.0,
        }
    }

    /// Scripted strokes through the real input path: used by `--demo` and `--bench`.
    fn scripted_strokes(&mut self) -> Vec<(String, f32, u32, usize)> {
        let vp = self.viewport();
        let c = self.canvas_origin() + vp * 0.5;
        let r = vp.y * 0.33;
        let mut report = Vec::new();
        let scripts: Vec<(BrushKind, f32, f32, bool, Vec<Vec2>)> = vec![
            // kind, radius, strength, invert, path (unit circle coords)
            (BrushKind::Draw, 0.18, 0.8, false, arc(0.2, 1.4, 0.55, 60)),
            (BrushKind::Draw, 0.12, 0.8, true, arc(-0.9, -0.1, 0.5, 40)),
            (BrushKind::Inflate, 0.2, 0.7, false, arc(2.0, 2.8, 0.6, 40)),
            (
                BrushKind::Flatten,
                0.35,
                0.9,
                false,
                vec![Vec2::new(0.0, 0.0); 20],
            ),
            (BrushKind::Smooth, 0.2, 0.8, false, arc(0.2, 1.4, 0.55, 40)),
            (
                BrushKind::Grab,
                0.3,
                0.6,
                false,
                line(Vec2::new(-0.5, 0.55), Vec2::new(-0.2, 0.9), 15),
            ),
            // Large-radius strokes: these cover a big share of the mesh.
            (BrushKind::Draw, 0.8, 0.5, false, arc(3.5, 4.5, 0.4, 30)),
            (
                BrushKind::Grab,
                1.2,
                0.5,
                false,
                line(Vec2::new(0.3, -0.3), Vec2::new(0.5, -0.1), 30),
            ),
        ];
        for (kind, radius, strength, invert, path) in scripts {
            self.brush = Brush {
                kind,
                radius,
                strength,
            };
            self.modifiers = if invert {
                ModifiersState::ALT
            } else {
                ModifiersState::empty()
            };
            let t = Instant::now();
            let mut dabs = 0;
            let mut count = 0;
            for (i, p) in path.iter().enumerate() {
                self.cursor = c + *p * r;
                if i == 0 {
                    self.begin_stroke();
                } else {
                    self.apply_stroke();
                }
            }
            if let Some(s) = &self.stroke {
                dabs = s.dabs;
                count = s.last_count;
            }
            self.end_stroke();
            report.push((
                kind.name().to_string(),
                t.elapsed().as_secs_f32() * 1000.0,
                dabs,
                count,
            ));
        }
        self.modifiers = ModifiersState::empty();
        self.brush = Brush::default();
        report
    }

    fn canvas_origin(&self) -> Vec2 {
        if self.canvas_rect.is_positive() {
            Vec2::new(self.canvas_rect.min.x, self.canvas_rect.min.y)
        } else {
            Vec2::ZERO
        }
    }

    fn viewport(&self) -> Vec2 {
        if self.canvas_rect.is_positive() {
            return Vec2::new(self.canvas_rect.width(), self.canvas_rect.height());
        }
        match &self.gfx {
            Some(g) => Vec2::new(g.config.width as f32, g.config.height as f32),
            None => Vec2::new(1600.0, 1000.0),
        }
    }

    fn cursor_ray(&self) -> (Vec3, Vec3) {
        let vp = self.viewport();
        let cursor = self.cursor - self.canvas_origin();
        self.camera.ray(cursor.x, cursor.y, vp.x, vp.y)
    }

    fn effective_brush(&self) -> Brush {
        let mut b = self.brush;
        if self.modifiers.shift_key() {
            b.kind = BrushKind::Smooth;
        }
        b
    }

    fn begin_stroke(&mut self) {
        self.stroke_brush = self.effective_brush();
        let gpu = self.gfx.is_some() && self.stroke_brush.kind == BrushKind::Grab;
        // Alt is ZBrush's alternate (ZSub) modifier.
        let invert = self.modifiers.alt_key() ^ self.workspace.subtract;
        let symmetry = self.workspace.symmetry;
        log::trace!("stroke {:?} invert={invert} symmetry={symmetry}", self.stroke_brush.kind);
        self.stroke = Some(Stroke::begin(invert, gpu, symmetry));
        self.apply_stroke();
    }

    fn apply_stroke(&mut self) {
        let t = Instant::now();
        let ray = self.cursor_ray();
        let fwd = self.camera.forward();
        let Some(stroke) = self.stroke.as_mut() else {
            return;
        };
        if stroke.grabbing() {
            stroke.dab(
                &mut self.mesh,
                &self.stroke_brush,
                Vec3::ZERO,
                Vec3::Y,
                fwd,
                ray,
            );
        } else if let Some(hit) = self.mesh.raycast(ray.0, ray.1) {
            self.hover = Some(hit.point);
            stroke.dab(
                &mut self.mesh,
                &self.stroke_brush,
                hit.point,
                hit.normal,
                fwd,
                ray,
            );
        }
        // Hand a GPU grab to the renderer: buffers once, then one delta per frame.
        if let (Some(stroke), Some(gfx)) = (self.stroke.as_mut(), self.gfx.as_mut()) {
            if !gfx.gpu_grab_active() {
                if let Some(setup) = stroke.gpu_grab_setup() {
                    let flags = self.mesh.normal_flags(setup.chunks);
                    gfx.begin_gpu_grab(setup, flags);
                }
            }
            if let Some(delta) = stroke.take_gpu_delta() {
                gfx.gpu_grab_delta(delta);
            }
        }
        self.stroke_ms = t.elapsed().as_secs_f32() * 1000.0;
        if let Some(s) = &self.stroke {
            self.last_dab_count = s.last_count;
        }
    }

    fn end_stroke(&mut self) {
        if self.stroke_pending {
            self.stroke_pending = false;
            self.apply_stroke();
        }
        if let Some(stroke) = self.stroke.take() {
            if let Some(step) = stroke.end(&mut self.mesh) {
                self.history.push(step);
            }
        }
        if let Some(gfx) = self.gfx.as_mut() {
            gfx.end_gpu_grab();
        }
    }

    fn update_hover(&mut self) {
        let (o, d) = self.cursor_ray();
        self.hover = self.mesh.raycast(o, d).map(|h| h.point);
        log::trace!(
            "hover {:?} cursor={:?} canvas={:?} yaw={} pitch={} roll={} o={o:?} d={d:?}",
            self.hover, self.cursor, self.canvas_rect, self.camera.yaw, self.camera.pitch, self.camera.roll
        );
    }

    fn frame_camera(&mut self) {
        let b = self.mesh.bounds();
        self.camera.target = (b.min + b.max) * 0.5;
        self.camera.distance = (b.max - b.min).length() * 1.1;
    }

    /// Default view angle plus a frame: `Home`.
    fn home_camera(&mut self) {
        let fresh = Camera::new();
        self.camera.yaw = fresh.yaw;
        self.camera.pitch = fresh.pitch;
        self.camera.roll = fresh.roll;
        self.frame_camera();
    }

    fn begin_nav(&mut self, button: MouseButton) {
        self.nav = Some(NavDrag {
            button,
            alt_used: self.modifiers.alt_key(),
            shift_at_press: self.modifiers.shift_key(),
        });
        // No brush ring while the camera moves.
        self.hover = None;
    }

    fn end_nav(&mut self, button: MouseButton) {
        if self.nav.map(|n| n.button) == Some(button) {
            self.nav = None;
            self.update_hover();
        }
    }

    fn nav_move(&mut self, drag: NavDrag, delta: Vec2) {
        let mode = drag.mode(self.modifiers);
        log::trace!("nav {mode:?} {:?} dx={} dy={}", drag.button, delta.x, delta.y);
        match mode {
            NavMode::Orbit => {
                self.camera.orbit(delta.x, delta.y);
                if self.modifiers.shift_key() {
                    self.camera.snap_to_axis();
                }
            }
            NavMode::Pan => self.camera.pan(delta.x, delta.y, self.viewport().y),
            NavMode::Zoom => self.camera.dolly(delta.x, delta.y),
            NavMode::Roll => {
                // Follow the cursor's angle about the canvas centre.
                let c = self.canvas_origin() + self.viewport() * 0.5;
                let (a, b) = (self.last_cursor - c, self.cursor - c);
                if a.length() > 8.0 && b.length() > 8.0 {
                    let d = b.y.atan2(b.x) - a.y.atan2(a.x);
                    let d = (d + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
                        - std::f32::consts::PI;
                    // Screen y points down: the scene follows the cursor around the centre.
                    self.camera.roll_by(d);
                }
            }
        }
    }

    /// Logical (egui) position of the cursor.
    fn cursor_pos(&self) -> egui::Pos2 {
        egui::pos2(self.cursor.x / self.scale, self.cursor.y / self.scale)
    }

    fn rebuild(&mut self) {
        let t = Instant::now();
        let (p, i) = cube_sphere(self.segments, 1.0);
        self.mesh = SculptMesh::from_raw(p, i);
        self.build_ms = t.elapsed().as_secs_f32() * 1000.0;
        self.history.clear();
        self.stroke = None;
        if let Some(gfx) = self.gfx.as_mut() {
            gfx.upload_mesh(&mut self.mesh);
        }
        self.status = format!(
            "Built {} vertices in {:.0} ms",
            fmt_count(self.mesh.vertex_count()),
            self.build_ms
        );
    }

    fn on_key(&mut self, key: KeyCode) {
        let ctrl = self.modifiers.control_key();
        let shift = self.modifiers.shift_key();
        match key {
            KeyCode::Escape if self.workspace.popup.is_some() => self.workspace.popup = None,
            KeyCode::Escape if self.workspace.focus => self.workspace.focus = false,
            KeyCode::KeyZ if ctrl && shift => self.pending_redo = true,
            KeyCode::KeyZ if ctrl => self.pending_undo = true,
            KeyCode::KeyY if ctrl => self.pending_redo = true,
            KeyCode::BracketLeft => self.brush.radius = (self.brush.radius * 0.8).max(0.01),
            KeyCode::BracketRight => self.brush.radius = (self.brush.radius * 1.25).min(2.0),
            KeyCode::KeyF => self.frame_camera(),
            KeyCode::Home => self.home_camera(),
            KeyCode::KeyP if shift => self.workspace.grid = !self.workspace.grid,
            KeyCode::KeyP => self.workspace.perspective = !self.workspace.perspective,
            KeyCode::Digit1 => self.brush.kind = BrushKind::Draw,
            KeyCode::Digit2 => self.brush.kind = BrushKind::Smooth,
            KeyCode::Digit3 => self.brush.kind = BrushKind::Grab,
            KeyCode::Digit4 => self.brush.kind = BrushKind::Flatten,
            KeyCode::Digit5 => self.brush.kind = BrushKind::Inflate,
            KeyCode::KeyQ => self.workspace.mode = Mode::Draw,
            KeyCode::KeyW => self.workspace.mode = Mode::Move,
            KeyCode::KeyE => self.workspace.mode = Mode::Scale,
            KeyCode::KeyR => self.workspace.mode = Mode::Rotate,
            KeyCode::KeyX => self.workspace.symmetry = !self.workspace.symmetry,
            KeyCode::KeyS => self.workspace.toggle_popup(QuickPopup::Size, self.cursor_pos()),
            KeyCode::KeyU => self.workspace.toggle_popup(QuickPopup::Intensity, self.cursor_pos()),
            KeyCode::KeyI => self.workspace.toggle_popup(QuickPopup::Rgb, self.cursor_pos()),
            KeyCode::KeyO => self.workspace.toggle_popup(QuickPopup::Focal, self.cursor_pos()),
            _ => {}
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        let actions = self.workspace.show(
            ui,
            &mut workspace::Model {
                brush: &mut self.brush,
                segments: &mut self.segments,
                export_path: &mut self.export_path,
                status: &self.status,
                can_undo: self.history.can_undo(),
                can_redo: self.history.can_redo(),
                history_bytes: self.history.bytes(),
                vertices: self.mesh.vertex_count(),
                triangles: self.mesh.triangle_count(),
                chunks: self.mesh.chunks.len(),
                fps: self.fps,
                frame_ms: self.frame_ms,
                stroke_ms: self.stroke_ms,
                upload_ms: self.upload_ms,
                render_ms: self.render_ms,
                adapter: self
                    .gfx
                    .as_ref()
                    .map(|g| g.adapter_name.as_str())
                    .unwrap_or(""),
                axes: {
                    let (r, u, f) = (self.camera.right(), self.camera.up(), self.camera.forward());
                    [Vec3::X, Vec3::Y, Vec3::Z].map(|v| [v.dot(r), v.dot(u), -v.dot(f)])
                },
            },
        );
        self.pending_rebuild |= actions.rebuild;
        self.pending_export |= actions.export;
        self.pending_undo |= actions.undo;
        self.pending_redo |= actions.redo;
        if actions.frame {
            self.frame_camera();
        }
        if actions.zoom != 0.0 {
            self.camera.zoom(actions.zoom);
        }
        if let Some((yaw, pitch)) = actions.view {
            self.camera.yaw = yaw;
            self.camera.pitch = pitch;
        }
    }

    fn redraw(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f32();
        self.last_frame = now;
        self.frame_ms = self.frame_ms * 0.9 + dt * 1000.0 * 0.1;
        self.fps = if self.frame_ms > 0.0 {
            1000.0 / self.frame_ms
        } else {
            0.0
        };

        let (Some(window), Some(state)) = (self.window.clone(), self.egui_state.as_mut()) else {
            return;
        };
        let raw = state.take_egui_input(&window);
        let ctx = self.egui_ctx.clone();
        let full = ctx.run_ui(raw, |ui| self.ui(ui));
        if let Some(state) = self.egui_state.as_mut() {
            state.handle_platform_output(&window, full.platform_output);
        }
        let scale = full.pixels_per_point;
        self.scale = scale;
        let logical_canvas = self.workspace.canvas;
        self.canvas_rect =
            egui::Rect::from_min_max(logical_canvas.min * scale, logical_canvas.max * scale);
        let jobs = ctx.tessellate(full.shapes, full.pixels_per_point);

        // Deferred UI actions.
        if self.pending_rebuild {
            self.pending_rebuild = false;
            self.rebuild();
        }
        if self.pending_undo {
            self.pending_undo = false;
            self.history.undo(&mut self.mesh);
        }
        if self.pending_redo {
            self.pending_redo = false;
            self.history.redo(&mut self.mesh);
        }
        if self.pending_export {
            self.pending_export = false;
            let t = Instant::now();
            self.status = match self.mesh.export_obj(&self.export_path) {
                Ok(()) => format!(
                    "Exported {} in {:.1} s",
                    self.export_path,
                    t.elapsed().as_secs_f32()
                ),
                Err(e) => format!("Export failed: {e}"),
            };
        }

        if let Some((left, frame_acc, dab_acc)) = self.stress {
            const TOTAL: u32 = 240;
            let vp = self.viewport();
            let c = self.canvas_origin() + vp * 0.5;
            if left == TOTAL {
                self.brush = Brush {
                    kind: BrushKind::Grab,
                    radius: 1.2,
                    strength: 0.5,
                };
                self.cursor = c + Vec2::new(0.3, -0.3) * vp.y * 0.33;
                self.begin_stroke();
            } else if left == 0 {
                self.end_stroke();
                log::info!(
                    "stress: {} frames of a {} vertex grab, avg frame {:.2} ms, avg dab (CPU) {:.2} ms, upload {:.2} ms, render submit {:.2} ms",
                    TOTAL,
                    fmt_count(self.last_dab_count),
                    frame_acc / TOTAL as f32,
                    dab_acc / TOTAL as f32,
                    self.stress_upload / TOTAL as f32,
                    self.stress_render / TOTAL as f32
                );
            } else {
                let a = (TOTAL - left) as f32 * 0.1;
                self.cursor =
                    c + Vec2::new(0.3 + 0.15 * a.cos(), -0.3 + 0.15 * a.sin()) * vp.y * 0.33;
                self.stroke_pending = true;
            }
            self.stress = if left == 0 {
                None
            } else {
                Some((left - 1, frame_acc + dt * 1000.0, dab_acc + self.stroke_ms))
            };
        }
        if self.stroke_pending {
            self.stroke_pending = false;
            self.apply_stroke();
        }

        let vp = self.viewport();
        self.camera.aspect = vp.x / vp.y;
        self.camera.ortho = !self.workspace.perspective;
        let brush_pos = self.hover.unwrap_or(Vec3::ZERO);
        let show_brush = self.hover.is_some() && !ctx.is_pointer_over_egui();
        let radius = if self.stroke.is_some() {
            self.stroke_brush.radius
        } else {
            self.effective_brush().radius
        };
        let px = self.camera.pixel_size_at(brush_pos, vp.y);
        let eye = self.camera.eye();
        let light = (self.camera.right() * 0.6 + self.camera.up() * 0.8
            - self.camera.forward() * 0.9)
            .normalize();
        let uniforms = Uniforms {
            view_proj: self.camera.view_proj().to_cols_array_2d(),
            inv_view_proj: self.camera.view_proj().inverse().to_cols_array_2d(),
            cam_pos: [eye.x, eye.y, eye.z, 1.0],
            light_dir: [light.x, light.y, light.z, 0.0],
            brush: [brush_pos.x, brush_pos.y, brush_pos.z, radius],
            params: [
                if show_brush { 1.0 } else { 0.0 },
                if self.workspace.wireframe { 1.0 } else { 0.0 },
                px,
                self.workspace.material as f32,
            ],
            viewport: [
                self.canvas_rect.min.x,
                self.canvas_rect.min.y,
                self.canvas_rect.width(),
                self.canvas_rect.height(),
            ],
            display: [
                if self.workspace.grid { 1.0 } else { 0.0 },
                if self.workspace.symmetry { 1.0 } else { 0.0 },
                0.0,
                0.0,
            ],
        };
        let Some(gfx) = self.gfx.as_mut() else { return };
        let t = Instant::now();
        gfx.upload_dirty(&mut self.mesh);
        self.upload_ms = t.elapsed().as_secs_f32() * 1000.0;
        let t = Instant::now();
        gfx.render(&uniforms, &jobs, full.textures_delta, full.pixels_per_point);
        self.render_ms = t.elapsed().as_secs_f32() * 1000.0;
        if self.stress.is_some() {
            self.stress_upload += self.upload_ms;
            self.stress_render += self.render_ms;
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("Klay")
                        .with_min_inner_size(LogicalSize::new(960.0, 640.0))
                        .with_inner_size(PhysicalSize::new(1600, 1000)),
                )
                .expect("create window"),
        );
        workspace::theme(&self.egui_ctx);
        let mut gfx = Gfx::new(window.clone(), self.vsync);
        gfx.upload_mesh(&mut self.mesh);
        let max_tex = gfx.device.limits().max_texture_dimension_2d as usize;
        let state = egui_winit::State::new(
            self.egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &*window,
            Some(window.scale_factor() as f32),
            window.theme(),
            Some(max_tex),
        );
        self.egui_state = Some(state);
        self.gfx = Some(gfx);
        self.window = Some(window);
        self.status = format!(
            "Built {} vertices in {:.0} ms",
            fmt_count(self.mesh.vertex_count()),
            self.build_ms
        );
        if self.demo {
            for (name, ms, dabs, count) in self.scripted_strokes() {
                log::info!("demo stroke {name}: {dabs} dabs over {count} verts in {ms:.1} ms");
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(window) = self.window.clone() else {
            return;
        };
        let consumed = match self.egui_state.as_mut() {
            Some(state) => state.on_window_event(&window, &event).consumed,
            None => false,
        };
        let over_ui = self.egui_ctx.is_pointer_over_egui()
            || (self.canvas_rect.is_positive()
                && !self
                    .canvas_rect
                    .contains(egui::pos2(self.cursor.x, self.cursor.y)))
            || egui::DragAndDrop::has_any_payload(&self.egui_ctx);

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(gfx) = self.gfx.as_mut() {
                    gfx.resize(size.width, size.height);
                }
            }
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::ModifiersChanged(m) => {
                self.modifiers = m.state();
                // Pressing Alt during a left drag arms the pan/zoom pair for the rest of it.
                if let Some(drag) = self.nav.as_mut() {
                    if drag.button == MouseButton::Left && self.modifiers.alt_key() {
                        drag.alt_used = true;
                    }
                    let drag = *drag;
                    if drag.mode(self.modifiers) == NavMode::Orbit && self.modifiers.shift_key() {
                        self.camera.snap_to_axis();
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = self.cursor;
                self.cursor = Vec2::new(position.x as f32, position.y as f32);
                let delta = self.cursor - self.last_cursor;
                if let Some(drag) = self.nav {
                    self.nav_move(drag, delta);
                } else if self.stroke.is_some() {
                    if self.stroke_brush.kind == BrushKind::Grab {
                        self.stroke_pending = true;
                    } else {
                        self.apply_stroke();
                    }
                } else if !over_ui {
                    self.update_hover();
                } else {
                    self.hover = None;
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let pressed = state == ElementState::Pressed;
                match button {
                    MouseButton::Left => {
                        if pressed {
                            if self.workspace.popup.is_some() {
                                // The press that closes a hotkey popup does nothing else.
                            } else if !consumed
                                && !over_ui
                                && self.nav.is_none()
                                && self.stroke.is_none()
                            {
                                // ZBrush: a left drag on the model sculpts (Alt = alternate
                                // brush, Ctrl = masking); on empty canvas it rotates, with
                                // Alt it moves, and releasing Alt turns it into a zoom.
                                let (o, d) = self.cursor_ray();
                                let hit = self.mesh.raycast(o, d);
                                log::trace!("press cursor={:?} hit={:?}", self.cursor, hit.as_ref().map(|h| h.point));
                                if hit.is_none() {
                                    self.begin_nav(MouseButton::Left);
                                } else if self.modifiers.control_key() {
                                    self.workspace.notify("Masking is not connected yet");
                                } else if self.workspace.mode != Mode::Draw {
                                    self.workspace.notify(format!(
                                        "{} mode is not connected yet · Q returns to Draw",
                                        self.workspace.mode.name()
                                    ));
                                } else {
                                    self.begin_stroke();
                                }
                            }
                        } else {
                            self.end_stroke();
                            self.end_nav(MouseButton::Left);
                        }
                    }
                    MouseButton::Right | MouseButton::Middle => {
                        if pressed {
                            if !over_ui && self.stroke.is_none() && self.nav.is_none() {
                                self.begin_nav(button);
                            }
                        } else {
                            self.end_nav(button);
                        }
                    }
                    _ => {}
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if !consumed && !over_ui {
                    let y = match delta {
                        MouseScrollDelta::LineDelta(_, y) => y,
                        MouseScrollDelta::PixelDelta(p) => p.y as f32 / 40.0,
                    };
                    if self.modifiers.control_key() {
                        self.brush.radius = (self.brush.radius * (1.0 + y * 0.1)).clamp(0.01, 2.0);
                    } else {
                        // Zoom towards the point under the cursor so it stays put.
                        let ray = self.cursor_ray();
                        self.camera.zoom_towards(y, ray);
                        if self.nav.is_none() {
                            self.update_hover();
                        }
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed
                    && !self.egui_ctx.egui_wants_keyboard_input()
                {
                    if let PhysicalKey::Code(code) = event.physical_key {
                        self.on_key(code);
                    }
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
}

fn fmt_count(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

fn arc(a0: f32, a1: f32, radius: f32, n: usize) -> Vec<Vec2> {
    (0..n)
        .map(|i| {
            let a = a0 + (a1 - a0) * i as f32 / (n - 1) as f32;
            Vec2::new(a.cos(), -a.sin()) * radius
        })
        .collect()
}

fn line(a: Vec2, b: Vec2, n: usize) -> Vec<Vec2> {
    (0..n)
        .map(|i| a.lerp(b, i as f32 / (n - 1) as f32))
        .collect()
}

/// Headless benchmark: build, scripted strokes, undo/redo, export.
fn bench(segments: u32) {
    let mut app = App::new(segments, false);
    println!(
        "mesh: {} vertices, {} triangles, {} chunks, built in {:.0} ms",
        fmt_count(app.mesh.vertex_count()),
        fmt_count(app.mesh.triangle_count()),
        app.mesh.chunks.len(),
        app.build_ms
    );
    let adj_mb = (app.mesh.adj_verts.len() + app.mesh.vt_tris.len()) as f32 * 4.0 / 1e6;
    let geo_mb =
        app.mesh.vertex_count() as f32 * 16.0 / 1e6 + app.mesh.indices.len() as f32 * 4.0 / 1e6;
    println!("memory: geometry {geo_mb:.0} MB, adjacency {adj_mb:.0} MB");
    let t = Instant::now();
    let hit = app.mesh.raycast(app.camera.eye(), app.camera.forward());
    println!(
        "raycast: {:.3} ms, hit = {}",
        t.elapsed().as_secs_f32() * 1000.0,
        hit.is_some()
    );
    for (name, ms, dabs, count) in app.scripted_strokes() {
        println!(
            "stroke {name:8} {count:>9} verts: {dabs:3} dabs, {ms:7.1} ms total, {:6.2} ms/dab",
            ms / dabs.max(1) as f32
        );
    }
    // Micro-profile of the giant grab's building blocks.
    if let Some(hit) = app.mesh.raycast(app.camera.eye(), app.camera.forward()) {
        let t = Instant::now();
        let sel = app.mesh.select_sphere(hit.point, 1.2);
        println!(
            "select_sphere r=1.2: {} verts in {:.2} ms",
            fmt_count(sel.len()),
            t.elapsed().as_secs_f32() * 1000.0
        );
        let chunks = sel.chunks();
        let orig = sel.par_map(|_, v| app.mesh.positions[v as usize]);
        let w: Vec<f32> = sel.dist.iter().map(|&d| sculpt::falloff(d, 1.2)).collect();
        let n = 20;
        let t = Instant::now();
        for k in 0..n {
            let delta = Vec3::new(0.001 * k as f32, 0.0, 0.0);
            app.mesh
                .scatter_positions_with(&sel, |i, _| orig[i] + delta * w[i]);
        }
        println!(
            "scatter_positions_with: {:.2} ms",
            t.elapsed().as_secs_f32() * 1000.0 / n as f32
        );
        let t = Instant::now();
        for _ in 0..n {
            app.mesh.finish_edit(&chunks);
        }
        println!(
            "finish_edit ({} chunks): {:.2} ms",
            chunks.len(),
            t.elapsed().as_secs_f32() * 1000.0 / n as f32
        );
    }
    let t = Instant::now();
    let normals = app.mesh.compute_all_normals();
    println!(
        "CPU normals for all {} vertices (export path only): {:.0} ms",
        fmt_count(normals.len()),
        t.elapsed().as_secs_f32() * 1000.0
    );
    let t = Instant::now();
    for _ in 0..3 {
        app.history.undo(&mut app.mesh);
    }
    for _ in 0..3 {
        app.history.redo(&mut app.mesh);
    }
    println!(
        "3 undo + 3 redo: {:.1} ms, history {:.1} MB",
        t.elapsed().as_secs_f32() * 1000.0,
        app.history.bytes() as f32 / 1e6
    );
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--bench") {
        let segments = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(400);
        bench(segments);
        return;
    }
    let demo = args.iter().any(|a| a == "--demo");
    let stress = args.iter().any(|a| a == "--stress");
    let vsync = !args.iter().any(|a| a == "--novsync");
    let segments = args
        .iter()
        .position(|a| a == "--segments")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(400);
    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::new(segments, demo);
    app.vsync = vsync;
    if stress {
        app.stress = Some((240, 0.0, 0.0));
    }
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("event loop error: {e}");
    }
}

#[cfg(test)]
mod nav_tests {
    use super::*;

    #[test]
    fn alt_click_release_alt_zooms() {
        // ZBrush: Alt + click starts a pan, releasing Alt turns the same drag into a zoom,
        // pressing Alt again goes back to panning.
        let drag = NavDrag {
            button: MouseButton::Left,
            alt_used: true,
            shift_at_press: false,
        };
        assert_eq!(drag.mode(ModifiersState::ALT), NavMode::Pan);
        assert_eq!(drag.mode(ModifiersState::empty()), NavMode::Zoom);
        assert_eq!(drag.mode(ModifiersState::ALT), NavMode::Pan);
    }

    #[test]
    fn plain_canvas_drag_orbits_until_alt_is_used() {
        let mut drag = NavDrag {
            button: MouseButton::Left,
            alt_used: false,
            shift_at_press: false,
        };
        assert_eq!(drag.mode(ModifiersState::empty()), NavMode::Orbit);
        drag.alt_used = true; // Alt pressed mid-drag
        assert_eq!(drag.mode(ModifiersState::ALT), NavMode::Pan);
        assert_eq!(drag.mode(ModifiersState::empty()), NavMode::Zoom);
    }

    #[test]
    fn shift_click_release_shift_rolls() {
        // ZBrush: Shift held at the click snaps while held; releasing it turns the drag
        // into a roll. Alt still wins for the pan/zoom pair.
        let drag = NavDrag {
            button: MouseButton::Left,
            alt_used: false,
            shift_at_press: true,
        };
        assert_eq!(drag.mode(ModifiersState::SHIFT), NavMode::Orbit);
        assert_eq!(drag.mode(ModifiersState::empty()), NavMode::Roll);
        assert_eq!(drag.mode(ModifiersState::ALT), NavMode::Pan);
        // Shift pressed only mid-drag never rolls.
        let plain = NavDrag {
            button: MouseButton::Left,
            alt_used: false,
            shift_at_press: false,
        };
        assert_eq!(plain.mode(ModifiersState::empty()), NavMode::Orbit);
    }

    #[test]
    fn right_button_modifiers() {
        let drag = NavDrag {
            button: MouseButton::Right,
            alt_used: false,
            shift_at_press: false,
        };
        assert_eq!(drag.mode(ModifiersState::empty()), NavMode::Orbit);
        assert_eq!(drag.mode(ModifiersState::ALT), NavMode::Pan);
        // Shift is the rotation snap, not a pan.
        assert_eq!(drag.mode(ModifiersState::SHIFT), NavMode::Orbit);
        assert_eq!(drag.mode(ModifiersState::CONTROL), NavMode::Zoom);
    }
}
