//! Language intelligence. `hub` routes documents to language-server
//! processes (`client`) — both gpui-free by design, so the identical code
//! serves the GUI's `LocalWorkspace` today and the headless ide-server in
//! milestone 5 (docs/remote.md). The UI reaches all of it exclusively
//! through the `WorkspaceService` trait; `providers` adapts that trait onto
//! gpui-component's editor provider interfaces.

pub mod client;
pub mod hub;

use std::path::{Path, PathBuf};
use std::str::FromStr as _;

/// file:// URI from an absolute path, percent-encoding what the RFC requires.
pub fn path_to_uri(path: &Path) -> lsp_types::Uri {
    let mut encoded = String::from("file://");
    for byte in path.to_string_lossy().as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(*byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    lsp_types::Uri::from_str(&encoded).expect("percent-encoded absolute path is a valid uri")
}

/// Path from a file:// URI; `None` for other schemes.
pub fn uri_to_path(uri: &lsp_types::Uri) -> Option<PathBuf> {
    if uri.scheme().map(|s| s.as_str()) != Some("file") {
        return None;
    }
    let raw = uri.path().as_str();
    let mut bytes = Vec::with_capacity(raw.len());
    let mut rest = raw.bytes();
    while let Some(byte) = rest.next() {
        if byte == b'%' {
            let hi = rest.next()?;
            let lo = rest.next()?;
            let hex = [hi, lo];
            let hex = std::str::from_utf8(&hex).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
        } else {
            bytes.push(byte);
        }
    }
    Some(PathBuf::from(String::from_utf8(bytes).ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_round_trip_plain() {
        let path = Path::new("/home/deepwater/code/fleet/tugboat/src/main.rs");
        let uri = path_to_uri(path);
        assert_eq!(
            uri.to_string(),
            "file:///home/deepwater/code/fleet/tugboat/src/main.rs"
        );
        assert_eq!(uri_to_path(&uri).as_deref(), Some(path));
    }

    #[test]
    fn uri_round_trip_special_chars() {
        let path = Path::new("/tmp/with space/héllo.rs");
        let uri = path_to_uri(path);
        assert_eq!(uri_to_path(&uri).as_deref(), Some(path));
    }

    #[test]
    fn non_file_uri_is_not_a_path() {
        let uri = lsp_types::Uri::from_str("https://doc.rust-lang.org/std/").unwrap();
        assert_eq!(uri_to_path(&uri), None);
    }
}
