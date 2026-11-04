//! Manual retopology against the visible triangles of one reference SubTool.
use super::*;
use zmodeler::{EdgeAction, Hover, PointAction, PolyAction, PolyMesh};

#[derive(Clone, Copy)]
pub enum Action {
    Reference(u64),
    Clear,
    Place,
    Snap,
}

pub(super) struct Reference {
    pub id: u64,
    level: usize,
    edits: u64,
    /// Available when the reference was indexed while active.
    generation: Option<u64>,
    size: f32,
    surface: project_all::Sources,
}

impl Reference {
    fn under_pixel(&self, view: &View<'_>, pixel: Vec2) -> Option<(Vec3, Vec3)> {
        if !pixel.is_finite() {
            return None;
        }
        let (origin, dir) = view.ray(pixel);
        self.surface
            .hit(origin, dir, f32::INFINITY)
            .map(|hit| (origin + dir * hit.t, hit.normal))
    }

    fn snap(&self, view: &View<'_>, point: Vec3) -> Option<Vec3> {
        self.under_pixel(view, view.project(point)).map(|(p, _)| p)
    }
}

impl App {
    fn reference_current(&self, reference: &Reference, index: usize) -> bool {
        if index == self.active_tool {
            self.doc.active() == reference.level
                && self.history.edits() == reference.edits
                && reference.generation == Some(self.doc.generation)
        } else {
            self.subtools[index].parked.as_ref().is_some_and(|p| {
                p.packed.active() == reference.level && p.history.edits() == reference.edits
            })
        }
    }

    fn usable_reference(&self) -> Option<(&Reference, usize)> {
        let reference = self.retopo_reference.as_ref()?;
        let index = self
            .subtools
            .iter()
            .position(|tool| tool.id == reference.id)?;
        (self.reference_current(reference, index)
            && (index == self.active_tool
                || (!self.workspace.solo && self.subtools[index].visible)))
            .then_some((reference, index))
    }

    pub(super) fn refresh_retopo_controls(&mut self) {
        if self.retopo_reference.as_ref().is_some_and(|reference| {
            self.subtools
                .iter()
                .position(|tool| tool.id == reference.id)
                .is_none_or(|index| !self.reference_current(reference, index))
        }) {
            self.retopo_reference = None;
            self.retopo_cage = None;
        }
        self.workspace.retopo_attached =
            self.retopo_cage == Some(self.subtools[self.active_tool].id);
        self.workspace.retopo_ready = self.usable_reference().is_some();
        self.retopo_placing &= self.workspace.retopo_ready;
        self.workspace.retopo_target = self.retopo_reference.as_ref().map(|r| r.id);
        self.workspace.retopo_placing = self.retopo_placing;
    }

    pub(super) fn retopo_action(&mut self, action: Action) {
        if self.busy() {
            return;
        }
        match action {
            Action::Reference(id) => self.index_retopo_reference(id),
            Action::Clear => {
                self.retopo_reference = None;
                self.retopo_cage = None;
                self.retopo_placing = false;
            }
            Action::Place => {
                self.retopo_placing = !self.retopo_placing && self.usable_reference().is_some();
                if self.retopo_placing {
                    self.workspace
                        .notify("Click the reference surface to place a quad");
                }
            }
            Action::Snap => self.snap_retopo_mesh(),
        }
    }

    fn index_retopo_reference(&mut self, id: u64) {
        let Some(index) = self.subtools.iter().position(|t| t.id == id && t.visible) else {
            return;
        };
        let (active, parked) = if index == self.active_tool {
            if self.doc.zspheres.is_some() {
                self.workspace
                    .notify("Retopology needs a polygon mesh as the reference");
                return;
            }
            let mesh = self.mesh();
            (
                Some((
                    mesh.positions.clone(),
                    mesh.visible_indices()
                        .unwrap_or_else(|| mesh.indices.clone()),
                )),
                None,
            )
        } else {
            let Some(parked) = self.subtools[index].parked.as_ref() else {
                return;
            };
            if parked.packed.has_zspheres() {
                self.workspace
                    .notify("Retopology needs a polygon mesh as the reference");
                return;
            }
            (None, Some((parked.packed.clone(), parked.history.edits())))
        };
        let (level, edits, generation) = parked.as_ref().map_or_else(
            || {
                (
                    self.doc.active(),
                    self.history.edits(),
                    Some(self.doc.generation),
                )
            },
            |(packed, edits)| (packed.active(), *edits, None),
        );
        self.start_geo("Index Retopology Reference", false, move || {
            let (positions, indices) = if let Some((packed, _)) = parked {
                let geometry = packed.into_geometry_at(level)?;
                (geometry.positions, geometry.indices)
            } else {
                active.expect("an active or parked reference")
            };
            if indices.is_empty() {
                return Err("The reference has no visible faces".into());
            }
            jobs::check()?;
            let mut bounds = mesh::Aabb::empty();
            for &point in &positions {
                bounds.grow(point);
            }
            let size = (bounds.max - bounds.min).length();
            if !size.is_finite() || size <= 0.0 {
                return Err("The reference has no usable surface".into());
            }
            let triangles = indices
                .chunks_exact(3)
                .map(|t| [t[0], t[1], t[2]])
                .collect();
            drop(indices);
            let surface = project_all::Sources::from_triangles(positions, triangles);
            jobs::check()?;
            Ok(GeoDone::RetopoReference(Reference {
                id,
                level,
                edits,
                generation,
                size,
                surface,
            }))
        });
    }

