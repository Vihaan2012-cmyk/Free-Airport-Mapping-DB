//! Turning a condition cell into tokens: words, and the punctuation a list of them is
//! built from. Everything is folded to upper case here, since the RAD's own keywords,
//! point idents and aerodrome codes are conventionally upper case and a restriction's
//! sense never turns on letter case.

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Tok {
    Word(String),
    LParen,
    RParen,
    Comma,
    /// `&`, joining terms; a no-op the parser skips over.
    Amp,
}

/// Words the grammar gives a meaning to, so a list of plain idents stops before one
/// rather than swallowing it: `VIA (LAMPU DCT NOMBO)` must not be read as three points
/// called LAMPU, DCT and NOMBO.
pub(crate) fn is_reserved(word: &str) -> bool {
    matches!(
        word,
        "ARR" | "DEP" | "ARR/DEP" | "DEP/ARR" | "VIA" | "RFL" | "ABV" | "ABOVE" | "BLW" | "BELOW" | "BTN" | "BETWEEN" | "AT" | "EXC" | "EXCEPT" | "SID" | "STAR" | "AND" | "OR" | "AND-THEN" | "DCT" | "WITH"
    )
}

/// Split a cell into tokens. A run of letters, digits, `.`, `:`, `-` and `/` is one
/// word; `(`, `)`, `,` and `&` are their own tokens; everything else is a separator.
pub(crate) fn tokenize(text: &str) -> Vec<Tok> {
    let upper = text.to_uppercase();
    let mut out = Vec::new();
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut Vec<Tok>| {
        if !word.is_empty() {
            out.push(Tok::Word(std::mem::take(word)));
        }
    };
    for c in upper.chars() {
        match c {
            '(' => {
                flush(&mut word, &mut out);
                out.push(Tok::LParen);
            }
            ')' => {
                flush(&mut word, &mut out);
                out.push(Tok::RParen);
            }
            ',' => {
                flush(&mut word, &mut out);
                out.push(Tok::Comma);
            }
            '&' => {
                flush(&mut word, &mut out);
                out.push(Tok::Amp);
            }
            c if c.is_alphanumeric() || c == '.' || c == ':' || c == '-' || c == '/' || c == '_' || c == '*' => word.push(c),
            _ => flush(&mut word, &mut out),
        }
    }
    flush(&mut word, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn punctuation_and_words_come_apart() {
        let toks = tokenize("ARR (EGLL, EGKK) & ABV FL245");
        assert_eq!(
            toks,
            vec![
                Tok::Word("ARR".into()),
                Tok::LParen,
                Tok::Word("EGLL".into()),
                Tok::Comma,
                Tok::Word("EGKK".into()),
                Tok::RParen,
                Tok::Amp,
                Tok::Word("ABV".into()),
                Tok::Word("FL245".into()),
            ]
        );
    }

    #[test]
    fn a_hyphenated_word_stays_one_token() {
        assert_eq!(tokenize("AND-THEN VIA X"), vec![Tok::Word("AND-THEN".into()), Tok::Word("VIA".into()), Tok::Word("X".into())]);
    }
}
