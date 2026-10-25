//! The actions shelf buttons run (ZBrush's custom shelf buttons): every brush, view toggle
//! and palette command that can be put on a shelf, and the drag source that palette
//! buttons and brush tiles become in Customize mode (see `shelves.rs`).
use super::*;

/// Actions other than brushes: id, label, hover text.
const ACTIONS: &[(&str, &str, &str)] = &[
    ("toggle:persp", "Persp", "Perspective · P"),
    ("toggle:floor", "Floor", "Floor grid · Shift + P"),
    ("toggle:sym", "Sym X", "Symmetry across X · X"),
    ("toggle:polyf", "PolyF", "Polygroup colours · Shift + F"),
    ("toggle:solo", "Solo", "Show only the active subtool"),
    ("toggle:detail", "Detail", "Surface detail shading"),
    ("toggle:sculptris", "Sculptris Pro", "Strokes add and remove triangles under the brush"),
    ("toggle:live-boolean", "Live Boolean", "Show the subtools' boolean result"),
    ("toggle:dyn", "Dyn", "Radius follows zoom"),
    ("toggle:zsub", "Zsub", "Carve instead of build (Zadd / Zsub)"),
    ("cmd:frame", "Frame", "Frame mesh · F"),
    ("cmd:hide-ui", "Hide UI", "Hide the interface · Tab"),
    ("cmd:undo", "Undo", "Ctrl + Z"),
    ("cmd:redo", "Redo", "Ctrl + Shift + Z"),
    ("cmd:divide", "Divide", "Add a subdivision level · Ctrl + D"),
    ("cmd:del-lower", "Del Lower", "Delete the levels below this one"),
    ("cmd:del-higher", "Del Higher", "Delete the levels above this one"),
    ("cmd:remesh", "Remesh", "Remesh again now"),
    ("cmd:zremesh", "ZRemesher", "Rebuild the subtool as quads"),
    ("cmd:del-hidden", "Del Hidden", "Delete the hidden polygons"),
    ("cmd:close-holes", "Close Holes", "Cap every open border"),
    ("cmd:mirror-weld", "Mirror And Weld", "Make the subtool symmetric across X"),
    ("cmd:append", "Append", "Add a new subtool"),
    ("cmd:duplicate", "Duplicate", "Copy the active subtool"),
    ("cmd:delete-subtool", "Delete", "Delete the active subtool"),
    ("cmd:subtool-up", "Up", "Move the active subtool up"),
    ("cmd:subtool-down", "Down", "Move the active subtool down"),
    ("cmd:merge-down", "Merge Down", "Join the subtool below into this one"),
    ("cmd:split-hidden", "Split Hidden", "The hidden polygons become their own subtool"),
    ("cmd:groups-split", "Groups Split", "Each polygroup becomes its own subtool"),
    ("cmd:mask-invert", "Invert mask", "Ctrl + I"),
    ("cmd:mask-clear", "Clear mask", "Ctrl + Shift + A"),
    ("cmd:mask-all", "Mask all", "Ctrl + A"),
    ("cmd:mask-blur", "Blur mask", "Soften the mask's edge"),
    ("cmd:mask-sharpen", "Sharpen mask", "Harden the mask's edge"),
    ("cmd:mask-grow", "Grow mask", "Spread the mask one ring"),
    ("cmd:mask-shrink", "Shrink mask", "Pull the mask back one ring"),
    ("cmd:group-visible", "Group Visible", "One polygroup of the visible polygons · Ctrl + W"),
    ("cmd:show-all", "Show all", "Show every polygon"),
    ("cmd:invert-visibility", "Invert", "Swap hidden and visible"),
    ("cmd:fullscreen", "Fullscreen", "F11"),
    ("cmd:export-stl", "Export STL", "The visible subtools as one STL, sized by 3D Print"),
    ("cmd:send-slicer", "Send to Slicer", "Open the visible subtools in the 3D Print slicer"),
    ("cmd:print-check", "Check Mesh", "Look for holes and non-manifold edges before printing"),
];

fn brush_of(id: &str) -> Option<BrushKind> {
    let name = id.strip_prefix("brush:")?;
    BrushKind::ALL.into_iter().find(|k| k.name() == name)
}

/// Whether `id` is an action My shelf knows (others in a saved workspace are dropped).
pub fn known(id: &str) -> bool {
    brush_of(id).is_some() || ACTIONS.iter().any(|(a, _, _)| *a == id)
}

/// The button's label and hover text.
pub fn label(id: &str) -> (String, String) {
    if let Some(k) = brush_of(id) {
        return (k.name().to_string(), format!("{} · B {}", k.description(), k.code().chars().map(String::from).collect::<Vec<_>>().join(" ")));
    }
    ACTIONS
        .iter()
        .find(|(a, _, _)| *a == id)
        .map(|(_, l, h)| (l.to_string(), h.to_string()))
        .unwrap_or_else(|| (id.to_string(), String::new()))
}

/// The id of a brush's action.
pub fn brush_id(kind: BrushKind) -> String {
    format!("brush:{}", kind.name())
}