    pub(super) fn place_retopo_quad(&mut self) {
        let Some((reference, _)) = self.usable_reference() else {
            self.retopo_placing = false;
            return;
        };
        let view = self.view();
        let cursor = self.canvas_cursor();
        let Some((centre, normal)) = reference.under_pixel(&view, cursor) else {
            self.workspace
                .notify("Click a visible part of the reference");
            return;
        };
        let (_, dir) = view.ray(cursor);
        let normal = if normal.dot(dir) > 0.0 {
            -normal
        } else {
            normal
        };
        if normal == Vec3::ZERO {
            self.workspace
                .notify("Choose a part of the reference with a usable surface normal");
            return;
        }
        let half = reference.size * self.workspace.retopo_size * 0.005;
        let camera_right = self.camera.right();
        let right = (camera_right - normal * camera_right.dot(normal))
            .normalize_or(normal.any_orthonormal_vector())
            * half;
        let up = normal.cross(right);
        let corners = [
            centre - right - up,
            centre + right - up,
            centre + right + up,
            centre - right + up,
        ];
        let Some(positions) = corners
            .into_iter()
            .map(|p| reference.snap(&view, p))
            .collect::<Option<Vec<_>>>()
        else {
            self.workspace.notify(
                "The quad crosses the reference silhouette · move inward or lower Quad Size",
            );
            return;
        };
        let poly = PolyMesh {
            pos: positions,
            faces: vec![vec![0, 1, 2, 3]],
            groups: vec![0],
            ..Default::default()
        };
        let (mesh, _, _) = poly.to_mesh();
        let tool = self.parked_subtool(
            "Retopo Quad".into(),
            true,
            Multires::new(mesh, self.doc.budget),
        );
        self.subtools.push(tool);
        self.select_subtool(self.subtools.len() - 1);
        self.retopo_cage = Some(self.subtools[self.active_tool].id);
        self.retopo_placing = false;
        self.workspace.retopo_snap = true;
        self.workspace
            .select_brush(&mut self.brush, BrushKind::ZModeler);
        self.workspace.zm_edge = EdgeAction::Extrude;
        self.workspace.mode = Mode::Draw;
        self.update_zmodel_hover();
        self.status = "Retopo Quad added".into();
    }

    fn snap_retopo_mesh(&mut self) {
        let Some((reference, index)) = self.usable_reference() else {
            return;
        };
        if index == self.active_tool || self.doc.count() != 1 {
            return;
        }
        let mesh = self.mesh();
        let reach = reference.size * 0.15;
        let positions: Vec<_> = mesh
            .positions
            .par_iter()
            .enumerate()
            .map(|(i, &p)| {
                if mesh.shown.get(i) == Some(&0) {
                    return p;
                }
                let free = mask::freedom(&mesh.mask, i as u32);
                reference
                    .surface
                    .closest(p, Vec3::ZERO)
                    .filter(|(q, _)| p.distance_squared(*q) <= reach * reach)
                    .map_or(p, |(q, _)| p.lerp(q, free))
            })
            .collect();
        let changed = positions != mesh.positions;
        self.retopo_cage = Some(self.subtools[self.active_tool].id);
        self.workspace.retopo_snap = true;
        if !changed {
            self.workspace
                .notify("No vertices moved · place the cage closer to the reference");
            return;
        }
        self.reshape_shown("Snap Active Mesh", &positions);
    }

    pub(super) fn retopo_live(&self) -> bool {
        self.workspace.retopo_snap
            && self.retopo_cage == Some(self.subtools[self.active_tool].id)
            && self.doc.count() == 1
            && self
                .usable_reference()
                .is_some_and(|(_, index)| index != self.active_tool)
    }

