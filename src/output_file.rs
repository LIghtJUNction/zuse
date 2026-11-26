use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);

// Linking a finished, closed file publishes it atomically and fails if another
// installer already owns the destination. Staging in the same directory keeps
// both names on one filesystem.
pub(crate) fn publish(destination: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = destination.parent().unwrap_or(Path::new("."));
    for _ in 0..32 {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let tmp = dir.join(format!(".install-{}-{id}", std::process::id()));
        let mut open = OpenOptions::new();
        open.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            open.mode(0o600);
        }
        let mut file = match open.open(&tmp) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        let write = file.write_all(bytes).and_then(|()| file.sync_all());
        drop(file);
        let result = write.and_then(|()| fs::hard_link(&tmp, destination));
        let _ = fs::remove_file(tmp);
        return result;
    }
    Err(io::Error::other(
        "could not allocate a temporary output file",
    ))
}
