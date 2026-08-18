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
// variant below, since callers are cfg-agnostic. Takes `path` alongside `metadata` because the
// Windows variant needs to reopen the file itself (see below); Unix reads everything it needs
// off `metadata`.
#[cfg(unix)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "Option is required by the Windows variant of this cfg-gated function"
)]
pub fn file_id(_path: &std::path::Path, metadata: &std::fs::Metadata) -> Option<FileId> {
    use std::os::unix::fs::MetadataExt;
    Some(FileId {
        device: metadata.dev(),
        file_index: metadata.ino(),
    })
}

// `std::fs::Metadata` on Windows already carries the volume serial number and file index
// internally (that's what `std::os::windows::fs::MetadataExt::volume_serial_number`/
// `file_index` read), but those accessors are still gated behind the unstable
// `windows_by_handle` feature — unusable on stable Rust. So instead of reading them off
// `metadata`, this reopens `path` itself and asks Windows directly via
// `GetFileInformationByHandle`, the same underlying API std uses. `dwDesiredAccess: 0` opens
// a handle that can only be used to query metadata, not read/write the file's contents — the
// same trick `std::fs::metadata` itself relies on, and it's why this doesn't need any actual
// file permissions the OS-level walk hasn't already implied. `FILE_FLAG_BACKUP_SEMANTICS` is
// required to open a directory handle at all, not just files.
#[cfg(windows)]
pub fn file_id(path: &std::path::Path, _metadata: &std::fs::Metadata) -> Option<FileId> {
    use std::{os::windows::ffi::OsStrExt, ptr};

    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle,
            OPEN_EXISTING,
        },
    };

    let wide_path: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: `wide_path` is a valid null-terminated UTF-16 string for the duration of this
    // call. `handle` is checked against `INVALID_HANDLE_VALUE` before being passed to
    // `GetFileInformationByHandle`, and is always closed exactly once before returning.
    unsafe {
        let handle: HANDLE = CreateFileW(
            wide_path.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            0,
        );

        if handle == INVALID_HANDLE_VALUE {
            return None;
        }

        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        let succeeded = GetFileInformationByHandle(handle, &raw mut info) != 0;
        CloseHandle(handle);

        succeeded.then(|| FileId {
            device: u64::from(info.dwVolumeSerialNumber),
            file_index: u64::from(info.nFileIndexHigh).wrapping_shl(32)
                | u64::from(info.nFileIndexLow),
        })
    }
}

#[cfg(not(any(unix, windows)))]
pub fn file_id(_path: &std::path::Path, _metadata: &std::fs::Metadata) -> Option<FileId> {
    None
}