    /// Preserve the original edge and snap just the extruded points. A move whose
    /// projection leaves the reference keeps the previous valid drag preview.
    pub(super) fn retopo_zmodel_result(&self, drag: &ZDrag, result: ZResult) -> Option<ZResult> {
        if !self.retopo_live() || drag.handle.is_some() {
            return Some(result);
        }
        let (reference, _) = self.usable_reference()?;
        let view = self.view();
        match result {
            ZResult::Mesh(mut poly)
                if matches!(drag.hover, Hover::Edge(..))
                    && self.workspace.zm_edge == EdgeAction::Extrude =>
            {
                if poly.pos.len() == drag.base.pos.len() {
                    return None;
                }
                for point in &mut poly.pos[drag.base.pos.len()..] {
                    *point = reference.snap(&view, *point)?;
                }
                Some(ZResult::Mesh(poly))
            }
            ZResult::Points(mut points)
                if match drag.hover {
                    Hover::Poly(_) => self.workspace.zm_poly == PolyAction::Move,
                    Hover::Edge(..) => self.workspace.zm_edge == EdgeAction::Move,
                    Hover::Point(_) => self.workspace.zm_point == PointAction::Move,
                } =>
            {
                for (vertex, point) in &mut points {
                    let old = drag.base.pos[*vertex as usize];
                    let freedom = mask::freedom(&drag.base.mask, *vertex);
                    if freedom == 0.0 {
                        *point = old;
                    } else if *point != old {
                        *point = old.lerp(reference.snap(&view, *point)?, freedom);
                    }
                }
                Some(ZResult::Points(points))
            }
            other => Some(other),
        }
    }

    fn projected_retopo_face(&self, poly: &PolyMesh, face: usize) -> Option<Vec<Vec2>> {
        if poly
            .normal(face as u32)
            .dot(self.camera.eye() - poly.centre(face as u32))
            <= 0.0
        {
            return None;
        }
        poly.faces[face]
            .iter()
            .map(|&v| self.project_front(poly.pos[v as usize]))
            .collect()
    }

    pub(super) fn retopo_feature_at_cursor(&mut self) -> Option<Hover> {
        self.zmodel_view();
        let poly = &self.zmodel.as_ref()?.poly;
        let mut point: Option<(f32, u32)> = None;
        let mut edge: Option<(f32, u32, u32)> = None;
        for (f, face) in poly.faces.iter().enumerate() {
            let Some(projected) = self.projected_retopo_face(poly, f) else {
                continue;
            };
            for (i, &a) in projected.iter().enumerate() {
                let distance = self.cursor.distance(a);
                if distance < 9.0 * self.scale && point.is_none_or(|p| distance < p.0) {
                    point = Some((distance, face[i]));
                }
                let next = (i + 1) % face.len();
                let distance = gizmo::segment_distance(self.cursor, a, projected[next]);
                if distance < 7.0 * self.scale && edge.is_none_or(|e| distance < e.0) {
                    edge = Some((distance, face[i], face[next]));
                }
            }
        }
        point
            .map(|(_, v)| Hover::Point(v))
            .or_else(|| edge.map(|(_, a, b)| Hover::Edge(a, b)))
    }

