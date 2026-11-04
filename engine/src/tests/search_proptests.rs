use super::*;
use proptest::prelude::*;

struct MonotoneGate {
    ceiling: u32,
}
impl Probe for MonotoneGate {
    fn probe(&mut self, c: u32) -> Option<Sample> {
        Some(Sample::new(c as f64, c <= self.ceiling))
    }
}

struct AlwaysPasses;
impl Probe for AlwaysPasses {
    fn probe(&mut self, c: u32) -> Option<Sample> {
        Some(Sample::new(c as f64, true))
    }
}

proptest! {
    // Any monotone pass/fail curve with a true ceiling inside [min_conc, max_conc) lands on exactly that
    // ceiling, and the recorded proof is ceiling+1, measured and failing.
    #[test]
    fn bisect_lands_on_any_true_ceiling(ceiling in 1u32..999u32) {
        let min_conc = 1u32;
        let max_conc = 1000u32;
        let mut probe = MonotoneGate { ceiling };
        let r = bisect_ceiling(&mut probe, min_conc, max_conc);
        prop_assert_eq!(r.ceiling.copied(), Some(ceiling));
        prop_assert!(r.points.iter().any(|p| p.concurrency == ceiling + 1 && !p.passed));
    }

    // A curve that still passes at the top of the range never yields a number.
    #[test]
    fn bisect_top_passing_is_always_exhausted(min_conc in 1u32..100u32, max_conc in 100u32..10_000u32) {
        let mut probe = AlwaysPasses;
        let r = bisect_ceiling(&mut probe, min_conc, max_conc);
        prop_assert_eq!(r.ceiling.copied(), None);
        prop_assert_eq!(r.ceiling.reason(), Some(&Absent::SearchExhausted));
    }

}
