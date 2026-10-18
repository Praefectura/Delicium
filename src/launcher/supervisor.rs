//! Restart policy for the supervised engine.
//!
//! Kept separate from the process code so the decisions are plain values a
//! test can drive with an explicit clock: a clean exit ends the launcher, a
//! crash is retried with backoff, and three early crashes in a row discard a
//! freshly installed version and fall back to the one that was running
//! before.

use std::time::{Duration, Instant};

/// What to do after the engine process ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Exit code 0: the user closed the pet. The launcher ends too.
    Quit,
    /// Start the engine again after waiting.
    Restart {
        after: Duration,
        /// The target was a freshly installed version that has now failed
        /// three times; discard it and start the previous one.
        discard_pending: bool,
    },
    /// Nothing left to try. Reported to the user.
    GiveUp(String),
}

/// The policy and the history it needs.
#[derive(Clone, Debug)]
pub struct Supervisor {
    /// How long a run must last before it counts as healthy.
    confirm_after: Duration,
    /// Crashes are counted only when they happen within this window.
    rapid_window: Duration,
    /// Early crashes in a row that discard a pending version.
    max_rapid: u32,
    /// Start times of recent early crashes.
    failures: Vec<Instant>,
    /// When the current run started.
    started: Instant,
    /// Whether the current target came from `pending`.
    pending: bool,
}

impl Supervisor {
    /// The policy the launcher uses.
    pub fn new(pending: bool) -> Self {
        Self {
            confirm_after: Duration::from_secs(5),
            rapid_window: Duration::from_secs(600),
            max_rapid: 3,
            failures: Vec::new(),
            started: Instant::now(),
            pending,
        }
    }

    /// The run of `pending` against a target began now.
    pub fn begin(&mut self, pending: bool, now: Instant) {
        self.pending = pending;
        self.started = now;
    }

    /// Whether the current run has lasted long enough to count as started.
    pub fn confirmed(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.started) >= self.confirm_after
    }

    /// The decision for an engine that ended with `code` at `now`.
    pub fn on_exit(&mut self, code: i32, now: Instant) -> Decision {
        if code == 0 {
            return Decision::Quit;
        }
        let lasted = now.saturating_duration_since(self.started);
        if lasted >= self.confirm_after {
            // It ran for a while and then failed; that is not a bad install.
            self.failures.clear();
            return Decision::Restart {
                after: Duration::from_secs(1),
                discard_pending: false,
            };
        }
        self.failures
            .retain(|start| now.saturating_duration_since(*start) <= self.rapid_window);
        self.failures.push(self.started);
        if self.failures.len() as u32 >= self.max_rapid {
            self.failures.clear();
            if self.pending {
                return Decision::Restart {
                    after: Duration::from_secs(1),
                    discard_pending: true,
                };
            }
            return Decision::GiveUp(format!(
                "Delicium stopped {count} times in a row (last exit code {code}).",
                count = self.max_rapid
            ));
        }
        let seconds = 1u64 << (self.failures.len() - 1).min(2);
        Decision::Restart {
            after: Duration::from_secs(seconds),
            discard_pending: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, seconds: u64) -> Instant {
        base + Duration::from_secs(seconds)
    }

    #[test]
    fn a_clean_exit_ends_the_launcher() {
        let mut supervisor = Supervisor::new(false);
        let base = Instant::now();
        supervisor.begin(false, base);
        assert_eq!(supervisor.on_exit(0, at(base, 3)), Decision::Quit);
    }

    #[test]
    fn early_crashes_back_off_then_give_up() {
        let mut supervisor = Supervisor::new(false);
        let base = Instant::now();
        supervisor.begin(false, base);
        assert_eq!(
            supervisor.on_exit(101, at(base, 1)),
            Decision::Restart {
                after: Duration::from_secs(1),
                discard_pending: false
            }
        );
        supervisor.begin(false, at(base, 2));
        assert_eq!(
            supervisor.on_exit(101, at(base, 3)),
            Decision::Restart {
                after: Duration::from_secs(2),
                discard_pending: false
            }
        );
        supervisor.begin(false, at(base, 4));
        match supervisor.on_exit(101, at(base, 5)) {
            Decision::GiveUp(message) => assert!(message.contains("3 times"), "{message}"),
            other => panic!("expected GiveUp, got {other:?}"),
        }
    }

    #[test]
    fn three_early_crashes_discard_a_pending_version() {
        let mut supervisor = Supervisor::new(true);
        let base = Instant::now();
        for step in 0..3u64 {
            supervisor.begin(true, at(base, step * 2));
            let decision = supervisor.on_exit(1, at(base, step * 2 + 1));
            if step < 2 {
                assert_eq!(
                    decision,
                    Decision::Restart {
                        after: Duration::from_secs(1 << step),
                        discard_pending: false
                    }
                );
            } else {
                assert_eq!(
                    decision,
                    Decision::Restart {
                        after: Duration::from_secs(1),
                        discard_pending: true
                    }
                );
            }
        }
    }

    #[test]
    fn a_long_run_resets_the_crash_count() {
        let mut supervisor = Supervisor::new(false);
        let base = Instant::now();
        supervisor.begin(false, base);
        assert!(matches!(
            supervisor.on_exit(1, at(base, 1)),
            Decision::Restart { .. }
        ));
        // A run that lasts counts as healthy even if it later crashes.
        supervisor.begin(false, at(base, 2));
        assert!(supervisor.confirmed(at(base, 20)));
        assert_eq!(
            supervisor.on_exit(7, at(base, 100)),
            Decision::Restart {
                after: Duration::from_secs(1),
                discard_pending: false
            }
        );
        // The counter is empty again, so the next early crash is the first.
        supervisor.begin(false, at(base, 101));
        assert_eq!(
            supervisor.on_exit(7, at(base, 101)),
            Decision::Restart {
                after: Duration::from_secs(1),
                discard_pending: false
            }
        );
    }

    #[test]
    fn crashes_outside_the_window_do_not_accumulate() {
        let mut supervisor = Supervisor::new(false);
        let base = Instant::now();
        for step in 0..4u64 {
            supervisor.begin(false, at(base, step * 700));
            let decision = supervisor.on_exit(1, at(base, step * 700 + 1));
            // Every crash is more than 600 s after the previous one.
            assert!(
                matches!(decision, Decision::Restart { .. }),
                "step {step}: {decision:?}"
            );
        }
    }
}
