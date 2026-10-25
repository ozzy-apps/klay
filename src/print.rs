//! 3D Print palette (ZBrush: 3D Print Hub): the model's size in real units, STL export
//! placed for a printer, sending it to a slicer, and a check for what stops a mesh from
//! printing (holes, non-manifold edges, flipped triangles).

use glam::Vec3;
use rayon::prelude::*;
use std::path::{Path, PathBuf};

/// The unit sizes are shown and typed in. Files are always written in millimetres, the
/// unit every slicer assumes for STL.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Unit {
    #[default]
    Mm,
    Cm,
    In,
}

impl Unit {
    pub const ALL: [Self; 3] = [Self::Mm, Self::Cm, Self::In];

    pub fn name(self) -> &'static str {
        match self {
            Self::Mm => "mm",
            Self::Cm => "cm",
            Self::In => "in",
        }
    }

    /// Millimetres in one of this unit.
    pub fn mm(self) -> f32 {
        match self {
            Self::Mm => 1.0,
            Self::Cm => 10.0,
            Self::In => 25.4,
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|u| u.name() == s)
    }
}

/// 3D Print settings, kept with the workspace.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub unit: Unit,
    /// Millimetres per Klay unit: the model is not changed, only what is written.
    pub scale: f32,
    /// Klay is Y up, printers Z up: turn the model so it stands on the bed as it stands
    /// on screen (the front, +Z, faces the front of the bed, −Y).
    pub z_up: bool,
    /// The slicer Send opens: a [`Slicer::id`], or [`CUSTOM`] for `command`.
    pub slicer: String,
    /// The custom slicer's command line; `{file}` is the STL (added at the end without it).
    pub command: String,
}

/// [`Settings::slicer`] for the custom command.
pub const CUSTOM: &str = "custom";

impl Default for Settings {
    fn default() -> Self {
        Self { unit: Unit::Mm, scale: 25.0, z_up: true, slicer: String::new(), command: String::new() }
    }
}

impl Settings {
    /// Where a model point lands in the written file (millimetres, printer axes).
    pub fn place(&self, p: Vec3) -> Vec3 {
        let p = p * self.scale;
        if self.z_up { Vec3::new(p.x, -p.z, p.y) } else { p }
    }

    /// Width, depth and height on the printer (in mm) of a model box of `extent`.
    pub fn printed_size(&self, extent: Vec3) -> Vec3 {
        let e = extent * self.scale;
        if self.z_up { Vec3::new(e.x, e.z, e.y) } else { e }
    }

    pub(crate) fn encode(&self) -> String {
        let clean = |s: &str| s.replace(['\n', '\r'], " ");
        format!("print|{}|{:.6}|{}|{}|{}\n", self.unit.name(), self.scale, self.z_up, clean(&self.slicer).replace('|', "/"), clean(&self.command))
    }

    /// From the fields after `print|`; the command may itself hold `|`.
    pub(crate) fn decode(parts: &[&str]) -> Option<Self> {
        let [unit, scale, z_up, slicer, command @ ..] = parts else { return None };
        let scale: f32 = scale.parse().ok().filter(|s: &f32| s.is_finite() && *s > 0.0)?;
        Some(Self { unit: Unit::parse(unit)?, scale, z_up: z_up.parse().ok()?, slicer: slicer.to_string(), command: command.join("|") })
    }
}

// ------------------------------------------------------------------ printability check

/// What stops a mesh from printing, from its triangles alone.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Check {
    pub triangles: usize,
    /// Edges with one triangle: the rims of holes.
    pub open_edges: usize,
    /// Holes: loops of open edges.
    pub holes: usize,
    /// Edges shared by three or more triangles.
    pub non_manifold: usize,
    /// Edges whose two triangles disagree about which side is outside.
    pub flipped: usize,
    /// Separate pieces (connected by edges or corners).
    pub shells: usize,
}

impl Check {
    /// Closed, manifold and consistently wound: what a slicer wants.
    pub fn watertight(&self) -> bool {
        self.open_edges == 0 && self.non_manifold == 0 && self.flipped == 0
    }

    pub fn add(&mut self, o: &Check) {
        self.triangles += o.triangles;
        self.open_edges += o.open_edges;
        self.holes += o.holes;
        self.non_manifold += o.non_manifold;
        self.flipped += o.flipped;
        self.shells += o.shells;
    }
}

/// 3D Print › Check over the subtools that would be written.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    /// Each visible subtool by name, in list order.
    pub rows: Vec<(String, Check)>,
    pub total: Check,
    /// Visible subtools set to Subtract or Intersect: written as they are, not carved.
    pub booleans: usize,
    /// Which model state it was for, so the palette can say when it is out of date.
    pub stamp: u64,
}

