//! Brush catalog, image pickers and the compact Space palette.
use super::*;
use crate::brush_asset::{BrushImage, Pattern};
use crate::sculpt::StampSettings;
use std::sync::mpsc::{self, Receiver};

#[derive(Clone, Copy, PartialEq)]
enum BrushGroup {
    All,
    Sculpt,
    Mask,
    Detail,
    Clip,
    Curve,
}
impl BrushGroup {
    const ALL: [Self; 6] = [
        Self::All,
        Self::Sculpt,
        Self::Mask,
        Self::Detail,
        Self::Clip,
        Self::Curve,
    ];
    fn accepts(self, k: BrushKind) -> bool {
        match self {
            Self::All => true,
            Self::Mask => k.is_mask(),
            Self::Detail => k.is_detail(),
            Self::Clip => k == BrushKind::ClipLine,
            Self::Curve => k.is_insert(),
            Self::Sculpt => !k.is_mask() && !k.is_detail() && !k.is_insert() && k != BrushKind::ClipLine,
        }
    }
}

pub(super) struct BrushUi {
    library: crate::asset_library::Library,
    recent: Vec<BrushKind>,
    catalog: Option<Pos2>,
    filter: Option<char>,
    group: BrushGroup,
    search: String,
    palette_masks: bool,
    last_sculpt: BrushKind,
    preview: Option<BrushKind>,
    images: HashMap<(u64, bool), TextureHandle>,
    import: Option<Receiver<(bool, Result<Option<Pattern>, String>)>>,
    path_dialog: Option<bool>,
    path: String,
}

impl BrushUi {
    pub(super) fn new(path: PathBuf) -> Self {
        Self {
            library: crate::asset_library::Library::load(path),
            recent: vec![
                BrushKind::Draw,
                BrushKind::DamStandard,
                BrushKind::ClayBuildup,
                BrushKind::Grab,
                BrushKind::Smooth,
                BrushKind::Pinch,
            ],
            catalog: None,
            filter: None,
            group: BrushGroup::All,
            search: String::new(),
            palette_masks: false,
            last_sculpt: BrushKind::Draw,
            preview: None,
            images: HashMap::new(),
            import: None,
            path_dialog: None,
            path: String::new(),
        }
    }
    pub(super) fn settings(&self) -> StampSettings {
        self.library.settings.clone()
    }
}

impl Workspace {
    pub fn brush_catalog_open(&self) -> bool {
        self.brush_ui.catalog.is_some()
            || self.brush_ui.path_dialog.is_some()
            || self.brush_ui.import.is_some()
    }

    pub fn open_brush_catalog(&mut self, pos: Pos2) {
        if self.about_open {
            return;
        }
        self.brush_ui.catalog = Some(pos);
        self.brush_ui.filter = None;
        self.brush_ui.search.clear();
        self.brush_ui.group = if self.brush_ui.palette_masks {
            BrushGroup::Mask
        } else {
            BrushGroup::All
        };
        self.brush_ui.preview = if self.brush_ui.palette_masks {
            Some(self.mask_brush)
        } else {
            None
        };
        self.dropdown = None;
        self.popup = None;
    }

    pub(super) fn choose_brush(&mut self, m: &mut Model<'_>, kind: BrushKind) {
        self.select_brush(m.brush, kind);
    }

    pub fn select_brush(&mut self, brush: &mut Brush, kind: BrushKind) {
        let was_special = brush.kind == BrushKind::Paint
            || brush.kind.is_mask()
            || brush.kind == BrushKind::ClipLine;
        brush.kind = kind;
        self.brush_ui.palette_masks = kind.is_mask();
        if !kind.is_mask() {
            self.brush_ui.last_sculpt = kind;
        }
        if kind.is_mask() {
            self.mask_brush = kind;
            brush.sculpt = false;
            brush.paint = false;
            self.mode = Mode::Draw;
        } else if kind == BrushKind::ClipLine {
            brush.sculpt = true;
            brush.paint = false;
            self.mode = Mode::Draw;
        } else if kind == BrushKind::Paint {
            brush.sculpt = false;
            brush.paint = true;
        } else if was_special {
            brush.sculpt = true;
            brush.paint = false;
        }
        self.brush_ui.recent.retain(|k| *k != kind);
        self.brush_ui.recent.insert(0, kind);
        self.brush_ui.recent.truncate(6);
    }

    pub(super) fn paint_modes(&mut self, ui: &mut Ui, m: &mut Model<'_>) {
        if m.brush.kind.is_mask() || m.brush.kind == BrushKind::ClipLine {
            ui.label(if m.brush.kind.is_mask() {
                "Mask"
            } else {
                "Clip Line"
            });
            return;
        }
        let selected = if m.brush.paint {
            if m.brush.sculpt { 0 } else { 1 }
        } else {
            2
        };
        if let Some(i) = seg(ui, &["Z+RGB", "RGB", "Z"], selected, false) {
            m.brush.paint = i != 2;
            m.brush.sculpt = i != 1;
            if m.brush.sculpt && m.brush.kind == BrushKind::Paint {
                m.brush.kind = BrushKind::Draw;
            }
        }
    }

