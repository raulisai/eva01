//! The p50/p95 latency numbers from `docs/PLAN.md` §7 — the metric that
//! actually blocks a release, unlike WER: "el p50 miente: lo que te saca de
//! la herramienta es la cola, no la mediana."

/// Returns the `p`th percentile (`0.0..=100.0`) of `values`, using the
/// nearest-rank method. `values` does not need to be pre-sorted — this
/// clones and sorts its own copy. Returns `None` for an empty slice.
pub fn percentile(values: &[std::time::Duration], p: f64) -> Option<std::time::Duration> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort();

    // Nearest-rank: index = ceil(p/100 * n) - 1, clamped into range.
    let n = sorted.len();
    let rank = ((p / 100.0) * n as f64).ceil() as usize;
    let index = rank.saturating_sub(1).min(n - 1);
    Some(sorted[index])
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;
    use std::time::Duration;

    fn ms(values: &[u64]) -> Vec<Duration> {
        values.iter().map(|&v| Duration::from_millis(v)).collect()
    }

    #[test]
    fn empty_input_returns_none() {
        assert_eq!(percentile(&[], 50.0), None);
    }

    #[test]
    fn single_value_is_every_percentile() {
        let values = ms(&[100]);
        assert_eq!(percentile(&values, 50.0), Some(Duration::from_millis(100)));
        assert_eq!(percentile(&values, 95.0), Some(Duration::from_millis(100)));
    }

    #[test]
    fn p50_of_ten_sorted_values_is_the_median() {
        let values = ms(&[10, 20, 30, 40, 50, 60, 70, 80, 90, 100]);
        assert_eq!(percentile(&values, 50.0), Some(Duration::from_millis(50)));
    }

    #[test]
    fn p95_of_ten_values_is_close_to_the_top() {
        let values = ms(&[10, 20, 30, 40, 50, 60, 70, 80, 90, 100]);
        assert_eq!(percentile(&values, 95.0), Some(Duration::from_millis(100)));
    }

    #[test]
    fn works_regardless_of_input_order() {
        let sorted = ms(&[10, 20, 30, 40, 50]);
        let shuffled = ms(&[40, 10, 50, 20, 30]);
        assert_eq!(percentile(&sorted, 50.0), percentile(&shuffled, 50.0));
    }

    #[test]
    fn a_p95_outlier_is_visible_even_when_p50_looks_fine() {
        // This is the whole reason `docs/PLAN.md` §7 insists on p95, not p50:
        // one slow run must show up here even though the median looks great.
        //
        // The sample count matters for nearest-rank: with exactly 20 values,
        // `ceil(0.95 * 20) = 19` lands on the 19th value, and 19/20 = 95%
        // of the samples genuinely are <=100ms — the single outlier is
        // mathematically just *above* p95 (in the top 5%), not at it. Using
        // 18 good values (19 total) makes `ceil(0.95 * 19) = 19` land on
        // the last slot, the outlier, which is the case this test means to
        // exercise.
        let mut values = vec![Duration::from_millis(100); 18];
        values.push(Duration::from_secs(5));
        let p50 = percentile(&values, 50.0).expect("must succeed");
        let p95 = percentile(&values, 95.0).expect("must succeed");
        assert_eq!(p50, Duration::from_millis(100));
        assert_eq!(p95, Duration::from_secs(5));
    }

    proptest::proptest! {
        #[test]
        fn never_panics_on_arbitrary_millisecond_lists(ms_values in proptest::collection::vec(0u64..100_000, 0..50), p in 0.0f64..=100.0f64) {
            let values: Vec<_> = ms_values.into_iter().map(std::time::Duration::from_millis).collect();
            let _ = percentile(&values, p);
        }
    }
}
