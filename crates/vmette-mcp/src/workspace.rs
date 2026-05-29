//! Workspace state held by the MCP server across tool calls.
//!
//! A workspace is a host directory the server creates and owns; the
//! agent gets a handle (`workspace_id`) and a small set of tools that
//! manipulate it (write files, read files, run shell commands inside
//! a microVM with the dir mounted as `/mnt/work`).
//!
//! Lifetime: per-MCP-session. The `WorkspaceState` owns a base directory
//! under `$TMPDIR/vmette-mcp-<pid>/`; each workspace gets a UUID subdir.
//! Dropping `WorkspaceState` removes the whole base dir on graceful
//! exit. For ungraceful exits (SIGKILL, panic-abort) a startup reaper
//! removes orphaned dirs whose owning PID is gone or whose mtime is
//! older than 24 hours.
//!
//! Path safety: [`open_for_write`] and [`open_for_read`] use an
//! `openat`-walk with `O_DIRECTORY | O_NOFOLLOW` at every component.
//! This closes the nested-path-symlink race that a naive
//! `safe_join + final-component O_NOFOLLOW` leaves open — an agent
//! that creates `ws/a -> /etc` via `workspace_run` cannot trick
//! `workspace_write` into following the symlink during
//! `create_dir_all` of `ws/a/b/c`, because each path component is
//! opened by inode, not re-resolved by name.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, Context, Result};
use nix::errno::Errno;
use nix::fcntl::{open, openat, OFlag};
use nix::sys::stat::{mkdirat, Mode};
use nix::unistd::Pid;
use tracing::debug;

/// One agent-visible workspace: a host dir + the runtime config used
/// when the agent invokes `workspace_run` against it.
#[derive(Debug, Clone)]
pub struct Workspace {
    pub id: String,
    pub dir: PathBuf,
    pub image: String,
    pub net: bool,
}

/// Process-wide registry of open workspaces. Thread-safe.
pub struct WorkspaceState {
    base_dir: PathBuf,
    cap: usize,
    inner: Mutex<HashMap<String, Workspace>>,
}

impl WorkspaceState {
    /// Initialise under a fresh per-instance subdir of TMPDIR. Also
    /// reaps any orphan `vmette-mcp-<pid>-<nonce>/` dirs from previous
    /// runs. For long-running servers, call [`reap_orphans`]
    /// periodically from a background task too — startup-only would
    /// let peer instances accumulate during this server's lifetime.
    ///
    /// The base-dir name includes both the PID (for the reaper's
    /// liveness check) AND a random nonce. The nonce defends against:
    /// (1) concurrent tests in the same process from clobbering each
    ///     other when one test's Drop runs while another is still
    ///     creating workspaces, and
    /// (2) PID reuse across reboots aliasing onto a previous run's
    ///     leftover dir.
    pub fn new(cap: usize) -> Result<Self> {
        reap_orphans();
        let nonce = uuid::Uuid::new_v4().simple();
        let base_dir =
            std::env::temp_dir().join(format!("vmette-mcp-{}-{}", std::process::id(), nonce));
        std::fs::create_dir_all(&base_dir)
            .with_context(|| format!("creating workspace base dir {}", base_dir.display()))?;
        Ok(Self {
            base_dir,
            cap,
            inner: Mutex::new(HashMap::new()),
        })
    }

