//! A single chat drives the screen at any moment. Two agents sharing one pointer, or one agent using refs another
//! has just invalidated, would hit the wrong things. A chat's first computer use call claims a lease, a lock-guarded
//! file in the data folder that every other chat of this user is turned away by, in this process or any other. The
//! lease ends with the chat's turn, with its process, or [`IDLE`] after the chat's latest call.
//! [`IDLE`] after the chat's last call.

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Seconds of inactivity after which a lease expires, even if the holding turn carries on with other work.
pub const IDLE: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Holder {
    pub pid: u32,
    pub thread: String,
    /// Milliseconds since the epoch of the holder's latest call.
    pub at: u64,
}

pub struct Lease {
    file: PathBuf,
    lock: PathBuf,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| {
            u64::try_from(time.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(not(unix))]
fn alive(_pid: u32) -> bool {
    true
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // Signal 0 checks that the process exists; EPERM means it does but belongs to another user.
    match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) {
        Ok(()) => true,
        Err(error) => error == nix::errno::Errno::EPERM,
    }
}

impl Lease {
    #[must_use]
    pub fn new(folder: &Path) -> Self {
        Self {
            file: folder.join("lease.json"),
            lock: folder.join("lease.lock"),
        }
    }

    fn read(&self) -> Option<Holder> {
        serde_json::from_slice(&fs::read(&self.file).ok()?).ok()
    }

    /// Whether `holder` keeps `thread` of this process off the screen.
    fn blocks(holder: &Holder, thread: &str) -> bool {
        if holder.pid == std::process::id() && holder.thread == thread {
            return false;
        }
        if now().saturating_sub(holder.at) >= u64::try_from(IDLE.as_millis()).unwrap_or(u64::MAX) {
            return false;
        }
        holder.pid == std::process::id() || alive(holder.pid)
    }

    fn locked<T>(&self, body: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        if let Some(parent) = self.lock.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&self.lock)?;
        file.lock_exclusive()?;
        let result = body();
        let _ = FileExt::unlock(&file);
        result
    }

    fn write(&self, holder: &Holder) -> io::Result<()> {
        let temp = self
            .file
            .with_extension(format!("{}.tmp", uuid::Uuid::now_v7()));
        let result = (|| {
            let mut file = fs::File::create(&temp)?;
            file.write_all(&serde_json::to_vec(holder).map_err(io::Error::other)?)?;
            fs::rename(&temp, &self.file)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }

    /// Claims or refreshes the lease for `thread`; while another chat holds it, the error tells the agent what to do.
    pub fn take(&self, thread: &str) -> Result<(), String> {
        let outcome = self.locked(|| {
            if let Some(holder) = self.read()
                && Self::blocks(&holder, thread)
            {
                return Ok(Err(holder));
            }
            self.write(&Holder {
                pid: std::process::id(),
                thread: thread.to_owned(),
                at: now(),
            })?;
            Ok(Ok(()))
        });
        match outcome {
            Ok(Ok(())) => Ok(()),
            Ok(Err(holder)) => {
                let left = u64::try_from(IDLE.as_millis())
                    .unwrap_or(u64::MAX)
                    .saturating_sub(now().saturating_sub(holder.at));
                let seconds = left.div_ceil(1000).max(1);
                let other = if holder.pid == std::process::id() {
                    "another chat"
                } else {
                    "a Haarness agent running elsewhere on this Mac"
                };
                Err(format!(
                    "The screen is busy: {other} is using it, and only one chat can at a time. It becomes free when that turn finishes, or {seconds} seconds after its latest action. Let the person know, or retry later."
                ))
            }
            Err(error) => Err(format!("Checking the screen lease failed: {error}.")),
        }
    }

    /// Drops `thread`'s lease when this process holds it, reporting whether anything was released.
    pub fn release(&self, thread: &str) -> bool {
        self.locked(|| match self.read() {
            Some(holder) if holder.pid == std::process::id() && holder.thread == thread => {
                fs::remove_file(&self.file)?;
                Ok(true)
            }
            _ => Ok(false),
        })
        .unwrap_or(false)
    }

    /// This process's current lease holder, if its lease is still fresh.
    #[must_use]
    pub fn current(&self) -> Option<Holder> {
        let holder = self.read()?;
        let fresh =
            now().saturating_sub(holder.at) < u64::try_from(IDLE.as_millis()).unwrap_or(u64::MAX);
        (holder.pid == std::process::id() && fresh).then_some(holder)
    }

    /// Whoever uses the screen now, in any process.
    #[must_use]
    pub fn anyone(&self) -> Option<Holder> {
        let holder = self.read()?;
        let fresh =
            now().saturating_sub(holder.at) < u64::try_from(IDLE.as_millis()).unwrap_or(u64::MAX);
        (fresh && (holder.pid == std::process::id() || alive(holder.pid))).then_some(holder)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_chat_at_a_time_until_release_or_a_dead_holder() {
        let dir = tempfile::tempdir().unwrap();
        let lease = Lease::new(dir.path());
        lease.take("a").unwrap();
        lease.take("a").unwrap();
        let refused = lease.take("b").unwrap_err();
        assert!(refused.contains("another chat"), "{refused}");
        assert!(!lease.release("b"));
        assert!(lease.release("a"));
        lease.take("b").unwrap();
        // A holder in a process that is gone frees the screen at once.
        lease
            .write(&Holder {
                pid: 999_999,
                thread: "x".into(),
                at: now(),
            })
            .unwrap();
        lease.take("c").unwrap();
        // A stale holder frees it too.
        lease
            .write(&Holder {
                pid: std::process::id(),
                thread: "x".into(),
                at: now() - 31_000,
            })
            .unwrap();
        lease.take("d").unwrap();
        assert_eq!(lease.current().unwrap().thread, "d");
    }
}
