//! Advisory target-type check for client-side socket paths.
//!
//! Lexical validation lives in [`crate::path_policy`]; this module
//! owns the filesystem-metadata sanity check the client runs before
//! calling `UnixStream::connect`. Rejecting a regular file or
//! directory at the target path catches adapter-side misconfiguration
//! loudly instead of letting `connect` bury it under a generic
//! `ConnectionRefused` or `NotADirectory`.
//!
//! This is intentionally *advisory*: the kernel's `connect` is the
//! real source of truth. The check exists only to turn obvious wrong
//! shapes into immediate, readable errors.

use std::path::Path;

use crate::error::IpcError;

/// Reject non-socket targets at `path` (if the path exists at all).
/// Missing paths fall through so `UnixStream::connect` can surface a
/// real `NotFound` / `ConnectionRefused`.
pub(super) fn validate_client_target_type(path: &Path) -> Result<(), IpcError> {
    #[cfg(unix)]
    {
        use std::io;
        use std::os::unix::fs::FileTypeExt;

        match std::fs::symlink_metadata(path) {
            Ok(meta) => {
                let file_type = meta.file_type();
                // Symlinks get resolved by `connect`; sockets are the
                // expected happy path; everything else is wrong.
                if file_type.is_socket() || file_type.is_symlink() {
                    Ok(())
                } else {
                    Err(IpcError::Io(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!(
                            "socket path is not a Unix socket (is {}): {}",
                            describe_file_type(file_type),
                            path.display()
                        ),
                    )))
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(IpcError::Io(e)),
        }
    }

    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(unix)]
fn describe_file_type(file_type: std::fs::FileType) -> &'static str {
    use std::os::unix::fs::FileTypeExt;
    if file_type.is_file() {
        "regular file"
    } else if file_type.is_dir() {
        "directory"
    } else if file_type.is_fifo() {
        "FIFO"
    } else if file_type.is_block_device() {
        "block device"
    } else if file_type.is_char_device() {
        "character device"
    } else {
        "non-socket filesystem object"
    }
}
