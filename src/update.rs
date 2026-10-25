//! Checking for a newer Klay: GitHub's "latest release" of the package repository, asked in
//! the background with the system's `curl` (Linux, macOS and Windows 10+ have it), so Klay
//! needs no network library. A release is newer when its tag (`v0.25.0` or `0.25.0`) is a
//! higher version than this build.
use std::sync::mpsc::{self, Receiver};

/// A published release.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// "0.25.0" (the tag without its "v").
    pub version: String,
    /// The release's web page.
    pub page: String,
    /// The download for this platform, when the release has one: (file name, URL).
    pub asset: Option<(String, String)>,
    /// The first lines of the release notes.
    pub notes: String,
}

/// What a check found: a newer release, none (up to date), or why it could not tell.
pub type Outcome = Result<Option<Release>, String>;

/// The API address of the latest release of `repository` ("https://github.com/owner/name").
pub fn latest_url(repository: &str) -> Option<String> {
    let path = repository.trim_end_matches('/').strip_prefix("https://github.com/")?;
    let mut parts = path.split('/');
    let (owner, name) = (parts.next()?, parts.next()?);
    Some(format!("https://api.github.com/repos/{owner}/{name}/releases/latest"))
}

/// Start a check in the background; the outcome arrives on the receiver.
pub fn check_in_background(repository: &str, current: &str) -> Receiver<Outcome> {
    let (tx, rx) = mpsc::channel();
    let (repository, current) = (repository.to_string(), current.to_string());
    std::thread::spawn(move || {
        let _ = tx.send(check(&repository, &current));
    });
    rx
}

/// Ask GitHub now (blocking: run it off the interface thread).
pub fn check(repository: &str, current: &str) -> Outcome {
    let url = latest_url(repository).ok_or("the repository is not on GitHub")?;
    let out = std::process::Command::new("curl")
        .args(["-sS", "-L", "-m", "15", "-w", "\n%{http_code}"])
        .args(["-H", "Accept: application/vnd.github+json"])
        .args(["-H", &format!("User-Agent: Klay/{current}")])
        .arg(&url)
        .output()
        .map_err(|e| format!("curl is not available ({e})"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let (body, code) = text.rsplit_once('\n').unwrap_or((&text, ""));
    match code.trim() {
        "200" => {}
        "404" => return Err("no published release found (the repository may be private)".into()),
        "" => return Err(format!("no answer: {}", String::from_utf8_lossy(&out.stderr).trim())),
        code => return Err(format!("GitHub answered {code}")),
    }
    let release = parse(body).ok_or("the release could not be read")?;
    Ok(newer(&release.version, current).then_some(release))
}

/// The release in GitHub's JSON answer (only the fields Klay uses, read by hand).
pub fn parse(json: &str) -> Option<Release> {
    let tag = string_field(json, "tag_name")?;
    let version = tag.trim_start_matches(['v', 'V']).to_string();
    let page = string_field(json, "html_url").unwrap_or_default();
    let notes: String = string_field(json, "body").unwrap_or_default().lines().take(6).collect::<Vec<_>>().join("\n");
    // Assets: every name with its download address, in order.
    let mut assets = Vec::new();
    let mut rest = json;
    while let Some(i) = rest.find("\"browser_download_url\"") {
        let before = &rest[..i];
        let name = before.rfind("\"name\"").and_then(|k| string_field(&before[k..], "name"));
        let url = string_field(&rest[i..], "browser_download_url");
        if let (Some(name), Some(url)) = (name, url) {
            assets.push((name, url));
        }
        rest = &rest[i + 22..];
    }
    let asset = pick_asset(&assets);
    Some(Release { version, page, asset, notes })
}

/// The download that suits this computer: a Linux AppImage or archive, a Windows installer
/// or zip, a macOS disk image.
fn pick_asset(assets: &[(String, String)]) -> Option<(String, String)> {
    let wanted: &[&str] = if cfg!(target_os = "windows") {
        &[".msi", ".exe", "windows", ".zip"]
    } else if cfg!(target_os = "macos") {
        &[".dmg", "macos", "darwin"]
    } else {
        &[".appimage", "linux", ".tar.gz", ".tar.xz"]
    };
    wanted.iter().find_map(|w| assets.iter().find(|(n, _)| n.to_lowercase().contains(w)).cloned())
}

/// The JSON string value of `"key": "…"` (first one), unescaped.
fn string_field(json: &str, key: &str) -> Option<String> {
    let k = json.find(&format!("\"{key}\""))?;
    let after = &json[k + key.len() + 2..];
    let colon = after.find(':')?;
    let value = after[colon + 1..].trim_start();
    let mut chars = value.strip_prefix('"')?.chars();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => {}
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    out.push(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32).unwrap_or('?'));
                }
                other => out.push(other),
            },
            c => out.push(c),
        }
    }
    None
}

/// Whether `candidate` ("0.25.0") is a higher version than `current`; pre-release
/// suffixes ("-beta") count as below the plain version.
pub fn newer(candidate: &str, current: &str) -> bool {
    let key = |v: &str| -> (Vec<u64>, bool) {
        let (core, pre) = match v.split_once('-') {
            Some((c, _)) => (c, true),
            None => (v, false),
        };
        (core.split('.').map(|p| p.trim().parse().unwrap_or(0)).collect(), !pre)
    };
    let (a, b) = (key(candidate), key(current));
    let n = a.0.len().max(b.0.len());
    for i in 0..n {
        let (x, y) = (a.0.get(i).copied().unwrap_or(0), b.0.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    a.1 && !b.1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number_and_pre_releases_come_first() {
        assert!(newer("0.25.0", "0.24.0"));
        assert!(newer("0.24.10", "0.24.9"));
        assert!(newer("1.0", "0.99.99"));
        assert!(!newer("0.24.0", "0.24.0"));
        assert!(!newer("0.23.1", "0.24.0"));
        assert!(newer("0.25.0", "0.25.0-beta"));
        assert!(!newer("0.25.0-beta", "0.25.0"));
    }

    #[test]
    fn a_github_release_is_read_with_its_download() {
        let json = r###"{"url":"x","html_url":"https://github.com/ozzy-apps/klay/releases/tag/v0.25.0",
            "tag_name":"v0.25.0","name":"Klay 0.25","body":"## Shelves\r\n- every \"item\" moves\n- more",
            "assets":[{"name":"Klay-0.25.0-x86_64.AppImage","browser_download_url":"https://github.com/a/Klay-0.25.0-x86_64.AppImage"},
                      {"name":"Klay-0.25.0-windows.msi","browser_download_url":"https://github.com/a/Klay-0.25.0-windows.msi"},
                      {"name":"Klay-0.25.0.dmg","browser_download_url":"https://github.com/a/Klay-0.25.0.dmg"}]}"###;
        let r = parse(json).unwrap();
        assert_eq!(r.version, "0.25.0");
        assert_eq!(r.page, "https://github.com/ozzy-apps/klay/releases/tag/v0.25.0");
        assert!(r.notes.starts_with("## Shelves\n- every \"item\" moves"));
        let (name, url) = r.asset.unwrap();
        let expect = if cfg!(target_os = "windows") { "windows.msi" } else if cfg!(target_os = "macos") { ".dmg" } else { ".AppImage" };
        assert!(name.ends_with(expect) && url.ends_with(expect), "{name}");
    }

    #[test]
    fn the_api_address_comes_from_the_repository() {
        assert_eq!(latest_url("https://github.com/ozzy-apps/klay").as_deref(), Some("https://api.github.com/repos/ozzy-apps/klay/releases/latest"));
        assert_eq!(latest_url("https://example.com/x"), None);
    }
}