    /// Allocate a new workspace with a random UUID. Fails if `cap`
    /// workspaces are already open.
    pub fn create(&self, image: String, net: bool) -> Result<Workspace> {
        let mut guard = self.inner.lock().unwrap();
        if guard.len() >= self.cap {
            return Err(anyhow!(
                "workspace cap reached ({} open); destroy one before creating another",
                self.cap
            ));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let dir = self.base_dir.join(&id);
        std::fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;
        let ws = Workspace {
            id: id.clone(),
            dir,
            image,
            net,
        };
        guard.insert(id, ws.clone());
        Ok(ws)
    }

    pub fn get(&self, id: &str) -> Result<Workspace> {
        let guard = self.inner.lock().unwrap();
        guard.get(id).cloned().ok_or_else(|| {
            anyhow!(
                "workspace {id:?} not found (already destroyed, or never created in this session)"
            )
        })
    }

    /// Remove the workspace's on-disk tree and drop the registry entry.
    /// Idempotent.
    pub fn destroy(&self, id: &str) -> Result<()> {
        let removed = {
            let mut guard = self.inner.lock().unwrap();
            guard.remove(id)
        };
        if let Some(ws) = removed {
            let _ = std::fs::remove_dir_all(&ws.dir);
        }
        Ok(())
    }
}

impl Drop for WorkspaceState {
    fn drop(&mut self) {
        // Best-effort cleanup on session close. Ungraceful exits
        // (SIGKILL, panic-abort) skip Drop entirely — the orphan
        // reaper in `new()` cleans those up on the next startup.
        let _ = std::fs::remove_dir_all(&self.base_dir);
    }
}

// ---- orphan reaper ------------------------------------------------------

/// Remove `$TMPDIR/vmette-mcp-<pid>/` entries whose owning PID is no
/// longer alive (after at least a minute of mtime grace, to avoid
/// racing a just-starting peer server) OR whose mtime is older than 24
/// hours regardless. The double check defends against PID reuse: a
/// short-lived process that exits, then a new one happens to be
/// assigned the same PID, won't see its dir reaped because we require
/// BOTH conditions for the under-1-minute case.
///
/// Safe to call repeatedly. Call from a background task on a few-hour
/// interval to handle the long-running-server case where peer
/// instances die during this server's uptime.
pub fn reap_orphans() {
    let Ok(read) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    let now = SystemTime::now();
    for entry in read.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        let Some(rest) = name_str.strip_prefix("vmette-mcp-") else {
            continue;
        };
        // Name is `vmette-mcp-<pid>-<nonce>`. Take the leading numeric
        // prefix as the PID — split on the first `-`, fall back to
        // parsing the whole tail (handles older builds that used the
        // PID-only form).
        let pid_str = rest.split('-').next().unwrap_or(rest);
        let Ok(pid) = pid_str.parse::<i32>() else {
            continue;
        };
        let Ok(meta) = entry.metadata() else { continue };
        let age = meta
            .modified()
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .unwrap_or(Duration::ZERO);
        let alive = nix::sys::signal::kill(Pid::from_raw(pid), None).is_ok();
        let stale =
            (age >= Duration::from_secs(60) && !alive) || age >= Duration::from_secs(86_400);
        if stale {
            let path = entry.path();
            if std::fs::remove_dir_all(&path).is_ok() {
                debug!(path = %path.display(), age_s = age.as_secs(), alive, "reaped orphan workspace dir");
            }
        }
    }
}

// ---- path safety: openat-walk -------------------------------------------

/// Open `base/rel` for writing, creating intermediate directories as
/// needed. All openat steps use `O_DIRECTORY | O_NOFOLLOW`; mkdirat
/// fails atomically if an intermediate name already exists (even as a
/// symlink), so a race in which an agent's `workspace_run` creates
/// `base/foo -> /etc` between our openat-ENOENT and our mkdirat
/// surfaces as an error rather than a host-escape.
///
/// Returns an `OwnedFd` the caller can convert to `std::fs::File`.
pub fn open_for_write(base: &Path, rel: &str) -> Result<OwnedFd> {
    let rel_path = check_rel_syntactic(rel)?;
    let flags = OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_TRUNC;
    // 0o600 — only the server user can read what the agent wrote.
    let mode = Mode::S_IRUSR | Mode::S_IWUSR;
    open_under(
        base, rel_path, flags, mode, /*create_intermediates=*/ true,
    )
}

/// Open `base/rel` for reading. Refuses if any path component is a
/// symlink, and refuses to follow if the final component is.
pub fn open_for_read(base: &Path, rel: &str) -> Result<OwnedFd> {
    let rel_path = check_rel_syntactic(rel)?;
    open_under(base, rel_path, OFlag::O_RDONLY, Mode::empty(), false)
}

/// Reject syntactically-dangerous paths up front: absolute, rooted, or
/// containing `..`. We do this even though `openat`'s `O_NOFOLLOW`
/// would block actual escape — it gives clearer errors and ensures
/// the `Component::Normal`-only walk below sees the path the caller
/// intended.
fn check_rel_syntactic(rel: &str) -> Result<&Path> {
    let p = Path::new(rel);
    if p.is_absolute() {
        return Err(anyhow!("path must be relative: {rel:?}"));
    }
    let mut had_normal = false;
    for c in p.components() {
        match c {
            Component::ParentDir => return Err(anyhow!("path may not contain '..': {rel:?}")),
            Component::Prefix(_) | Component::RootDir => {
                return Err(anyhow!("path may not be rooted: {rel:?}"))
            }
            Component::Normal(_) => had_normal = true,
            _ => {} // CurDir / etc. — ignore
        }
    }
    if !had_normal {
        return Err(anyhow!("path is empty: {rel:?}"));
    }
    Ok(p)
}

fn open_under(
    base: &Path,
    rel_path: &Path,
    last_flags: OFlag,
    last_mode: Mode,
    create_intermediates: bool,
) -> Result<OwnedFd> {
    // nix 0.29 returns RawFd, not OwnedFd — wrap manually so the Drop
    // closes the fd. `from_raw_fd` is unsafe purely because the caller
    // promises uniqueness, which is true here (we just opened it).
    let base_raw: RawFd = open(base, OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW, Mode::empty())
        .map_err(|e| anyhow!("open base {}: {}", base.display(), e))?;
    let mut current: OwnedFd = unsafe { OwnedFd::from_raw_fd(base_raw) };

    let names: Vec<&OsStr> = rel_path
        .components()
        .filter_map(|c| {
            if let Component::Normal(n) = c {
                Some(n)
            } else {
                None
            }
        })
        .collect();
    let (last, intermediates) = names
        .split_last()
        .ok_or_else(|| anyhow!("empty relative path"))?;

    for name in intermediates {
        current = open_or_create_dir(current.as_raw_fd(), name, create_intermediates)?;
    }

    let final_raw: RawFd = openat(
        Some(current.as_raw_fd()),
        Path::new(last),
        last_flags | OFlag::O_NOFOLLOW,
        last_mode,
    )
    .map_err(|e| anyhow!("open {:?}: {}", last, e))?;
    Ok(unsafe { OwnedFd::from_raw_fd(final_raw) })
}