    pub(super) fn brush_assets_frame(&mut self, ctx: &Context) {
        if let Some(rx) = &self.brush_ui.import {
            match rx.try_recv() {
                Ok((texture, Ok(Some(p)))) => {
                    let p = self.brush_ui.library.import(p, texture);
                    if texture {
                        self.assets.texture = p;
                    } else {
                        self.assets.alpha = p;
                    }
                    self.brush_ui.import = None;
                }
                Ok((_, Ok(None))) => self.brush_ui.import = None,
                Ok((texture, Err(e))) => {
                    self.brush_ui.import = None;
                    self.brush_ui.path_dialog = Some(texture);
                    self.flash(ctx, e);
                }
                Err(mpsc::TryRecvError::Disconnected) => self.brush_ui.import = None,
                Err(mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(100))
                }
            }
        }
        if let Some(e) = self.brush_ui.library.error.take() {
            self.flash(ctx, e);
        }
    }

    pub(super) fn save_brush_assets(&mut self) {
        self.brush_ui.library.save(&self.assets);
    }

    fn start_import(&mut self, texture: bool) {
        if self.brush_ui.import.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.brush_ui.import = Some(rx);
        std::thread::spawn(move || {
            let result = (|| -> Result<Option<Pattern>, String> {
                let output = std::process::Command::new("zenity")
                    .args([
                        "--file-selection",
                        "--title",
                        if texture {
                            "Import Texture"
                        } else {
                            "Import Alpha"
                        },
                        "--file-filter=Images | *.png *.jpg *.jpeg *.bmp *.PNG *.JPG *.JPEG *.BMP",
                    ])
                    .output()
                    .map_err(|_| "Enter the image path to import".to_string())?;
                if !output.status.success() {
                    return if output.status.code() == Some(1) {
                        Ok(None)
                    } else {
                        Err("Enter the image path to import".into())
                    };
                }
                let path =
                    String::from_utf8(output.stdout).map_err(|_| "Image path is not valid text")?;
                let p = path.trim_end_matches(['\r', '\n']);
                if p.is_empty() {
                    return Ok(None);
                }
                BrushImage::load(std::path::Path::new(p)).map(|i| Some(Pattern::Image(i)))
            })();
            let _ = tx.send((texture, result));
        });
    }

    fn pattern_texture(&mut self, ctx: &Context, p: &Pattern, color: bool) -> TextureId {
        let key = (p.key(), color);
        self.brush_ui
            .images
            .entry(key)
            .or_insert_with(|| {
                let n = 96;
                let mut data = Vec::with_capacity(n * n * 4);
                for y in 0..n {
                    for x in 0..n {
                        let uv =
                            glam::Vec2::new(x as f32 / (n - 1) as f32, y as f32 / (n - 1) as f32);
                        let c = p.sample(uv, false);
                        let rgb = if color {
                            c.truncate()
                        } else {
                            glam::Vec3::splat(p.alpha(uv))
                        };
                        data.extend([
                            (rgb.x * 255.0) as u8,
                            (rgb.y * 255.0) as u8,
                            (rgb.z * 255.0) as u8,
                            if color { (c.w * 255.0) as u8 } else { 255 },
                        ]);
                    }
                }
                ctx.load_texture(
                    format!("brush-asset-{}-{color}", p.key()),
                    ColorImage::from_rgba_unmultiplied([n, n], &data),
                    TextureOptions::LINEAR,
                )
            })
            .id()
    }

    pub(super) fn asset_grid(&mut self, ui: &mut Ui, texture: bool) {
        let selected = if texture {
            self.assets.texture.clone()
        } else {
            self.assets.alpha.clone()
        };
        let list = if texture {
            self.brush_ui.library.textures.clone()
        } else {
            self.brush_ui.library.alphas.clone()
        };
        let cols = ((ui.available_width() + 4.0) / 74.0).floor().max(1.0) as usize;
        let width = (ui.available_width() - 4.0 * (cols - 1) as f32) / cols as f32;
        for row in list.chunks(cols) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for p in row {
                    let tex = self.pattern_texture(ui.ctx(), p, texture);
                    let (r, resp) =
                        ui.allocate_exact_size(vec2(width, width + 22.0), Sense::click());
                    let on = p == &selected;
                    let painter = ui.painter();
                    painter.rect_filled(r, 2, if on { ACC_DIM } else { BG3 });
                    painter.rect_stroke(
                        r,
                        2,
                        Stroke::new(1.0, if on { ACC_LINE } else { LINE }),
                        StrokeKind::Inside,
                    );
                    if p != &Pattern::Off {
                        painter.image(
                            tex,
                            Rect::from_min_size(
                                r.min + vec2(4.0, 4.0),
                                vec2(width - 8.0, width - 8.0),
                            ),
                            Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    } else {
                        glyph_large(painter, r.center() - vec2(0.0, 9.0), "alpha", FG3);
                    }
                    let mut job = egui::text::LayoutJob::simple(
                        p.name().to_owned(),
                        FontId::proportional(11.5),
                        FG,
                        width - 6.0,
                    );
                    job.wrap.max_rows = 1;
                    job.wrap.break_anywhere = true;
                    let galley = painter.layout_job(job);
                    painter
                        .with_clip_rect(r.intersect(painter.clip_rect()))
                        .galley(
                            pos2(r.center().x - galley.size().x * 0.5, r.bottom() - 19.0),
                            galley,
                            FG,
                        );
                    let resp = resp.on_hover_text(p.name());
                    if resp.clicked() {
                        if texture {
                            self.assets.texture = p.clone();
                        } else {
                            self.assets.alpha = p.clone();
                        }
                    }
                }
            });
        }
        if full_button(
            ui,
            if texture {
                "Import Texture…"
            } else {
                "Import Alpha…"
            },
        )
        .clicked()
        {
            self.start_import(texture);
        }
        if texture {
            zslider(
                ui,
                "Relief",
                &mut self.assets.texture_strength,
                0.0..=1.0,
                false,
                false,
                2,
            );
            zslider(
                ui,
                "Tiling",
                &mut self.assets.texture_scale,
                0.25..=16.0,
                true,
                false,
                2,
            );
        }
    }

    pub(super) fn asset_shelf(&mut self, ui: &mut Ui, texture: bool) {
        let p = if texture {
            self.assets.texture.clone()
        } else {
            self.assets.alpha.clone()
        };
        let label = format!("{} {}", if texture { "Texture" } else { "Alpha" }, p.name());
        let tex = self.pattern_texture(ui.ctx(), &p, texture);
        let resp = thumb(
            ui,
            if p == Pattern::Off {
                Thumb::Glyph(if texture { "texture" } else { "alpha" })
            } else {
                Thumb::Texture(tex)
            },
            &label,
            false,
            false,
        );
        egui::Popup::from_toggle_button_response(&resp)
            .align(egui::RectAlign::RIGHT_START)
            .gap(6.0)
            .frame(popup_frame())
            .show(|ui| {
                ui.set_width(280.0);
                ScrollArea::vertical()
                    .id_salt(("shelf-assets", texture))
                    .max_height((ui.ctx().content_rect().height() - 40.0).min(480.0))
                    .show(ui, |ui| self.asset_grid(ui, texture));
            });
    }

    pub(super) fn brush_palette(&mut self, ui: &mut Ui, m: &mut Model<'_>) {
        if full_button(ui, &format!("All Brushes ({})", BrushKind::ALL.len()))
            .on_hover_text("Open brush catalog · B")
            .clicked()
        {
            self.open_brush_catalog(ui.next_widget_position());
        }
        if let Some(i) = seg(
            ui,
            &["Sculpt", "Mask"],
            self.brush_ui.palette_masks as usize,
            false,
        ) {
            let kind = if i == 1 {
                self.mask_brush
            } else {
                self.brush_ui.last_sculpt
            };
            self.select_brush(m.brush, kind);
        }
        let width = (ui.available_width() - 4.0) * 0.5;
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let kind = m.brush.kind;
            let tex = self.texture(ui.ctx(), Some(kind), self.subtract);
            if compact_brush_tile(ui, tex, kind, true, width, true).clicked() {
                self.open_brush_catalog(ui.next_widget_position());
            }
            ui.vertical(|ui| {
                ui.set_width(width);
                let recent: Vec<_> = self
                    .brush_ui
                    .recent
                    .iter()
                    .copied()
                    .filter(|k| *k != kind)
                    .take(2)
                    .collect();
                for k in recent {
                    let tex = self.texture(ui.ctx(), Some(k), self.subtract);
                    if compact_brush_tile(ui, tex, k, false, width, false).clicked() {
                        self.choose_brush(m, k);
                    }
                }
            });
        });
        // A fixed working set keeps the side palette small as the full catalog grows.
        if self.brush_ui.palette_masks {
            self.mask_picker(ui, m);
            return;
        }
        let picks = [
            BrushKind::Clay,
            BrushKind::ClayBuildup,
            BrushKind::DamStandard,
            BrushKind::Pinch,
            BrushKind::Grab,
            BrushKind::SnakeHook,
            BrushKind::Layer,
            BrushKind::FormSoft,
            BrushKind::Zipper,
            BrushKind::ClipLine,
            BrushKind::Smooth,
            BrushKind::Paint,
        ];
        for row in picks.chunks(2) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for &k in row {
                    let tex = self.texture(ui.ctx(), Some(k), self.subtract);
                    if compact_brush_tile(ui, tex, k, m.brush.kind == k, width, false).clicked() {
                        self.choose_brush(m, k);
                    }
                }
            });
        }
    }

    pub(super) fn mask_picker(&mut self, ui: &mut Ui, m: &mut Model<'_>) {
        let width = (ui.available_width() - 4.0) * 0.5;
        let kinds = [
            BrushKind::MaskPen,
            BrushKind::MaskSoft,
            BrushKind::MaskRect,
            BrushKind::MaskLasso,
            BrushKind::MaskCircle,
            BrushKind::MaskLine,
        ];
        for row in kinds.chunks(2) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for &k in row {
                    let tex = self.texture(ui.ctx(), Some(k), false);
                    if compact_brush_tile(ui, tex, k, self.mask_brush == k, width, false).clicked()
                    {
                        self.choose_brush(m, k);
                    }
                }
            });
        }
    }

    pub(super) fn brush_catalog_ui(&mut self, ui: &mut Ui, m: &mut Model<'_>) {
        let Some(anchor) = self.brush_ui.catalog else {
            return;
        };
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.brush_ui.catalog = None;
            return;
        }
        let bounds = ctx.content_rect().shrink(10.0);
        let width = (bounds.width() - 12.0).min(850.0);
        let height = bounds.height().min(620.0);
        let pos = pos2(
            anchor
                .x
                .min(bounds.right() - width - 8.0)
                .max(bounds.left()),
            anchor.y.min(bounds.bottom() - height).max(bounds.top()),
        );
        let mut chosen = None;
        let mut close = false;
        let area = egui::Area::new(Id::unique("brush-catalog"))
            .order(egui::Order::Foreground)
            .default_size(vec2(width + 8.0, height))
            .fixed_pos(pos)
            .show(&ctx, |ui| {
                popup_frame().show(ui, |ui| {
                    ui.set_width(width);
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("Brushes ({})", BrushKind::ALL.len()))
                                .size(14.0)
                                .strong(),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            close |= ui.button("Close").clicked();
                        });
                    });
                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut self.brush_ui.search)
                                    .hint_text("Search brushes")
                                    .desired_width(170.0),
                            )
                            .changed()
                        {
                            self.brush_ui.group = BrushGroup::All;
                            self.brush_ui.filter = None;
                        }
                        if let Some(i) = seg(
                            ui,
                            &["All", "Sculpt", "Mask", "Detail", "Clip", "Curve"],
                            BrushGroup::ALL
                                .iter()
                                .position(|g| *g == self.brush_ui.group)
                                .unwrap(),
                            false,
                        ) {
                            self.brush_ui.group = BrushGroup::ALL[i];
                            self.brush_ui.filter = None;
                            if self.brush_ui.group == BrushGroup::Mask {
                                self.brush_ui.preview = Some(self.mask_brush);
                            }
                        }
                    });
                    ui.separator();
                    ui.horizontal_top(|ui| {
                        let grid_width = width - 158.0;
                        ui.vertical(|ui| {
                            ui.set_width(grid_width);
                            ScrollArea::vertical()
                                .id_salt(("brush-catalog-scroll", self.brush_ui.group as u8))
                                .max_height(height - 76.0)
                                .show(ui, |ui| {
                                    let group = self.brush_ui.group;
                                    let recent: Vec<_> = self
                                        .brush_ui
                                        .recent
                                        .iter()
                                        .copied()
                                        .filter(|k| group.accepts(*k))
                                        .collect();
                                    if !recent.is_empty() && self.brush_ui.search.is_empty() {
                                        ui.label(RichText::new("Quick Pick").size(12.0).color(FG2));
                                        ui.horizontal(|ui| {
                                            for k in recent {
                                                let tex =
                                                    self.texture(ui.ctx(), Some(k), self.subtract);
                                                let r = brush_tile(
                                                    ui,
                                                    tex,
                                                    k.name(),
                                                    0,
                                                    m.brush.kind == k,
                                                    70.0,
                                                );
                                                if r.hovered() {
                                                    self.brush_ui.preview = Some(k);
                                                }
                                                if r.clicked() {
                                                    chosen = Some(k);
                                                }
                                            }
                                        });
                                        ui.separator();
                                    }
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().button_padding = vec2(3.0, 1.0);
                                        ui.spacing_mut().item_spacing.x = 1.0;
                                        if ui
                                            .selectable_label(self.brush_ui.filter.is_none(), "All")
                                            .clicked()
                                        {
                                            self.brush_ui.filter = None;
                                        }
                                        for letter in 'A'..='Z' {
                                            let exists = BrushKind::ALL.iter().any(|k| {
                                                group.accepts(*k) && k.name().starts_with(letter)
                                            });
                                            if ui
                                                .add_enabled(
                                                    exists,
                                                    egui::Button::new(letter.to_string())
                                                        .selected(
                                                            self.brush_ui.filter == Some(letter),
                                                        )
                                                        .frame_when_inactive(false),
                                                )
                                                .clicked()
                                            {
                                                self.brush_ui.filter = Some(letter);
                                            }
                                        }
                                    });
                                    let search = self.brush_ui.search.trim().to_lowercase();
                                    let mut list: Vec<_> = BrushKind::ALL
                                        .into_iter()
                                        .filter(|k| {
                                            group.accepts(*k)
                                                && k.name().to_lowercase().contains(&search)
                                                && self
                                                    .brush_ui
                                                    .filter
                                                    .is_none_or(|c| k.name().starts_with(c))
                                        })
                                        .collect();
                                    list.sort_by_key(|k| k.name());
                                    let cols =
                                        ((grid_width + 4.0) / 84.0).floor().max(1.0) as usize;
                                    let tile = (grid_width - 4.0 * (cols - 1) as f32) / cols as f32;
                                    if list.is_empty() {
                                        ui.label("No brushes found");
                                    }
                                    for row in list.chunks(cols) {
                                        ui.horizontal(|ui| {
                                            ui.spacing_mut().item_spacing.x = 4.0;
                                            for &k in row {
                                                let tex =
                                                    self.texture(ui.ctx(), Some(k), self.subtract);
                                                let key = BrushKind::ALL
                                                    .iter()
                                                    .position(|x| *x == k)
                                                    .unwrap()
                                                    + 1;
                                                let r = brush_tile(
                                                    ui,
                                                    tex,
                                                    k.name(),
                                                    key,
                                                    m.brush.kind == k,
                                                    tile,
                                                );
                                                if r.hovered() {
                                                    self.brush_ui.preview = Some(k);
                                                }
                                                if r.clicked() {
                                                    chosen = Some(k);
                                                }
                                            }
                                        });
                                    }
                                });
                        });
                        ui.separator();
                        ui.vertical(|ui| {
                            ui.set_width(142.0);
                            let k = self.brush_ui.preview.unwrap_or(m.brush.kind);
                            let tex = self.texture(ui.ctx(), Some(k), self.subtract);
                            ui.add(egui::Image::new((tex, vec2(136.0, 136.0))));
                            ui.label(RichText::new(k.name()).size(15.0).color(FG));
                            ui.label(
                                RichText::new(format!("Base type: {}", k.base_type()))
                                    .size(12.0)
                                    .color(FG2),
                            );
                            ui.label(RichText::new(k.description()).size(12.0).color(FG2));
                        });
                    });
                });
            });
        if let Some(k) = chosen {
            self.choose_brush(m, k);
            close = true;
        }
        if ctx.input(|i| {
            i.pointer.any_pressed()
                && i.pointer
                    .latest_pos()
                    .is_some_and(|p| !area.response.rect.contains(p))
        }) {
            close = true;
        }
        if close {
            self.brush_ui.catalog = None;
        }
    }

    pub(super) fn draw_quick_menu(&mut self, ui: &mut Ui, m: &mut Model<'_>) {
        let Some((pos, held)) = self.quick_menu else {
            return;
        };
        let ctx = ui.ctx().clone();
        if !held && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.quick_menu = None;
            return;
        }
        let area = egui::Area::new(Id::unique("quick-menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos - vec2(130.0, 20.0))
            .show(&ctx, |ui| {
                popup_frame().show(ui, |ui| {
                    ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                    ui.horizontal_top(|ui| {
                        ui.vertical(|ui| {
                            ui.set_width(94.0);
                            color_picker(ui, &mut self.color);
                            ui.horizontal(|ui| {
                                let (r, _) =
                                    ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                                ui.painter().rect_filled(r, 0, Color32::from(self.color));
                                let [r, g, b] = self.color.to_srgb();
                                ui.label(
                                    RichText::new(format!("#{r:02X}{g:02X}{b:02X}"))
                                        .font(FontId::monospace(10.5))
                                        .color(FG2),
                                );
                            });
                            if ui.button("Swap colors").clicked() {
                                std::mem::swap(&mut self.color, &mut self.color2);
                            }
                            let modes_enabled =
                                !m.brush.kind.is_mask() && m.brush.kind != BrushKind::ClipLine;
                            ui.add_enabled(
                                modes_enabled,
                                egui::Checkbox::new(&mut m.brush.paint, "RGB"),
                            );
                            if ui
                                .add_enabled(
                                    modes_enabled,
                                    egui::Checkbox::new(&mut m.brush.sculpt, "Sculpt"),
                                )
                                .changed()
                                && m.brush.sculpt
                                && m.brush.kind == BrushKind::Paint
                            {
                                m.brush.kind = BrushKind::Draw;
                            }
                        });
                        ui.separator();
                        ui.vertical(|ui| {
                            ui.set_width(320.0);
                            zslider(
                                ui,
                                "Draw Size",
                                &mut m.brush.radius,
                                0.01..=2.0,
                                true,
                                true,
                                2,
                            );
                            zslider(
                                ui,
                                "Z Intensity",
                                &mut m.brush.strength,
                                0.0..=1.0,
                                false,
                                true,
                                2,
                            );
                            zslider(
                                ui,
                                "Focal Shift",
                                &mut m.brush.focal_shift,
                                -100.0..=100.0,
                                false,
                                true,
                                0,
                            );
                            zslider(
                                ui,
                                "RGB Intensity",
                                &mut m.brush.rgb_intensity,
                                0.0..=1.0,
                                false,
                                true,
                                2,
                            );
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Brushes").size(11.5).color(FG2));
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if ui.small_button("All Brushes · B").clicked() {
                                        self.open_brush_catalog(pos);
                                    }
                                });
                            });
                            let mut list = self.brush_ui.recent.clone();
                            list.retain(|k| *k != m.brush.kind);
                            list.insert(0, m.brush.kind);
                            list.truncate(5);
                            ui.horizontal(|ui| {
                                for k in list {
                                    let tex = self.texture(ui.ctx(), Some(k), self.subtract);
                                    if brush_tile(ui, tex, k.name(), 0, m.brush.kind == k, 60.0)
                                        .clicked()
                                    {
                                        self.choose_brush(m, k);
                                    }
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.scope(|ui| {
                                    ui.set_width(100.0);
                                    if let Some(i) =
                                        seg(ui, &["Zadd", "Zsub"], self.subtract as usize, false)
                                    {
                                        self.subtract = i == 1;
                                    }
                                });
                                let modes = [Mode::Draw, Mode::Move, Mode::Scale, Mode::Rotate];
                                let selected =
                                    modes.iter().position(|x| *x == self.mode).unwrap_or(0);
                                if let Some(i) =
                                    seg(ui, &["Draw", "Move", "Scale", "Rotate"], selected, false)
                                {
                                    self.mode = modes[i];
                                }
                            });
                        });
                    });
                });
            });
        if !held
            && ctx.input(|i| {
                i.pointer.any_pressed()
                    && i.pointer
                        .latest_pos()
                        .is_some_and(|p| !area.response.rect.contains(p))
            })
        {
            self.quick_menu = None;
        }
    }

    pub(super) fn import_path_ui(&mut self, ui: &mut Ui) {
        let Some(texture) = self.brush_ui.path_dialog else {
            return;
        };
        let mut open = true;
        egui::Window::new(if texture {
            "Import Texture"
        } else {
            "Import Alpha"
        })
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .show(ui.ctx(), |ui| {
            ui.label("Image path");
            ui.text_edit_singleline(&mut self.brush_ui.path);
            if ui.button("Import").clicked() {
                match BrushImage::load(std::path::Path::new(&self.brush_ui.path)) {
                    Ok(i) => {
                        let p = self.brush_ui.library.import(Pattern::Image(i), texture);
                        if texture {
                            self.assets.texture = p
                        } else {
                            self.assets.alpha = p
                        }
                        self.brush_ui.path_dialog = None;
                    }
                    Err(e) => self.flash(ui.ctx(), e),
                }
            }
        });
        if !open {
            self.brush_ui.path_dialog = None;
        }
    }
}

