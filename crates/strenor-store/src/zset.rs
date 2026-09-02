//! Sorted set: members ranked by an `f64` score.
//!
//! Unlike the other structures, the engine *does* interpret one thing here — the
//! score — because it must order and range by it. The **member** stays an opaque
//! `[tag][payload]` blob (decoded by the JS layer), exactly like Redis, which
//! orders by score without interpreting the member.
//!
//! Two views are kept in sync:
//! - `scores: HashMap<member, score>` — O(1) `zscore`/`zadd`/`zrem`.
//! - `sorted: BTreeSet<(OrderedF64, member)>` — ordered `zrange`/`zrank`.
//!
//! Ties (equal scores) break by member bytes, matching Redis's lexicographic
//! tiebreak.

use std::collections::{BTreeSet, HashMap};

/// An `f64` that is totally ordered so it can live in a `BTreeSet`.
///
/// `f64` isn't `Ord` because of `NaN`. We reject `NaN` at the door (`zadd`
/// refuses it), so every value here is a real number and `total_cmp` gives a
/// correct, panic-free ordering.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrderedF64(pub f64);

impl Eq for OrderedF64 {}

impl PartialOrd for OrderedF64 {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderedF64 {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

/// A member together with its score, returned by the `*_with_scores` queries.
#[derive(Debug, Clone, PartialEq)]
pub struct Scored {
    pub member: Vec<u8>,
    pub score: f64,
}

#[derive(Default, Clone)]
pub struct ZSet {
    scores: HashMap<Vec<u8>, f64>,
    sorted: BTreeSet<(OrderedF64, Vec<u8>)>,
}

impl ZSet {
    pub fn new() -> Self {
        ZSet::default()
    }

    pub fn len(&self) -> usize {
        self.scores.len()
    }

    pub fn is_empty(&self) -> bool {
        self.scores.is_empty()
    }

    /// Insert or update `member` with `score`. Returns true if the member is new.
    /// Rejects `NaN` (the caller surfaces this as an error).
    pub fn add(&mut self, member: Vec<u8>, score: f64) -> Result<bool, ()> {
        if score.is_nan() {
            return Err(());
        }
        match self.scores.insert(member.clone(), score) {
            Some(old) => {
                // Move the entry in the ordered view to its new position.
                self.sorted.remove(&(OrderedF64(old), member.clone()));
                self.sorted.insert((OrderedF64(score), member));
                Ok(false)
            }
            None => {
                self.sorted.insert((OrderedF64(score), member));
                Ok(true)
            }
        }
    }

    /// Add `delta` to a member's score (creating it at `delta` if absent) and
    /// return the new score. Rejects a non-finite result.
    pub fn incr_by(&mut self, member: Vec<u8>, delta: f64) -> Result<f64, ()> {
        let current = self.scores.get(&member).copied().unwrap_or(0.0);
        let next = current + delta;
        if !next.is_finite() {
            return Err(());
        }
        self.add(member, next)?;
        Ok(next)
    }

    pub fn score(&self, member: &[u8]) -> Option<f64> {
        self.scores.get(member).copied()
    }

    /// Remove `member`. Returns true if it was present.
    pub fn remove(&mut self, member: &[u8]) -> bool {
        match self.scores.remove(member) {
            Some(score) => {
                self.sorted.remove(&(OrderedF64(score), member.to_vec()));
                true
            }
            None => false,
        }
    }

    /// 0-based rank (position) of `member`, low score first. `None` if missing.
    pub fn rank(&self, member: &[u8]) -> Option<u32> {
        let score = self.scores.get(member)?;
        let target = (OrderedF64(*score), member.to_vec());
        Some(self.sorted.range(..&target).count() as u32)
    }

    /// Members in the inclusive rank range `[start, stop]`, low score first.
    /// Redis-style indices: negative counts from the end, out-of-range clamps.
    pub fn range(&self, start: i64, stop: i64) -> Vec<Vec<u8>> {
        self.range_scored(start, stop)
            .into_iter()
            .map(|s| s.member)
            .collect()
    }

