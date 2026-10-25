//! The four shelves (ZBrush): top, left, right and bottom. Each is a list of items: mode
//! buttons, sliders, thumbnails, the colour picker, view buttons, separators, and any brush
//! or command dragged from the palettes. In Customize mode every item can be dragged to
//! another place or shelf, and dropped on the canvas to take it off; the built-in items
//! taken off wait in Preferences › Interface to be dragged back.
use super::custom_shelf::{ActionDrag, ShelfSource};
use super::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum ShelfSide {
    Top,
    Left,
    Right,
    Bottom,
}

impl ShelfSide {
    pub const ALL: [ShelfSide; 4] = [Self::Top, Self::Left, Self::Right, Self::Bottom];

    pub fn name(self) -> &'static str {
        match self {
            Self::Top => "Top",
            Self::Left => "Left",
            Self::Right => "Right",
            Self::Bottom => "Bottom",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|x| x.name() == s)
    }

    fn horizontal(self) -> bool {
        matches!(self, Self::Top | Self::Bottom)
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// What each shelf holds until it is customized.
pub fn default_items(side: ShelfSide) -> Vec<String> {
    let list: &[&str] = match side {
        ShelfSide::Top => &[
            "mode:sculpt", "mode:move", "mode:scale", "mode:rotate", "sep",
            "toggle:sculptris", "toggle:live-boolean", "sep",
            "seg:paint", "slider:rgb", "sep",
            "seg:zadd", "slider:z", "sep",
            "slider:focal", "slider:size", "space",
            "cmd:undo", "cmd:redo",
        ],
        ShelfSide::Left => &["thumb:brush", "thumb:stroke", "thumb:alpha", "thumb:texture", "thumb:material", "color", "alternate"],
        ShelfSide::Right => &[
            "cmd:frame", "toggle:persp", "toggle:floor", "toggle:sym", "sep",
            "toggle:polyf", "stub:local", "stub:transp", "stub:ghost", "toggle:solo", "sep",
            "toggle:detail", "cmd:hide-ui",
        ],
        ShelfSide::Bottom => &[],
    };
    list.iter().map(|s| s.to_string()).collect()
}

/// Items that are only shelf pieces (not palette actions), with their names.
const BUILTIN: &[(&str, &str)] = &[
    ("mode:sculpt", "Sculpt"),
    ("mode:move", "Move"),
    ("mode:scale", "Scale"),
    ("mode:rotate", "Rotate"),
    ("seg:paint", "Z+RGB / RGB / Z"),
    ("slider:rgb", "RGB Intensity"),
    ("seg:zadd", "Zadd / Zsub"),
    ("slider:z", "Z Intensity"),
    ("slider:focal", "Focal Shift"),
    ("slider:size", "Draw Size"),
    ("thumb:brush", "Brush"),
    ("thumb:stroke", "Stroke"),
    ("thumb:alpha", "Alpha"),
    ("thumb:texture", "Texture"),
    ("thumb:material", "Material"),
    ("color", "Colour picker"),
    ("alternate", "Alternate"),
    ("stub:local", "Local"),
    ("stub:transp", "Transp"),
    ("stub:ghost", "Ghost"),
    ("sep", "Separator"),
    ("space", "Spring (pushes the rest to the end)"),
];

pub fn known(id: &str) -> bool {
    BUILTIN.iter().any(|(b, _)| *b == id) || custom_shelf::known(id)
}

/// The item's name, for tooltips and the catalog.
pub fn item_label(id: &str) -> String {
    BUILTIN.iter().find(|(b, _)| *b == id).map(|(_, l)| l.to_string()).unwrap_or_else(|| custom_shelf::label(id).0)
}

/// Separators and springs can be on a shelf many times; everything else once.
fn unique(id: &str) -> bool {
    id != "sep" && id != "space"
}

/// A shelf item being dragged in Customize mode.
#[derive(Clone, Copy, Debug)]
pub struct ItemDrag {
    pub side: ShelfSide,
    pub index: usize,
}

/// What dropped on a shelf place.
enum Incoming {
    Move(ItemDrag),
    New(String),
}

/// Take a drop meant for shelves. The payload's type is checked first: egui's take empties
/// the drag whatever it holds.
fn take_drop(resp: &egui::Response) -> Option<Incoming> {
    if resp.dnd_hover_payload::<ItemDrag>().is_some() {
        return resp.dnd_release_payload::<ItemDrag>().map(|d| Incoming::Move(*d));
    }
    if resp.dnd_hover_payload::<ActionDrag>().is_some() {
        return resp.dnd_release_payload::<ActionDrag>().map(|d| Incoming::New(d.0.clone()));
    }
    None
}

fn incoming(resp: &egui::Response) -> bool {
    resp.dnd_hover_payload::<ItemDrag>().is_some() || resp.dnd_hover_payload::<ActionDrag>().is_some()
}

/// How an item sits on a top or bottom shelf: a full-height column, half of a two-row
/// column, a separator or a spring.
#[derive(Clone, Copy, PartialEq)]
enum Fit {
    Tall(f32),
    Half(f32),
    Sep,
    Space,
}

fn fit(id: &str) -> Fit {
    match id {
        "sep" => Fit::Sep,
        "space" => Fit::Space,
        _ if id.starts_with("mode:") => Fit::Tall(56.0),
        _ if id.starts_with("thumb:") => Fit::Tall(64.0),
        "color" => Fit::Tall(150.0),
        "slider:size" => Fit::Half(236.0),
        "slider:rgb" | "slider:z" | "slider:focal" | "seg:paint" | "seg:zadd" => Fit::Half(200.0),
        "toggle:sculptris" | "toggle:live-boolean" => Fit::Half(120.0),
        "cmd:undo" | "cmd:redo" => Fit::Tall(64.0),
        _ => Fit::Half(104.0),
    }
}

/// Right-shelf style icons for the actions that have one.
fn icon(id: &str) -> Option<&'static str> {
    Some(match id {
        "mode:sculpt" => "sculpt",
        "mode:move" => "move",
        "mode:scale" => "scale",
        "mode:rotate" => "rotate",
        "cmd:frame" => "frame",
        "toggle:persp" => "persp",
        "toggle:floor" => "floor",
        "toggle:sym" => "sym",
        "toggle:polyf" => "polyf",
        "stub:local" => "local",
        "stub:transp" => "transp",
        "stub:ghost" => "ghost",
        "toggle:solo" => "solo",
        "toggle:detail" => "detail",
        "cmd:hide-ui" => "hide",
        _ => return None,
    })
}

