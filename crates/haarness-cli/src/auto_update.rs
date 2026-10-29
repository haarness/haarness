//! Keeps installed resources current from a long-running server: marketplace
//! installs that follow new versions, and Git plugin marketplaces that follow
//! their repository. One-shot commands never start it.
//!
//! Several servers run on one computer (one per folder). A stamp file, read
//! and written under an editor lock, lets one of them do each round: one
//! stamp in the data folder for everything installed for all projects, and
//! one in a project's `.haarness/marketplace/` for what is installed for that
//! project only.
use crate::management::ManagementService;
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::{
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio_util::sync::CancellationToken;

/// `0`, `false`, `off` or `no` turns the background rounds off.
pub(crate) const SWITCH: &str = "HAARNESS_AUTO_UPDATE";
/// Seconds between rounds (default 1800, at least 60).
pub(crate) const INTERVAL: &str = "HAARNESS_AUTO_UPDATE_INTERVAL_SECS";
const DEFAULT_INTERVAL: Duration = Duration::from_secs(30 * 60);
const MIN_INTERVAL: Duration = Duration::from_secs(60);
/// After start, the first round waits this long (or the interval when it is
/// shorter), so starting a server never waits on the network.
const FIRST_ROUND_AFTER: Duration = Duration::from_secs(120);
/// Rounds of several servers do not start at the same instant; a stamp this
/// close to a full interval old counts as due.
const SLACK: Duration = Duration::from_secs(5);
/// The stamp's file name. The configuration scan skips it, so writing it
/// never reloads anything.
pub(crate) const STAMP: &str = ".auto-update-checked";

pub(crate) fn enabled() -> bool {
    !std::env::var(SWITCH).is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        )
    })
}

pub(crate) fn interval() -> Duration {
    interval_from(std::env::var(INTERVAL).ok().as_deref())
}

fn interval_from(value: Option<&str>) -> Duration {
    value
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map_or(DEFAULT_INTERVAL, Duration::from_secs)
        .max(MIN_INTERVAL)
}

/// Starts the rounds for the server `management` belongs to, until `shutdown`.
pub(crate) fn spawn(management: Arc<ManagementService>, shutdown: CancellationToken) {
    if !enabled() {
        tracing::info!("automatic updates are off ({SWITCH})");
        return;
    }
    let every = interval();
    tokio::spawn(async move {
        let mut wait = FIRST_ROUND_AFTER.min(every);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return,
                () = tokio::time::sleep(wait) => {}
            }
            tokio::select! {
                () = shutdown.cancelled() => return,
                () = management.automatic_update_round(every) => {}
            }
            wait = every;
        }
    });
}

/// Claims the round `stamp` stands for: `true`, with the stamp rewritten,
/// when no Haarness process on this computer did that round within `every`.
pub(crate) fn claim(data: &Path, stamp: &Path, every: Duration, now: SystemTime) -> Result<bool> {
    fs::create_dir_all(data)?;
    let _lock = crate::desktop::editor_lock(data, "auto-update")?;
    let now = DateTime::<Utc>::from(now);
    let last = fs::read_to_string(stamp)
        .ok()
        .and_then(|text| DateTime::parse_from_rfc3339(text.trim()).ok())
        .map(|last| last.with_timezone(&Utc));
    // A stamp from the future (a clock set back) does not hold rounds off.
    if let Some(last) = last
        && last <= now
        && (now - last).to_std().unwrap_or_default() + SLACK < every
    {
        return Ok(false);
    }
    // Written beside it and renamed over it, so a reader sees a whole time.
    let parent = stamp.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!("{STAMP}.{}.tmp", uuid::Uuid::now_v7()));
    fs::write(&temporary, now.to_rfc3339())?;
    if let Err(error) = fs::rename(&temporary, stamp) {
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_round_per_interval_whichever_server_asks() {
        let root =
            std::env::temp_dir().join(format!("haarness-auto-update-{}", uuid::Uuid::now_v7()));
        let data = root.join("data");
        let stamp = data.join(STAMP);
        let every = Duration::from_secs(1800);
        let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        // The first server to ask does the round; the others, a moment or
        // most of an interval later, find it done.
        assert!(claim(&data, &stamp, every, start).unwrap());
        assert!(!claim(&data, &stamp, every, start + Duration::from_secs(1)).unwrap());
        assert!(!claim(&data, &stamp, every, start + Duration::from_secs(1700)).unwrap());
        // A full interval later (give or take a few seconds) it is due again,
        // once.
        let next = start + every - Duration::from_secs(2);
        assert!(claim(&data, &stamp, every, next).unwrap());
        assert!(!claim(&data, &stamp, every, next + Duration::from_secs(30)).unwrap());
        // A stamp ahead of the clock does not stop rounds.
        assert!(claim(&data, &stamp, every, start).unwrap());
        // A project's stamp is its own.
        let project = root.join("project/.haarness/marketplace").join(STAMP);
        assert!(claim(&data, &project, every, start).unwrap());
        assert!(!claim(&data, &project, every, start + Duration::from_secs(5)).unwrap());
        // An unreadable stamp counts as no round yet.
        fs::write(&stamp, "not a time").unwrap();
        assert!(claim(&data, &stamp, every, start).unwrap());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_interval_has_a_floor_and_a_default() {
        assert_eq!(interval_from(None), Duration::from_secs(1800));
        assert_eq!(interval_from(Some("soon")), Duration::from_secs(1800));
        assert_eq!(interval_from(Some("5")), Duration::from_secs(60));
        assert_eq!(interval_from(Some(" 600 ")), Duration::from_secs(600));
        // A short interval brings the first round forward too.
        assert_eq!(
            FIRST_ROUND_AFTER.min(interval_from(Some("60"))),
            Duration::from_secs(60)
        );
    }
}
