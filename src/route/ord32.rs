//! A total order over `f32`, for the one place a plain float needs to sit in a
//! `BinaryHeap`: every distance and cost in the search is a real, finite number, so the
//! `NaN` a float's own `Ord` refuses to promise never comes up.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrderedF32(pub f32);

impl Eq for OrderedF32 {}

impl PartialOrd for OrderedF32 {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderedF32 {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}
