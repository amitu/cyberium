//! Spend, and what a budget is allowed to stop.
//!
//! **Unix seconds, never days.** Nothing here knows what a timezone is, and no
//! filename contains a date, because what counts as a day is policy and policy
//! changes — a ledger already bucketed could not be re-read under a new rule, and
//! one made of instants can.
//!
//! ```text
//! <root>/tenants/payments/spend.log
//!   1786968121 108 r7 12 cm-w-1,cm-w-3
//!   └ when      └ credits
//!                   └ reservation
//!                      └ minutes
//!                          └ machines
//! ```
//!
//! Append-only, read backwards until the timestamps leave the window. Deliberately
//! not a database: an operator should be able to read it, and it is what makes "why
//! is our budget gone" answerable — a running total never is.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub const FILE: &str = "spend.log";

/// A rolling window when nothing says otherwise. Twenty-four hours needs no
/// timezone, no daylight saving and no calendar, which is why the deterministic gate
/// can enforce a budget at all.
pub const WINDOW_SECS: u64 = 86_400;

pub fn path_in(tenant_dir: &Path) -> PathBuf {
    tenant_dir.join(FILE)
}

/// One closed reservation, as written down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub at: u64,
    pub credits: u64,
    pub reservation: String,
    pub minutes: u64,
    pub machines: Vec<String>,
}

impl Entry {
    fn line(&self) -> String {
        format!(
            "{} {} {} {} {}\n",
            self.at,
            self.credits,
            self.reservation,
            self.minutes,
            self.machines.join(",")
        )
    }

    /// Parse what we wrote. Unknown trailing fields are ignored so an older ledger
    /// stays readable after the format grows.
    fn parse(line: &str) -> Option<Self> {
        let mut parts = line.split_whitespace();
        Some(Self {
            at: parts.next()?.parse().ok()?,
            credits: parts.next()?.parse().ok()?,
            reservation: parts.next().unwrap_or("?").to_string(),
            minutes: parts.next().and_then(|m| m.parse().ok()).unwrap_or(0),
            machines: parts
                .next()
                .map(|m| m.split(',').map(str::to_string).collect())
                .unwrap_or_default(),
        })
    }
}

/// Append one closed reservation.
///
/// Failing to record spend must never fail the release: the machines have already
/// come back, and refusing to acknowledge that would strand them. So the caller logs
/// the error and carries on — an unwritten line is a billing problem, a stuck
/// reservation is an outage.
pub fn record(tenant_dir: &Path, entry: &Entry) -> Result<()> {
    std::fs::create_dir_all(tenant_dir)?;
    let path = path_in(tenant_dir);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    file.write_all(entry.line().as_bytes())
        .with_context(|| format!("appending to {}", path.display()))?;
    Ok(())
}

/// Everything spent in the last `window` seconds, ending at `now`.
pub fn spent(tenant_dir: &Path, now: u64, window: u64) -> u64 {
    entries_since(tenant_dir, now.saturating_sub(window))
        .iter()
        .map(|e| e.credits)
        .sum()
}

/// Entries at or after `from`, oldest first.
///
/// Reads the whole file and filters. Honest about what that costs: a day of
/// allocations is a few hundred lines, and reading backwards through a growing file
/// is the optimisation to reach for when it is measurably needed rather than now.
pub fn entries_since(tenant_dir: &Path, from: u64) -> Vec<Entry> {
    let Ok(text) = std::fs::read_to_string(path_in(tenant_dir)) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(Entry::parse)
        .filter(|e| e.at >= from)
        .collect()
}

/// What a budget permits right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Room {
    pub budget: u64,
    pub spent: u64,
    pub committed: u64,
}

impl Room {
    /// Credits still available. Saturating, because being over budget is a real
    /// state — a reservation can overrun its estimate — and it must read as zero
    /// rather than wrap into an enormous allowance.
    pub fn left(&self) -> u64 {
        self.budget.saturating_sub(self.spent + self.committed)
    }

