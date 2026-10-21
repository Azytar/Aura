use std::time::{Duration, Instant};

/// A stalled client cannot keep the last presented image frozen indefinitely.
pub struct SyncGate {
    deadline: Option<Instant>,
    timeout: Duration,
}
impl SyncGate {
    pub fn new(timeout: Duration) -> Self {
        Self {
            deadline: None,
            timeout,
        }
    }
    pub fn update(&mut self, active: bool, now: Instant) -> bool {
        if !active {
            self.deadline = None;
            return false;
        }
        let deadline = *self.deadline.get_or_insert(now + self.timeout);
        if now >= deadline {
            self.deadline = None;
            false
        } else {
            true
        }
    }
    pub fn timeout_ms(&self) -> i32 {
        self.deadline.map_or(-1, |deadline| {
            deadline
                .saturating_duration_since(Instant::now())
                .as_nanos()
                .div_ceil(1_000_000)
                .min(i32::MAX as u128) as i32
        })
    }
    pub fn active(&self) -> bool {
        self.deadline.is_some()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retries_do_not_extend_a_stalled_transaction_and_expiry_recovers() {
        let mut gate = SyncGate::new(Duration::from_millis(150));
        let now = Instant::now();
        assert!(gate.update(true, now));
        assert!(gate.update(true, now + Duration::from_millis(100)));
        assert!(!gate.update(true, now + Duration::from_millis(150)));
        assert!(!gate.active());
        assert!(gate.update(true, now + Duration::from_millis(151)));
        assert!(!gate.update(false, now + Duration::from_millis(152)));
        assert_eq!(gate.timeout_ms(), -1);
    }
}
