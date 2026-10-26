use std::{io::Read, path::Path};
use zip::ZipArchive;
const MAX_FILE_BYTES: u64 = 96 * 1024 * 1024;
pub fn atomic_write(
    path: &Path,
    write: impl FnOnce(&mut std::fs::File) -> Result<(), String>,
) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .ok_or("Invalid output path")?
        .to_string_lossy();
    let tmp = parent.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        crate::new_id()
    ));
    let result = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| e.to_string())?;
        write(&mut f)?;
        f.sync_all().map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    result
}
pub fn open_archive(path: &Path) -> Result<ZipArchive<std::fs::File>, String> {
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    if f.metadata().map_err(|e| e.to_string())?.len() > MAX_FILE_BYTES {
        return Err("Package exceeds 96 MiB prototype limit".into());
    }
    let mut z = ZipArchive::new(f).map_err(|e| e.to_string())?;
    if z.len() > 512 {
        return Err("Too many package entries".into());
    }
    let mut total = 0u64;
    let mut names = std::collections::HashSet::new();
    for i in 0..z.len() {
        let entry = z.by_index(i).map_err(|e| e.to_string())?;
        if entry.enclosed_name().is_none()
            || entry.name().contains('\\')
            || !names.insert(entry.name().to_string())
        {
            return Err("Unsafe or duplicate package entry".into());
        }
        total = total
            .checked_add(entry.size())
            .ok_or("Package size overflow")?;
        if total > MAX_FILE_BYTES {
            return Err("Expanded package exceeds 96 MiB prototype limit".into());
        }
    }
    Ok(z)
}
pub fn read_entry(
    z: &mut ZipArchive<std::fs::File>,
    name: &str,
    limit: u64,
) -> Result<Vec<u8>, String> {
    let f = z.by_name(name).map_err(|e| e.to_string())?;
    if f.size() > limit {
        return Err(format!("Package entry too large: {name}"));
    }
    let mut bytes = vec![];
    f.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("Package entry exceeds declared limit".into());
    }
    Ok(bytes)
}