    pub(super) fn paint_retopo(&mut self, ctx: &egui::Context) {
        if self.workspace.modal_open() {
            return;
        }
        if self.retopo_placing && self.workspace.canvas.contains(self.cursor_pos()) {
            ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        if !self.retopo_live()
            || !self.brush.kind.is_zmodeler()
            || self.workspace.mode != Mode::Draw
        {
            return;
        }
        self.zmodel_view();
        let Some(model) = self.zmodel.as_ref() else {
            return;
        };
        let poly = &model.poly;
        let painter = ctx
            .layer_painter(egui::LayerId::new(
                egui::Order::Middle,
                egui::Id::unique("retopo-cage"),
            ))
            .with_clip_rect(self.workspace.canvas);
        let accent = workspace::palette().acc;
        let stroke = egui::Stroke::new(1.5, accent);
        let mut drawn = crate::fx::FxHashSet::default();
        for (f, face) in poly.faces.iter().enumerate() {
            let Some(projected) = self.projected_retopo_face(poly, f) else {
                continue;
            };
            let pos = |p: Vec2| egui::pos2(p.x / self.scale, p.y / self.scale);
            for (i, &a) in projected.iter().enumerate() {
                let j = (i + 1) % face.len();
                let (v, w) = (face[i], face[j]);
                if drawn.insert((v.min(w), v.max(w))) {
                    painter.line_segment([pos(a), pos(projected[j])], stroke);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quad_and_zmodeler_extrusions_follow_the_reference_shown_level() {
        let mut app = App::new(24, false, usize::MAX);
        app.camera.aspect = 1.6;
        app.camera.yaw = 0.0;
        app.camera.pitch = 0.0;
        app.canvas_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 1000.0));
        app.divide();
        assert_eq!(app.doc.active(), 1);
        let high: Vec<_> = app.mesh().positions.iter().map(|&p| p * 1.2).collect();
        app.reshape_shown("Detail", &high);
        let id = app.subtools[0].id;
        app.retopo_action(Action::Reference(id));
        app.poll_geo(true);
        app.retopo_action(Action::Place);
        let pixel = app.project(Vec3::ZERO) + Vec2::new(34.0, 18.0);
        app.cursor_moved(pixel, false);
        app.left_press(false, false);
        app.left_release();
        assert_eq!(
            (app.subtools.len(), app.active_tool),
            (2, 1),
            "{}",
            app.status
        );
        assert_eq!(app.brush.kind, BrushKind::ZModeler);
        assert_eq!(app.workspace.zm_edge, EdgeAction::Extrude);
        assert!(app.mesh().positions.iter().all(|p| p.z > 1.1));
        assert_eq!(app.zmodel_view().poly.faces.len(), 1);

        // Reindex the parked reference and verify it uses the shown level there too.
        app.retopo_action(Action::Reference(id));
        app.poll_geo(true);
        assert!(app.retopo_live());
        let on_surface = |app: &App| {
            let (reference, _) = app.usable_reference().unwrap();
            app.mesh().positions.iter().all(|&p| {
                reference
                    .surface
                    .closest(p, Vec3::ZERO)
                    .is_some_and(|(q, _)| p.distance(q) < 1e-4)
            })
        };
        assert!(on_surface(&app));
        let seed = app.mesh().positions.clone();
        let right_edge = |app: &mut App| {
            let poly = app.zmodel_view().poly.clone();
            let f = &poly.faces[0];
            (0..4)
                .map(|i| {
                    let a = poly.pos[f[i] as usize];
                    let b = poly.pos[f[(i + 1) % 4] as usize];
                    app.project((a + b) * 0.5)
                })
                .max_by(|a, b| a.x.total_cmp(&b.x))
                .unwrap()
        };
        let start = right_edge(&mut app) + Vec2::X * 2.0;
        app.cursor_moved(start, false);
        assert!(
            matches!(app.zm_hover, Some(Hover::Edge(..))),
            "{:?}",
            app.zm_hover
        );
        app.left_press(false, false);
        app.cursor_moved(start + Vec2::new(95.0, -18.0), false);
        assert_eq!(app.mesh().vertex_count(), 6);
        assert!(on_surface(&app));
        assert!(seed.iter().all(|p| app.mesh().positions.contains(p)));
        let shown = app.mesh().positions.clone();
        app.cursor_moved(Vec2::new(1550.0, 50.0), false);
        assert_eq!(
            app.mesh().positions,
            shown,
            "a missed ray keeps the last valid preview"
        );
        app.left_release();
        assert_eq!(app.zmodel_view().poly.faces.len(), 2);
        app.history_jump(-1);
        assert_eq!(app.mesh().positions, seed);
        app.history_jump(1);
        assert_eq!(app.mesh().vertex_count(), 6);
        assert!(on_surface(&app));

        let poly = app.zmodel_view().poly.clone();
        let corner = poly
            .pos
            .iter()
            .map(|&p| app.project(p))
            .max_by(|a, b| a.x.total_cmp(&b.x))
            .unwrap();
        app.workspace.zm_point = PointAction::Move;
        app.cursor_moved(corner, false);
        assert!(matches!(app.zm_hover, Some(Hover::Point(_))));
        app.left_press(false, false);
        app.cursor_moved(corner + Vec2::new(16.0, -20.0), false);
        app.left_release();
        assert_eq!(app.mesh().vertex_count(), 6);
        assert!(on_surface(&app));

        // Snap Active also works for an existing cage and undoes through regular history.
        app.apply_transform(Affine::translation(Vec3::Z * 0.04));
        let shifted = app.mesh().positions.clone();
        app.retopo_action(Action::Snap);
        assert!(on_surface(&app));
        app.history_jump(-1);
        assert_eq!(app.mesh().positions, shifted);
        // Hiding and re-showing the reference pauses and resumes live snapping.
        app.toggle_subtool(0);
        assert!(!app.retopo_live());
        app.toggle_subtool(0);
        assert!(app.retopo_live());
        app.append_primitive();
        assert!(!app.retopo_live(), "an unrelated SubTool stays independent");
        app.select_subtool(1);
        assert!(app.retopo_live());
        // Editing a previously parked reference cannot leave a stale index attached.
        app.select_subtool(0);
        assert!(app.retopo_reference.is_none());
    }
}
