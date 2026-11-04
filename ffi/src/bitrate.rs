//! Follows what the connection can carry. The queue to the network filling up is the signal: it
//! shows the moment the encoder outruns Wi-Fi, before frames have to be dropped. Time comes from
//! the frames themselves.

const CUT_TO_PERCENT: u64 = 70;
const RAISE_TO_PERCENT: u64 = 115;
const MIN_BITRATE: u32 = 1_500_000;
/// How long the encoder and the queue get to settle after a cut.
const CUT_EVERY_US: i64 = 2_000_000;
const RAISE_AFTER_US: i64 = 8_000_000;
const LONGEST_RAISE_AFTER_US: i64 = 64_000_000;
/// A cut this soon after raising means that level was too much, so it waits longer next time.
const FAILED_RAISE_US: i64 = 5_000_000;

pub struct BitrateGovernor {
    target: u32,
    current: u32,
    floor: u32,
    last_cut: Option<i64>,
    last_raise: Option<i64>,
    clear_since: Option<i64>,
    raise_after: i64,
}

impl BitrateGovernor {
    pub fn new(target: u32) -> Self {
        Self {
            target,
            current: target,
            floor: MIN_BITRATE.min(target),
            last_cut: None,
            last_raise: None,
            clear_since: None,
            raise_after: RAISE_AFTER_US,
        }
    }

    fn scaled(&self, percent: u64) -> u32 {
        (u64::from(self.current) * percent / 100) as u32
    }

    /// Called for each frame, with how full the queue was. A new bitrate when it should change.
    pub fn on_frame(&mut self, pts_us: i64, queued: usize, capacity: usize) -> Option<u32> {
        if queued * 4 >= capacity * 3 {
            self.clear_since = None;
            if self.current <= self.floor || self.last_cut.is_some_and(|at| pts_us - at < CUT_EVERY_US) {
                return None;
            }
            if self.last_raise.is_some_and(|at| pts_us - at < FAILED_RAISE_US) {
                self.raise_after = (self.raise_after * 2).min(LONGEST_RAISE_AFTER_US);
            }
            self.last_cut = Some(pts_us);
            self.current = self.scaled(CUT_TO_PERCENT).max(self.floor);
            return Some(self.current);
        }

        if self.current >= self.target {
            self.clear_since = None;
            self.raise_after = RAISE_AFTER_US;
            return None;
        }
        let since = *self.clear_since.get_or_insert(pts_us);
        if pts_us - since < self.raise_after {
            return None;
        }
        self.clear_since = Some(pts_us);
        self.last_raise = Some(pts_us);
        self.current = self.scaled(RAISE_TO_PERCENT).min(self.target);
        Some(self.current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: i64 = 1_000_000;
    const CAPACITY: usize = 8;

    /// Frames at 10 a second over a span of time. What it asked for along the way.
    fn run(governor: &mut BitrateGovernor, from: i64, seconds: i64, queued: usize) -> Vec<u32> {
        (0..seconds * 10).filter_map(|frame| governor.on_frame(from + frame * SECOND / 10, queued, CAPACITY)).collect()
    }

    #[test]
    fn a_clear_connection_changes_nothing() {
        let mut governor = BitrateGovernor::new(16_000_000);
        assert!(run(&mut governor, 0, 120, 1).is_empty());
    }

    #[test]
    fn a_filling_queue_cuts_the_bitrate_and_not_too_often() {
        let mut governor = BitrateGovernor::new(10_000_000);
        assert_eq!(run(&mut governor, 0, 5, 6), vec![7_000_000, 4_900_000, 3_430_000]);
    }

    #[test]
    fn it_never_goes_below_the_floor() {
        let mut governor = BitrateGovernor::new(10_000_000);
        let asked = run(&mut governor, 0, 60, CAPACITY);
        assert_eq!(*asked.last().unwrap(), MIN_BITRATE);
        assert_eq!(asked.iter().filter(|rate| **rate == MIN_BITRATE).count(), 1, "stops asking at the floor");
    }

    #[test]
    fn a_stream_already_below_the_floor_stays_put() {
        let mut governor = BitrateGovernor::new(MIN_BITRATE / 2);
        assert!(run(&mut governor, 0, 30, CAPACITY).is_empty());
    }

    #[test]
    fn it_climbs_back_to_the_target_once_clear() {
        let mut governor = BitrateGovernor::new(10_000_000);
        run(&mut governor, 0, 1, CAPACITY);
        let asked = run(&mut governor, SECOND, 60, 0);
        assert_eq!(*asked.last().unwrap(), 10_000_000);
        assert!(asked.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(run(&mut governor, 61 * SECOND, 60, 0).is_empty(), "and stays there");
    }

    #[test]
    fn a_raise_that_fails_waits_longer_next_time() {
        let mut governor = BitrateGovernor::new(10_000_000);
        run(&mut governor, 0, 1, CAPACITY);

        // Clear for long enough to try once, then full again straight away
        let asked = run(&mut governor, SECOND, 9, 0);
        assert_eq!(asked.len(), 1);
        let asked = run(&mut governor, 10 * SECOND, 1, CAPACITY);
        assert_eq!(asked.len(), 1);

        assert!(run(&mut governor, 11 * SECOND, 9, 0).is_empty(), "the old wait isn't enough now");
        assert_eq!(run(&mut governor, 20 * SECOND, 9, 0).len(), 1);
    }

    #[test]
    fn half_full_is_not_congestion() {
        let mut governor = BitrateGovernor::new(10_000_000);
        assert!(run(&mut governor, 0, 30, 5).is_empty());
    }
}
