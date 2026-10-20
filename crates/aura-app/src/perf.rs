//! Opt-in host intervals; none of these measure display presentation latency.
use std::time::{Duration, Instant};

use aura_render::RenderTimings;

const RETAINED: usize = 256;
const NAMES: [&str; 12] = [
    "parse",
    "font_resize",
    "window_resize",
    "zoom_reflow",
    "frame_total",
    "prepare",
    "fence_wait",
    "acquire",
    "upload",
    "record_submit",
    "present_call",
    "surface_rebuild",
];

pub fn flag(name: &str) -> Result<bool, std::io::Error> {
    match std::env::var_os(name) {
        None => Ok(false),
        Some(value) if value == "0" => Ok(false),
        Some(value) if value == "1" => Ok(true),
        _ => Err(std::io::Error::other(format!("{name} must be 0 or 1"))),
    }
}

#[derive(Clone)]
struct Samples {
    values: [Duration; RETAINED],
    count: u64,
    next: usize,
    maximum: Duration,
}

impl Default for Samples {
    fn default() -> Self {
        Self {
            values: [Duration::ZERO; RETAINED],
            count: 0,
            next: 0,
            maximum: Duration::ZERO,
        }
    }
}

impl Samples {
    fn add(&mut self, value: Duration) {
        self.values[self.next] = value;
        self.next = (self.next + 1) % RETAINED;
        self.count = self.count.saturating_add(1);
        self.maximum = self.maximum.max(value);
    }

    fn summary(&self) -> Option<(usize, [Duration; 3])> {
        let retained = self.count.min(RETAINED as u64) as usize;
        if retained == 0 {
            return None;
        }
        let mut sorted = self.values;
        sorted[..retained].sort_unstable();
        Some((
            retained,
            [50usize, 95, 99].map(|percentile| sorted[(retained * percentile).div_ceil(100) - 1]),
        ))
    }
}

pub struct Perf {
    window_start: Option<Instant>,
    stages: [Samples; 12],
    render_calls: u64,
}

impl Perf {
    pub fn new(enabled: bool) -> Self {
        Self {
            window_start: enabled.then(Instant::now),
            stages: std::array::from_fn(|_| Samples::default()),
            render_calls: 0,
        }
    }
    pub fn enabled(&self) -> bool {
        self.window_start.is_some()
    }
    pub fn start(&self) -> Option<Instant> {
        self.enabled().then(Instant::now)
    }
    pub fn finish(&mut self, stage: &str, start: Option<Instant>) {
        if let Some(start) = start {
            self.add(stage, Some(start.elapsed()));
        }
    }
    fn add(&mut self, stage: &str, elapsed: Option<Duration>) {
        if !self.enabled() {
            return;
        }
        if let Some(elapsed) = elapsed {
            let index = NAMES
                .iter()
                .position(|name| *name == stage)
                .expect("known timing stage");
            self.stages[index].add(elapsed);
        }
    }
    pub fn surface_rebuild(&mut self, elapsed: Option<Duration>) {
        self.add("surface_rebuild", elapsed);
    }
    pub fn renderer(&mut self, timings: RenderTimings) {
        if !self.enabled() {
            return;
        }
        self.render_calls += 1;
        for (stage, elapsed) in [
            ("prepare", timings.prepare),
            ("fence_wait", timings.fence_wait),
            ("acquire", timings.acquire),
            ("upload", timings.upload),
            ("record_submit", timings.record_submit),
            ("present_call", timings.present_call),
            ("surface_rebuild", timings.surface_rebuild),
        ] {
            self.add(stage, elapsed);
        }
    }
    pub fn maybe_report(&mut self) {
        let Some(start) = self.window_start else {
            return;
        };
        let elapsed = start.elapsed();
        if elapsed < Duration::from_secs(5) {
            return;
        }
        eprintln!(
            "aura perf: render_calls/s={:.2} host_intervals",
            self.render_calls as f64 / elapsed.as_secs_f64()
        );
        for (name, samples) in NAMES.iter().zip(&mut self.stages) {
            if let Some((retained, quantiles)) = samples.summary() {
                let [p50, p95, p99] = quantiles.map(|d| d.as_secs_f64() * 1000.0);
                eprintln!(
                    "aura perf: {name} n={} retained={retained} p50={p50:.3} p95={p95:.3} p99={p99:.3} max={:.3} ms",
                    samples.count,
                    samples.maximum.as_secs_f64() * 1000.0
                );
            }
            *samples = Samples::default();
        }
        self.render_calls = 0;
        self.window_start = Some(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn us(value: u64) -> Duration {
        Duration::from_micros(value)
    }
    #[test]
    fn empty_single_and_even_nearest_rank() {
        let mut samples = Samples::default();
        assert_eq!(samples.summary(), None);
        samples.add(us(4));
        assert_eq!(samples.summary(), Some((1, [us(4); 3])));
        samples.add(us(2));
        assert_eq!(samples.summary(), Some((2, [us(2), us(4), us(4)])));
    }
    #[test]
    fn ring_keeps_recent_quantiles_and_interval_maximum() {
        let mut samples = Samples::default();
        samples.add(us(10_000));
        for n in 1..=300 {
            samples.add(us(n));
        }
        assert_eq!(samples.count, 301);
        assert_eq!(samples.summary(), Some((256, [us(172), us(288), us(298)])));
        assert_eq!(samples.maximum, us(10_000));
    }
    #[test]
    fn unreached_stages_have_no_samples() {
        let mut perf = Perf::new(true);
        perf.renderer(RenderTimings {
            prepare: Some(us(2)),
            acquire: Some(us(3)),
            ..RenderTimings::default()
        });
        assert_eq!(perf.stages[5].count, 1);
        assert_eq!(perf.stages[7].count, 1);
        for index in [0, 1, 2, 3, 4, 6, 8, 9, 10, 11] {
            assert_eq!(perf.stages[index].count, 0);
        }
        let mut off = Perf::new(false);
        assert!(off.start().is_none());
        off.renderer(RenderTimings {
            prepare: Some(us(2)),
            ..RenderTimings::default()
        });
        assert_eq!(off.stages[5].count, 0);
    }
}