/// Count open, non-manifold and flipped edges, holes and shells of an indexed triangle
/// mesh. Works per vertex in parallel, so memory stays near one index per corner.
pub fn check(indices: &[u32]) -> Check {
    let n = indices.iter().copied().max().map_or(0, |m| m as usize + 1);
    // Triangles around each vertex.
    let mut offsets = vec![0u32; n + 1];
    for &v in indices {
        offsets[v as usize + 1] += 1;
    }
    for i in 0..n {
        offsets[i + 1] += offsets[i];
    }
    let mut fill = offsets.clone();
    let mut around = vec![0u32; indices.len()];
    for (c, &v) in indices.iter().enumerate() {
        around[fill[v as usize] as usize] = (c / 3) as u32;
        fill[v as usize] += 1;
    }
    drop(fill);
    // Each edge is counted at its lower vertex: how many triangles use it, and how many run
    // it low → high.
    let (open, non_manifold, flipped, rims) = (0..n as u32)
        .into_par_iter()
        .fold(
            || (0usize, 0usize, 0usize, Vec::new(), Vec::<(u32, bool)>::new()),
            |(mut open, mut nm, mut fl, mut rims, mut list), v| {
                list.clear();
                for &t in &around[offsets[v as usize] as usize..offsets[v as usize + 1] as usize] {
                    let tri = &indices[3 * t as usize..3 * t as usize + 3];
                    let k = tri.iter().position(|&x| x == v).unwrap();
                    let (next, prev) = (tri[(k + 1) % 3], tri[(k + 2) % 3]);
                    if next > v {
                        list.push((next, true));
                    }
                    if prev > v {
                        list.push((prev, false));
                    }
                }
                list.sort_unstable();
                let mut i = 0;
                while i < list.len() {
                    let w = list[i].0;
                    let j = i + list[i..].iter().take_while(|e| e.0 == w).count();
                    let forward = list[i..j].iter().filter(|e| e.1).count();
                    match j - i {
                        1 => {
                            open += 1;
                            rims.push((v, w));
                        }
                        2 if forward != 1 => fl += 1,
                        2 => {}
                        _ => nm += 1,
                    }
                    i = j;
                }
                (open, nm, fl, rims, list)
            },
        )
        .map(|(a, b, c, d, _)| (a, b, c, d))
        .reduce(
            || (0, 0, 0, Vec::new()),
            |a, b| (a.0 + b.0, a.1 + b.1, a.2 + b.2, [a.3, b.3].concat()),
        );
    let mut shells = Dsu::new(n);
    for t in indices.chunks_exact(3) {
        shells.union(t[0], t[1]);
        shells.union(t[0], t[2]);
    }
    let used: Vec<bool> = offsets.windows(2).map(|w| w[1] > w[0]).collect();
    let shell_count = (0..n as u32).filter(|&v| used[v as usize] && shells.find(v) == v).count();
    let mut loops = Dsu::new(n);
    for &(a, b) in &rims {
        loops.union(a, b);
    }
    let mut roots: Vec<u32> = rims.iter().map(|&(a, _)| loops.find(a)).collect();
    roots.sort_unstable();
    roots.dedup();
    Check { triangles: indices.len() / 3, open_edges: open, holes: roots.len(), non_manifold, flipped, shells: shell_count }
}

struct Dsu(Vec<u32>);

impl Dsu {
    fn new(n: usize) -> Self {
        Self((0..n as u32).collect())
    }
    fn find(&mut self, mut v: u32) -> u32 {
        while self.0[v as usize] != v {
            let up = self.0[self.0[v as usize] as usize];
            self.0[v as usize] = up;
            v = up;
        }
        v
    }
    fn union(&mut self, a: u32, b: u32) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.0[a.max(b) as usize] = a.min(b);
        }
    }
}

// ------------------------------------------------------------------------------ slicers

/// A slicer Send can open the STL in.
#[derive(Clone, Debug, PartialEq)]
pub struct Slicer {
    /// Stable key stored with the workspace (a desktop file's name, or a program path).
    pub id: String,
    /// Shown in the list: "OrcaSlicer (Flatpak)".
    pub name: String,
    /// Its command line, with `%f`-style field codes where the file goes.
    pub exec: String,
}

/// Slicers by the words in their names (lower case, no spaces).
const KNOWN: [&str; 10] =
    ["prusaslicer", "superslicer", "orcaslicer", "bambustudio", "cura", "crealityprint", "elegooslicer", "anycubicslicer", "qidistudio", "slic3r"];

