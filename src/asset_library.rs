//! Local image library and stable selections; image paths stay on this computer.
use crate::brush_asset::{BrushImage, Pattern};
use crate::sculpt::StampSettings;
use std::path::PathBuf;

pub struct Library {
    pub alphas: Vec<Pattern>,
    pub textures: Vec<Pattern>,
    pub settings: StampSettings,
    pub error: Option<String>,
    path: PathBuf,
    saved: String,
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}
fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some(c) => out.push(c),
                None => out.push('\\'),
            }
        } else {
            out.push(c)
        }
    }
    out
}

impl Library {
    pub fn load(path: PathBuf) -> Self {
        let mut l = Self {
            alphas: Pattern::alphas(),
            textures: Pattern::textures(),
            settings: StampSettings::default(),
            error: None,
            path,
            saved: String::new(),
        };
        if let Ok(text) = std::fs::read_to_string(&l.path) {
            if text.len() <= 1 << 20 && text.starts_with("# Klay brush assets v1\n") {
                let mut alpha = String::new();
                let mut texture = String::new();
                let mut count = 0;
                for line in text.lines().skip(1) {
                    let Some((key, v)) = line.split_once('\t') else {
                        continue;
                    };
                    let v = unescape(v);
                    match key {
                        "alpha-file" | "texture-file" if count < 256 => {
                            count += 1;
                            match BrushImage::load(std::path::Path::new(&v)) {
                                Ok(i) => {
                                    if key == "alpha-file" {
                                        l.alphas.push(Pattern::Image(i))
                                    } else {
                                        l.textures.push(Pattern::Image(i))
                                    }
                                }
                                Err(_) => {
                                    l.error = Some("Some brush images could not be loaded".into())
                                }
                            }
                        }
                        "alpha" => alpha = v,
                        "texture" => texture = v,
                        "strength" => {
                            if let Ok(x) = v.parse::<f32>() {
                                if x.is_finite() {
                                    l.settings.texture_strength = x.clamp(0.0, 1.0)
                                }
                            }
                        }
                        "scale" => {
                            if let Ok(x) = v.parse::<f32>() {
                                if x.is_finite() {
                                    l.settings.texture_scale = x.clamp(0.25, 16.0)
                                }
                            }
                        }
                        "rotate" => {
                            if let Ok(x) = v.parse::<f32>() {
                                if x.is_finite() {
                                    l.settings.texture_angle = x.clamp(-180.0, 180.0)
                                }
                            }
                        }
                        _ => {}
                    }
                }
                if let Some(p) = l.alphas.iter().find(|p| p.locator() == alpha) {
                    l.settings.alpha = p.clone();
                }
                if let Some(p) = l.textures.iter().find(|p| p.locator() == texture) {
                    l.settings.texture = p.clone();
                }
            }
        }
        l.saved = l.encode(&l.settings);
        l
    }

    pub fn import(&mut self, p: Pattern, texture: bool) -> Pattern {
        let items = if texture {
            &mut self.textures
        } else {
            &mut self.alphas
        };
        if let Some(old) = items.iter().find(|old| old.locator() == p.locator()) {
            return old.clone();
        }
        items.push(p.clone());
        p
    }

    fn encode(&self, s: &StampSettings) -> String {
        let mut out = format!(
            "# Klay brush assets v1\nalpha\t{}\ntexture\t{}\nstrength\t{}\nscale\t{}\nrotate\t{}\n",
            escape(&s.alpha.locator()),
            escape(&s.texture.locator()),
            s.texture_strength,
            s.texture_scale,
            s.texture_angle
        );
        for (key, items) in [
            ("alpha-file", &self.alphas),
            ("texture-file", &self.textures),
        ] {
            for p in items {
                if let Pattern::Image(i) = p {
                    out.push_str(&format!("{key}\t{}\n", escape(&i.source.to_string_lossy())));
                }
            }
        }
        out
    }

    pub fn save(&mut self, s: &StampSettings) {
        let text = self.encode(s);
        if text == self.saved {
            return;
        }
        let result = (|| -> std::io::Result<()> {
            if let Some(p) = self.path.parent() {
                std::fs::create_dir_all(p)?;
            }
            let tmp = self.path.with_extension("saving");
            std::fs::write(&tmp, &text)?;
            std::fs::rename(tmp, &self.path)
        })();
        match result {
            Ok(()) => self.saved = text,
            Err(e) => self.error = Some(format!("Cannot save brush assets: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn library_restores_imported_images_and_choices() {
        let root = std::env::temp_dir().join(format!("klay-assets-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("test image.png");
        image::RgbaImage::from_pixel(3, 2, image::Rgba([180, 90, 30, 255]))
            .save(&file)
            .unwrap();
        let path = root.join("assets");
        let mut l = Library::load(path.clone());
        let p = l.import(Pattern::Image(BrushImage::load(&file).unwrap()), false);
        let mut s = StampSettings::default();
        s.alpha = p;
        s.texture = Pattern::Marble;
        s.texture_scale = 3.0;
        s.texture_angle = -35.0;
        l.save(&s);
        let back = Library::load(path);
        assert_eq!(back.settings.alpha.locator(), s.alpha.locator());
        assert_eq!(back.settings.texture, Pattern::Marble);
        assert_eq!(back.settings.texture_scale, 3.0);
        assert_eq!(back.settings.texture_angle, -35.0);
        assert_eq!(unescape(&escape("a\\b\tc\nd\r")), "a\\b\tc\nd\r");
        let _ = std::fs::remove_dir_all(root);
    }
}
