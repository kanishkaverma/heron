//! The two append-only streams (SPEC §2): `chat/main/*.jsonl` (messages) and
//! `chat/tree/*.jsonl` (nodes), one fsynced line per record, one local-day
//! file per stream, and the single-writer lock.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::memory::{Memory, Message, Node, VIEW};

pub struct Store {
    dir: PathBuf,
    /// Serializes appends so lines land whole and in order.
    write: Mutex<()>,
    _lock: Lock,
}

/// What loading found besides the memory: torn lines skipped, files repaired.
pub type Reports = Vec<String>;

impl Store {
    /// Take the lock, repair torn files, and fold the memory from message 0.
    pub fn open(home: &Path) -> Result<(Store, Memory, Reports), String> {
        let dir = home.join("chat");
        for sub in ["main", "tree"] {
            std::fs::create_dir_all(dir.join(sub))
                .map_err(|e| format!("create {}: {e}", dir.join(sub).display()))?;
        }
        let lock = Lock::take(&dir.join("lock"))?;
        let (memory, reports) = load(&dir, true)?;
        Ok((
            Store {
                dir,
                write: Mutex::new(()),
                _lock: lock,
            },
            memory,
            reports,
        ))
    }

    pub fn append_message(&self, message: &Message) -> Result<(), String> {
        self.append("main", message)
    }

    pub fn append_node(&self, node: &Node) -> Result<(), String> {
        self.append("tree", node)
    }

    fn append(&self, stream: &str, record: &impl serde::Serialize) -> Result<(), String> {
        let mut line = serde_json::to_string(record).map_err(|e| e.to_string())?;
        line.push('\n');
        let day = chrono::Local::now().format("%Y-%m-%d");
        let path = self.dir.join(stream).join(format!("{day}.jsonl"));
        let _guard = self.write.lock().unwrap_or_else(|e| e.into_inner());
        let created = !path.exists();
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("open {}: {e}", path.display()))?;
        // One write, then fsync, before returning (SPEC §2 durability).
        file.write_all(line.as_bytes())
            .and_then(|()| file.sync_data())
            .map_err(|e| format!("write {}: {e}", path.display()))?;
        if created {
            sync_dir(&self.dir.join(stream));
        }
        Ok(())
    }
}

fn sync_dir(dir: &Path) {
    if let Ok(handle) = File::open(dir) {
        let _ = handle.sync_all();
    }
}

fn day_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(format!("read {}: {e}", dir.display())),
    };
    files.sort();
    Ok(files)
}

/// Parse every line of one stream. A line that is not valid JSON (a crash
/// mid-write) is reported and skipped; with `repair`, a file not ending in
/// a newline gets one, so the next append starts on its own line.
fn read_stream<T: serde::de::DeserializeOwned>(
    dir: &Path,
    repair: bool,
    reports: &mut Reports,
) -> Result<Vec<T>, String> {
    let mut out = Vec::new();
    for path in day_files(dir)? {
        let bytes = std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        if repair && !bytes.is_empty() && !bytes.ends_with(b"\n") {
            OpenOptions::new()
                .append(true)
                .open(&path)
                .and_then(|mut f| f.write_all(b"\n").and_then(|()| f.sync_data()))
                .map_err(|e| format!("repair {}: {e}", path.display()))?;
            reports.push(format!("{}: added the missing final newline", path.display()));
        }
        for (n, line) in bytes.split(|b| *b == b'\n').enumerate() {
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            match serde_json::from_slice::<T>(line) {
                Ok(record) => out.push(record),
                Err(err) => reports.push(format!(
                    "{}:{}: torn line skipped ({err})",
                    path.display(),
                    n + 1
                )),
            }
        }
    }
    Ok(out)
}

/// Fold the memory from message 0 (SPEC §5.2 "At load"). `repair` only when
/// holding the lock.
pub fn load(dir: &Path, repair: bool) -> Result<(Memory, Reports), String> {
    let mut reports = Reports::new();
    let mut messages: Vec<Message> = read_stream(&dir.join("main"), repair, &mut reports)?;
    let nodes: Vec<Node> = read_stream(&dir.join("tree"), repair, &mut reports)?;
    messages.sort_by_key(|m| m.i);
    let mut memory = Memory::default();
    for node in nodes {
        memory.insert(node);
    }
    for message in messages {
        if message.i < memory.len() {
            reports.push(format!("message {} appears twice; kept the first", message.i));
            continue;
        }
        if message.i > memory.len() {
            return Err(format!(
                "the log jumps from message {} to {}: refusing to renumber history",
                memory.len(),
                message.i
            ));
        }
        memory.append(message)?;
        memory.fit(VIEW);
    }
    for report in &reports {
        tracing::warn!(target: "optchat", "{report}");
    }
    Ok((memory, reports))
}

/// The single-writer lock (SPEC §2): listen on a Unix socket for the life of
/// the process. A second process that can connect to it gives up; a socket
/// that refuses connections is stale (its owner died) and is taken over.
pub struct Lock {
    #[cfg(unix)]
    path: PathBuf,
    #[cfg(unix)]
    _listener: std::os::unix::net::UnixListener,
}

impl Lock {
    #[cfg(unix)]
    pub fn take(path: &Path) -> Result<Lock, String> {
        use std::os::unix::net::{UnixListener, UnixStream};
        let path = socket_path(path);
        for _ in 0..2 {
            match UnixListener::bind(&path) {
                Ok(listener) => {
                    return Ok(Lock {
                        path,
                        _listener: listener,
                    });
                }
                Err(err) if err.kind() == std::io::ErrorKind::AddrInUse => {
                    if UnixStream::connect(&path).is_ok() {
                        return Err(format!(
                            "another process is using this OptChat memory (lock {})",
                            path.display()
                        ));
                    }
                    let _ = std::fs::remove_file(&path);
                }
                Err(err) => return Err(format!("lock {}: {err}", path.display())),
            }
        }
        Err(format!("could not take the lock {}", path.display()))
    }

    #[cfg(not(unix))]
    pub fn take(_path: &Path) -> Result<Lock, String> {
        Ok(Lock {})
    }
}

/// `sun_path` holds ~104 bytes on macOS. A longer home (an app-support
/// directory) gets a socket in the temp dir named by a hash of the real path,
/// so every process on the same chat still meets at the same address.
#[cfg(unix)]
fn socket_path(path: &Path) -> PathBuf {
    use sha2::Digest;
    use std::os::unix::ffi::OsStrExt;
    if path.as_os_str().len() < 100 {
        return path.to_path_buf();
    }
    let digest = sha2::Sha256::digest(path.as_os_str().as_bytes());
    let name: String = digest[..12].iter().map(|b| format!("{b:02x}")).collect();
    std::env::temp_dir().join(format!("optchat-{name}.lock"))
}

#[cfg(unix)]
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
