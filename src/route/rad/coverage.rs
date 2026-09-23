//! How much of the RAD actually got checked. Every row goes one way or the other —
//! parsed into a rule this crate evaluates, or skipped, with why and a sample of the
//! text — so the flight plan can print an honest fraction rather than imply the whole
//! document was understood.

/// How many distinct examples of each skip reason to keep, for the flight plan to
/// print. More than a few is not more informative, only longer.
const SAMPLES_PER_REASON: usize = 3;

#[derive(Debug, Clone, Default)]
pub(crate) struct Coverage {
    parsed: usize,
    skipped: usize,
    /// Reason to a running count and a few examples of the text that hit it.
    reasons: Vec<(String, usize, Vec<String>)>,
}

impl Coverage {
    pub(crate) fn parsed(&mut self) {
        self.parsed += 1;
    }

    pub(crate) fn skip(&mut self, reason: impl Into<String>, text: &str) {
        self.skipped += 1;
        let reason = reason.into();
        let sample = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let sample: String = sample.chars().take(160).collect();
        match self.reasons.iter_mut().find(|(r, _, _)| *r == reason) {
            Some((_, n, samples)) => {
                *n += 1;
                if samples.len() < SAMPLES_PER_REASON {
                    samples.push(sample);
                }
            }
            None => self.reasons.push((reason, 1, vec![sample])),
        }
    }

    /// How many rows parsed into a rule this crate can check, and how many were
    /// skipped — the two together are every row read.
    pub(crate) fn counts(&self) -> (usize, usize) {
        (self.parsed, self.skipped)
    }

    /// Why rows were skipped, most common first, with a few example texts each.
    pub(crate) fn skipped_reasons(&self) -> Vec<(String, usize, Vec<String>)> {
        let mut out = self.reasons.clone();
        out.sort_by(|a, b| b.1.cmp(&a.1));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_reasons_accumulate() {
        let mut c = Coverage::default();
        c.parsed();
        c.parsed();
        c.skip("a route, not a point", "VIA (A DCT B)");
        c.skip("a route, not a point", "VIA (C DCT D)");
        c.skip("AND-THEN", "X AND-THEN Y");
        assert_eq!(c.counts(), (2, 3));
        let reasons = c.skipped_reasons();
        assert_eq!(reasons[0].0, "a route, not a point");
        assert_eq!(reasons[0].1, 2);
        assert_eq!(reasons[0].2.len(), 2);
    }

    #[test]
    fn samples_are_capped_per_reason() {
        let mut c = Coverage::default();
        for i in 0..10 {
            c.skip("same reason", &format!("text {i}"));
        }
        assert_eq!(c.skipped_reasons()[0].2.len(), SAMPLES_PER_REASON);
    }
}
