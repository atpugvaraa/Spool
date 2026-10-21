//! Opt-in, per-thread diagnostics, enabled in debug builds by the presence of
//! `SPOOL_DIAGNOSTICS` (even an empty value). The environment is checked once.
//! Reports do not reset state. Timer totals and averages include every event;
//! percentiles use the most recent 4,096 samples, with nearest-rank selection.
//! Geometry visibility is independent of diagnostics and remains active in release.

use std::time::Instant;

#[cfg(debug_assertions)]
pub use debug::{count, enabled, record, report, reset, start};

/// Turn the diagnostics on for the calling thread, in a test.
///
/// The environment variable is the way the app turns them on; a test has no
/// environment to speak of, and a test that cannot see the numbers the module
/// keeps cannot use them. A release build has no diagnostics to turn on, so
/// this does nothing there.
#[cfg(test)]
pub fn enable_for_tests() {
    #[cfg(debug_assertions)]
    debug::enable_for_tests();
}

#[cfg(not(debug_assertions))]
#[inline]
pub fn enabled() -> bool {
    false
}

#[cfg(not(debug_assertions))]
#[inline]
pub fn start() -> Option<Instant> {
    None
}

#[cfg(not(debug_assertions))]
#[inline]
pub fn record(_kind: &'static str, _start: Option<Instant>) {}

#[cfg(not(debug_assertions))]
#[inline]
pub fn count(_kind: &'static str, _amount: u64) {}

#[cfg(not(debug_assertions))]
#[inline]
pub fn reset() {}

#[cfg(not(debug_assertions))]
#[inline]
pub fn report(_label: &str) {}

/// Tests only the geometry bounding box against the viewport, including touching
/// edges. Offset is in world units; viewport is in screen units. Stroke, shadows,
/// and other visual overflow are deliberately excluded. Invalid geometry, empty
/// viewports, and non-positive zoom are not visible.
#[inline]
pub fn intersects(
    position: gpui::Point<f32>,
    size: gpui::Size<f32>,
    offset: gpui::Point<f32>,
    viewport: gpui::Size<f32>,
    zoom: f32,
) -> bool {
    if ![
        position.x,
        position.y,
        size.width,
        size.height,
        offset.x,
        offset.y,
        viewport.width,
        viewport.height,
        zoom,
    ]
    .iter()
    .all(|value| value.is_finite())
        || zoom <= 0.0
        || size.width < 0.0
        || size.height < 0.0
        || viewport.width <= 0.0
        || viewport.height <= 0.0
    {
        return false;
    }

    // Compute in f64 to avoid overflow for finite f32 coordinates and zoom.
    let left = (f64::from(position.x) - f64::from(offset.x)) * f64::from(zoom);
    let top = (f64::from(position.y) - f64::from(offset.y)) * f64::from(zoom);
    let right = left + f64::from(size.width) * f64::from(zoom);
    let bottom = top + f64::from(size.height) * f64::from(zoom);
    left <= f64::from(viewport.width)
        && top <= f64::from(viewport.height)
        && right >= 0.0
        && bottom >= 0.0
}

#[cfg(debug_assertions)]
mod debug {
    use super::Instant;
    use std::{cell::RefCell, collections::BTreeMap, sync::OnceLock};

    const MAX_SAMPLES: usize = 4096;
    static ENABLED: OnceLock<bool> = OnceLock::new();

    thread_local! {
        static STATS: RefCell<BTreeMap<&'static str, Stats>> = const { RefCell::new(BTreeMap::new()) };
    }

    #[derive(Default)]
    struct Stats {
        counter: Option<u64>,
        timer_samples: u64,
        total_us: f64,
        samples: Vec<f64>,
        next_sample: usize,
    }

    impl Stats {
        fn count(&mut self, amount: u64) {
            self.counter = Some(self.counter.unwrap_or(0).saturating_add(amount));
        }

        fn record_us(&mut self, elapsed_us: f64) {
            self.timer_samples = self.timer_samples.saturating_add(1);
            self.total_us += elapsed_us;
            if self.samples.is_empty() {
                // Reserve once per timer kind, never grow on subsequent events.
                self.samples.reserve_exact(MAX_SAMPLES);
            }
            if self.samples.len() < MAX_SAMPLES {
                self.samples.push(elapsed_us);
            } else {
                self.samples[self.next_sample] = elapsed_us;
                self.next_sample = (self.next_sample + 1) % MAX_SAMPLES;
            }
        }
    }

    /// Nearest-rank percentile of an ascending slice; empty input yields zero.
    fn percentile(sorted: &[f64], percent: usize) -> f64 {
        if sorted.is_empty() {
            return 0.0;
        }
        let rank = (sorted.len() * percent.min(100)).div_ceil(100);
        sorted[rank.saturating_sub(1)]
    }

    #[cfg(test)]
    thread_local! { static TEST_ENABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

    /// Turn the diagnostics on for this thread, in a test.
    #[cfg(test)]
    pub fn enable_for_tests() {
        TEST_ENABLED.with(|enabled| enabled.set(true));
    }

    pub fn enabled() -> bool {
        #[cfg(test)]
        if TEST_ENABLED.with(|enabled| enabled.get()) {
            return true;
        }
        *ENABLED.get_or_init(|| std::env::var_os("SPOOL_DIAGNOSTICS").is_some())
    }

    pub fn start() -> Option<Instant> {
        enabled().then(Instant::now)
    }

    pub fn record(kind: &'static str, start: Option<Instant>) {
        let Some(start) = start else { return };
        if !enabled() {
            return;
        }
        let elapsed_us = start.elapsed().as_secs_f64() * 1_000_000.0;
        STATS.with(|stats| {
            stats
                .borrow_mut()
                .entry(kind)
                .or_default()
                .record_us(elapsed_us)
        });
    }

    pub fn count(kind: &'static str, amount: u64) {
        if enabled() {
            STATS.with(|stats| stats.borrow_mut().entry(kind).or_default().count(amount));
        }
    }

    pub fn reset() {
        if enabled() {
            STATS.with(|stats| stats.borrow_mut().clear());
        }
    }

    /// Emits sorted, stable key/value lines to stderr for the calling thread.
    /// Counters have zero timing fields. A kind used for both APIs emits two lines.
    /// Labels and kinds use Rust debug-string escaping to keep each record one line.
    pub fn report(label: &str) {
        if !enabled() {
            return;
        }
        STATS.with(|stats| {
            for (kind, stat) in stats.borrow().iter() {
                if let Some(value) = stat.counter {
                    eprintln!(
                        "spool_diagnostics label={label:?} kind={kind:?} type=counter count={value} samples=0 avg_us=0.000 p50_us=0.000 p95_us=0.000 p99_us=0.000 total_us=0.000"
                    );
                }
                if stat.timer_samples > 0 {
                    let mut sorted = stat.samples.clone();
                    sorted.sort_unstable_by(f64::total_cmp);
                    eprintln!(
                        "spool_diagnostics label={label:?} kind={kind:?} type=timer samples={} retained={} avg_us={:.3} p50_us={:.3} p95_us={:.3} p99_us={:.3} total_us={:.3}",
                        stat.timer_samples,
                        sorted.len(),
                        stat.total_us / stat.timer_samples as f64,
                        percentile(&sorted, 50),
                        percentile(&sorted, 95),
                        percentile(&sorted, 99),
                        stat.total_us,
                    );
                }
            }
        });
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn counters_accumulate_and_reset() {
            // Thread-local override avoids races with other tests or environment mutation.
            TEST_ENABLED.with(|enabled| enabled.set(true));
            reset();
            count("objects", 2);
            count("objects", 3);
            count("empty", 0);
            STATS.with(|stats| {
                let stats = stats.borrow();
                assert_eq!(stats["objects"].counter, Some(5));
                assert_eq!(stats["empty"].counter, Some(0));
            });
            reset();
            STATS.with(|stats| assert!(stats.borrow().is_empty()));
            TEST_ENABLED.with(|enabled| enabled.set(false));
        }

        #[test]
        fn nearest_rank_percentiles() {
            assert_eq!(percentile(&[], 95), 0.0);
            assert_eq!(percentile(&[7.0], 99), 7.0);
            let values: Vec<_> = (1..=100).map(f64::from).collect();
            for percent in [50, 95, 99] {
                assert_eq!(percentile(&values, percent), percent as f64);
            }
            assert_eq!(percentile(&[1.0, 2.0, 3.0], 50), 2.0);
            assert_eq!(percentile(&values, 0), 1.0);
            assert_eq!(percentile(&values, 100), 100.0);
        }

        #[test]
        fn timer_samples_are_bounded_and_totals_include_evicted_samples() {
            let mut stats = Stats::default();
            for value in 1..=MAX_SAMPLES + 10 {
                stats.record_us(value as f64);
            }
            assert_eq!(stats.samples.len(), MAX_SAMPLES);
            assert_eq!(stats.timer_samples, (MAX_SAMPLES + 10) as u64);
            let n = (MAX_SAMPLES + 10) as f64;
            assert_eq!(stats.total_us, n * (n + 1.0) / 2.0);
            assert_eq!(stats.samples.iter().copied().reduce(f64::min), Some(11.0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, size};

    #[test]
    fn visibility_includes_all_touching_edges() {
        for position in [
            point(-10.0, 40.0),
            point(100.0, 40.0),
            point(40.0, -10.0),
            point(40.0, 100.0),
            point(100.0, 100.0),
            point(40.0, 40.0),
        ] {
            assert!(intersects(
                position,
                size(10.0, 10.0),
                point(0.0, 0.0),
                size(100.0, 100.0),
                1.0
            ));
        }
    }

    #[test]
    fn visibility_rejects_offscreen_bounds() {
        for position in [
            point(-11.0, 40.0),
            point(101.0, 40.0),
            point(40.0, -11.0),
            point(40.0, 101.0),
        ] {
            assert!(!intersects(
                position,
                size(10.0, 10.0),
                point(0.0, 0.0),
                size(100.0, 100.0),
                1.0
            ));
        }
    }

    #[test]
    fn visibility_applies_world_offset_and_zoom() {
        assert!(intersects(
            point(60.0, 20.0),
            size(5.0, 5.0),
            point(10.0, 20.0),
            size(100.0, 100.0),
            2.0
        ));
        assert!(!intersects(
            point(61.0, 20.0),
            size(5.0, 5.0),
            point(10.0, 20.0),
            size(100.0, 100.0),
            2.0
        ));
    }

    #[test]
    fn visibility_rejects_invalid_inputs() {
        for zoom in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert!(!intersects(
                point(0.0, 0.0),
                size(10.0, 10.0),
                point(0.0, 0.0),
                size(100.0, 100.0),
                zoom
            ));
        }
        assert!(!intersects(
            point(0.0, 0.0),
            size(10.0, 10.0),
            point(0.0, 0.0),
            size(0.0, 100.0),
            1.0
        ));
    }

    #[cfg(not(debug_assertions))]
    #[test]
    fn release_diagnostics_are_disabled() {
        assert!(!enabled());
        assert!(start().is_none());
        count("counter", 1);
        record("timer", Some(Instant::now()));
        reset();
        report("release");
    }
}
