//! Widest contiguous passing region. A strict width comparison retains the
//! first region in a tie, including coreboot's zero-width failure sentinel.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Range {
    pub start: i32,
    pub end: i32,
}
impl Range {
    pub fn width(self) -> i32 {
        self.end - self.start
    }
    pub fn center(self) -> i32 {
        self.start + self.width() / 2
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Linear {
    current: Range,
    pub largest: Range,
}
impl Linear {
    pub fn record(&mut self, pass: bool, value: i32, start: i32, step: i32) {
        if value == start {
            let x = if pass { start } else { start - step };
            self.current = Range { start: x, end: x };
            self.largest = self.current;
        } else if pass {
            if self.current.end != value - step {
                self.current.start = value;
            }
            self.current.end = value;
            self.update_largest();
        }
    }
    fn update_largest(&mut self) {
        if self.current.width() > self.largest.width() {
            self.largest = self.current;
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Phase {
    initial: Range,
    linear: Linear,
    last_pass: bool,
}
impl Phase {
    pub fn largest(self) -> Range {
        self.linear.largest
    }
    pub fn record(&mut self, pass: bool, value: i32, start: i32, step: i32) {
        self.last_pass = pass;
        self.linear.record(pass, value, start, step);
        if value == start {
            self.initial = self.linear.current;
        } else if pass && self.initial.end == value - step {
            self.initial.end = value;
        }
    }
    /// Join across the wrap at the end of a sweep. `below_start` selects the
    /// equivalent phase below the first point instead of above the last.
    pub fn wrap(&mut self, start: i32, period: i32, below_start: bool) {
        if !self.last_pass || self.initial.start != start {
            return;
        }
        let tail = &mut self.linear.current;
        if tail.start == start {
            // No failing sample (and hence no measured edge), even when the
            // step does not divide the period and never samples its last PI.
            tail.end = start + period;
        } else if below_start {
            tail.start -= period;
            tail.end = self.initial.end;
        } else {
            tail.end = self.initial.end + period;
        }
        self.linear.update_largest();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wrapped_eye_centers() {
        let mut p = Phase::default();
        for x in (0..16).step_by(2) {
            p.record(x <= 2 || x >= 10, x, 0, 2);
        }
        assert_eq!(p.largest(), Range { start: 10, end: 14 });
        let mut n = p;
        p.wrap(0, 16, false);
        n.wrap(0, 16, true);
        assert_eq!((p.largest().center(), n.largest().center()), (14, -2));
        assert_eq!(p.largest().width(), 8);
    }
    #[test]
    fn uneven_sweep_only_joins_passing_endpoints() {
        for (case, expected) in [
            (0, Range { start: 0, end: 12 }),
            (1, Range { start: 0, end: 128 }),
            (
                2,
                Range {
                    start: 120,
                    end: 140,
                },
            ),
        ] {
            let mut p = Phase::default();
            for x in (0..128).step_by(6) {
                p.record(
                    x <= 12
                        || match case {
                            0 => (60..=72).contains(&x),
                            1 => true,
                            _ => x >= 120,
                        },
                    x,
                    0,
                    6,
                );
            }
            p.wrap(0, 128, false);
            assert_eq!(p.largest(), expected);
        }
    }
    #[test]
    fn failed_first_point_and_tied_eyes() {
        let mut l = Linear::default();
        for x in -8..7 {
            l.record((-5..=-3).contains(&x) || (3..=5).contains(&x), x, -8, 1);
        }
        assert_eq!(l.largest, Range { start: -5, end: -3 });
    }
}