/// Program names to look for in `PATH` when no desktop entry names the slicer.
const PROGRAMS: [(&str, &str); 6] = [
    ("prusa-slicer", "PrusaSlicer"),
    ("superslicer", "SuperSlicer"),
    ("orca-slicer", "OrcaSlicer"),
    ("bambu-studio", "Bambu Studio"),
    ("cura", "UltiMaker Cura"),
    ("slic3r", "Slic3r"),
];

/// The slicers installed on this computer: desktop entries (packages, Flatpaks, Snaps and
/// AppImages added to the menu) and programs in `PATH`.
pub fn detect() -> Vec<Slicer> {
    let mut found = Vec::new();
    for dir in application_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "desktop")).collect();
        files.sort();
        for path in files {
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let Some((name, exec)) = desktop_slicer(&text) else { continue };
            let id = path.file_name().unwrap().to_string_lossy().into_owned();
            // The first directory with a desktop file wins, as in the menu.
            if !found.iter().any(|s: &Slicer| s.id == id) {
                found.push(Slicer { id, name, exec });
            }
        }
    }
    for (program, name) in PROGRAMS {
        let key = name.to_lowercase().replace(' ', "");
        if found.iter().any(|s| s.name.to_lowercase().replace(' ', "").contains(&key)) {
            continue;
        }
        if let Some(path) = in_path(program) {
            let p = path.to_string_lossy().into_owned();
            found.push(Slicer { id: p.clone(), name: name.to_string(), exec: format!("{} %F", quote(&p)) });
        }
    }
    found.extend(platform_slicers());
    // Tell apart two installs of one slicer by where they come from.
    let names: Vec<String> = found.iter().map(|s| s.name.clone()).collect();
    for i in 0..found.len() {
        let tag = if found[i].exec.contains("flatpak") {
            "Flatpak"
        } else if found[i].exec.to_lowercase().contains(".appimage") {
            "AppImage"
        } else if found[i].exec.contains("snap") {
            "Snap"
        } else {
            ""
        };
        if names.iter().filter(|n| **n == names[i]).count() > 1 && !tag.is_empty() {
            found[i].name = format!("{} ({tag})", found[i].name);
        }
    }
    found
}

/// The name and command of a desktop entry that is a slicer.
fn desktop_slicer(text: &str) -> Option<(String, String)> {
    let (mut name, mut exec, mut try_exec, mut hidden, mut in_entry) = (None, None, None, false, false);
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        if let Some(v) = line.strip_prefix("Name=") {
            name = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("Exec=") {
            exec = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("TryExec=") {
            try_exec = Some(v.trim().to_string());
        } else if line == "Hidden=true" {
            hidden = true;
        }
    }
    let (name, exec) = (name?, exec?);
    let key = name.to_lowercase().replace([' ', '-', '_'], "");
    if hidden || !KNOWN.iter().any(|k| key.contains(k)) {
        return None;
    }
    if let Some(t) = try_exec {
        let ok = if t.contains('/') { Path::new(&t).exists() } else { in_path(&t).is_some() };
        if !ok {
            return None;
        }
    }
    Some((name, exec))
}

/// `$XDG_DATA_HOME/applications`, every `$XDG_DATA_DIRS/applications`, and the Flatpak
/// and Snap export folders.
fn application_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut dirs = Vec::new();
    match std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        Some(d) => dirs.push(PathBuf::from(d)),
        None => dirs.extend(home.as_ref().map(|h| h.join(".local/share"))),
    }
    let data_dirs = std::env::var("XDG_DATA_DIRS").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    dirs.extend(data_dirs.split(':').map(PathBuf::from));
    dirs.extend(home.map(|h| h.join(".local/share/flatpak/exports/share")));
    dirs.push("/var/lib/flatpak/exports/share".into());
    dirs.push("/var/lib/snapd/desktop".into());
    let mut out: Vec<PathBuf> = Vec::new();
    for d in dirs {
        let d = d.join("applications");
        if !out.contains(&d) {
            out.push(d);
        }
    }
    out
}

fn in_path(program: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(program)).find(|p| p.is_file())
}

#[cfg(target_os = "macos")]
fn platform_slicers() -> Vec<Slicer> {
    let Ok(entries) = std::fs::read_dir("/Applications") else { return Vec::new() };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.path().file_stem()?.to_string_lossy().into_owned();
            let key = name.to_lowercase().replace([' ', '-'], "");
            KNOWN.iter().any(|k| key.contains(k)).then(|| {
                let path = e.path().to_string_lossy().into_owned();
                Slicer { id: path.clone(), name, exec: format!("open -a {} %F", quote(&path)) }
            })
        })
        .collect()
}

