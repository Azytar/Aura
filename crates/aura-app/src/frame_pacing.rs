use std::time::{Duration, Instant};

/// Coalesce output bursts while continuing to service X11 and the PTY.
pub struct FramePacer {
    deadline: Option<Instant>,
    next_frame: Option<Instant>,
    interval: Duration,
}

impl FramePacer {
    pub fn new(max_fps: u32) -> Self {
        Self {
            deadline: None,
            next_frame: None,
            interval: Duration::from_nanos(1_000_000_000 / u64::from(max_fps)),
        }
    }

    pub fn set_max_fps(&mut self, max_fps: u32) {
        self.interval = Duration::from_nanos(1_000_000_000 / u64::from(max_fps));
        self.deadline = None;
        self.next_frame = None;
    }

    pub fn request(&mut self, now: Instant) {
        self.deadline.get_or_insert_with(|| {
            (now + Duration::from_millis(1)).max(self.next_frame.unwrap_or(now))
        });
    }

    pub fn ready(&self, now: Instant) -> bool {
        self.deadline.is_some_and(|deadline| now >= deadline)
    }

    pub fn submitted(&mut self, now: Instant) {
        self.deadline = None;
        self.next_frame = Some(now + self.interval);
    }

    pub fn suspend(&mut self) {
        self.deadline = None;
    }

    pub fn timeout_ms(&self, now: Instant) -> i32 {
        self.deadline.map_or(-1, |deadline| {
            deadline
                .saturating_duration_since(now)
                .as_nanos()
                .div_ceil(1_000_000)
                .min(i32::MAX as u128) as i32
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bursts_have_a_fixed_deadline_and_idle_does_not_poll() {
        let now = Instant::now();
        let mut pacer = FramePacer::new(120);
        assert_eq!(pacer.timeout_ms(now), -1);
        pacer.request(now);
        pacer.request(now + Duration::from_micros(900));
        assert!(!pacer.ready(now + Duration::from_micros(999)));
        assert!(pacer.ready(now + Duration::from_millis(1)));
        pacer.submitted(now + Duration::from_millis(1));
        assert_eq!(pacer.timeout_ms(now), -1);
        pacer.request(now + Duration::from_millis(2));
        assert!(!pacer.ready(now + Duration::from_millis(9)));
        assert!(pacer.ready(now + Duration::from_millis(10)));
        pacer.submitted(now + Duration::from_millis(10));
        pacer.request(now + Duration::from_secs(5));
        assert_eq!(pacer.timeout_ms(now + Duration::from_secs(5)), 1);
    }

    #[test]
    fn synchronization_suspends_pending_wakeups_and_reload_changes_the_rate() {
        let now = Instant::now();
        let mut pacer = FramePacer::new(120);
        pacer.request(now);
        pacer.suspend();
        assert_eq!(pacer.timeout_ms(now + Duration::from_secs(1)), -1);
        pacer.request(now + Duration::from_secs(1));
        assert!(pacer.ready(now + Duration::from_secs(1) + Duration::from_millis(1)));
        pacer.submitted(now + Duration::from_secs(1));
        pacer.set_max_fps(30);
        pacer.request(now + Duration::from_secs(2));
        pacer.submitted(now + Duration::from_secs(2) + Duration::from_millis(1));
        pacer.request(now + Duration::from_secs(2) + Duration::from_millis(2));
        assert!(!pacer.ready(now + Duration::from_secs(2) + Duration::from_millis(34)));
        assert!(pacer.ready(now + Duration::from_secs(2) + Duration::from_millis(35)));
    }
}