const SHELF_TALL: f32 = 56.0;
const ROW_GAP: f32 = 6.0;

impl Workspace {
    /// Every shelf and the drops on the canvas that take items off (Customize mode).
    pub(super) fn shelves_before_trays(&mut self, ui: &mut Ui, m: &mut Model<'_>, a: &mut Actions) {
        self.shelf_horizontal(ui, ShelfSide::Top, m, a);
        self.shelf_horizontal(ui, ShelfSide::Bottom, m, a);
    }

    pub(super) fn shelves_after_trays(&mut self, ui: &mut Ui, m: &mut Model<'_>, a: &mut Actions) {
        self.shelf_vertical(ui, ShelfSide::Left, m, a);
        self.shelf_vertical(ui, ShelfSide::Right, m, a);
    }

    /// An item dropped where no shelf took it: on the canvas it comes off its shelf.
    pub(super) fn shelf_drop_outside(&mut self, ctx: &Context) {
        if !self.customize || !ctx.input(|i| i.pointer.any_released()) {
            return;
        }
        let over_canvas = ctx.pointer_latest_pos().is_some_and(|p| self.canvas.contains(p));
        if over_canvas && egui::DragAndDrop::has_payload_of_type::<ItemDrag>(ctx) {
            if let Some(d) = egui::DragAndDrop::take_payload::<ItemDrag>(ctx) {
                let list = &mut self.layout.shelves[d.side.index()];
                if d.index < list.len() {
                    let id = list.remove(d.index);
                    self.notify(format!("{} taken off the {} shelf", item_label(&id), d.side.name().to_lowercase()));
                }
            }
        }
    }

