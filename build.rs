//! Build script for single-ion.
//!
//! When targeting Windows, converts `ion/art/ion_logo.png` into a multi-size
//! `.ico` file and embeds it as the executable icon via `winresource`.
//! On non-Windows targets this script is a no-op.

fn main() {
    // CARGO_CFG_WINDOWS is set by Cargo when the *target* is Windows,
    // regardless of the build host — correct for cross-compilation too.
    if std::env::var("CARGO_CFG_WINDOWS").is_err() {
        return;
    }

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let png_path = format!("{manifest_dir}/../ion/art/ion_logo.png");

    // Tell Cargo to re-run this script if the source image changes.
    println!("cargo:rerun-if-changed={png_path}");

    let out_dir = std::env::var("OUT_DIR").unwrap();
    let ico_path = format!("{out_dir}/ion.ico");

    build_ico(&png_path, &ico_path);

    let mut res = winresource::WindowsResource::new();
    res.set_icon(&ico_path);
    res.compile().expect("winresource: failed to embed icon");
}

/// Convert a PNG at `src` into a multi-size ICO written to `dst`.
///
/// Standard Windows icon sizes: 16 × 16, 32 × 32, 48 × 48, 256 × 256.
fn build_ico(src: &str, dst: &str) {
    use std::io::BufWriter;

    let img = image::open(src)
        .unwrap_or_else(|e| panic!("build.rs: could not open {src}: {e}"));

    let mut icon_dir = ico::IconDir::new(ico::ResourceType::Icon);

    for size in [16u32, 32, 48, 256] {
        let resized = img.resize(size, size, image::imageops::FilterType::Lanczos3);
        let rgba = resized.to_rgba8();
        let icon_image = ico::IconImage::from_rgba_data(size, size, rgba.into_raw());
        let entry = ico::IconDirEntry::encode(&icon_image)
            .unwrap_or_else(|e| panic!("build.rs: encode ico frame {size}x{size}: {e}"));
        icon_dir.add_entry(entry);
    }

    let file = std::fs::File::create(dst)
        .unwrap_or_else(|e| panic!("build.rs: could not create {dst}: {e}"));
    icon_dir
        .write(BufWriter::new(file))
        .unwrap_or_else(|e| panic!("build.rs: could not write {dst}: {e}"));
}