/// ZBrush-style side-palette cells: one hero preview, small paired working brushes.
fn compact_brush_tile(
    ui: &mut Ui,
    tex: TextureId,
    kind: BrushKind,
    on: bool,
    width: f32,
    hero: bool,
) -> egui::Response {
    let height = if hero { 104.0 } else { 50.0 };
    let (r, resp) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    let p = ui.painter();
    p.rect_filled(
        r,
        1,
        if on {
            ACC_DIM
        } else if resp.hovered() {
            BG4
        } else {
            BG2
        },
    );
    p.rect_stroke(
        r,
        1,
        Stroke::new(1.0, if on { ACC_LINE } else { LINE }),
        StrokeKind::Inside,
    );
    let size = if hero { (width - 6.0).min(80.0) } else { 33.0 };
    let image_rect = Rect::from_min_size(
        pos2(r.center().x - size * 0.5, r.top() + 2.0),
        vec2(size, size),
    );
    p.image(
        tex,
        image_rect,
        Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
        Color32::WHITE,
    );
    let color = if on { FG } else { FG2 };
    let mut job = egui::text::LayoutJob::simple(
        kind.name().to_owned(),
        FontId::proportional(11.5),
        color,
        width - 4.0,
    );
    job.wrap.max_rows = if hero { 2 } else { 1 };
    job.wrap.break_anywhere = true;
    let galley = p.layout_job(job);
    let text_rect = Rect::from_min_size(
        pos2(
            r.center().x - galley.size().x * 0.5,
            r.bottom() - galley.size().y - 2.0,
        ),
        galley.size(),
    );
    p.with_clip_rect(r.intersect(p.clip_rect()))
        .galley(text_rect.min, galley, color);
    resp.on_hover_text(format!("{}\n{}", kind.name(), kind.description()))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn app(name: &str) -> crate::App {
        let mut app = crate::App::new(8, false, 200_000);
        let root = std::env::temp_dir().join(format!("klay-020-ui-{}-{name}", std::process::id()));
        app.workspace.layout = DockLayout::default();
        app.workspace.saved = app.workspace.layout.encode();
        app.workspace.persistence_path = root.join("layout");
        app.workspace.brush_ui = BrushUi::new(root.join("assets"));
        app.workspace.assets = StampSettings::default();
        theme(&app.egui_ctx);
        app
    }
    fn frame(app: &mut crate::App, events: Vec<egui::Event>) -> egui::FullOutput {
        let ctx = app.egui_ctx.clone();
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1600.0, 1000.0))),
                events,
                focused: true,
                ..Default::default()
            },
            |ui| app.ui(ui),
        )
    }
    fn text(out: &egui::FullOutput, label: &str, predicate: impl Fn(Pos2) -> bool) -> Pos2 {
        out.shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.job.text == label && predicate(t.pos) => {
                    Some(t.pos)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing {label}"))
    }
    fn click(app: &mut crate::App, p: Pos2) -> egui::FullOutput {
        frame(app, vec![egui::Event::PointerMoved(p)]);
        frame(
            app,
            vec![egui::Event::PointerButton {
                pos: p,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        frame(
            app,
            vec![egui::Event::PointerButton {
                pos: p,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        )
    }
    #[test]
    fn space_menu_has_requested_order_live_controls_and_left_color_picker() {
        let mut a = app("space");
        a.workspace.open_quick_menu(pos2(900.0, 300.0), true);
        frame(&mut a, vec![]);
        let out = frame(&mut a, vec![]);
        let at = |label| text(&out, label, |p| p.x > 800.0 && p.y > 250.0);
        let (size, z, focal, rgb, brushes) = (
            at("Draw Size"),
            at("Z Intensity"),
            at("Focal Shift"),
            at("RGB Intensity"),
            at("Brushes"),
        );
        assert!(size.y < z.y && z.y < focal.y && focal.y < rgb.y && rgb.y < brushes.y);
        let hex = out
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.job.text.starts_with('#') && t.pos.y > 250.0 => {
                    Some(t.pos)
                }
                _ => None,
            })
            .unwrap();
        assert!(hex.x < size.x);
        click(&mut a, focal + vec2(235.0, 7.0));
        assert!(a.brush.focal_shift > 20.0);
        click(&mut a, rgb + vec2(80.0, 7.0));
        assert!(a.brush.rgb_intensity > 0.1 && a.brush.rgb_intensity < 0.6);
        let before = a.brush.color;
        click(&mut a, hex + vec2(2.0, -30.0));
        assert_ne!(a.brush.color, before);
        assert_eq!(a.brush.color, a.workspace.color.to_srgb());
    }
    #[test]
    fn catalog_selects_real_brushes_and_blocks_canvas_shortcuts() {
        use winit::keyboard::KeyCode;
        let mut a = app("catalog");
        a.on_key(KeyCode::KeyB);
        assert!(a.workspace.brush_catalog_open());
        let before = a.doc.mesh().positions.clone();
        a.on_key(KeyCode::KeyD);
        a.left_press(false, false);
        assert!(a.pending_level.is_none());
        assert!(a.stroke.is_none());
        assert!(a.nav.is_none());
        assert_eq!(a.doc.mesh().positions, before);
        frame(&mut a, vec![]);
        let out = frame(&mut a, vec![]);
        let at = text(&out, "Dam Standard", |p| p.x > 300.0);
        click(&mut a, at + vec2(5.0, 4.0));
        assert_eq!(a.brush.kind, BrushKind::DamStandard);
        assert!(!a.workspace.brush_catalog_open());
        a.workspace.select_brush(&mut a.brush, BrushKind::Paint);
        assert!(a.brush.paint && !a.brush.sculpt);
        a.on_key(KeyCode::Digit7);
        assert_eq!(a.brush.kind, BrushKind::Pinch);
        assert!(a.brush.sculpt && !a.brush.paint);
    }

    #[test]
    fn catalog_last_row_is_readable_at_minimum_window_size() {
        let mut a = app("catalog-minimum");
        a.workspace.open_brush_catalog(pos2(500.0, 300.0));
        a.workspace.brush_ui.search = "Trim".into();
        let ctx = a.egui_ctx.clone();
        let mut output = egui::FullOutput::default();
        for _ in 0..4 {
            output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(960.0, 640.0))),
                    focused: true,
                    ..Default::default()
                },
                |ui| a.ui(ui),
            );
        }
        assert!(
            output.shapes.iter().any(|s| match &s.shape {
                egui::Shape::Text(t)
                    if t.galley.job.text == "Trim Dynamic" && t.pos.x > 90.0 && t.pos.y > 100.0 =>
                {
                    s.clip_rect
                        .contains_rect(Rect::from_min_size(t.pos, t.galley.size()))
                        && t.pos.y + t.galley.size().y < 640.0
                }
                _ => false,
            }),
            "catalog's last brush label must not be clipped"
        );
    }
    #[test]
    fn alpha_and_texture_tiles_select_engine_assets() {
        let mut a = app("assets");
        frame(&mut a, vec![]);
        let out = frame(&mut a, vec![]);
        let p = text(&out, "Alpha", |p| p.y < 30.0);
        click(&mut a, p + vec2(4.0, 4.0));
        let out = frame(&mut a, vec![]);
        let p = text(&out, "Dots", |p| p.y > 30.0);
        click(&mut a, p + vec2(4.0, 4.0));
        assert_eq!(a.workspace.assets.alpha, Pattern::Dots);
        a.workspace.dropdown = None;
        let out = frame(&mut a, vec![]);
        let p = text(&out, "Texture", |p| p.y < 30.0);
        click(&mut a, p + vec2(4.0, 4.0));
        let out = frame(&mut a, vec![]);
        let p = text(&out, "Grain", |p| p.y > 30.0);
        click(&mut a, p + vec2(4.0, 4.0));
        assert_eq!(a.workspace.assets.texture, Pattern::Grain);
    }

    #[test]
    fn compact_palette_fits_working_set_and_selects_advanced_brushes() {
        let mut a = app("compact-palette");
        frame(&mut a, vec![]);
        let out = frame(&mut a, vec![]);
        let top = text(&out, "Brushes", |p| p.x < 200.0);
        let modifiers = text(&out, "Modifiers", |p| p.x < 200.0);
        assert!(
            modifiers.y - top.y < 550.0,
            "palette must keep modifiers within reach"
        );
        let snake = text(&out, "Snake Hook", |p| p.x < 200.0);
        click(&mut a, snake + vec2(6.0, 3.0));
        assert_eq!(a.brush.kind, BrushKind::SnakeHook);
        let out = frame(&mut a, vec![]);
        let button = text(
            &out,
            &format!("All Brushes ({})", BrushKind::ALL.len()),
            |p| p.x < 200.0,
        );
        click(&mut a, button + vec2(5.0, 3.0));
        assert!(a.workspace.brush_catalog_open());
    }

    #[test]
    fn mask_tab_selects_real_tools_and_catalog_search_finds_layer() {
        let mut a = app("mask-library");
        frame(&mut a, vec![]);
        let out = frame(&mut a, vec![]);
        let tab = text(&out, "Mask", |p| p.x < 200.0 && p.y > 140.0);
        click(&mut a, tab + vec2(4.0, 3.0));
        let out = frame(&mut a, vec![]);
        let rect = text(&out, "Mask Rect", |p| p.x < 200.0);
        click(&mut a, rect + vec2(4.0, 3.0));
        assert_eq!(a.brush.kind, BrushKind::MaskRect);
        assert_eq!(a.workspace.mask_brush, BrushKind::MaskRect);
        assert!(!a.brush.paint);
        a.workspace.select_brush(&mut a.brush, BrushKind::Draw);
        assert_eq!(a.workspace.mask_brush, BrushKind::MaskRect);
        assert!(a.brush.sculpt);
        a.workspace.open_brush_catalog(pos2(500.0, 100.0));
        a.workspace.brush_ui.search = "Layer".into();
        frame(&mut a, vec![]);
        let out = frame(&mut a, vec![]);
        let layer = text(&out, "Layer", |p| p.x > 300.0 && p.y > 200.0);
        click(&mut a, layer + vec2(4.0, 3.0));
        assert_eq!(a.brush.kind, BrushKind::Layer);
    }
}
