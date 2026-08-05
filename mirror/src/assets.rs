//! The image cache: every picture the public page shows is a file this service
//! owns, addressed by the hash of its own bytes.
//!
//! Nothing is hot-linked back to Fizzy. A hot-linked image would ask each
//! visitor's browser to fetch from a hostname that only resolves on the
//! tailnet (so it would simply fail), and in the general case it would hand a
//! third party the visitor's IP address and the ability to change what the
//! page shows after publication.
//!
//! # Metadata
//!
//! Images arrive here straight from the devices that made them. The account
//! owner's avatar, as served today, carries a complete EXIF block naming the
//! camera; EXIF is also where a phone writes GPS coordinates. Publishing that
//! verbatim would put the photographer's location on a public page, so
//! [`strip_metadata`] removes it before anything is written to disk — losslessly,
//! by dropping the metadata segments rather than re-encoding the pixels.
//!
//! JPEG, PNG and WebP are handled. **Any other format is stored unchanged**
//! and reported by the caller: dropping the image would be a silent hole in
//! the page, so the honest failure mode is a loud log line instead. A card
//! image uploaded as HEIC, AVIF or GIF therefore keeps whatever metadata it
//! came with.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::PathBuf;

use img_parts::{Bytes, DynImage, ImageEXIF};
use sha2::{Digest, Sha256};

/// The URL prefix assets are served under. Also the on-disk file name, minus
/// the directory: `/a/<sha256>.<ext>`.
pub const URL_PREFIX: &str = "/a/";

/// What happened to one stored image, so a sync pass can report honestly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stripped {
    /// Metadata segments were removed (or there were none to remove).
    Yes,
    /// The format is not one this crate can take apart; stored as received.
    UnknownFormat,
}

pub struct Cache {
    dir: PathBuf,
}

impl Cache {
    pub fn new(dir: impl Into<PathBuf>) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    /// Store `bytes`, returning the public path (`/a/<hash>.<ext>`) and
    /// whether metadata could be stripped.
    ///
    /// Returns `None` for anything that is not a recognized image type: this
    /// cache exists to serve pictures, and a service that will store and hand
    /// back arbitrary bytes from elsewhere is a file drop, not a mirror.
    pub fn store(
        &self,
        bytes: &[u8],
        content_type: Option<&str>,
    ) -> io::Result<Option<(String, Stripped)>> {
        let Some(extension) = extension_for(bytes, content_type) else {
            return Ok(None);
        };
        let (bytes, stripped) = strip_metadata(bytes);
        let key = format!("{}.{extension}", hex(&Sha256::digest(&bytes)));
        let path = self.dir.join(&key);
        if !path.exists() {
            // Write-then-rename: a reader can only ever see a complete file,
            // even if the process dies mid-write.
            let temporary = self.dir.join(format!(".{key}.partial"));
            fs::write(&temporary, &bytes)?;
            fs::rename(&temporary, &path)?;
        }
        Ok(Some((format!("{URL_PREFIX}{key}"), stripped)))
    }

    /// Read a stored asset. `key` is the file name from the URL — validated
    /// here rather than trusted, because it arrives from the public internet.
    pub fn read(&self, key: &str) -> Option<(Vec<u8>, &'static str)> {
        if !is_valid_key(key) {
            return None;
        }
        let bytes = fs::read(self.dir.join(key)).ok()?;
        let extension = key.rsplit_once('.')?.1;
        Some((bytes, content_type_for(extension)))
    }

    /// Delete every stored file whose key is not in `keep`, and report how
    /// many went. Called after a sync: cards get edited and images replaced,
    /// and without this the directory only ever grows.
    pub fn retain(&self, keep: &HashSet<String>) -> io::Result<usize> {
        let mut removed = 0;
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            // Leave anything unrecognized alone; only this cache's own files
            // are this cache's business.
            if !is_valid_key(name) || keep.contains(name) {
                continue;
            }
            fs::remove_file(entry.path())?;
            removed += 1;
        }
        Ok(removed)
    }
}