#[cfg(windows)]
fn platform_slicers() -> Vec<Slicer> {
    let roots = ["ProgramFiles", "ProgramFiles(x86)"].iter().filter_map(|v| std::env::var_os(v)).map(PathBuf::from).collect::<Vec<_>>();
    let known = [
        ("Prusa3D\\PrusaSlicer\\prusa-slicer.exe", "PrusaSlicer"),
        ("OrcaSlicer\\orca-slicer.exe", "OrcaSlicer"),
        ("Bambu Studio\\bambu-studio.exe", "Bambu Studio"),
        ("Creality\\Creality Print\\CrealityPrint.exe", "Creality Print"),
    ];
    let mut found = Vec::new();
    for root in &roots {
        for (rel, name) in known {
            let p = root.join(rel);
            if p.is_file() {
                let s = p.to_string_lossy().into_owned();
                found.push(Slicer { id: s.clone(), name: name.into(), exec: format!("{} %F", quote(&s)) });
            }
        }
        // Cura's folder carries its version: "UltiMaker Cura 5.8.1".
        if let Ok(entries) = std::fs::read_dir(root) {
            for e in entries.flatten() {
                let exe = e.path().join("UltiMaker-Cura.exe");
                if e.file_name().to_string_lossy().starts_with("UltiMaker Cura") && exe.is_file() {
                    let s = exe.to_string_lossy().into_owned();
                    found.push(Slicer { id: s.clone(), name: e.file_name().to_string_lossy().into_owned(), exec: format!("{} %F", quote(&s)) });
                }
            }
        }
    }
    found
}

#[cfg(not(any(target_os = "macos", windows)))]
fn platform_slicers() -> Vec<Slicer> {
    Vec::new()
}

fn quote(s: &str) -> String {
    if s.contains([' ', '"', '\'', '\\']) { format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")) } else { s.to_string() }
}

/// Split a command line into words: spaces separate them, double or single quotes keep
/// them together, a backslash takes the next character as it is.
pub fn split_words(line: &str) -> Vec<String> {
    let (mut words, mut word, mut quote, mut any) = (Vec::new(), String::new(), None, false);
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some('\''), '\'') | (Some('"'), '"') => quote = None,
            (Some('\''), _) => word.push(c),
            (_, '\\') => {
                word.extend(chars.next());
                any = true;
            }
            (None, '"' | '\'') => {
                quote = Some(c);
                any = true;
            }
            (None, c) if c.is_whitespace() => {
                if any || !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
                any = false;
            }
            _ => word.push(c),
        }
    }
    if any || !word.is_empty() {
        words.push(word);
    }
    words
}

/// The program and arguments that open `file`: `%f %F` (desktop entries) and `{file}`
/// (custom commands) become its path, `%u %U` its `file://` URL (what Flatpak's file
/// forwarding expects), other `%` codes go, and with none of them the path goes at the end.
pub fn command_for(exec: &str, file: &Path) -> Vec<String> {
    let path = file.to_string_lossy();
    let url = file_url(file);
    let mut placed = false;
    let mut argv = Vec::new();
    for word in split_words(exec) {
        if matches!(word.as_str(), "%i" | "%c" | "%k") {
            continue;
        }
        let mut out = String::new();
        let mut chars = word.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '%' {
                match chars.next() {
                    Some('f' | 'F') => {
                        out.push_str(&path);
                        placed = true;
                    }
                    Some('u' | 'U') => {
                        out.push_str(&url);
                        placed = true;
                    }
                    Some('%') => out.push('%'),
                    _ => {}
                }
            } else {
                out.push(c);
            }
        }
        if out.contains("{file}") {
            out = out.replace("{file}", &path);
            placed = true;
        }
        argv.push(out);
    }
    if !placed {
        argv.push(path.into_owned());
    }
    argv
}

/// `file://` and the path with every byte but letters, digits, `/` and `-._~` written as
/// `%XX`.
fn file_url(file: &Path) -> String {
    let mut url = String::from("file://");
    for &b in file.to_string_lossy().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            url.push(b as char);
        } else {
            url.push_str(&format!("%{b:02X}"));
        }
    }
    url
}

/// Start `argv` without waiting for it (a thread reaps it when it exits).
pub fn launch(argv: &[String]) -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    let (program, args) = argv.split_first().ok_or_else(|| std::io::Error::other("the slicer command is empty"))?;
    let mut child = Command::new(program).args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    std::thread::Builder::new().name("slicer".into()).spawn(move || {
        let _ = child.wait();
    })?;
    Ok(())
}

