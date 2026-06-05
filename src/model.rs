use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BreakType {
    Short,
    Long,
}

#[derive(Clone, Debug)]
pub struct Break {
    pub kind: BreakType,
    pub name: String,
    pub interval: i64,
    pub duration: i64,
    pub image: Option<String>,
}

impl Break {
    pub fn is_long(&self) -> bool {
        self.kind == BreakType::Long
    }
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_short(&self) -> bool {
        self.kind == BreakType::Short
    }
}

/// interleaves shorts with periodic longs. defaults give four shorts then a long, repeating.
pub struct BreakQueue {
    long_interval: i64,
    random_order: bool,
    shorts: Vec<Break>,
    longs: Vec<Break>,
    cur_short: usize,
    cur_long: usize,
    /// shorts eat into this. when it drops to or below the next short's interval, a long takes the slot.
    seconds_until_long: i64,
    current: Break,
    rng: Rng,
}

impl BreakQueue {
    pub fn create(
        _short_interval: i64,
        long_interval: i64,
        random_order: bool,
        mut shorts: Vec<Break>,
        mut longs: Vec<Break>,
    ) -> Option<BreakQueue> {
        if shorts.is_empty() && longs.is_empty() {
            return None;
        }

        let mut rng = Rng::new();
        if random_order {
            rng.shuffle(&mut shorts);
            rng.shuffle(&mut longs);
        }

        let mut q = BreakQueue {
            long_interval,
            random_order,
            shorts,
            longs,
            cur_short: 0,
            cur_long: 0,
            seconds_until_long: long_interval,
            // overwritten by select_next below
            current: Break {
                kind: BreakType::Short,
                name: String::new(),
                interval: 0,
                duration: 0,
                image: None,
            },
            rng,
        };
        q.select_next(None);
        Some(q)
    }

    pub fn current(&self) -> &Break {
        &self.current
    }

    pub fn has_short(&self) -> bool {
        !self.shorts.is_empty()
    }

    pub fn has_long(&self) -> bool {
        !self.longs.is_empty()
    }

    pub fn advance(&mut self, force: Option<BreakType>) -> &Break {
        self.select_next(force);
        &self.current
    }

    fn select_next(&mut self, force: Option<BreakType>) {
        let has_s = self.has_short();
        let has_l = self.has_long();

        let pick_long = if !has_s {
            true
        } else if !has_l {
            false
        } else {
            force == Some(BreakType::Long)
                || self.seconds_until_long <= self.shorts[self.cur_short].interval
        };

        if pick_long {
            let mut b = self.longs[self.cur_long].clone();
            if has_s {
                // wait the leftover, then reset the long countdown
                b.interval = self.seconds_until_long.max(0);
            }
            self.seconds_until_long = self.long_interval;
            self.current = b;

            self.cur_long += 1;
            if self.cur_long >= self.longs.len() {
                self.cur_long = 0;
                if self.random_order {
                    self.rng.shuffle(&mut self.longs);
                }
            }
        } else {
            let b = self.shorts[self.cur_short].clone();
            self.seconds_until_long -= b.interval;
            self.current = b;

            self.cur_short += 1;
            if self.cur_short >= self.shorts.len() {
                self.cur_short = 0;
                if self.random_order {
                    self.rng.shuffle(&mut self.shorts);
                }
            }
        }
    }

    /// used by smart-pause to decide whether a long idle already counts as a long break
    pub fn next_long_duration(&self) -> Option<i64> {
        self.longs.get(self.cur_long).map(|b| b.duration)
    }

    /// reset the long countdown and skip past a current long. used when a long idle stood in for it.
    pub fn skip_long_break(&mut self) {
        self.seconds_until_long = self.long_interval;
        if self.current.is_long() && self.has_short() {
            self.select_next(Some(BreakType::Short));
        }
    }
}

/// xorshift64, just enough to shuffle break order without a dep
struct Rng(u64);

impl Rng {
    fn new() -> Self {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9e37_79b9_7f4a_7c15)
            | 1;
        Rng(seed)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn shuffle<T>(&mut self, s: &mut [T]) {
        for i in (1..s.len()).rev() {
            let j = (self.next_u64() % (i as u64 + 1)) as usize;
            s.swap(i, j);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(kind: BreakType, name: &str, interval: i64) -> Break {
        Break {
            kind,
            name: name.into(),
            interval,
            duration: if kind == BreakType::Short { 15 } else { 60 },
            image: None,
        }
    }

    fn shorts(n: usize, interval: i64) -> Vec<Break> {
        (0..n)
            .map(|i| mk(BreakType::Short, &format!("s{i}"), interval))
            .collect()
    }
    fn longs(n: usize, interval: i64) -> Vec<Break> {
        (0..n)
            .map(|i| mk(BreakType::Long, &format!("l{i}"), interval))
            .collect()
    }

    #[test]
    fn interleaves_four_shorts_then_long() {
        let mut q = BreakQueue::create(15, 75, false, shorts(13, 15), longs(6, 75)).unwrap();
        // create() already picked the first break
        let mut types = vec![q.current().kind];
        for _ in 0..9 {
            types.push(q.advance(None).kind);
        }
        use BreakType::*;
        assert_eq!(
            types,
            vec![Short, Short, Short, Short, Long, Short, Short, Short, Short, Long]
        );
    }

    #[test]
    fn long_break_waits_leftover() {
        let mut q = BreakQueue::create(15, 75, false, shorts(13, 15), longs(6, 75)).unwrap();
        // s0..s3 then the long. the long is due after the 15s leftover, not its configured 75.
        for _ in 0..4 {
            q.advance(None);
        }
        assert!(q.current().is_long());
        assert_eq!(q.current().interval, 15);
    }

    #[test]
    fn only_short_breaks() {
        let mut q = BreakQueue::create(15, 75, false, shorts(3, 15), vec![]).unwrap();
        assert!(q.current().is_short());
        for _ in 0..5 {
            assert!(q.advance(None).is_short());
        }
    }

    #[test]
    fn only_long_breaks() {
        let q = BreakQueue::create(15, 75, false, vec![], longs(2, 75)).unwrap();
        assert!(q.current().is_long());
        assert_eq!(q.current().interval, 75);
    }

    #[test]
    fn empty_queue_is_none() {
        assert!(BreakQueue::create(15, 75, false, vec![], vec![]).is_none());
    }
}