    /// Put `incoming` on `side`'s shelf before `before` (or at the end).
    fn shelf_insert(&mut self, side: ShelfSide, before: Option<usize>, incoming: Incoming) {
        let (id, from) = match incoming {
            Incoming::Move(d) => match self.layout.shelves[d.side.index()].get(d.index) {
                Some(id) => (id.clone(), Some(d)),
                None => return,
            },
            Incoming::New(id) => (id, None),
        };
        if !known(&id) {
            return;
        }
        let mut before = before;
        // Take it from where it was (a dragged item, or a unique item already on a shelf).
        let take = |s: ShelfSide, i: usize, before: &mut Option<usize>, layout: &mut DockLayout| {
            layout.shelves[s.index()].remove(i);
            if s == side {
                if let Some(b) = before.as_mut() {
                    if *b > i {
                        *b -= 1;
                    }
                }
            }
        };
        if let Some(d) = from {
            if d.side == side && (Some(d.index) == before || before == Some(d.index + 1)) {
                return;
            }
            take(d.side, d.index, &mut before, &mut self.layout);
        } else if unique(&id) {
            for s in ShelfSide::ALL {
                if let Some(i) = self.layout.shelves[s.index()].iter().position(|x| *x == id) {
                    take(s, i, &mut before, &mut self.layout);
                }
            }
        }
        let list = &mut self.layout.shelves[side.index()];
        let at = before.unwrap_or(list.len()).min(list.len());
        list.insert(at, id.clone());
        if from.is_none() {
            self.notify(format!("{} → {} shelf", item_label(&id), side.name().to_lowercase()));
        }
    }

    /// Customize mode over an item at `rect`: drag it away, drop others before it.
    fn shelf_item_overlay(&mut self, ui: &mut Ui, side: ShelfSide, index: usize, rect: Rect, drops: &mut Option<(Option<usize>, Incoming)>) {
        let resp = ui.interact(rect, Id::unique(("shelf-item", side, index)), Sense::drag());
        resp.dnd_set_drag_payload(ItemDrag { side, index });
        let p = ui.painter();
        let hover = incoming(&resp);
        let color = if hover || resp.dragged() { ACC() } else if resp.hovered() { ACC_TEXT() } else { ACC().gamma_multiply(0.5) };
        p.rect_stroke(rect.expand(1.5), 4, Stroke::new(1.0, color), StrokeKind::Outside);
        if hover {
            if side.horizontal() {
                p.vline(rect.left() - 3.0, rect.y_range(), Stroke::new(2.0, ACC()));
            } else {
                p.hline(rect.x_range(), rect.top() - 3.0, Stroke::new(2.0, ACC()));
            }
        }
        if let Some(d) = take_drop(&resp) {
            *drops = Some((Some(index), d));
        }
        resp.on_hover_text(format!("{} · drag to move, drop on the canvas to take it off", item_label(&self.layout.shelves[side.index()][index])));
    }