    /// Same as `range`, but each element carries its score.
    pub fn range_scored(&self, start: i64, stop: i64) -> Vec<Scored> {
        let len = self.sorted.len() as i64;
        let mut s = if start < 0 { len + start } else { start };
        let mut t = if stop < 0 { len + stop } else { stop };
        if s < 0 {
            s = 0;
        }
        if t >= len {
            t = len - 1;
        }
        if len == 0 || s > t || s >= len {
            return Vec::new();
        }
        self.sorted
            .iter()
            .skip(s as usize)
            .take((t - s + 1) as usize)
            .map(|(score, member)| Scored {
                member: member.clone(),
                score: score.0,
            })
            .collect()
    }

    /// Every member with its score, in rank order — used for snapshots/AOF.
    pub fn entries(&self) -> impl Iterator<Item = (&Vec<u8>, f64)> {
        self.sorted.iter().map(|(score, member)| (member, score.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(s: &str) -> Vec<u8> {
        s.as_bytes().to_vec()
    }

    #[test]
    fn add_orders_by_score() {
        let mut z = ZSet::new();
        assert_eq!(z.add(b("bob"), 50.0), Ok(true));
        assert_eq!(z.add(b("alice"), 100.0), Ok(true));
        assert_eq!(z.add(b("carol"), 10.0), Ok(true));
        assert_eq!(z.add(b("bob"), 50.0), Ok(false)); // update, not new
        assert_eq!(z.len(), 3);
        assert_eq!(z.range(0, -1), vec![b("carol"), b("bob"), b("alice")]);
    }

    #[test]
    fn update_moves_position() {
        let mut z = ZSet::new();
        z.add(b("a"), 1.0).unwrap();
        z.add(b("b"), 2.0).unwrap();
        z.add(b("a"), 3.0).unwrap(); // a jumps above b
        assert_eq!(z.range(0, -1), vec![b("b"), b("a")]);
        assert_eq!(z.score(&b("a")), Some(3.0));
    }

    #[test]
    fn rank_and_scores() {
        let mut z = ZSet::new();
        z.add(b("low"), 1.0).unwrap();
        z.add(b("mid"), 2.0).unwrap();
        z.add(b("high"), 3.0).unwrap();
        assert_eq!(z.rank(&b("low")), Some(0));
        assert_eq!(z.rank(&b("high")), Some(2));
        assert_eq!(z.rank(&b("ghost")), None);

        let scored = z.range_scored(-2, -1);
        assert_eq!(
            scored[0],
            Scored {
                member: b("mid"),
                score: 2.0
            }
        );
        assert_eq!(
            scored[1],
            Scored {
                member: b("high"),
                score: 3.0
            }
        );
    }

    #[test]
    fn ties_break_by_member() {
        let mut z = ZSet::new();
        z.add(b("banana"), 5.0).unwrap();
        z.add(b("apple"), 5.0).unwrap();
        assert_eq!(z.range(0, -1), vec![b("apple"), b("banana")]);
    }

    #[test]
    fn remove_and_incr() {
        let mut z = ZSet::new();
        z.add(b("p"), 10.0).unwrap();
        assert_eq!(z.incr_by(b("p"), 5.0), Ok(15.0));
        assert_eq!(z.incr_by(b("new"), 3.0), Ok(3.0)); // created at delta
        assert!(z.remove(&b("p")));
        assert!(!z.remove(&b("p")));
        assert_eq!(z.score(&b("p")), None);
    }

    #[test]
    fn rejects_nan_and_non_finite() {
        let mut z = ZSet::new();
        assert_eq!(z.add(b("x"), f64::NAN), Err(()));
        z.add(b("y"), f64::MAX).unwrap();
        assert_eq!(z.incr_by(b("y"), f64::MAX), Err(())); // overflow to +inf
    }

    #[test]
    fn range_out_of_bounds() {
        let mut z = ZSet::new();
        z.add(b("a"), 1.0).unwrap();
        assert_eq!(z.range(5, 10), Vec::<Vec<u8>>::new());
        assert!(z.range(0, -1).len() == 1);
        let empty = ZSet::new();
        assert_eq!(empty.range(0, -1), Vec::<Vec<u8>>::new());
    }
}