/// A stored asset's key is exactly `<64 hex chars>.<2-4 lowercase alphanumerics>`.
///
/// This is the check that keeps `GET /a/<key>` from becoming an arbitrary file
/// read: the key is concatenated onto a directory path, so `..` or a slash in
/// it would escape the cache.
fn is_valid_key(key: &str) -> bool {
    let Some((hash, extension)) = key.rsplit_once('.') else {
        return false;
    };
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        && (2..=4).contains(&extension.len())
        && extension
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// Remove metadata segments losslessly. Pixels are untouched: this rewrites
/// the container, it does not decode and re-encode.
pub fn strip_metadata(bytes: &[u8]) -> (Vec<u8>, Stripped) {
    let buffer = Bytes::copy_from_slice(bytes);
    match DynImage::from_bytes(buffer) {
        Ok(Some(mut image)) => {
            image.set_exif(None);
            match &mut image {
                // Beyond EXIF, each container has its own places to hide a
                // location: JPEG's other APPn segments (XMP lives in APP1
                // alongside EXIF, Photoshop's IRB in APP13) and its COM
                // comments; PNG's text chunks. The ICC profile stays — it is
                // colour information, not provenance, and dropping it shifts
                // how wide-gamut photographs render.
                DynImage::Jpeg(jpeg) => {
                    jpeg.segments_mut().retain(|segment| {
                        let marker = segment.marker();
                        let is_application = (0xE0..=0xEF).contains(&marker);
                        let is_comment = marker == 0xFE;
                        let is_jfif = marker == 0xE0;
                        let is_icc =
                            marker == 0xE2 && segment.contents().starts_with(b"ICC_PROFILE\0");
                        (!is_application && !is_comment) || is_jfif || is_icc
                    });
                }
                DynImage::Png(png) => {
                    for kind in [b"tEXt", b"zTXt", b"iTXt", b"eXIf", b"tIME"] {
                        png.remove_chunks_by_type(*kind);
                    }
                }
                DynImage::WebP(webp) => {
                    for id in [b"EXIF", b"XMP "] {
                        webp.remove_chunks_by_id(*id);
                    }
                }
            }
            (image.encoder().bytes().to_vec(), Stripped::Yes)
        }
        // Not a container img-parts knows, or a malformed one. Either way the
        // bytes are passed through unaltered and the caller says so out loud.
        Ok(None) | Err(_) => (bytes.to_vec(), Stripped::UnknownFormat),
    }
}

/// The file extension to store an image under, from the served content type,
/// falling back to the magic bytes when the header is missing or useless.
///
/// SVG is deliberately absent even though Fizzy serves one for users with no
/// uploaded avatar: an SVG is a document that can carry script, and this
/// service would be serving it from its own origin. The initials fall back to
/// text on the page instead.
fn extension_for(bytes: &[u8], content_type: Option<&str>) -> Option<&'static str> {
    let by_header = match content_type.map(str::trim) {
        Some("image/jpeg" | "image/jpg") => Some("jpg"),
        Some("image/png") => Some("png"),
        Some("image/gif") => Some("gif"),
        Some("image/webp") => Some("webp"),
        Some("image/avif") => Some("avif"),
        _ => None,
    };
    by_header.or_else(|| sniff(bytes))
}

fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("gif")
    } else if bytes.len() > 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else {
        None
    }
}

fn content_type_for(extension: &str) -> &'static str {
    match extension {
        "jpg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        // Unreachable for anything `store` wrote; a conservative default keeps
        // a stray file from being interpreted as markup.
        _ => "application/octet-stream",
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";

    #[test]
    fn keys_are_hashes_and_nothing_else() {
        assert!(is_valid_key(&format!("{}.png", "a".repeat(64))));
        assert!(!is_valid_key(&format!("{}.png", "A".repeat(64))));
        assert!(!is_valid_key("../../etc/passwd"));
        assert!(!is_valid_key("short.png"));
        assert!(!is_valid_key(&format!("{}.p/g", "a".repeat(64))));
        assert!(!is_valid_key(&format!("{}.png", "z".repeat(64))));
    }

    #[test]
    fn identifies_images_by_header_then_by_magic() {
        assert_eq!(extension_for(b"", Some("image/jpeg")), Some("jpg"));
        assert_eq!(extension_for(PNG, None), Some("png"));
        assert_eq!(
            extension_for(PNG, Some("application/octet-stream")),
            Some("png")
        );
        assert_eq!(extension_for(b"<svg/>", Some("image/svg+xml")), None);
        assert_eq!(extension_for(b"not an image", None), None);
    }

    #[test]
    fn refuses_to_store_non_images() {
        let dir = std::env::temp_dir().join(format!("mirror-test-{}", std::process::id()));
        let cache = Cache::new(&dir).unwrap();
        assert!(cache.store(b"<html>", Some("text/html")).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stores_addressed_by_content() {
        let dir = std::env::temp_dir().join(format!("mirror-store-{}", std::process::id()));
        let cache = Cache::new(&dir).unwrap();
        let (first, _) = cache.store(PNG, Some("image/png")).unwrap().unwrap();
        let (second, _) = cache.store(PNG, Some("image/png")).unwrap().unwrap();
        assert_eq!(first, second, "identical bytes must land on one file");
        assert!(first.starts_with(URL_PREFIX));

        let key = first.trim_start_matches(URL_PREFIX);
        let (read, content_type) = cache.read(key).unwrap();
        assert_eq!(read, PNG);
        assert_eq!(content_type, "image/png");
        assert!(cache.read("../../etc/passwd").is_none());

        assert_eq!(cache.retain(&HashSet::new()).unwrap(), 1);
        assert!(cache.read(key).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_formats_pass_through_and_say_so() {
        let (bytes, stripped) = strip_metadata(b"GIF89a nonsense");
        assert_eq!(bytes, b"GIF89a nonsense");
        assert_eq!(stripped, Stripped::UnknownFormat);
    }
}
