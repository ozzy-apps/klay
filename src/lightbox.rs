//! LightBox, the workspace's local asset browser.
use super::*;
use crate::{brush_asset::Pattern, brush_preset, model_thumbnail, primitive::Primitive};
use std::{
    path::{Path, PathBuf},
    sync::mpsc,
    time::SystemTime,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
enum Category {
    #[default]
    Projects,
    QuickSave,
    Tools,
    Brushes,
    Materials,
    Alphas,
    Textures,
}

impl Category {
    const ALL: [Self; 7] = [
        Self::Projects,
        Self::QuickSave,
        Self::Tools,
        Self::Brushes,
        Self::Materials,
        Self::Alphas,
        Self::Textures,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Projects => "Projects",
            Self::Tools => "Tools",
            Self::Brushes => "Brushes",
            Self::Materials => "Materials",
            Self::Alphas => "Alphas",
            Self::Textures => "Textures",
            Self::QuickSave => "QuickSave",
        }
    }

    fn accepts(self, path: &Path) -> bool {
        let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
            return false;
        };
        let ext = ext.to_ascii_lowercase();
        match self {
            Self::Projects | Self::QuickSave => ext == "klay",
            Self::Tools => model_thumbnail::is_model(path),
            Self::Materials | Self::Alphas | Self::Textures => {
                matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "bmp")
                    && !path
                        .file_stem()
                        .is_some_and(|stem| model_thumbnail::is_model(Path::new(stem)))
            }
            Self::Brushes => ext == brush_preset::EXTENSION,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct Location {
    user: bool,
    category: Category,
    relative: PathBuf,
}

#[derive(Clone)]
enum Entry {
    DefaultSphere,
    Primitive(Primitive),
    Brush(BrushKind),
    Material(usize),
    Pattern(Pattern),
    Folder(PathBuf),
    Up,
    File(LocalFile),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct FileRevision {
    modified: Option<SystemTime>,
    len: u64,
}

impl From<std::fs::Metadata> for FileRevision {
    fn from(metadata: std::fs::Metadata) -> Self {
        Self {
            modified: metadata.modified().ok(),
            len: metadata.len(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct LocalFile {
    path: PathBuf,
    revision: Option<FileRevision>,
    sidecar: Option<FileRevision>,
    brush: Option<(BrushKind, bool)>,
}

impl LocalFile {
    fn new(path: PathBuf, metadata: Option<std::fs::Metadata>) -> Self {
        let revision = metadata.map(FileRevision::from);
        let sidecar = model_thumbnail::is_model(&path)
            .then(|| sidecar_path(&path).metadata().ok().map(FileRevision::from))
            .flatten();
        let brush = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case(brush_preset::EXTENSION))
            .then(|| brush_preset::preview(&path))
            .flatten();
        Self {
            path,
            revision,
            sidecar,
            brush,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum PreviewSource {
    File(LocalFile, bool),
    Primitive(Primitive),
}

impl PreviewSource {
    fn is_model(&self) -> bool {
        match self {
            Self::File(file, _) => model_thumbnail::is_model(&file.path),
            Self::Primitive(_) => true,
        }
    }
}

impl Entry {
    fn label(&self) -> String {
        match self {
            Self::DefaultSphere => "DefaultSphere".into(),
            Self::Primitive(kind) => kind.name().into(),
            Self::Brush(kind) => kind.name().into(),
            Self::Material(i) => material_name(*i),
            Self::Pattern(pattern) => pattern.name().into(),
            Self::Up => "Parent Folder".into(),
            Self::File(LocalFile { path, .. })
                if path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case(brush_preset::EXTENSION)) =>
            {
                path.file_stem()
                    .unwrap_or(path.as_os_str())
                    .to_string_lossy()
                    .into_owned()
            }
            Self::Folder(path) | Self::File(LocalFile { path, .. }) => path
                .file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy()
                .into_owned(),
        }
    }
}

/// File and scene operations run through the app's existing loaders.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    ChooseFolder,
    NewSphere,
    Primitive(Primitive),
    Project { path: PathBuf, recovery: bool },
    Tool(PathBuf),
    Mesh(PathBuf),
    MatCap(PathBuf),
    Spotlight(PathBuf),
}

type Thumbnail = (ColorImage, Vec2);
type ThumbnailRequest = (PreviewSource, mpsc::Receiver<Option<Thumbnail>>);

#[derive(Default)]
pub(super) struct Browser {
    pub(super) open: bool,
    pub(super) folder: PathBuf,
    quicksave: PathBuf,
    folder_config: PathBuf,
    location: Location,
    back: Vec<Location>,
    forward: Vec<Location>,
    files: Vec<Entry>,
    refresh: bool,
    read_error: Option<String>,
    previews: HashMap<PreviewSource, Option<(TextureHandle, Vec2)>>,
    requests: Vec<ThumbnailRequest>,
    selected: Option<usize>,
    replace: Option<Entry>,
}

impl Browser {
    pub(super) fn new(config_root: &Path) -> Self {
        #[cfg(not(test))]
        let quicksave = crate::App::quicksave_dir();
        #[cfg(test)]
        let quicksave = config_root.join("quicksave");
        let folder_config = config_root.join("klay/lightbox.folder");
        let folder = std::fs::read_to_string(&folder_config)
            .ok()
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| quicksave.with_file_name("lightbox"));
        Self {
            quicksave,
            folder,
            folder_config,
            refresh: true,
            ..Self::default()
        }
    }

    pub(super) fn set_folder(&mut self, folder: PathBuf) -> std::io::Result<()> {
        if !folder.is_dir() || !folder.is_absolute() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Choose an existing folder",
            ));
        }
        let Some(text) = folder.to_str() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Folder path must be UTF-8",
            ));
        };
        if let Some(parent) = self.folder_config.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temp = self.folder_config.with_extension("saving");
        std::fs::write(&temp, text)?;
        std::fs::rename(&temp, &self.folder_config)?;
        self.folder = folder;
        self.location.user = true;
        if self.location.category == Category::QuickSave {
            self.location.category = Category::Projects;
        }
        self.location.relative.clear();
        self.back.clear();
        self.forward.clear();
        self.replace = None;
        self.refresh = true;
        Ok(())
    }

    fn go(&mut self, next: Location) {
        if next != self.location {
            self.back.push(std::mem::replace(&mut self.location, next));
            self.forward.clear();
            self.replace = None;
            self.refresh = true;
        }
    }

    fn root(&self) -> PathBuf {
        if self.location.category == Category::QuickSave {
            return self.quicksave.clone();
        }
        let category = self.folder.join(self.location.category.name());
        if category.is_dir() {
            category
        } else {
            self.folder.clone()
        }
    }

    pub(super) fn brush_folder(&self) -> PathBuf {
        self.folder.join(Category::Brushes.name())
    }

    fn scan(&mut self) {
        self.refresh = false;
        self.files.clear();
        self.selected = None;
        self.read_error = None;
        let category = self.location.category;
        if !self.location.user && category != Category::QuickSave {
            return;
        }
        let directory = self.root().join(&self.location.relative);
        if !self.location.relative.as_os_str().is_empty() {
            self.files.push(Entry::Up);
        }
        let listing = match std::fs::read_dir(&directory) {
            Ok(listing) => listing,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                self.read_error = Some(format!("Cannot read folder: {e}"));
                return;
            }
        };
        let mut items: Vec<_> = listing
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().to_lowercase();
                if name.starts_with('.') {
                    return None;
                }
                let kind = entry.file_type().ok()?;
                let path = entry.path();
                if kind.is_dir() && category != Category::QuickSave {
                    Some((false, name, Entry::Folder(path)))
                } else if kind.is_file() && category.accepts(&path) {
                    Some((
                        true,
                        name,
                        Entry::File(LocalFile::new(path, entry.metadata().ok())),
                    ))
                } else {
                    None
                }
            })
            .collect();
        if category == Category::QuickSave {
            items.sort_by_cached_key(|(_, _, entry)| match entry {
                Entry::File(file) => {
                    std::cmp::Reverse(file.revision.as_ref().and_then(|r| r.modified))
                }
                _ => std::cmp::Reverse(None),
            });
        } else {
            items.sort_unstable_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
        }
        self.files
            .extend(items.into_iter().map(|(_, _, item)| item));
    }

    fn entries(&self, ws: &Workspace) -> Vec<Entry> {
        let category = self.location.category;
        if category == Category::QuickSave || !self.location.relative.as_os_str().is_empty() {
            return self.files.clone();
        }
        if self.location.user {
            let mut entries = self.files.clone();
            match category {
                Category::Alphas | Category::Textures => {
                    entries.extend(
                        ws.lightbox_patterns(category == Category::Textures, true)
                            .into_iter()
                            .filter(|p| {
                                let Pattern::Image(image) = p else {
                                    return false;
                                };
                                !self.files.iter().any(
                                    |e| matches!(e, Entry::File(file) if image.source.as_ref() == Some(&file.path)),
                                )
                            })
                            .map(Entry::Pattern),
                    );
                }
                Category::Materials => entries.extend(
                    (MATERIALS.len()..crate::matcap::material_count()).map(Entry::Material),
                ),
                _ => {}
            }
            return entries;
        }
        match category {
            Category::Projects => vec![Entry::DefaultSphere],
            Category::Tools => Primitive::ALL.into_iter().map(Entry::Primitive).collect(),
            Category::Brushes => {
                let mut brushes = BrushKind::ALL;
                brushes.sort_unstable_by_key(|k| k.name());
                brushes.into_iter().map(Entry::Brush).collect()
            }
            Category::Materials => (0..MATERIALS.len()).map(Entry::Material).collect(),
            Category::Alphas | Category::Textures => ws
                .lightbox_patterns(category == Category::Textures, false)
                .into_iter()
                .map(Entry::Pattern)
                .collect(),
            Category::QuickSave => unreachable!(),
        }
    }

    fn thumbnail(&mut self, ctx: &Context, source: PreviewSource) -> Option<(TextureId, Vec2)> {
        if let Some(preview) = self.previews.get(&source) {
            return preview.as_ref().map(|(handle, size)| (handle.id(), *size));
        }
        // A project may contain millions of vertices. Only one model is decoded at a time.
        if self.requests.len() < 8
            && !self
                .requests
                .iter()
                .any(|(pending, _)| *pending == source || (source.is_model() && pending.is_model()))
        {
            let (send, receive) = mpsc::channel();
            self.requests.push((source.clone(), receive));
            let ctx = ctx.clone();
            rayon::spawn(move || {
                let _ = send.send(read_thumbnail(&source));
                ctx.request_repaint();
            });
        }
        None
    }

    fn poll_thumbnails(&mut self, ctx: &Context) {
        if self.previews.len() >= 256 {
            self.previews.clear();
        }
        self.requests.retain(|(path, rx)| match rx.try_recv() {
            Ok(preview) => {
                let preview = preview.map(|(image, size)| {
                    (
                        ctx.load_texture("lightbox thumbnail", image, TextureOptions::LINEAR),
                        size,
                    )
                });
                self.previews.insert(path.clone(), preview);
                false
            }
            Err(mpsc::TryRecvError::Disconnected) => false,
            Err(mpsc::TryRecvError::Empty) => true,
        });
    }

    fn tile(
        &mut self,
        ui: &mut Ui,
        ws: &mut Workspace,
        entry: &Entry,
        rect: Rect,
        index: usize,
    ) -> egui::Response {
        let label = entry.label();
        let id = Id::unique("lightbox-tile").with((&self.location, index));
        let response = ui.interact(rect, id, Sense::click()).on_hover_text(&label);
        let selected = self.selected == Some(index);
        let painter = ui.painter();
        painter.rect_filled(
            rect,
            4,
            if selected {
                ACC_DIM()
            } else if response.hovered() {
                BG3()
            } else {
                TILE()
            },
        );
        painter.rect_stroke(
            rect,
            4,
            Stroke::new(
                1.0,
                if selected || response.hovered() {
                    ACC()
                } else {
                    LINE()
                },
            ),
            StrokeKind::Inside,
        );
        let image = Rect::from_min_max(rect.min + vec2(8.0, 5.0), rect.max - vec2(8.0, 24.0));
        let texture = match entry {
            Entry::DefaultSphere | Entry::Primitive(Primitive::Sphere) => {
                Some((ws.material_texture(ui.ctx(), 0), Vec2::ONE))
            }
            Entry::Primitive(kind) => self.thumbnail(ui.ctx(), PreviewSource::Primitive(*kind)),
            Entry::Brush(kind) => Some((ws.texture(ui.ctx(), Some(*kind), ws.subtract), Vec2::ONE)),
            Entry::Material(i) => Some((ws.material_texture(ui.ctx(), *i), Vec2::ONE)),
            Entry::Pattern(pattern) if *pattern != Pattern::Off => Some((
                ws.pattern_texture(
                    ui.ctx(),
                    pattern,
                    self.location.category == Category::Textures,
                ),
                Vec2::ONE,
            )),
            Entry::File(file) if self.location.category == Category::Brushes => file
                .brush
                .map(|(kind, subtract)| (ws.texture(ui.ctx(), Some(kind), subtract), Vec2::ONE)),
            Entry::File(file) => {
                let stl_z_up = ws.layout.print.z_up
                    && file
                        .path
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("stl"));
                self.thumbnail(ui.ctx(), PreviewSource::File(file.clone(), stl_z_up))
            }
            _ => None,
        };
        if let Some((texture, size)) = texture {
            let scale = (image.width() / size.x).min(image.height() / size.y);
            let fitted = Rect::from_center_size(image.center(), size * scale);
            painter.image(
                texture,
                fitted,
                Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        } else {
            let badge = match entry {
                Entry::Primitive(kind) => match kind {
                    Primitive::Sphere => "●",
                    Primitive::Cube => "■",
                    Primitive::Cylinder => "▥",
                    Primitive::Ring => "◎",
                    Primitive::Plane => "◇",
                }
                .into(),
                Entry::Folder(_) => "▱".into(),
                Entry::Up => "↑".into(),
                Entry::File(file) => file
                    .path
                    .extension()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_uppercase(),
                _ => "—".into(),
            };
            painter.text(
                image.center(),
                Align2::CENTER_CENTER,
                badge,
                font(22.0, Weight::SemiBold),
                FG2(),
            );
        }
        painter.with_clip_rect(rect.shrink(5.0)).text(
            rect.center_bottom() - vec2(0.0, 11.0),
            Align2::CENTER_CENTER,
            label,
            font(11.5, Weight::Medium),
            FG(),
        );
        response
    }

    fn activate(
        &mut self,
        ws: &mut Workspace,
        model: &mut Model<'_>,
        actions: &mut Actions,
        entry: Entry,
    ) {
        let category = self.location.category;
        match entry {
            Entry::Up => {
                let mut next = self.location.clone();
                next.relative.pop();
                self.go(next);
                return;
            }
            Entry::Folder(path) => {
                let mut next = self.location.clone();
                if let Ok(relative) = path.strip_prefix(self.root()) {
                    next.relative = relative.to_path_buf();
                    self.go(next);
                }
                return;
            }
            Entry::DefaultSphere => actions.lightbox = Some(Action::NewSphere),
            Entry::Primitive(kind) => actions.lightbox = Some(Action::Primitive(kind)),
            Entry::Brush(kind) => ws.select_brush(model.brush, kind),
            Entry::Material(i) => {
                ws.material = i;
                ws.prerender_previews();
            }
            Entry::Pattern(pattern) => {
                if let Pattern::Image(image) = &pattern
                    && let Some(path) = &image.source
                    && ws.spotlight.as_ref().is_some_and(|s| s.shown)
                {
                    actions.lightbox = Some(Action::Spotlight(path.clone()));
                    self.open = false;
                    return;
                }
                if category == Category::Textures {
                    ws.assets.texture = pattern;
                } else {
                    ws.assets.alpha = pattern;
                }
            }
            Entry::File(file) => {
                let path = file.path;
                actions.lightbox = match category {
                    Category::Projects | Category::QuickSave => Some(Action::Project {
                        path,
                        recovery: category == Category::QuickSave,
                    }),
                    Category::Tools
                        if path
                            .extension()
                            .is_some_and(|e| e.eq_ignore_ascii_case("klay")) =>
                    {
                        Some(Action::Tool(path))
                    }
                    Category::Tools => Some(Action::Mesh(path)),
                    Category::Materials => Some(Action::MatCap(path)),
                    Category::Alphas | Category::Textures => {
                        if ws.spotlight.as_ref().is_some_and(|s| s.shown) {
                            Some(Action::Spotlight(path))
                        } else {
                            if let Err(error) =
                                ws.load_lightbox_pattern(&path, category == Category::Textures)
                            {
                                self.read_error = Some(error);
                                return;
                            }
                            None
                        }
                    }
                    Category::Brushes => {
                        if let Err(error) = ws.load_brush_preset(&path, model.brush) {
                            self.read_error = Some(format!("Could not load brush: {error}"));
                            return;
                        }
                        None
                    }
                };
            }
        }
        self.open = false;
    }

    pub(super) fn show(
        &mut self,
        ctx: &Context,
        ws: &mut Workspace,
        model: &mut Model<'_>,
        actions: &mut Actions,
    ) {
        self.poll_thumbnails(ctx);
        let width = (ctx.content_rect().width() - 44.0).max(250.0);
        let id = Id::unique("lightbox");
        let area = egui::Modal::default_area(id).anchor(Align2::CENTER_TOP, vec2(0.0, 38.0));
        let frame = popup_frame().inner_margin(10);
        let response = egui::Modal::new(id)
            .area(area)
            .frame(frame)
            .backdrop_color(Color32::from_black_alpha(85))
            .show(ctx, |ui| {
                ui.set_width(width);
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new("LightBox")
                            .font(font(15.0, Weight::SemiBold))
                            .color(FG()),
                    );
                    ui.add_space(8.0);
                    for (user, label) in [(false, "Klay"), (true, "User")] {
                        if ui
                            .selectable_label(self.location.user == user, label)
                            .clicked()
                        {
                            let category = if self.location.category == Category::QuickSave {
                                Category::Projects
                            } else {
                                self.location.category
                            };
                            self.go(Location {
                                user,
                                category,
                                relative: PathBuf::new(),
                            });
                        }
                    }
                    ui.separator();
                    if ui
                        .button("Folder…")
                        .on_hover_text(self.folder.display().to_string())
                        .clicked()
                    {
                        actions.lightbox = Some(Action::ChooseFolder);
                    }
                    if ui.button("Refresh").clicked() {
                        self.refresh = true;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Close").on_hover_text("Comma · Esc").clicked() {
                            self.open = false;
                        }
                    });
                });
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(!self.back.is_empty(), egui::Button::new("‹"))
                        .on_hover_text("Back")
                        .clicked()
                    {
                        let previous = self.back.pop().unwrap();
                        self.forward
                            .push(std::mem::replace(&mut self.location, previous));
                        self.replace = None;
                        self.refresh = true;
                    }
                    if ui
                        .add_enabled(!self.forward.is_empty(), egui::Button::new("›"))
                        .on_hover_text("Forward")
                        .clicked()
                    {
                        let next = self.forward.pop().unwrap();
                        self.back.push(std::mem::replace(&mut self.location, next));
                        self.replace = None;
                        self.refresh = true;
                    }
                    for category in Category::ALL {
                        if ui
                            .selectable_label(category == self.location.category, category.name())
                            .clicked()
                        {
                            self.go(Location {
                                category,
                                relative: PathBuf::new(),
                                ..self.location.clone()
                            });
                        }
                    }
                    ui.separator();
                    ui.label("Rows");
                    for count in 1..=4 {
                        if ui
                            .selectable_label(ws.layout.lightbox_rows == count, count.to_string())
                            .clicked()
                        {
                            ws.layout.lightbox_rows = count;
                        }
                    }
                });
                ui.separator();
                if self.refresh {
                    self.scan();
                    self.files = self.entries(ws);
                }
                let category = self.location.category;
                if self.location.user || category == Category::QuickSave {
                    let path = self
                        .root()
                        .join(&self.location.relative)
                        .display()
                        .to_string();
                    ui.add(
                        egui::Label::new(RichText::new(&path).size(11.5).color(FG2())).truncate(),
                    )
                    .on_hover_text(path);
                }
                if let Some(entry) = self.replace.clone() {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(format!(
                            "Open {} and replace the current project?",
                            entry.label()
                        ));
                        if ui.button("Open").clicked() {
                            self.replace = None;
                            self.activate(ws, model, actions, entry);
                        }
                        if ui.button("Cancel").clicked() {
                            self.replace = None;
                        }
                    });
                    return;
                }
                if let Some(error) = &self.read_error {
                    ui.label(error);
                }
                if self.files.is_empty() {
                    ui.label(if category == Category::QuickSave {
                        "No QuickSaves"
                    } else {
                        "No assets in this folder"
                    });
                    return;
                }
                let entries = std::mem::take(&mut self.files);
                const TILE_WIDTH: f32 = 104.0;
                const TILE_HEIGHT: f32 = 108.0;
                const GAP: f32 = 7.0;
                let available = ctx.content_rect().bottom() - ui.next_widget_position().y - 32.0;
                let fit = ((available + GAP) / (TILE_HEIGHT + GAP)).floor().max(1.0) as usize;
                let rows = usize::from(ws.layout.lightbox_rows).min(fit);
                let columns = entries.len().div_ceil(rows);
                let height = rows.min(entries.len()) as f32 * (TILE_HEIGHT + GAP) - GAP;
                let mut chosen = if ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                    self.selected.and_then(|index| entries.get(index)).cloned()
                } else {
                    None
                };
                ScrollArea::horizontal()
                    .id_salt((&self.location, rows))
                    .scroll_source(egui::scroll_area::ScrollSource::ALL)
                    .show_viewport(ui, |ui, viewport| {
                        let step = TILE_WIDTH + GAP;
                        let size = vec2(columns as f32 * step - GAP, height);
                        let (_, bounds) = ui.allocate_space(size);
                        let first = (viewport.left() / step).floor().max(0.0) as usize;
                        let last = ((viewport.right() / step).ceil() as usize).min(columns);
                        for column in first..last {
                            for row in 0..rows {
                                let index = column * rows + row;
                                let Some(entry) = entries.get(index) else {
                                    break;
                                };
                                let tile = Rect::from_min_size(
                                    bounds.min
                                        + vec2(
                                            column as f32 * step,
                                            row as f32 * (TILE_HEIGHT + GAP),
                                        ),
                                    vec2(TILE_WIDTH, TILE_HEIGHT),
                                );
                                let response = self.tile(ui, ws, entry, tile, index);
                                if response.clicked() {
                                    if self.selected == Some(index)
                                        && (response.double_clicked() || response.triple_clicked())
                                    {
                                        chosen = Some(entry.clone());
                                    }
                                    self.selected = Some(index);
                                }
                            }
                        }
                    });
                self.files = entries;
                if let Some(entry) = chosen {
                    let replaces = matches!(entry, Entry::DefaultSphere)
                        || (matches!(entry, Entry::File(_))
                            && matches!(category, Category::Projects | Category::QuickSave));
                    if replaces && (model.can_undo || model.can_redo || model.subtools.len() > 1) {
                        self.replace = Some(entry);
                    } else {
                        self.activate(ws, model, actions, entry);
                    }
                }
            });
        if response.should_close() {
            if self.replace.is_some() {
                self.replace = None;
            } else {
                self.open = false;
            }
        }
        if !self.open {
            self.replace = None;
        }
    }

    pub(super) fn toggle(&mut self) {
        self.open = !self.open;
        self.replace = None;
        self.refresh = self.open;
    }

    pub(super) fn refresh_quicksaves(&mut self) {
        if self.location.category == Category::QuickSave {
            self.refresh = true;
        }
    }

    pub(super) fn open_category(&mut self, name: &str) {
        let (user, name) = name
            .strip_prefix("User ")
            .map_or((false, name), |name| (true, name));
        if let Some(category) = Category::ALL
            .into_iter()
            .find(|c| c.name().eq_ignore_ascii_case(name))
        {
            self.go(Location {
                category,
                user,
                ..Location::default()
            });
        }
    }
}