    /// A top or bottom shelf: items in columns, half-height ones two to a column.
    fn shelf_horizontal(&mut self, ui: &mut Ui, side: ShelfSide, m: &mut Model<'_>, a: &mut Actions) {
        let items = self.layout.shelves[side.index()].clone();
        if items.is_empty() && !(self.customize && side == ShelfSide::Bottom) && side == ShelfSide::Bottom {
            return;
        }
        let panel = match side {
            ShelfSide::Top => egui::Panel::top("shelf"),
            _ => egui::Panel::bottom("shelf-bottom"),
        };
        let height = if items.is_empty() { 40.0 } else { SHELF_H };
        panel
            .exact_size(height)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::NONE.fill(BG1()).inner_margin(Margin::symmetric(12, 0)))
            .show(ui, |ui| {
                let rect = ui.max_rect();
                let p = ui.painter();
                if side == ShelfSide::Top {
                    p.hline(rect.x_range(), rect.bottom() - 0.5, (1.0, LINE()));
                } else {
                    p.hline(rect.x_range(), rect.top() + 0.5, (1.0, LINE()));
                }
                let zone = ui.interact(rect, Id::unique(("shelf-zone", side)), Sense::hover());
                // Columns: (first item, how many, width) or a spring.
                let mut cols: Vec<(Vec<usize>, Fit)> = Vec::new();
                for (i, id) in items.iter().enumerate() {
                    let f = fit(id);
                    match (f, cols.last_mut()) {
                        (Fit::Half(w), Some((col, Fit::Half(cw)))) if col.len() == 1 => {
                            col.push(i);
                            *cw = cw.max(w);
                        }
                        _ => cols.push((vec![i], f)),
                    }
                }
                let width_of = |f: Fit| match f {
                    Fit::Tall(w) | Fit::Half(w) => w,
                    Fit::Sep => 1.0,
                    Fit::Space => 0.0,
                };
                let gap = 6.0;
                let sep_pad = 6.0;
                let fixed: f32 = cols
                    .iter()
                    .map(|(_, f)| width_of(*f) + if *f == Fit::Sep { 2.0 * sep_pad } else { 0.0 })
                    .sum::<f32>()
                    + gap * cols.len().saturating_sub(1) as f32;
                let springs = cols.iter().filter(|(_, f)| *f == Fit::Space).count();
                let spare = ((rect.width() - fixed).max(0.0)) / springs.max(1) as f32;
                let mid = rect.center().y;
                let mut x = rect.left();
                let mut drops = None;
                let customize = self.customize;
                for (col, f) in &cols {
                    let w = match f {
                        Fit::Space => spare,
                        Fit::Sep => 1.0 + 2.0 * sep_pad,
                        _ => width_of(*f),
                    };
                    for (k, &i) in col.iter().enumerate() {
                        let r = match f {
                            Fit::Tall(_) => Rect::from_center_size(pos2(x + w * 0.5, mid), vec2(w, SHELF_TALL)),
                            Fit::Half(_) if col.len() == 1 => Rect::from_min_size(pos2(x, mid - CONTROL_H * 0.5), vec2(w, CONTROL_H)),
                            Fit::Half(_) => Rect::from_min_size(
                                pos2(x, mid - CONTROL_H - ROW_GAP * 0.5 + k as f32 * (CONTROL_H + ROW_GAP)),
                                vec2(w, CONTROL_H),
                            ),
                            Fit::Sep => {
                                ui.painter().vline(x + sep_pad + 0.5, (mid - 26.0)..=(mid + 26.0), (1.0, LINE()));
                                Rect::from_center_size(pos2(x + w * 0.5, mid), vec2(w, 52.0))
                            }
                            Fit::Space => {
                                if customize {
                                    let r = Rect::from_center_size(pos2(x + w * 0.5, mid), vec2(w.max(24.0), 20.0));
                                    ui.painter().text(r.center(), Align2::CENTER_CENTER, "· spring ·", font(11.0, Weight::Regular), FG3());
                                    r
                                } else {
                                    Rect::NOTHING
                                }
                            }
                        };
                        if matches!(f, Fit::Tall(_) | Fit::Half(_)) {
                            ui.scope_builder(UiBuilder::new().max_rect(r).layout(Layout::top_down(Align::Min)), |ui| {
                                ui.add_enabled_ui(!customize, |ui| self.shelf_item(ui, &items[i], true, m, a));
                            });
                        }
                        if customize && r != Rect::NOTHING {
                            self.shelf_item_overlay(ui, side, i, r, &mut drops);
                        }
                    }
                    x += w + gap;
                }
                if customize {
                    if incoming(&zone) {
                        ui.painter().rect_stroke(rect.shrink(1.0), 0, Stroke::new(2.0, ACC()), StrokeKind::Inside);
                    }
                    if items.is_empty() {
                        ui.painter().text(rect.center(), Align2::CENTER_CENTER, "Bottom shelf · drag buttons here", font(12.5, Weight::Medium), FG3());
                    }
                    if drops.is_none() {
                        if let Some(d) = take_drop(&zone) {
                            drops = Some((None, d));
                        }
                    }
                }
                if let Some((before, d)) = drops {
                    self.shelf_insert(side, before, d);
                }
            });
    }

    /// A left or right shelf: items one under another, full width, scrolling by pen drag.
    fn shelf_vertical(&mut self, ui: &mut Ui, side: ShelfSide, m: &mut Model<'_>, a: &mut Actions) {
        let items = self.layout.shelves[side.index()].clone();
        if items.is_empty() && !self.customize {
            return;
        }
        let (panel, width) = match side {
            ShelfSide::Left => (egui::Panel::left("shelf-left"), LEFT_SHELF_W),
            _ => (egui::Panel::right("shelf-right"), RIGHT_SHELF_W),
        };
        let margin = if side == ShelfSide::Left { 7 } else { 6 };
        panel
            .exact_size(width)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::NONE.fill(SHELF()).inner_margin(Margin::symmetric(margin, 10)))
            .show(ui, |ui| {
                let rect = ui.max_rect();
                let p = ui.painter();
                if side == ShelfSide::Left {
                    p.vline(rect.right() - 0.5 + margin as f32, rect.y_range(), (1.0, LINE()));
                } else {
                    p.vline(rect.left() + 0.5 - margin as f32, rect.y_range(), (1.0, LINE()));
                }
                let zone = ui.interact(rect, Id::unique(("shelf-zone", side)), Sense::hover());
                let customize = self.customize;
                let mut drops = None;
                ScrollArea::vertical()
                    .id_salt(("shelf-scroll", side))
                    .scroll_source(if customize { egui::scroll_area::ScrollSource::SCROLL_BAR | egui::scroll_area::ScrollSource::MOUSE_WHEEL } else { egui::scroll_area::ScrollSource::ALL })
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = if side == ShelfSide::Left { 8.0 } else { 6.0 };
                        for (i, id) in items.iter().enumerate() {
                            let r = ui
                                .scope(|ui| {
                                    ui.add_enabled_ui(!customize, |ui| self.shelf_item(ui, id, false, m, a));
                                })
                                .response
                                .rect;
                            if customize {
                                self.shelf_item_overlay(ui, side, i, r, &mut drops);
                            }
                        }
                        if customize {
                            // Room under the last item to drop at the end.
                            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
                            if items.is_empty() {
                                ui.painter().text(r.center(), Align2::CENTER_CENTER, "Drop\\nhere", font(11.0, Weight::Regular), FG3());
                            }
                        }
                    });
                if customize {
                    if incoming(&zone) {
                        ui.painter().rect_stroke(rect.expand(margin as f32 - 1.0), 0, Stroke::new(2.0, ACC()), StrokeKind::Inside);
                    }
                    if drops.is_none() {
                        if let Some(d) = take_drop(&zone) {
                            drops = Some((None, d));
                        }
                    }
                }
                if let Some((before, d)) = drops {
                    self.shelf_insert(side, before, d);
                }
            });
    }

    /// One shelf item, drawn to the space it is given (a cell of a top or bottom shelf, or
    /// the width of a side shelf).
    fn shelf_item(&mut self, ui: &mut Ui, id: &str, horizontal: bool, m: &mut Model<'_>, a: &mut Actions) {
        let w = ui.available_width();
        match id {
            "sep" => {
                let (r, _) = ui.allocate_exact_size(vec2(w, 7.0), Sense::hover());
                ui.painter().hline(r.x_range().shrink(4.0), r.center().y, (1.0, LINE()));
            }
            "space" => {
                ui.add_space(6.0);
            }
            _ if id.starts_with("mode:") => {
                let (glyph, label, mode, hint) = match id {
                    "mode:move" => ("move", "Move", Mode::Move, "Move · W · drag the model for an action line, its middle circle moves"),
                    "mode:scale" => ("scale", "Scale", Mode::Scale, "Scale · E · drag the action line's end circle; its middle circle squashes along the line"),
                    "mode:rotate" => ("rotate", "Rotate", Mode::Rotate, "Rotate · R · drag the action line's end circle"),
                    _ => ("sculpt", "Sculpt", Mode::Draw, "Draw · Q"),
                };
                let size = if horizontal { vec2(56.0, 56.0) } else { vec2(w, 56.0) };
                if big_button(ui, glyph, label, size, self.mode == mode, true).on_hover_text(hint).clicked() {
                    self.mode = mode;
                }
            }
            "toggle:sculptris" | "toggle:live-boolean" => {
                let (label, on, hint) = if id == "toggle:sculptris" {
                    ("Sculptris Pro", self.sculptris, "Strokes add and remove triangles under the brush · Detail in Brush › Sculptris Pro")
                } else {
                    ("Live Boolean", self.live_boolean, "Show the subtools' Add / Subtract / Intersect result · Mesh › Boolean")
                };
                let label = if w < 90.0 { if id == "toggle:sculptris" { "Sculptris" } else { "LiveBool" } } else { label };
                if small_toggle(ui, label, on, vec2(w, CONTROL_H)).on_hover_text(hint).clicked() {
                    if id == "toggle:sculptris" {
                        self.sculptris = !self.sculptris;
                    } else {
                        self.live_boolean = !self.live_boolean;
                    }
                }
            }
            "seg:paint" => self.paint_modes(ui, m),
            "slider:rgb" => {
                zslider(ui, "RGB Intensity", &mut m.brush.rgb_intensity, 0.0..=1.0, false, true, 2);
            }
            "slider:z" => {
                zslider(ui, "Z Intensity", &mut m.brush.strength, 0.0..=1.0, false, true, 2);
            }
            "slider:focal" => {
                zslider(ui, "Focal Shift", &mut m.brush.focal_shift, -100.0..=100.0, false, true, 0);
            }
            "slider:size" => {
                let mut dyn_toggle = |ui: &mut Ui, size: Vec2| {
                    if small_toggle(ui, "Dyn", self.dynamic, size).on_hover_text("Radius follows zoom").clicked() {
                        self.dynamic = !self.dynamic;
                    }
                };
                if w >= 150.0 {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let sw = ui.available_width() - 44.0;
                        ui.scope(|ui| {
                            ui.set_width(sw);
                            draw_size_slider(ui, "Draw Size", &mut m.brush.radius, true);
                        });
                        dyn_toggle(ui, vec2(40.0, CONTROL_H));
                    });
                } else {
                    draw_size_slider(ui, "Draw Size", &mut m.brush.radius, true);
                    dyn_toggle(ui, vec2(w, CONTROL_H));
                }
            }
            "seg:zadd" => {
                if w >= 150.0 {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let sw = ui.available_width() - 54.0;
                        ui.scope(|ui| {
                            ui.set_width(sw);
                            if let Some(i) = seg(ui, &["Zadd", "Zsub"], self.subtract as usize, false) {
                                self.subtract = i == 1;
                            }
                        });
                        ui.add_enabled(false, egui::Button::new("Zcut").min_size(vec2(50.0, CONTROL_H)));
                    });
                } else if let Some(i) = seg(ui, &["Zadd", "Zsub"], self.subtract as usize, false) {
                    self.subtract = i == 1;
                }
            }
            "thumb:brush" => {
                let tex = self.texture(ui.ctx(), Some(m.brush.kind), self.subtract);
                let resp = thumb(ui, "Brush", Thumb::Texture(tex), m.brush.kind.name(), true, false);
                if resp.clicked() {
                    self.open_brush_catalog(resp.rect.right_top() + vec2(6.0, 0.0));
                }
            }
            "thumb:stroke" => {
                let kind = m.brush.kind;
                let mode = self.stroke_mode(kind);
                let modes = Self::stroke_modes(kind);
                let resp = thumb(ui, "Stroke", Thumb::Glyph("stroke"), mode, false, false);
                if modes.len() < 2 {
                    resp.on_hover_text(format!("{} strokes {} only", kind.name(), mode));
                } else if resp.on_hover_text(format!("Click for the next stroke: {}", modes.join(" / "))).clicked() {
                    let at = modes.iter().position(|m| *m == mode).unwrap_or(0);
                    self.set_stroke_mode(kind, modes[(at + 1) % modes.len()]);
                }
            }
            "thumb:alpha" => self.asset_shelf(ui, false),
            "thumb:texture" => self.asset_shelf(ui, true),
            "thumb:material" => {
                let tex = self.texture(ui.ctx(), None, false);
                let resp = thumb(ui, "Material", Thumb::Texture(tex), MATERIALS[self.material].1.trim_start_matches("MatCap "), false, false);
                egui::Popup::from_toggle_button_response(&resp)
                    .align(if horizontal { egui::RectAlign::BOTTOM_START } else { egui::RectAlign::RIGHT_START })
                    .gap(6.0)
                    .frame(popup_frame())
                    .show(|ui| {
                        ui.label(RichText::new("MATERIAL").size(11.0).color(FG3()));
                        self.material_swatches(ui);
                    });
            }
            "color" => self.color_item(ui, horizontal),
            "alternate" => {
                ui.add_enabled_ui(false, |ui| {
                    let _ = ui.add(egui::Button::new(RichText::new("Alternate").color(ACC_FG())).fill(ACC()).min_size(vec2(w, CONTROL_H)));
                });
            }
            _ => {
                // Actions: a shelf button with its icon on a side shelf, a text button on a
                // top or bottom one; brushes show their preview.
                let stub = id.starts_with("stub:");
                let on = !stub && self.shelf_action_on(id, m);
                let (label, hint) = if stub { (item_label(id), NC.to_string()) } else { custom_shelf::label(id) };
                let label = if id == "toggle:sym" && !on { "Sym".to_string() } else { label };
                let resp = if let (Some(glyph), false) = (icon(id), horizontal) {
                    big_button(ui, glyph, &label, vec2(w, 50.0), on, !stub)
                } else if horizontal && matches!(id, "cmd:undo" | "cmd:redo") {
                    let enabled = if id == "cmd:undo" { m.can_undo } else { m.can_redo };
                    ui.add_space(((ui.available_height() - 32.0) * 0.5).max(0.0));
                    ui.add_enabled(enabled, egui::Button::new(label.as_str()).min_size(vec2(w, 32.0)))
                } else {
                    self.action_button(ui, id, &label, on, !stub, if horizontal { CONTROL_H } else { 44.0 })
                };
                if resp.on_hover_text(hint).clicked() && !stub {
                    self.run_action(id, m, a);
                }
            }
        }
    }

    /// A text (and preview) button for an action, `h` high and as wide as there is room.
    fn action_button(&mut self, ui: &mut Ui, id: &str, label: &str, on: bool, enabled: bool, h: f32) -> egui::Response {
        let w = ui.available_width();
        let (r, resp) = ui.allocate_exact_size(vec2(w, h), if enabled { Sense::click() } else { Sense::hover() });
        let brush = id.strip_prefix("brush:").and_then(|n| BrushKind::ALL.into_iter().find(|k| k.name() == n));
        let tex = brush.map(|k| self.texture(ui.ctx(), Some(k), self.subtract && !k.is_mask()));
        let p = ui.painter();
        p.rect_filled(r, 4, if on { ACC_DIM() } else if resp.hovered() { BG4() } else { BG3() });
        p.rect_stroke(r, 4, Stroke::new(1.0, if on { ACC() } else { LINE() }), StrokeKind::Inside);
        let tall = h > 36.0;
        let mut text_rect = r.shrink2(vec2(6.0, 2.0));
        if let Some(tex) = tex {
            let side = if tall { (h - 18.0).min(w - 8.0) } else { h - 6.0 };
            let im = if tall {
                Rect::from_center_size(pos2(r.center().x, r.top() + 3.0 + side * 0.5), vec2(side, side))
            } else {
                Rect::from_center_size(pos2(r.left() + 3.0 + side * 0.5, r.center().y), vec2(side, side))
            };
            p.image(tex, im, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
            text_rect = if tall { Rect::from_min_max(pos2(r.left() + 3.0, im.bottom()), r.max - vec2(3.0, 1.0)) } else { Rect::from_min_max(pos2(im.right() + 5.0, r.top()), r.max - vec2(4.0, 0.0)) };
        }
        let color = if !enabled { FG3() } else if on { FG() } else { FG2() };
        let mut job = egui::text::LayoutJob::simple(label.to_owned(), font(if tall { 11.0 } else { 12.0 }, Weight::Medium), color, text_rect.width().max(10.0));
        job.wrap.max_rows = if tall && tex.is_none() { 2 } else { 1 };
        job.wrap.break_anywhere = false;
        job.halign = if tall { Align::Center } else { Align::Min };
        let g = p.layout_job(job);
        let x = if tall { text_rect.center().x } else { text_rect.left() };
        p.galley(pos2(x, text_rect.center().y - g.size().y * 0.5), g, color);
        resp
    }

    fn shelf_action_on(&self, id: &str, m: &Model<'_>) -> bool {
        custom_shelf::action_on(self, id, m)
    }

    /// The colour picker with the two colours and the hex code.
    fn color_item(&mut self, ui: &mut Ui, horizontal: bool) {
        let swatches = |ws: &mut Self, ui: &mut Ui| {
            let (r, resp) = ui.allocate_exact_size(vec2(36.0, 32.0), Sense::click());
            let p = ui.painter();
            let back = Rect::from_min_size(r.min + vec2(14.0, 10.0), vec2(22.0, 22.0));
            let front = Rect::from_min_size(r.min, vec2(22.0, 22.0));
            for (rect, c) in [(back, ws.color2), (front, ws.color)] {
                p.rect_filled(rect, 0, Color32::from(c));
                p.rect_stroke(rect, 0, Stroke::new(1.0, LINE()), StrokeKind::Inside);
            }
            glyph(p, r.right_top() + vec2(-6.0, 6.0), "swap", if resp.hovered() { FG() } else { FG2() });
            if resp.on_hover_text("Switch colours").clicked() {
                std::mem::swap(&mut ws.color, &mut ws.color2);
            }
        };
        let [cr, cg, cb] = self.color.to_srgb();
        let hex = format!("#{cr:02X}{cg:02X}{cb:02X}");
        if horizontal {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                color_picker_sized(ui, &mut self.color, vec2(90.0, 56.0));
                ui.vertical(|ui| {
                    swatches(self, ui);
                    ui.label(RichText::new(hex).font(font(11.5, Weight::Medium)).color(FG2()));
                });
            });
        } else {
            let w = ui.available_width();
            color_picker_sized(ui, &mut self.color, vec2(w, (w * 0.84).min(72.0)));
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                swatches(self, ui);
                if w >= 70.0 {
                    ui.label(RichText::new(hex).font(font(12.0, Weight::Medium)).color(FG2()));
                }
            });
        }
    }

    /// Preferences › Interface while customizing: the built-in items taken off every shelf,
    /// to drag back.
    pub(super) fn shelf_catalog(&mut self, ui: &mut Ui) {
        let on_shelves: Vec<&String> = self.layout.shelves.iter().flatten().collect();
        let missing: Vec<&str> = BUILTIN
            .iter()
            .map(|(id, _)| *id)
            .filter(|id| !unique(id) || !on_shelves.iter().any(|s| s.as_str() == *id))
            .collect();
        ui.label(RichText::new("Shelf items").font(font(13.0, Weight::Medium)).color(FG2()));
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
            for id in missing {
                let label = item_label(id);
                let label = label.split(" (").next().unwrap_or("").to_string();
                ui.add(egui::Button::new(RichText::new(label).font(font(12.0, Weight::Regular))).min_size(vec2(0.0, 24.0)))
                    .shelf(ui, id);
            }
        });
        note(ui, "Drag these, and the outlined buttons and brushes in the palettes, onto any shelf · drop a shelf item on the canvas to take it off");
    }
}

