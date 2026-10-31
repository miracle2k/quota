//! Small file helpers shared by the credential writers.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Replace `path` with `content` in one step, readable by the owner only.
/// The new text goes to a temporary file beside it first, so a crash or a
/// concurrent reader never sees half a file. A symlink is followed, so the file
/// it points at is updated instead of the link being replaced.
pub fn write_private_atomic(path: &Path, content: &str) -> Result<(), String> {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    let temp_path = parent.join(format!(".{}.quota-{}.tmp", name, std::process::id()));

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let written = options.open(&temp_path).and_then(|mut file| {
        file.write_all(content.as_bytes())?;
        file.sync_all()
    });
    if let Err(err) = written {
        let _ = std::fs::remove_file(&temp_path);
        return Err(format!("could not write {}: {}", path.display(), err));
    }

    // On Windows a scanner or indexer holding the file open makes the rename
    // fail for a moment, so it is worth a few more tries.
    let mut attempts = 0;
    loop {
        match std::fs::rename(&temp_path, &path) {
            Ok(()) => return Ok(()),
            Err(_) if attempts < 10 => {
                attempts += 1;
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(err) => {
                let _ = std::fs::remove_file(&temp_path);
                return Err(format!("could not replace {}: {}", path.display(), err));
            }
        }
    }
}

/// Keep text that must not be lost, such as a renewed login whose own store
/// refused the write, in `<name>.quota-renewed` beside `path`. Returns where it
/// went.
pub fn save_recovery_copy(path: &Path, content: &str) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    let copy = path.with_file_name(format!("{}.quota-renewed", name));
    write_private_atomic(&copy, content).ok().map(|()| copy)
}
