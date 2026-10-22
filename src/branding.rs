//! The approved Coral Fold artwork and package metadata, shared by the native
//! window, menu-bar button, and About dialog. Embed it so the binary is portable.

use std::sync::OnceLock;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
pub const LANGUAGE: &str = "Rust";

fn logo() -> &'static image::RgbaImage {
    static IMAGE: OnceLock<image::RgbaImage> = OnceLock::new();
    IMAGE.get_or_init(|| {
        image::load_from_memory_with_format(
            include_bytes!("../assets/icon/klay-512.png"),
            image::ImageFormat::Png,
        )
        .expect("embedded Coral Fold logo must be a valid PNG")
        .into_rgba8()
    })
}

pub fn window_icon() -> winit::window::Icon {
    let image = logo();
    winit::window::Icon::from_rgba(image.as_raw().clone(), image.width(), image.height())
        .expect("embedded Coral Fold logo must have valid RGBA dimensions")
}

pub fn texture(ctx: &egui::Context) -> egui::TextureHandle {
    let image = logo();
    ctx.load_texture(
        "klay-coral-fold",
        egui::ColorImage::from_rgba_unmultiplied(
            [image.width() as usize, image.height() as usize],
            image.as_raw(),
        ),
        egui::TextureOptions::LINEAR,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_logo_is_the_transparent_coral_asset_and_can_be_a_window_icon() {
        let image = logo();
        assert_eq!(image.dimensions(), (512, 512));
        assert_eq!(image.get_pixel(0, 0)[3], 0);
        let color = image.get_pixel(160, 210);
        assert_eq!(color[3], 255);
        assert!(
            color[0] > color[1] && color[1] > color[2],
            "the selected logo is coral"
        );
        let _ = window_icon();
    }
}
