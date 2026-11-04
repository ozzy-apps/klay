//! Batch export runs one map at a time, in a new folder so earlier exports stay intact.
use crate::{bake, multires::Packed};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

pub const KINDS: [bake::Kind; 4] = [
    bake::Kind::Normal,
    bake::Kind::Displacement,
    bake::Kind::VectorDisplacement,
    bake::Kind::Color,
];

pub struct Job {
    pub stop: Arc<AtomicBool>,
    pub result: mpsc::Receiver<Result<String, String>>,
}

pub fn start(
    folder: PathBuf,
    tools: Vec<(String, Packed)>,
    kinds: Vec<bake::Kind>,
    settings: bake::Settings,
) -> Job {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let (send, result) = mpsc::channel();
    std::thread::spawn(move || {
        let result = std::panic::catch_unwind(|| run(folder, tools, &kinds, &settings, &flag))
            .unwrap_or_else(|_| Err("The map export stopped unexpectedly".into()));
        let _ = send.send(result);
    });
    Job { stop, result }
}

fn run(
    folder: PathBuf,
    tools: Vec<(String, Packed)>,
    kinds: &[bake::Kind],
    settings: &bake::Settings,
    stop: &AtomicBool,
) -> Result<String, String> {
    let target = (1..)
        .find_map(|i| {
            let path = folder.join(if i == 1 {
                "Klay Maps".into()
            } else {
                format!("Klay Maps {i}")
            });
            match std::fs::create_dir(&path) {
                Ok(()) => Some(Ok(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(e) => Some(Err(e.to_string())),
            }
        })
        .unwrap()?;
    let (mut count, mut skipped, mut errors) = (0, 0, Vec::new());
    'tools: for (index, (name, packed)) in tools.into_iter().enumerate() {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let mut doc = match packed.try_unpack() {
            Ok(doc) => doc,
            Err(e) => {
                errors.push(format!("{name}: {e}"));
                continue;
            }
        };
        for &kind in kinds {
            if stop.load(Ordering::Relaxed) {
                break 'tools;
            }
            if kind.needs_levels() && doc.count() < 2 {
                skipped += 1;
                continue;
            }
            let (suffix, ext) = match kind {
                bake::Kind::Normal => ("normal", "png"),
                bake::Kind::Displacement => {
                    ("displacement", if settings.exr { "exr" } else { "png" })
                }
                bake::Kind::VectorDisplacement => ("vector", "exr"),
                bake::Kind::Color => ("color", "png"),
            };
            let filename =
                crate::file_dialog::file_name(&format!("{:03} {name} {suffix}", index + 1), ext);
            let path = target.join(filename);
            let temporary = target.join(format!(".writing.{ext}"));
            let result = bake::bake(&bake::Source::of(&mut doc, kind), kind, settings)
                .and_then(|map| bake::write(&map, &temporary))
                .and_then(|_| std::fs::rename(&temporary, &path).map_err(|e| e.to_string()));
            if let Err(error) = result {
                let _ = std::fs::remove_file(&temporary);
                errors.push(format!("{name} {}: {error}", kind.name()));
            } else {
                count += 1;
            }
        }
    }
    let stopped = if stop.load(Ordering::Relaxed) {
        " (cancelled)"
    } else {
        ""
    };
    let failed = errors.first().map_or(String::new(), |e| {
        format!(", {} failed ({e})", errors.len())
    });
    Ok(format!(
        "Exported {count} maps to {}{stopped} · {skipped} skipped{failed}",
        target.display()
    ))
}

#[cfg(test)]
#[test]
fn parity_batch_keeps_each_subtool_map_and_uses_a_fresh_output_folder() {
    let root = std::env::temp_dir().join(format!("klay-map-batch-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let mut doc = crate::obj::parse(
        "v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nvt 0 0\nvt 1 0\nvt 1 1\nvt 0 1\nf 1/1 2/2 3/3 4/4",
    )
    .unwrap()
    .into_doc(200_000);
    let flat = doc.pack_copy();
    doc.divide();
    let settings = bake::Settings {
        size: 32,
        padding: 1,
        samples: 1,
        ..Default::default()
    };
    let mut tools = vec![("Same".into(), doc.pack()), ("Same".into(), flat)];
    let kinds = [bake::Kind::VectorDisplacement, bake::Kind::Color];
    let stop = AtomicBool::new(false);
    let result = run(root.clone(), tools.clone(), &kinds, &settings, &stop).unwrap();
    assert!(result.contains("Exported 3 maps") && result.contains("1 skipped"));
    let output = root.join("Klay Maps");
    let exr = std::fs::read(output.join("001 Same vector.exr")).unwrap();
    assert_eq!(&exr[..4], &[0x76, 0x2f, 0x31, 0x01]);
    assert!(output.join("001 Same color.png").is_file());
    assert!(output.join("002 Same color.png").is_file());
    tools.truncate(1);
    run(root.clone(), tools, &kinds, &settings, &stop).unwrap();
    assert!(root.join("Klay Maps 2/001 Same vector.exr").is_file());
    assert_eq!(
        std::fs::read(output.join("001 Same vector.exr")).unwrap(),
        exr
    );
    std::fs::remove_dir_all(root).unwrap();
}
