//! Low-level file handling: the store lock, durable single-line appends and
//! tolerant JSONL reads. Private to the crate.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::settings::IntakeError;

const LOCK_WAIT: Duration = Duration::from_secs(10);

fn io<'a>(op: &'static str, path: &'a Path) -> impl FnOnce(std::io::Error) -> IntakeError + 'a {
    move |source| IntakeError::Io {
        op,
        path: path.to_path_buf(),
        source,
    }
}

/// Create the store directory (mode 0700 on unix).
pub(crate) fn ensure_dir(dir: &Path) -> Result<(), IntakeError> {
    std::fs::create_dir_all(dir).map_err(io("create directory", dir))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(io("restrict permissions of", dir))?;
    }
    Ok(())
}

/// Open options for a store file: create, mode 0600 on unix.
pub(crate) fn private_options() -> OpenOptions {
    let mut o = OpenOptions::new();
    o.create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o
}

/// Exclusive advisory lock on `<dir>/.lock`, released on drop. Same
/// `flock(2)` convention as the session action audit log, but polled so a
/// wedged holder produces an actionable timeout instead of a silent hang.
pub(crate) struct StoreLock {
    _file: File,
}

impl StoreLock {
    pub(crate) fn acquire(dir: &Path) -> Result<StoreLock, IntakeError> {
        ensure_dir(dir)?;
        let path = dir.join(".lock");
        let file = private_options()
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(io("open lock file", &path))?;
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let start = Instant::now();
            loop {
                // SAFETY: the fd is valid for the lifetime of `file`.
                let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
                if rc == 0 {
                    break;
                }
                let err = std::io::Error::last_os_error();
                if err.kind() != std::io::ErrorKind::WouldBlock
                    && err.raw_os_error() != Some(libc::EINTR)
                {
                    return Err(IntakeError::Io {
                        op: "lock",
                        path,
                        source: err,
                    });
                }
                if start.elapsed() >= LOCK_WAIT {
                    return Err(IntakeError::LockTimeout {
                        path,
                        waited_secs: LOCK_WAIT.as_secs(),
                    });
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        #[cfg(not(unix))]
        {
            let _ = (Instant::now(), LOCK_WAIT);
        }
        Ok(StoreLock { _file: file })
    }
}

/// Append exactly one JSON line with a single `write_all` and fsync it.
///
/// If a previous writer died mid-line the file does not end in `\n`; the
/// terminating newline is prepended inside the same write so the new record
/// can never be glued to the torn fragment. Callers must hold the store lock.
pub(crate) fn append_line<T: Serialize>(
    path: &Path,
    what: &'static str,
    value: &T,
    max_bytes: usize,
) -> Result<(), IntakeError> {
    let mut line =
        serde_json::to_string(value).map_err(|source| IntakeError::Serialize { what, source })?;
    if line.len() > max_bytes {
        return Err(IntakeError::TooLarge {
            what,
            bytes: line.len(),
            max: max_bytes,
        });
    }
    line.push('\n');
    let mut file = private_options()
        .read(true)
        .append(true)
        .open(path)
        .map_err(io("open", path))?;
    let mut buf = Vec::with_capacity(line.len() + 1);
    if ends_mid_line(&mut file).map_err(io("inspect", path))? {
        buf.push(b'\n');
    }
    buf.extend_from_slice(line.as_bytes());
    file.write_all(&buf).map_err(io("append to", path))?;
    file.sync_data().map_err(io("fsync", path))?;
    Ok(())
}

fn ends_mid_line(file: &mut File) -> std::io::Result<bool> {
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(false);
    }
    file.seek(SeekFrom::Start(len - 1))?;
    let mut last = [0u8; 1];
    file.read_exact(&mut last)?;
    Ok(last[0] != b'\n')
}

/// Parsed lines plus notes about what was skipped.
pub(crate) struct JsonlRead<T> {
    pub(crate) items: Vec<T>,
    pub(crate) warnings: Vec<String>,
}

/// Read a JSONL file tolerantly: a missing file is empty; a trailing line
/// that does not parse (a torn append) is skipped; unparseable interior lines
/// are skipped and counted. Nothing here ever fails on bad content.
pub(crate) fn read_jsonl<T: DeserializeOwned>(path: &Path) -> Result<JsonlRead<T>, IntakeError> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(JsonlRead {
                items: Vec::new(),
                warnings: Vec::new(),
            })
        }
        Err(e) => return Err(io("read", path)(e)),
    };
    let ends_clean = bytes.last().is_none_or(|b| *b == b'\n');
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.split('\n').collect();
    let mut items = Vec::new();
    let mut warnings = Vec::new();
    let mut corrupt = 0usize;
    for (i, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let is_last_segment = i + 1 == lines.len();
        match serde_json::from_str::<T>(line) {
            Ok(v) => items.push(v),
            Err(_) if is_last_segment && !ends_clean => {
                tracing::warn!(path = %path.display(), bytes = line.len(), "skipping torn trailing line");
                warnings.push(format!(
                    "{}: skipped a torn trailing line ({} bytes) left by an interrupted append",
                    path.display(),
                    line.len()
                ));
            }
            Err(_) => corrupt += 1,
        }
    }
    if corrupt > 0 {
        tracing::warn!(path = %path.display(), corrupt, "skipping unparseable lines");
        warnings.push(format!(
            "{}: skipped {corrupt} unparseable line(s)",
            path.display()
        ));
    }
    Ok(JsonlRead { items, warnings })
}

/// Paths of one store directory.
#[derive(Debug, Clone)]
pub(crate) struct Layout {
    pub(crate) dir: PathBuf,
}

impl Layout {
    pub(crate) fn queue(&self) -> PathBuf {
        self.dir.join("queue.jsonl")
    }
    pub(crate) fn ledger(&self) -> PathBuf {
        self.dir.join("ledger.jsonl")
    }
    pub(crate) fn batch(&self, id: &str) -> PathBuf {
        self.dir.join(format!("batch-{id}.jsonl"))
    }
}