    /// How many of these machines fit, given in the order they would be taken.
    ///
    /// Takes the **actual rates**, not an average or the cheapest, because machines
    /// are not interchangeable: at rates 1, 2 and 8, pricing all three at the
    /// cheapest says a grant costs 3 a minute when it costs 11, and a budget that
    /// over-permits is a budget that discovers the overspend afterwards.
    ///
    /// Worst case on purpose — a grant is allowed only if it stays within budget when
    /// nobody releases it. Since selection is cheapest-first, each further machine
    /// costs at least as much as the last, so this walks the list and stops.
    pub fn affordable(&self, rates: &[u32], lifetime: u64) -> u32 {
        let minutes = lifetime.div_ceil(60).max(1);
        let left = self.left();
        let mut running = 0u64;
        let mut count = 0u32;
        for rate in rates {
            running += *rate as u64 * minutes;
            if running > left {
                break;
            }
            count += 1;
        }
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let dir = crate::testing::scratch("budget");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entry(at: u64, credits: u64) -> Entry {
        Entry {
            at,
            credits,
            reservation: format!("r{at}"),
            minutes: 1,
            machines: vec!["w1".into()],
        }
    }

    #[test]
    fn it_round_trips() {
        let dir = temp();
        let e = Entry {
            at: 1786968121,
            credits: 108,
            reservation: "r7".into(),
            minutes: 12,
            machines: vec!["cm-w-1".into(), "cm-w-3".into()],
        };
        record(&dir, &e).unwrap();
        assert_eq!(entries_since(&dir, 0), vec![e]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_ledger_is_no_spend_not_an_error() {
        // A tenant that has never run anything must not be refused for it.
        let dir = temp();
        assert_eq!(spent(&dir, 1_000_000, WINDOW_SECS), 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_window_rolls() {
        let dir = temp();
        let now = 1_000_000u64;
        record(&dir, &entry(now - 100, 10)).unwrap();       // inside
        record(&dir, &entry(now - WINDOW_SECS - 5, 999)).unwrap(); // fell out
        // No calendar involved: purely "within this many seconds of now".
        assert_eq!(spent(&dir, now, WINDOW_SECS), 10);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_line_we_cannot_read_is_skipped_not_fatal() {
        // Somebody will hand-edit this file. A bad line must not make a tenant's
        // whole history unreadable, and so unenforceable.
        let dir = temp();
        record(&dir, &entry(500, 7)).unwrap();
        std::fs::write(
            path_in(&dir),
            format!("{}oops not a line\n", std::fs::read_to_string(path_in(&dir)).unwrap()),
        )
        .unwrap();
        assert_eq!(spent(&dir, 1000, WINDOW_SECS), 7);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn room_counts_commitments_not_only_spend() {
        // The hole this closes: a hundred runs started at once while comfortably
        // under budget, discovered afterwards.
        let room = Room { budget: 1000, spent: 200, committed: 700 };
        assert_eq!(room.left(), 100);

        let optimistic = Room { budget: 1000, spent: 200, committed: 0 };
        assert_eq!(optimistic.left(), 800);
    }

    #[test]
    fn being_over_budget_reads_as_zero_not_as_plenty() {
        // A reservation can overrun its estimate, so this state is reachable and
        // must not wrap into an enormous allowance.
        let room = Room { budget: 100, spent: 500, committed: 0 };
        assert_eq!(room.left(), 0);
        assert_eq!(room.affordable(&[1, 1, 1, 1], 600), 0);
    }

    #[test]
    fn affordable_is_worst_case() {
        // 600s is 10 minutes; at 5 credits a minute each machine could cost 50, so
        // 120 credits buys two and not three.
        let room = Room { budget: 120, spent: 0, committed: 0 };
        assert_eq!(room.affordable(&[5, 5, 5], 600), 2);
        assert_eq!(room.affordable(&[5], 600), 1);
        // And never more than was offered.
        assert_eq!(room.affordable(&[1, 1], 60), 2);
    }

    #[test]
    fn machines_are_priced_individually_not_averaged() {
        // The bug this replaced: pricing every machine at the cheapest rate. At
        // 1, 2 and 8 over ten minutes the three cost 10, 20 and 80 — so 40 credits
        // buys the first two and not the third, where a flat cheapest-rate estimate
        // would have said all three fit in 30.
        let room = Room { budget: 40, spent: 0, committed: 0 };
        assert_eq!(room.affordable(&[1, 2, 8], 600), 2);

        let plenty = Room { budget: 110, spent: 0, committed: 0 };
        assert_eq!(plenty.affordable(&[1, 2, 8], 600), 3);
    }
}
