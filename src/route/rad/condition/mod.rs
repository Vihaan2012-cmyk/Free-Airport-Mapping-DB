//! The condition language the RAD writes its restrictions in: semi-structured English
//! such as "Not available for traffic ARR EGLL, EGKK" or "Only available for traffic
//! DEP LF** above FL245". A tokenizer (`lexer`), a parser into an expression tree
//! (`ast`, `parser`) and an evaluator (`eval`) — kept deliberately narrower than the
//! full range of English the RAD's authors write in, so that a phrasing outside it fails
//! to parse instead of being silently misread. See `parser` for exactly what is and is
//! not understood.

mod ast;
mod eval;
mod lexer;
mod parser;

pub(crate) use ast::{Clause, Keyword, LevelCmp, LevelTerm, Restriction, Term};
pub(crate) use eval::Ctx;
pub(crate) use parser::{parse_level_phrase, parse_restriction, parse_via_phrase};

/// A cell that may hold more than one restriction, one below another, separated by a
/// line of dashes, with the time applicability column carrying one entry per block in
/// the same order. Where the two do not line up one for one, every block gets the whole
/// applicability text rather than a guessed pairing.
pub(crate) fn split_blocks(utilization: &str, time_applicability: &str) -> Vec<(String, String)> {
    let u_blocks: Vec<&str> = utilization.split("----------").map(str::trim).filter(|s| !s.is_empty()).collect();
    let t_blocks: Vec<&str> = time_applicability.split("----------").map(str::trim).collect();
    u_blocks.iter().enumerate().map(|(i, u)| ((*u).to_string(), if t_blocks.len() == u_blocks.len() { t_blocks[i].to_string() } else { time_applicability.to_string() })).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_blocks_are_paired_and_mismatched_ones_share_the_whole_column() {
        let paired = split_blocks("NOT AVBL\n----------\nONLY AVBL", "H24\n----------\nMON-FRI 06:00-18:00");
        assert_eq!(paired, vec![("NOT AVBL".to_string(), "H24".to_string()), ("ONLY AVBL".to_string(), "MON-FRI 06:00-18:00".to_string())]);
        let mismatched = split_blocks("NOT AVBL\n----------\nONLY AVBL\n----------\nCOMPULSORY", "H24");
        assert!(mismatched.iter().all(|(_, t)| t == "H24"));
    }
}