fn sidecar_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".png");
    PathBuf::from(name)
}

fn read_thumbnail(source: &PreviewSource) -> Option<Thumbnail> {
    let image = match source {
        PreviewSource::Primitive(kind) => model_thumbnail::primitive(*kind)?,
        PreviewSource::File(file, stl_z_up) if model_thumbnail::is_model(&file.path) => {
            let current_sidecar = file.sidecar.as_ref().is_some_and(|sidecar| {
                sidecar.modified >= file.revision.as_ref().and_then(|r| r.modified)
            });
            if current_sidecar
                && let Some(preview) = read_image_thumbnail(&sidecar_path(&file.path))
            {
                return Some(preview);
            }
            model_thumbnail::from_file(&file.path, *stl_z_up)?
        }
        PreviewSource::File(file, _) => return read_image_thumbnail(&file.path),
    };
    let size = vec2(image.size[0] as f32, image.size[1] as f32);
    Some((image, size))
}

fn read_image_thumbnail(path: &Path) -> Option<Thumbnail> {
    if path.metadata().ok()?.len() > 64 << 20 {
        return None;
    }
    let mut reader = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 << 20);
    reader.limits(limits);
    let image = reader.decode().ok()?.thumbnail(128, 128).into_rgba8();
    let size = vec2(image.width() as f32, image.height() as f32);
    let pixels =
        ColorImage::from_rgba_unmultiplied([size.x as usize, size.y as usize], image.as_raw());
    Some((pixels, size))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sculpt::StampSettings;
    use winit::keyboard::KeyCode;

    fn frame(app: &mut crate::App, events: Vec<egui::Event>) -> egui::FullOutput {
        let ctx = app.egui_ctx.clone();
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1400.0, 840.0))),
                focused: true,
                events,
                ..Default::default()
            },
            |ui| app.ui(ui),
        )
    }

    fn label(output: &egui::FullOutput, name: &str) -> Pos2 {
        output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == name => Some(text.pos),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing LightBox label {name}"))
    }

    fn click(app: &mut crate::App, pos: Pos2) -> egui::FullOutput {
        frame(app, vec![egui::Event::PointerMoved(pos)]);
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(app, vec![button(true)]);
        frame(app, vec![button(false)])
    }

    #[test]
    fn comma_and_double_click_select_an_asset_without_canvas_input() {
        let mut app = crate::App::new(8, false, 200_000);
        frame(&mut app, vec![]);
        app.on_key(KeyCode::Comma);
        assert!(app.workspace.lightbox_open());
        app.on_key(KeyCode::Comma);
        assert!(!app.workspace.lightbox_open());
        app.on_key(KeyCode::Comma);
        app.left_press(false, false);
        assert!(app.stroke.is_none());
        assert!(app.nav.is_none());

        frame(&mut app, vec![]);
        let output = frame(&mut app, vec![]);
        let materials = label(&output, "Materials");
        click(&mut app, materials + vec2(6.0, 6.0));
        let output = frame(&mut app, vec![]);
        let clay = label(&output, "Cool clay");
        let tile = clay + vec2(18.0, -30.0);
        click(&mut app, tile);
        assert_eq!(app.workspace.lightbox.selected, Some(1), "tile at {tile:?}");
        assert_eq!(
            app.workspace.material, 0,
            "single click leaves the material selected"
        );
        click(&mut app, tile);
        assert_eq!(app.workspace.material, 1);
        assert!(!app.workspace.lightbox_open());
    }

    #[test]
    fn user_folders_filter_assets_and_quicksave_opens_for_save_as() {
        let root = std::env::temp_dir().join(format!("klay-lightbox-{}", std::process::id()));
        let folder = root.join("assets");
        let projects = folder.join("Projects");
        std::fs::create_dir_all(projects.join("Studies")).unwrap();
        let mut app = crate::App::new(8, false, 200_000);
        let backup = projects.join("Head.KLAY");
        std::fs::write(&backup, app.project_bytes()).unwrap();
        std::fs::write(projects.join("Head.KLAY.png"), []).unwrap();
        std::fs::write(projects.join(".hidden.klay"), []).unwrap();
        std::fs::write(projects.join("Head.obj"), []).unwrap();

        let mut browser = Browser::new(&root);
        browser.set_folder(folder.clone()).unwrap();
        browser.scan();
        let names: Vec<_> = browser.files.iter().map(Entry::label).collect();
        assert_eq!(names, ["Studies", "Head.KLAY"]);
        let mut next = browser.location.clone();
        next.relative = PathBuf::from("Studies");
        browser.go(next);
        browser.scan();
        assert!(matches!(&browser.files[..], [Entry::Up]));
        assert_eq!(Browser::new(&root).folder, folder);

        app.apply_lightbox(Action::Project {
            path: backup,
            recovery: true,
        });
        assert!(!app.project_has_file);
        assert_eq!(app.project_path, "untitled.klay");
        assert!(app.status.starts_with("Opened "));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn custom_brush_loads_from_user_brushes_without_the_original_images() {
        let root = std::env::temp_dir().join(format!("klay-preset-{}", std::process::id()));
        let folder = root.join("assets");
        std::fs::create_dir_all(&folder).unwrap();
        let new_app = || {
            let mut app = crate::App::new(8, false, 200_000);
            app.workspace.lightbox = Browser::new(&root);
            app.workspace.brush_ui = brush_ui::BrushUi::new(root.join("brush-assets.v1"));
            app.workspace.assets = StampSettings::default();
            app.workspace.layout = DockLayout::default();
            app.workspace.saved = app.workspace.layout.encode();
            app.workspace.persistence_path = root.join("layout");
            app
        };
        let alpha = root.join("Asymmetric Pores.png");
        let texture = root.join("Skin Grain.png");
        image::RgbaImage::from_fn(3, 2, |x, y| {
            image::Rgba([40 * x as u8, 150 * y as u8, 63, 255])
        })
        .save(&alpha)
        .unwrap();
        image::RgbaImage::from_pixel(2, 1, image::Rgba([127, 91, 213, 190]))
            .save(&texture)
            .unwrap();
        let mut app = new_app();
        let kind = BrushKind::DamStandard;
        let ws = &mut app.workspace;
        ws.set_lightbox_folder(folder.clone());
        ws.select_brush(&mut app.brush, kind);
        ws.load_lightbox_pattern(&alpha, false).unwrap();
        ws.load_lightbox_pattern(&texture, true).unwrap();
        ws.assets.turn = crate::sculpt::AlphaTurn::Random;
        ws.assets.angle = 25.0;
        ws.assets.texture_angle = -17.0;
        ws.assets.texture_strength = 0.7;
        ws.assets.texture_scale = 2.0;
        ws.assets.wrap = 0.35;
        ws.assets.auto.topological = true;
        ws.set_stroke_mode(kind, "DragRect");
        ws.color = PickerColor::from_srgb([133, 42, 15]);
        ws.color2 = PickerColor::from_srgb([12, 199, 90]);
        ws.spacing = 0.35;
        ws.lazy_mouse = true;
        ws.lazy_radius = 95.0;
        ws.dynamic = true;
        ws.subtract = true;
        let mut pressure = ws.tablet(kind);
        pressure.size = false;
        ws.brush_pressure.insert(kind, pressure);
        app.brush.paint = true;
        app.brush.sculpt = true;
        app.brush.radius = 0.6;
        app.brush.strength = 0.75;
        app.brush.focal_shift = 20.0;
        app.brush.rgb_intensity = 0.42;

        ws.open_save_brush(kind);
        frame(&mut app, vec![]);
        let modifiers = egui::Modifiers::COMMAND;
        frame(
            &mut app,
            vec![
                egui::Event::Key {
                    key: egui::Key::A,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                },
                egui::Event::Text("Skin Stamp".into()),
            ],
        );
        let output = frame(&mut app, vec![]);
        let cancel = label(&output, "Cancel");
        let save = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text)
                    if text.galley.job.text == "Save"
                        && (text.pos.y - cancel.y).abs() < 2.0
                        && text.pos.x > cancel.x =>
                {
                    Some(text.pos)
                }
                _ => None,
            })
            .unwrap();
        click(&mut app, save + vec2(5.0, 5.0));
        let path = folder.join("Brushes/Skin Stamp.klaybrush");
        assert_eq!(app.workspace.brush_display_name(kind), "Skin Stamp");
        assert!(!app.workspace.save_brush_open());
        let original = std::fs::read(&path).unwrap();
        let duplicate = brush_preset::save(&path, &app.workspace.brush_preset(app.brush), false);
        assert_eq!(
            duplicate.unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
        drop(app);
        std::fs::remove_file(alpha).unwrap();
        std::fs::remove_file(texture).unwrap();

        let mut app = new_app();
        frame(&mut app, vec![]);
        assert_eq!(app.workspace.lightbox.folder, folder);
        app.on_key(KeyCode::Comma);
        frame(&mut app, vec![]);
        let output = frame(&mut app, vec![]);
        let user = label(&output, "User");
        let output = click(&mut app, user + vec2(5.0, 5.0));
        let brushes = label(&output, "Brushes");
        click(&mut app, brushes + vec2(5.0, 5.0));
        let output = frame(&mut app, vec![]);
        let tile = label(&output, "Skin Stamp") + vec2(18.0, -30.0);
        click(&mut app, tile);
        click(&mut app, tile);
        frame(&mut app, vec![]);
        let ws = &app.workspace;
        assert!(!ws.lightbox_open());
        assert_eq!(ws.brush_display_name(kind), "Skin Stamp");
        assert_eq!(app.brush.kind, kind);
        assert!(app.brush.paint && app.brush.sculpt);
        assert_eq!(ws.stroke_mode(kind), "DragRect");
        assert_eq!(ws.assets.turn.name(), "Random");
        assert_eq!(ws.assets.wrap, 0.35, "Wrap is saved with the brush");
        assert!(matches!(&ws.assets.alpha, Pattern::Image(image) if image.source.is_none()));
        assert!(matches!(&ws.assets.texture, Pattern::Image(image) if image.source.is_none()));

        // The loaded live settings and embedded pixels round-trip byte for byte.
        let round_trip = root.join("round-trip.klaybrush");
        brush_preset::save(&round_trip, &ws.brush_preset(app.brush), false).unwrap();
        assert_eq!(std::fs::read(&round_trip).unwrap(), original);
        std::fs::write(&round_trip, &original[..original.len() - 1]).unwrap();
        assert!(
            app.workspace
                .load_brush_preset(&round_trip, &mut app.brush)
                .is_err()
        );
        assert_eq!(app.workspace.brush_display_name(kind), "Skin Stamp");
        std::fs::remove_dir_all(root).unwrap();
    }
}
