use std::path::Path;

use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{database::FileType, util::u64_to_f64_lossy};

const SIZE_UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];

// Human-readable file size (`1536` -> `"1.5 KB"`), stepping through `SIZE_UNITS` by dividing
// in `f64` (integer division would need `checked_div`/`saturating_div` under this crate's
// `arithmetic_side_effects` lint, and loses the fractional part we want to show anyway).
pub fn size(bytes: u64) -> String {
    let mut value = u64_to_f64_lossy(bytes);
    let mut unit = SIZE_UNITS.first().copied().unwrap_or("B");

    for candidate in SIZE_UNITS.iter().skip(1) {
        if value < 1024.0 {
            break;
        }
        value /= 1024.0;
        unit = candidate;
    }

    if unit == "B" {
        format!("{value:.0} {unit}")
    } else {
        format!("{value:.1} {unit}")
    }
}

// Formats a unix timestamp (seconds) as `YYYY-MM-DD HH:MM`, falling back to a placeholder for
// anything that doesn't fit in the `time` crate's range (also covers the `modified: 0`
// sentinel `util::modified_secs` returns when a file's mtime couldn't be read).
pub fn modified(secs: u64) -> String {
    i64::try_from(secs)
        .ok()
        .and_then(|secs| OffsetDateTime::from_unix_timestamp(secs).ok())
        .and_then(|datetime| datetime.format(&Rfc3339).ok())
        .map_or_else(
            || "unknown".to_string(),
            |formatted| formatted.replace('T', " ").replace('Z', ""),
        )
}

// File extension without the leading dot, uppercased for display (`"txt"` -> `"TXT"`); files
// with no extension show as the file kind instead.
pub fn extension(path: &Path) -> Option<String> {
    path.extension()
        .map(|ext| ext.to_string_lossy().to_uppercase())
}

pub const fn kind_label(kind: FileType) -> &'static str {
    match kind {
        FileType::Text => "Text",
        FileType::Image => "Image",
        FileType::Binary => "Binary",
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{extension, kind_label, modified, size};
    use crate::database::FileType;

    #[test]
    fn size_stays_in_bytes_below_the_first_threshold() {
        assert_eq!(size(512), "512 B");
    }

    #[test]
    fn size_steps_up_to_kilobytes() {
        assert_eq!(size(1536), "1.5 KB");
    }

    #[test]
    fn size_steps_up_to_megabytes() {
        assert_eq!(size(5_242_880), "5.0 MB");
    }

    #[test]
    fn modified_formats_a_known_timestamp() {
        assert_eq!(modified(1_700_000_000), "2023-11-14 22:13:20");
    }

    #[test]
    fn modified_falls_back_for_the_unknown_sentinel() {
        assert_eq!(modified(0), "1970-01-01 00:00:00");
    }

    #[test]
    fn extension_uppercases_and_drops_the_dot() {
        assert_eq!(extension(Path::new("report.pdf")), Some("PDF".to_string()));
    }

    #[test]
    fn extension_is_none_without_one() {
        assert_eq!(extension(Path::new("README")), None);
    }

    #[test]
    fn kind_label_covers_every_variant() {
        assert_eq!(kind_label(FileType::Text), "Text");
        assert_eq!(kind_label(FileType::Image), "Image");
        assert_eq!(kind_label(FileType::Binary), "Binary");
    }
}
