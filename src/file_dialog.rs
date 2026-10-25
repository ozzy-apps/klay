//! The desktop's own Open and Save dialogs (through the XDG desktop portal, or zenity when
//! there is no portal). A dialog runs on its own thread so the canvas keeps drawing; the app
//! asks [`Pending::answer`] once a frame.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

/// What a dialog is picking a file for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// Document › Open, Ctrl + O.
    OpenProject,
    /// Document › Save As, and Save before the project has a file.
    SaveProject,
    /// SubTool › Save.
    SaveSubtool,
    /// SubTool › Load (several files at once).
    LoadSubtools,
    /// SubTool › Export OBJ.
    ExportSubtoolObj,
    /// Document › Export OBJ.
    ExportObj,
    /// Document › Import OBJ (several files at once).
    ImportObj,
}

impl Ask {
    pub fn title(self) -> &'static str {
        match self {
            Ask::OpenProject => "Open Project",
            Ask::SaveProject => "Save Project As",
            Ask::SaveSubtool => "Save SubTool",
            Ask::LoadSubtools => "Load SubTools",
            Ask::ExportSubtoolObj => "Export SubTool as OBJ",
            Ask::ExportObj => "Export OBJ",
            Ask::ImportObj => "Import OBJ",
        }
    }

    /// The file extension this dialog is for, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            Ask::OpenProject | Ask::SaveProject | Ask::SaveSubtool | Ask::LoadSubtools => "klay",
            Ask::ExportSubtoolObj | Ask::ExportObj | Ask::ImportObj => "obj",
        }
    }

    pub fn saves(self) -> bool {
        matches!(self, Ask::SaveProject | Ask::SaveSubtool | Ask::ExportSubtoolObj | Ask::ExportObj)
    }

    fn many(self) -> bool {
        matches!(self, Ask::LoadSubtools | Ask::ImportObj)
    }
}

/// A dialog that is open.
pub struct Pending {
    pub ask: Ask,
    rx: mpsc::Receiver<Vec<PathBuf>>,
}

impl Pending {
    /// `Some` once the dialog has closed: the picked files, empty when cancelled.
    pub fn answer(&self) -> Option<Vec<PathBuf>> {
        match self.rx.try_recv() {
            Ok(paths) => Some(paths),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Vec::new()),
        }
    }
}

/// Open the dialog for `ask` in `dir`, offering `file_name` when it saves, on top of `parent`.
pub fn open(ask: Ask, dir: &Path, file_name: &str, parent: Option<&winit::window::Window>) -> Pending {
    let ext = ask.extension();
    let kind = if ext == "klay" { "Klay project" } else { "Wavefront OBJ" };
    let mut dialog = rfd::FileDialog::new().set_title(ask.title()).set_directory(dir).add_filter(kind, &[ext]);
    if ask.saves() {
        dialog = dialog.set_file_name(file_name).set_can_create_directories(true);
    } else {
        dialog = dialog.add_filter("All files", &["*"]);
    }
    if let Some(window) = parent {
        dialog = dialog.set_parent(window);
    }
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("file dialog".into())
        .spawn(move || {
            let paths = if ask.saves() {
                dialog.save_file().map(|p| vec![with_extension(p, ext)])
            } else if ask.many() {
                dialog.pick_files()
            } else {
                dialog.pick_file().map(|p| vec![p])
            };
            let _ = tx.send(paths.unwrap_or_default());
        })
        .expect("starting the file dialog thread");
    Pending { ask, rx }
}

/// `path` with `.ext` added when it has another extension or none (a saved name typed
/// without one still opens as the right kind of file).
pub fn with_extension(path: PathBuf, ext: &str) -> PathBuf {
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext)) {
        return path;
    }
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(ext);
    path.with_file_name(name)
}

/// Where a dialog for `path` starts: the folder `path` is in when it exists, else
/// `fallback` (the folder of the last file picked), else the working folder.
pub fn start_dir(path: &str, fallback: Option<&Path>) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let parent = Path::new(path).parent().filter(|p| !p.as_os_str().is_empty()).map(|p| cwd.join(p));
    parent
        .filter(|p| p.is_dir())
        .or_else(|| fallback.filter(|p| p.is_dir()).map(Path::to_path_buf))
        .unwrap_or(cwd)
}

/// `name` as a file name: path separators and other characters file systems refuse become
/// `_`, and an empty name becomes `untitled`.
pub fn file_name(name: &str, ext: &str) -> String {
    let clean: String = name
        .trim()
        .chars()
        .map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { '_' } else { c })
        .collect();
    let stem = if clean.is_empty() || clean.chars().all(|c| c == '.') { "untitled".to_string() } else { clean };
    format!("{stem}.{ext}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_files_get_their_extension() {
        assert_eq!(with_extension("/a/head".into(), "klay"), PathBuf::from("/a/head.klay"));
        assert_eq!(with_extension("/a/head.KLAY".into(), "klay"), PathBuf::from("/a/head.KLAY"));
        assert_eq!(with_extension("/a/head.v2".into(), "klay"), PathBuf::from("/a/head.v2.klay"));
        assert_eq!(with_extension("/a/head.obj".into(), "obj"), PathBuf::from("/a/head.obj"));
    }

    #[test]
    fn subtool_names_become_file_names() {
        assert_eq!(file_name("Sphere 1", "klay"), "Sphere 1.klay");
        assert_eq!(file_name("arm/left: v2", "obj"), "arm_left_ v2.obj");
        assert_eq!(file_name("  ", "klay"), "untitled.klay");
        assert_eq!(file_name("..", "klay"), "untitled.klay");
    }

    #[test]
    fn dialogs_start_in_the_file_folder_then_the_last_one() {
        let dir = std::env::temp_dir().join(format!("klay-dialog-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("model.klay");
        assert_eq!(start_dir(&file.to_string_lossy(), None), dir);
        let other = std::env::temp_dir();
        assert_eq!(start_dir("untitled.klay", Some(&other)), other);
        assert_eq!(start_dir("/no/such/folder/x.klay", Some(&other)), other);
        assert_eq!(start_dir("untitled.klay", None), std::env::current_dir().unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
