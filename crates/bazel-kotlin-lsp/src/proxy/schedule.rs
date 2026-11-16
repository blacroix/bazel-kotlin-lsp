//! When to run the next sync: debounced after the last relevant change, never two at once.

use std::time::{Duration, Instant};

pub struct Scheduler {
    debounce: Duration,
    due: Option<Instant>,
    running: bool,
    /// The last change seen while a sync was running.
    changed_during_sync: Option<Instant>,
}

impl Scheduler {
    pub fn new(debounce: Duration) -> Self {
        Scheduler {
            debounce,
            due: None,
            running: false,
            changed_during_sync: None,
        }
    }

    /// Asks for a sync as soon as none is running.
    pub fn request_now(&mut self, now: Instant) {
        self.due = Some(now);
    }

    pub fn changed(&mut self, now: Instant) {
        if self.running {
            self.changed_during_sync = Some(now);
        } else {
            self.due = Some(now + self.debounce);
        }
    }

    /// Whether to start a sync now; it then counts as running until [`Scheduler::finished`].
    pub fn poll(&mut self, now: Instant) -> bool {
        if self.running || !self.due.is_some_and(|due| due <= now) {
            return false;
        }
        self.due = None;
        self.running = true;
        true
    }

    pub fn finished(&mut self) {
        self.running = false;
        if let Some(changed) = self.changed_during_sync.take() {
            self.due = Some(changed + self.debounce);
        }
    }

    /// When [`Scheduler::poll`] next needs calling, if a sync is waiting.
    pub fn deadline(&self) -> Option<Instant> {
        if self.running { None } else { self.due }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEBOUNCE: Duration = Duration::from_secs(2);

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn first_sync_starts_at_once() {
        let t0 = Instant::now();
        let mut s = Scheduler::new(DEBOUNCE);
        s.request_now(t0);
        assert_eq!(s.deadline(), Some(t0));
        assert!(s.poll(t0));
        assert!(!s.poll(t0 + ms(10)), "one sync at a time");
        s.finished();
        assert_eq!(s.deadline(), None);
        assert!(!s.poll(t0 + ms(5000)));
    }

    #[test]
    fn build_edit_syncs_once_after_burst() {
        let t0 = Instant::now();
        let mut s = Scheduler::new(DEBOUNCE);
        for offset in [0, 500, 1000] {
            s.changed(t0 + ms(offset));
        }
        assert_eq!(s.deadline(), Some(t0 + ms(3000)));
        assert!(!s.poll(t0 + ms(2999)));
        assert!(s.poll(t0 + ms(3000)));
        s.finished();
        assert!(!s.poll(t0 + ms(10_000)));
    }

    #[test]
    fn edits_during_sync_coalesce_into_one_rerun() {
        let t0 = Instant::now();
        let mut s = Scheduler::new(DEBOUNCE);
        s.request_now(t0);
        assert!(s.poll(t0));
        for offset in [100, 200, 300] {
            s.changed(t0 + ms(offset));
        }
        assert_eq!(s.deadline(), None, "nothing starts while a sync runs");
        assert!(!s.poll(t0 + ms(5000)));
        s.finished();
        assert_eq!(s.deadline(), Some(t0 + ms(2300)));
        assert!(s.poll(t0 + ms(5000)), "one re-run");
        s.finished();
        assert!(!s.poll(t0 + ms(20_000)), "and only one");
    }

    #[test]
    fn a_rerun_still_waits_for_the_debounce() {
        let t0 = Instant::now();
        let mut s = Scheduler::new(DEBOUNCE);
        s.request_now(t0);
        assert!(s.poll(t0));
        s.changed(t0 + ms(900));
        s.finished();
        assert!(!s.poll(t0 + ms(1000)));
        assert!(s.poll(t0 + ms(2900)));
    }
}