/// Customize mode, kept with the egui context (each window, and each test, its own).
fn customizing_id() -> Id {
    Id::unique("klay-customizing")
}

pub fn set_customizing(ctx: &Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(customizing_id(), on));
}

fn customizing(ctx: &Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(customizing_id())).unwrap_or(false)
}

/// A palette button or brush being dragged onto a shelf.
#[derive(Clone, Debug)]
pub struct ActionDrag(pub String);

/// Makes a button a drag source for the shelves while customizing (outlined in the accent);
/// otherwise does nothing. Its clicks still go to the button.
pub trait ShelfSource {
    fn shelf(self, ui: &Ui, id: &str) -> Self;
}

impl ShelfSource for egui::Response {
    fn shelf(self, ui: &Ui, id: &str) -> Self {
        if !customizing(ui.ctx()) {
            return self;
        }
        let drag = ui.interact(self.rect, self.id.with("shelf-source"), Sense::drag());
        drag.dnd_set_drag_payload(ActionDrag(id.to_string()));
        let strong = drag.hovered() || drag.dragged();
        ui.painter().rect_stroke(
            self.rect.expand(1.0),
            4,
            Stroke::new(1.0, ACC().gamma_multiply(if strong { 1.0 } else { 0.45 })),
            StrokeKind::Outside,
        );
        drag.on_hover_text("Drag onto a shelf");
        self
    }
}

/// Whether a toggle (or brush) action is on.
pub fn action_on(ws: &Workspace, id: &str, m: &Model<'_>) -> bool {
    match id {
        "toggle:persp" => ws.perspective,
        "toggle:floor" => ws.grid,
        "toggle:sym" => ws.symmetry,
        "toggle:polyf" => ws.polyframe,
        "toggle:solo" => ws.solo,
        "toggle:detail" => ws.wireframe,
        "toggle:sculptris" => ws.sculptris,
        "toggle:live-boolean" => ws.live_boolean,
        "toggle:dyn" => ws.dynamic,
        "toggle:zsub" => ws.subtract,
        _ => brush_of(id).is_some_and(|k| m.brush.kind == k),
    }
}

impl Workspace {
    /// Run a shelf button's action.
    pub(super) fn run_action(&mut self, id: &str, m: &mut Model<'_>, a: &mut Actions) {
        use crate::mask::MaskOp;
        if let Some(k) = brush_of(id) {
            self.choose_brush(m, k);
            return;
        }
        match id {
            "toggle:persp" => self.perspective = !self.perspective,
            "toggle:floor" => self.grid = !self.grid,
            "toggle:sym" => self.symmetry = !self.symmetry,
            "toggle:polyf" => self.polyframe = !self.polyframe,
            "toggle:solo" => self.solo = !self.solo,
            "toggle:detail" => self.wireframe = !self.wireframe,
            "toggle:sculptris" => self.sculptris = !self.sculptris,
            "toggle:live-boolean" => self.live_boolean = !self.live_boolean,
            "toggle:dyn" => self.dynamic = !self.dynamic,
            "toggle:zsub" => self.subtract = !self.subtract,
            "cmd:frame" => a.frame = true,
            "cmd:hide-ui" => self.set_focus(true),
            "cmd:undo" => a.undo = true,
            "cmd:redo" => a.redo = true,
            "cmd:divide" => a.divide = true,
            "cmd:del-lower" => a.del_lower = true,
            "cmd:del-higher" => a.del_higher = true,
            "cmd:remesh" => a.wedge_now = true,
            "cmd:zremesh" => a.zremesh = true,
            "cmd:del-hidden" => a.del_hidden = true,
            "cmd:close-holes" => a.close_holes = true,
            "cmd:mirror-weld" => a.mirror_weld = true,
            "cmd:append" => a.append = true,
            "cmd:duplicate" => a.duplicate = true,
            "cmd:delete-subtool" => a.delete = true,
            "cmd:subtool-up" => a.move_subtool = -1,
            "cmd:subtool-down" => a.move_subtool = 1,
            "cmd:merge-down" => a.merge_down = true,
            "cmd:split-hidden" => a.split_hidden = true,
            "cmd:groups-split" => a.groups_split = true,
            "cmd:mask-invert" => a.mask = Some(MaskOp::Invert),
            "cmd:mask-clear" => a.mask = Some(MaskOp::Clear),
            "cmd:mask-all" => a.mask = Some(MaskOp::All),
            "cmd:mask-blur" => a.mask = Some(MaskOp::Blur),
            "cmd:mask-sharpen" => a.mask = Some(MaskOp::Sharpen),
            "cmd:mask-grow" => a.mask = Some(MaskOp::Grow),
            "cmd:mask-shrink" => a.mask = Some(MaskOp::Shrink),
            "cmd:group-visible" => a.group_visible = true,
            "cmd:show-all" => a.show_all = true,
            "cmd:invert-visibility" => a.invert_visibility = true,
            "cmd:fullscreen" => a.fullscreen = true,
            "cmd:export-stl" => a.export_stl = true,
            "cmd:send-slicer" => a.send_slicer = true,
            "cmd:print-check" => a.print_check = true,
            _ => {}
        }
    }

}