fn open_or_create_dir(parent_fd: RawFd, name: &OsStr, create: bool) -> Result<OwnedFd> {
    // Try-open / on-ENOENT mkdir / retry-open. Bounded retry defends
    // against an unlikely tight race that keeps replacing the entry
    // with a symlink between attempts.
    for _ in 0..3 {
        match openat(
            Some(parent_fd),
            Path::new(name),
            OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW,
            Mode::empty(),
        ) {
            Ok(fd) => return Ok(unsafe { OwnedFd::from_raw_fd(fd) }),
            Err(Errno::ENOENT) if create => {
                match mkdirat(Some(parent_fd), Path::new(name), Mode::S_IRWXU) {
                    Ok(()) | Err(Errno::EEXIST) => continue,
                    Err(e) => return Err(anyhow!("mkdirat {:?}: {}", name, e)),
                }
            }
            Err(e) => return Err(anyhow!("openat {:?}: {}", name, e)),
        }
    }
    Err(anyhow!(
        "openat {:?}: gave up after 3 attempts (likely a concurrent symlink-replacement race)",
        name
    ))
}

// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn fresh_dir(label: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "vmette-mcp-test-{}-{}-{}",
            label,
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn write_and_read_simple() {
        let base = fresh_dir("simple");
        let fd = open_for_write(&base, "hello.txt").unwrap();
        let mut f = std::fs::File::from(fd);
        f.write_all(b"hi").unwrap();
        drop(f);

        let fd = open_for_read(&base, "hello.txt").unwrap();
        let mut f = std::fs::File::from(fd);
        let mut buf = String::new();
        f.read_to_string(&mut buf).unwrap();
        assert_eq!(buf, "hi");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn write_creates_intermediate_dirs() {
        let base = fresh_dir("nested");
        let fd = open_for_write(&base, "a/b/c/file.txt").unwrap();
        let mut f = std::fs::File::from(fd);
        f.write_all(b"nested").unwrap();
        drop(f);
        assert!(base.join("a/b/c/file.txt").is_file());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn read_missing_file_errors() {
        let base = fresh_dir("missing");
        assert!(open_for_read(&base, "nope.txt").is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn refuses_symlink_at_final_component() {
        let base = fresh_dir("symlink-final");
        std::os::unix::fs::symlink("/etc/hosts", base.join("evil")).unwrap();
        let err = open_for_read(&base, "evil").unwrap_err();
        // ELOOP is what NOFOLLOW returns on macOS/Linux when the target
        // is a symlink. Older NIX builds may use a different word; the
        // important thing is that it errored.
        assert!(
            err.to_string().contains("ELOOP")
                || err.to_string().contains("symbolic")
                || err.to_string().contains("loop"),
            "expected symlink-related error, got: {err}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn refuses_symlink_at_intermediate_component() {
        // The nested case: agent creates ws/a -> /etc, then asks for ws/a/passwd.
        let base = fresh_dir("symlink-mid");
        std::os::unix::fs::symlink("/etc", base.join("a")).unwrap();
        let err = open_for_read(&base, "a/passwd").unwrap_err();
        assert!(
            err.to_string().contains("ELOOP")
                || err.to_string().contains("symbolic")
                || err.to_string().contains("ENOTDIR")
                || err.to_string().contains("loop"),
            "expected symlink-or-not-a-dir error, got: {err}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn refuses_traversal_syntactically() {
        let base = fresh_dir("traversal");
        assert!(open_for_write(&base, "/abs").is_err());
        assert!(open_for_write(&base, "../escape").is_err());
        assert!(open_for_write(&base, "sub/../../escape").is_err());
        assert!(open_for_read(&base, "..").is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn workspace_state_cap_enforced() {
        let s = WorkspaceState::new(2).unwrap();
        let _a = s.create("alpine:3.20".into(), false).unwrap();
        let _b = s.create("alpine:3.20".into(), false).unwrap();
        let err = s.create("alpine:3.20".into(), false).unwrap_err();
        assert!(err.to_string().contains("cap reached"));
    }

    #[test]
    fn workspace_state_destroy_then_recreate() {
        let s = WorkspaceState::new(1).unwrap();
        let a = s.create("alpine:3.20".into(), false).unwrap();
        s.destroy(&a.id).unwrap();
        let _b = s.create("alpine:3.20".into(), false).unwrap();
    }

    #[test]
    fn workspace_state_get_missing_errors() {
        let s = WorkspaceState::new(4).unwrap();
        let err = s.get("nope").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }
}
