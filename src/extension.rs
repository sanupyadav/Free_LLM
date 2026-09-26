//! Embedded distribution of the browser extension.
//!
//! Extension files are baked into the binary at compile time with `include_str!` / `include_bytes!`,
//! and packaged into a zip on demand at runtime. This way, even a user who only has the single
//! executable (no source tree) can get a working one-click-login extension from the panel's "Download extension".
//!
//! The zip is hand-written in **stored (uncompressed)** mode: the extension is only a few dozen KB total, so skipping a compression library dependency is worth it.

/// Text files (zip path -> content)
const TEXT_FILES: &[(&str, &str)] = &[
    (
        "manifest.json",
        include_str!("../browser-extension/manifest.json"),
    ),
    (
        "background.js",
        include_str!("../browser-extension/background.js"),
    ),
    ("bridge.js", include_str!("../browser-extension/bridge.js")),
    (
        "options.html",
        include_str!("../browser-extension/options.html"),
    ),
    (
        "options.js",
        include_str!("../browser-extension/options.js"),
    ),
    ("README.md", include_str!("../browser-extension/README.md")),
];

/// Binary files
const BIN_FILES: &[(&str, &[u8])] = &[(
    "icons/icon128.png",
    include_bytes!("../browser-extension/icons/icon128.png"),
)];

/// Extension version (parsed from the embedded manifest.json, to avoid drifting out of sync with the extension itself)
pub fn version() -> String {
    let manifest = TEXT_FILES
        .iter()
        .find(|(n, _)| *n == "manifest.json")
        .map(|(_, c)| *c)
        .unwrap_or("");
    regex::Regex::new(r#""version"\s*:\s*"([^"]+)""#)
        .ok()
        .and_then(|re| re.captures(manifest))
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|| "unknown".into())
}

/// CRC-32 (IEEE 802.3, required by zip); bitwise implementation, negligible overhead at this data size (tens of KB)
fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// DOS timestamp for the current time (time, date)
fn dos_datetime() -> (u16, u16) {
    use chrono::{Datelike, Timelike};
    let now = chrono::Local::now();
    let year = (now.year().clamp(1980, 2107) - 1980) as u16;
    let date = (year << 9) | ((now.month() as u16) << 5) | (now.day() as u16);
    let time =
        ((now.hour() as u16) << 11) | ((now.minute() as u16) << 5) | ((now.second() as u16) / 2);
    (time, date)
}

/// Packages the embedded extension files into a zip (stored mode)
pub fn build_zip() -> Vec<u8> {
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    for (name, content) in TEXT_FILES {
        entries.push(((*name).to_string(), content.as_bytes().to_vec()));
    }
    for (name, bytes) in BIN_FILES {
        entries.push(((*name).to_string(), bytes.to_vec()));
    }

    let (dos_time, dos_date) = dos_datetime();
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    let count = entries.len() as u16;

    for (name, data) in &entries {
        let name_bytes = name.as_bytes();
        let crc = crc32(data);
        let size = data.len() as u32;
        let local_offset = out.len() as u32;

        // ---- Local file header ----
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&0u16.to_le_bytes()); // method 0 = stored
        out.extend_from_slice(&dos_time.to_le_bytes());
        out.extend_from_slice(&dos_date.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes()); // compressed
        out.extend_from_slice(&size.to_le_bytes()); // uncompressed
        out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra len
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(data);

        // ---- Central directory entry ----
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes()); // version made by
        central.extend_from_slice(&20u16.to_le_bytes()); // version needed
        central.extend_from_slice(&0u16.to_le_bytes()); // flags
        central.extend_from_slice(&0u16.to_le_bytes()); // method
        central.extend_from_slice(&dos_time.to_le_bytes());
        central.extend_from_slice(&dos_date.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes()); // extra
        central.extend_from_slice(&0u16.to_le_bytes()); // comment
        central.extend_from_slice(&0u16.to_le_bytes()); // disk number
        central.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        central.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        central.extend_from_slice(&local_offset.to_le_bytes());
        central.extend_from_slice(name_bytes);
    }

    let central_offset = out.len() as u32;
    let central_size = central.len() as u32;
    out.extend_from_slice(&central);

    // ---- End of central directory ----
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // this disk
    out.extend_from_slice(&0u16.to_le_bytes()); // disk with central dir
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&central_size.to_le_bytes());
    out.extend_from_slice(&central_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // comment len
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_known_vectors() {
        assert_eq!(crc32(b""), 0x0000_0000);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn zip_has_magic_and_eocd() {
        let z = build_zip();
        assert!(z.len() > 1000, "zip too small, extension files may not have been packed in");
        assert_eq!(
            &z[0..4],
            &[0x50, 0x4b, 0x03, 0x04],
            "missing zip local file header magic"
        );
        let eocd = &z[z.len() - 22..];
        assert_eq!(&eocd[0..4], &[0x50, 0x4b, 0x05, 0x06], "missing EOCD magic");
    }

    #[test]
    fn zip_contains_every_extension_file() {
        let z = build_zip();
        let text = String::from_utf8_lossy(&z);
        for (name, _) in TEXT_FILES {
            assert!(text.contains(name), "zip is missing {name}");
        }
        assert!(text.contains("icons/icon128.png"), "zip is missing the icon");
    }

    #[test]
    fn zip_entry_count_matches() {
        let z = build_zip();
        let eocd = &z[z.len() - 22..];
        let total = u16::from_le_bytes([eocd[10], eocd[11]]);
        assert_eq!(total as usize, TEXT_FILES.len() + BIN_FILES.len());
    }

    #[test]
    fn version_is_parsed_from_manifest() {
        let v = version();
        assert_ne!(v, "unknown", "failed to parse version from embedded manifest.json");
        assert!(
            v.chars().next().unwrap().is_ascii_digit(),
            "unexpected version format: {v}"
        );
    }
}
