//! the configured breaks and the order each kind comes in. which kind is due when is the
//! scheduler's call.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BreakType {
    Short,
    Long,
}

impl BreakType {
    fn other(self) -> BreakType {
        match self {
            BreakType::Short => BreakType::Long,
            BreakType::Long => BreakType::Short,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Break {
    pub kind: BreakType,
    pub name: String,
    pub duration: Duration,
    pub image: Option<String>,
}

/// the configured breaks, never both lists empty
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Breaks {
    shorts: Vec<Break>,
    longs: Vec<Break>,
}

impl Breaks {
    pub fn new(shorts: Vec<Break>, longs: Vec<Break>) -> Option<Breaks> {
        (!shorts.is_empty() || !longs.is_empty()).then_some(Breaks { shorts, longs })
    }
}

/// cycles through each kind's breaks, reshuffling every round when the order is random
pub struct BreakQueue {
    shorts: Deck,
    longs: Deck,
    /// None keeps the configured order
    rng: Option<Rng>,
}

struct Deck {
    breaks: Vec<Break>,
    next: usize,
}

impl BreakQueue {
    /// `seed` seeds the shuffle when the order is random
    pub fn new(breaks: &Breaks, random_order: bool, seed: SystemTime) -> BreakQueue {
        let mut rng = random_order.then(|| Rng::new(seed));
        let mut deck = |breaks: &[Break]| {
            let mut breaks = breaks.to_vec();
            if let Some(rng) = &mut rng {
                rng.shuffle(&mut breaks);
            }
            Deck { breaks, next: 0 }
        };
        BreakQueue {
            shorts: deck(&breaks.shorts),
            longs: deck(&breaks.longs),
            rng,
        }
    }

    pub fn has(&self, kind: BreakType) -> bool {
        !self.deck(kind).breaks.is_empty()
    }

    /// the next break of `kind`, or of the other kind when none of `kind` are configured
    pub fn peek(&self, kind: BreakType) -> &Break {
        let kind = if self.has(kind) { kind } else { kind.other() };
        let deck = self.deck(kind);
        &deck.breaks[deck.next]
    }

    /// moves `kind` on to its following break
    pub fn advance(&mut self, kind: BreakType) {
        let deck = match kind {
            BreakType::Short => &mut self.shorts,
            BreakType::Long => &mut self.longs,
        };
        if deck.breaks.is_empty() {
            return;
        }
        deck.next += 1;
        if deck.next == deck.breaks.len() {
            deck.next = 0;
            if let Some(rng) = &mut self.rng {
                rng.shuffle(&mut deck.breaks);
            }
        }
    }

    fn deck(&self, kind: BreakType) -> &Deck {
        match kind {
            BreakType::Short => &self.shorts,
            BreakType::Long => &self.longs,
        }
    }
}

/// xorshift64, just enough to shuffle break order without a dep
struct Rng(u64);

impl Rng {
    fn new(seed: SystemTime) -> Self {
        let seed = seed
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
pub fn brk(kind: BreakType, name: &str, duration_secs: u64) -> Break {
    Break {
        kind,
        name: name.into(),
        duration: Duration::from_secs(duration_secs),
        image: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(kind: BreakType, names: &[&str]) -> Vec<Break> {
        names.iter().map(|n| brk(kind, n, 15)).collect()
    }

    fn take_names(q: &mut BreakQueue, kind: BreakType, n: usize) -> Vec<String> {
        (0..n)
            .map(|_| {
                let name = q.peek(kind).name.clone();
                q.advance(kind);
                name
            })
            .collect()
    }

    #[test]
    fn fixed_order_cycles_each_kind_on_its_own() {
        let breaks = Breaks::new(
            named(BreakType::Short, &["s0", "s1", "s2"]),
            named(BreakType::Long, &["l0", "l1"]),
        )
        .unwrap();
        let mut q = BreakQueue::new(&breaks, false, UNIX_EPOCH);
        assert_eq!(
            take_names(&mut q, BreakType::Short, 4),
            ["s0", "s1", "s2", "s0"]
        );
        assert_eq!(take_names(&mut q, BreakType::Long, 3), ["l0", "l1", "l0"]);
        assert_eq!(q.peek(BreakType::Short).name, "s1");
    }

    #[test]
    fn peeking_does_not_advance() {
        let breaks = Breaks::new(named(BreakType::Short, &["s0", "s1"]), vec![]).unwrap();
        let q = BreakQueue::new(&breaks, false, UNIX_EPOCH);
        assert_eq!(q.peek(BreakType::Short).name, "s0");
        assert_eq!(q.peek(BreakType::Short).name, "s0");
    }

    #[test]
    fn random_order_shows_every_break_once_per_round() {
        let names = ["a", "b", "c", "d", "e"];
        let breaks = Breaks::new(named(BreakType::Short, &names), vec![]).unwrap();
        let mut q = BreakQueue::new(
            &breaks,
            true,
            UNIX_EPOCH + Duration::from_nanos(123_456_789),
        );
        for _ in 0..3 {
            let mut round = take_names(&mut q, BreakType::Short, names.len());
            round.sort();
            assert_eq!(round, names);
        }
    }

    #[test]
    fn a_missing_kind_falls_back_to_the_other() {
        let breaks = Breaks::new(named(BreakType::Short, &["s0"]), vec![]).unwrap();
        let mut q = BreakQueue::new(&breaks, false, UNIX_EPOCH);
        assert!(!q.has(BreakType::Long));
        assert_eq!(q.peek(BreakType::Long).kind, BreakType::Short);
        q.advance(BreakType::Long);
        assert_eq!(q.peek(BreakType::Short).name, "s0");
    }

    #[test]
    fn breaks_must_not_be_empty() {
        assert!(Breaks::new(vec![], vec![]).is_none());
    }
}