/// Decode the old per-shelf lines (0.23–0.24) into shelf item lists.
pub fn migrate(top: Option<&str>, right: Option<&str>, mine: Option<&str>) -> [Vec<String>; 4] {
    let mut shelves = ShelfSide::ALL.map(default_items);
    let mine: Vec<String> = mine.map(|l| l.split(';').filter(|id| custom_shelf::known(id)).map(str::to_string).collect()).unwrap_or_default();
    if let Some(list) = top {
        let mut items = Vec::new();
        for g in list.split(',') {
            if g.starts_with('-') {
                continue;
            }
            let group: Vec<String> = match g {
                "modes" => vec!["mode:sculpt", "mode:move", "mode:scale", "mode:rotate"],
                "toggles" => vec!["toggle:sculptris", "toggle:live-boolean"],
                "rgb" => vec!["seg:paint", "slider:rgb"],
                "zadd" => vec!["seg:zadd", "slider:z"],
                "size" => vec!["slider:focal", "slider:size"],
                "custom" => mine.iter().map(String::as_str).collect(),
                _ => vec![],
            }
            .into_iter()
            .map(str::to_string)
            .collect();
            if group.is_empty() {
                continue;
            }
            if !items.is_empty() {
                items.push("sep".to_string());
            }
            items.extend(group);
        }
        items.extend(["space", "cmd:undo", "cmd:redo"].map(str::to_string));
        shelves[0] = items;
    } else if !mine.is_empty() {
        let at = shelves[0].iter().position(|s| s == "space").unwrap_or(shelves[0].len());
        for (k, id) in mine.into_iter().enumerate() {
            shelves[0].insert(at + k, id);
        }
    }
    if let Some(list) = right {
        shelves[2] = list
            .split(',')
            .filter(|s| !s.starts_with('-'))
            .filter_map(|s| {
                Some(match s {
                    "frame" => "cmd:frame",
                    "persp" => "toggle:persp",
                    "floor" => "toggle:floor",
                    "sym" => "toggle:sym",
                    "polyf" => "toggle:polyf",
                    "local" => "stub:local",
                    "transp" => "stub:transp",
                    "ghost" => "stub:ghost",
                    "solo" => "toggle:solo",
                    "detail" => "toggle:detail",
                    "hide" => "cmd:hide-ui",
                    "sep1" | "sep2" => "sep",
                    _ => return None,
                })
            })
            .map(str::to_string)
            .collect();
    }
    shelves
}