/// Where Send writes the STL: `$XDG_CACHE_HOME/klay/print` (else `~/.cache/klay/print`,
/// else the temp folder).
pub fn send_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("klay")
        .join("print")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closed_cube_checks_watertight_and_an_open_one_shows_its_hole() {
        let (p, i, _) = crate::primitive::Settings { kind: crate::primitive::Primitive::Cube, ..Default::default() }.build(1.0);
        let c = check(&i);
        assert!(c.watertight(), "{c:?}");
        assert_eq!((c.shells, c.holes, c.triangles), (1, 0, i.len() / 3));
        // Drop the triangles of one face: one hole, its rim open.
        let top: Vec<u32> = i.chunks(3).filter(|t| !t.iter().all(|&v| p[v as usize].y > 0.999)).flatten().copied().collect();
        let c = check(&top);
        assert!(!c.watertight());
        assert_eq!(c.holes, 1);
        assert!(c.open_edges >= 4);
        // Two copies side by side are two shells; a flipped triangle shows; a fin of three
        // triangles on one edge is non-manifold.
        let n = p.len() as u32;
        let two: Vec<u32> = i.iter().copied().chain(i.iter().map(|v| v + n)).collect();
        assert_eq!(check(&two).shells, 2);
        let mut flip = i.clone();
        flip.swap(0, 1);
        assert_eq!(check(&flip).flipped, 3);
        let fin = [0, 1, 2, 1, 0, 3, 0, 1, 4];
        assert_eq!(check(&fin).non_manifold, 1);
    }

    #[test]
    fn sizes_are_millimetres_on_printer_axes() {
        let s = Settings { scale: 10.0, ..Default::default() };
        assert_eq!(s.place(Vec3::new(1.0, 2.0, 3.0)), Vec3::new(10.0, -30.0, 20.0));
        assert_eq!(s.printed_size(Vec3::new(1.0, 2.0, 3.0)), Vec3::new(10.0, 30.0, 20.0));
        let flat = Settings { z_up: false, ..s.clone() };
        assert_eq!(flat.place(Vec3::new(1.0, 2.0, 3.0)), Vec3::new(10.0, 20.0, 30.0));
        let custom = Settings { unit: Unit::In, slicer: CUSTOM.into(), command: "sh -c 'cat {file} | wc' x".into(), ..s };
        let line = custom.encode();
        let parts: Vec<&str> = line.trim_end().split('|').collect();
        assert_eq!(parts[0], "print");
        assert_eq!(Settings::decode(&parts[1..]), Some(custom));
        assert_eq!(Settings::decode(&["mm", "-1", "true", "", ""]), None);
    }

    #[test]
    fn commands_put_the_file_where_it_goes() {
        let f = Path::new("/tmp/my model.stl");
        assert_eq!(
            command_for("/usr/bin/flatpak run --file-forwarding com.orcaslicer.OrcaSlicer @@u %U @@", f),
            ["/usr/bin/flatpak", "run", "--file-forwarding", "com.orcaslicer.OrcaSlicer", "@@u", "file:///tmp/my%20model.stl", "@@"]
        );
        assert_eq!(command_for("env A=1 \"/opt/Prusa Slicer/prusa\" %F %i", f), ["env", "A=1", "/opt/Prusa Slicer/prusa", "/tmp/my model.stl"]);
        assert_eq!(command_for("cura", f), ["cura", "/tmp/my model.stl"]);
        assert_eq!(command_for("my-slicer --load={file} --go", f), ["my-slicer", "--load=/tmp/my model.stl", "--go"]);
        assert_eq!(split_words(r#"a 'b c' "d \" e" f\ g """#), ["a", "b c", "d \" e", "f g", ""]);
    }

    #[test]
    fn desktop_entries_name_slicers() {
        let orca = "[Desktop Entry]\nName=OrcaSlicer\nExec=/usr/bin/flatpak run com.orcaslicer.OrcaSlicer @@u %U @@\n[Desktop Action New]\nName=New Project\n";
        assert_eq!(desktop_slicer(orca).unwrap().0, "OrcaSlicer");
        assert!(desktop_slicer("[Desktop Entry]\nName=UltiMaker Cura\nExec=cura %F\n").is_some());
        assert!(desktop_slicer("[Desktop Entry]\nName=Blender\nExec=blender %f\n").is_none());
        assert!(desktop_slicer("[Desktop Entry]\nName=PrusaSlicer\nExec=x\nTryExec=/no/such/prusa\n").is_none());
        assert!(desktop_slicer("[Desktop Entry]\nName=Bambu Studio\nExec=x\nHidden=true\n").is_none());
    }
}
