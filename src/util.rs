// Converts a `u64` to `f64` without an `as` cast. Values above `u32::MAX` saturate
// instead of losing precision silently, which is precise enough for term/doc counters.
pub fn u64_to_f64_lossy(value: u64) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
}

// Converts `Metadata::modified()` into seconds since the Unix epoch, cross-platform (unlike
// `MetadataExt::mtime()`, which is Unix-only). Times before the epoch (clock skew, exotic
// filesystems) collapse to 0 rather than failing indexing over a bad mtime.
pub fn modified_secs(metadata: &std::fs::Metadata) -> u64 {
    metadata
        .modified()
        .and_then(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .map_err(std::io::Error::other)
        })
        .map_or(0, |d| d.as_secs())
}

// A platform file identifier stable across renames/moves within the same filesystem — unlike
// a path, which changes on rename. (device, inode) on Unix, (volume serial number, file
// index) on Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileId {
    device: u64,
    file_index: u64,
}

impl FileId {
    pub fn to_bytes(self) -> [u8; 16] {
        let mut bytes = [0u8; 16];
        bytes[..8].copy_from_slice(&self.device.to_be_bytes());
        bytes[8..].copy_from_slice(&self.file_index.to_be_bytes());
        bytes
    }

    // Reconstructs a `FileId` from bytes written by `to_bytes` — e.g. a raw key read back out
    // of the `file_ids` tree. Returns `None` if `bytes` isn't exactly 16 bytes, which would
    // mean the tree holds something other than what we wrote.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let device = u64::from_be_bytes(bytes.get(..8)?.try_into().ok()?);
        let file_index = u64::from_be_bytes(bytes.get(8..16)?.try_into().ok()?);
        Some(Self { device, file_index })
    }
}

// Returns `None` when the platform/filesystem can't provide a stable identifier (rare, but
// possible on some Windows volumes); callers fall back to path-based detection in that case.
// `Option` is always `Some` on Unix — kept for a signature shared with the fallible Windows
// variant below, since callers are cfg-agnostic.
#[cfg(unix)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "Option is required by the Windows variant of this cfg-gated function"
)]
pub fn file_id(metadata: &std::fs::Metadata) -> Option<FileId> {
    use std::os::unix::fs::MetadataExt;
    Some(FileId {
        device: metadata.dev(),
        file_index: metadata.ino(),
    })
}

#[cfg(windows)]
pub fn file_id(metadata: &std::fs::Metadata) -> Option<FileId> {
    use std::os::windows::fs::MetadataExt;
    Some(FileId {
        device: u64::from(metadata.volume_serial_number()?),
        file_index: metadata.file_index()?,
    })
}

#[cfg(not(any(unix, windows)))]
pub fn file_id(_metadata: &std::fs::Metadata) -> Option<FileId> {
    None
}
