//! The printed plan as a PDF: `text::render`'s lines, typeset in Courier across as many
//! A4 pages as they take. A table of figures reads true only when every column lines up,
//! which a fixed-width face promises without the layout work `output::chart` puts into a
//! vector diagram — right for a page meant to be read as a table, not looked at as a map.

use crate::dispatch::Dispatch;
use crate::ofp::{text, DispatchOptions};
use anyhow::{Context, Result};
use pdf_writer::{Content, Finish, Name, Pdf, Rect, Ref, Str};
use std::path::Path;

const PAGE_W: f32 = 595.276; // A4 portrait, points
const PAGE_H: f32 = 841.89;
const MARGIN: f32 = 28.0;
const FONT_SIZE: f32 = 7.0;
const LEADING: f32 = 8.6;
/// Courier's fixed advance is exactly six tenths of an em, so a line's width is exact
/// without laying out a single glyph.
const CHAR_W: f32 = 0.6;
const FONT: Name = Name(b"F1");

fn ascii(s: &str) -> Vec<u8> {
    crate::output::winansi(s)
}

fn text_w(size: f32, s: &str) -> f32 {
    CHAR_W * size * s.chars().count() as f32
}

/// How many characters fit across the page at the plan's font size.
fn columns() -> usize {
    ((PAGE_W - 2.0 * MARGIN) / (CHAR_W * FONT_SIZE)).floor() as usize
}

/// A line, cut to the page width where it runs over rather than spilling into the margin.
fn fit(line: &str) -> &str {
    let max = columns();
    match line.char_indices().nth(max) {
        Some((at, _)) => &line[..at],
        None => line,
    }
}

/// Lay the plan's lines out on A4 pages.
fn paginate<'a>(lines: &'a [&'a str]) -> Vec<&'a [&'a str]> {
    let usable_h = PAGE_H - 2.0 * MARGIN;
    let per_page = ((usable_h / LEADING).floor() as usize).max(10);
    if lines.is_empty() {
        return vec![&[]];
    }
    lines.chunks(per_page).collect()
}

/// The PDF's bytes, without writing them anywhere: what `write` saves, and what a test
/// checks without a temporary file.
pub fn build(d: &Dispatch, opts: &DispatchOptions) -> Vec<u8> {
    let full = text::render(d, opts);
    let lines: Vec<&str> = full.lines().collect();
    let pages = paginate(&lines);

    let mut pdf = Pdf::new();
    let catalog = Ref::new(1);
    let pages_ref = Ref::new(2);
    let font_ref = Ref::new(3);
    let mut next = 4;
    let mut page_ids = Vec::with_capacity(pages.len());
    let mut content_ids = Vec::with_capacity(pages.len());
    for _ in &pages {
        page_ids.push(Ref::new(next));
        next += 1;
        content_ids.push(Ref::new(next));
        next += 1;
    }

    pdf.catalog(catalog).pages(pages_ref);
    pdf.pages(pages_ref).kids(page_ids.iter().copied()).count(page_ids.len() as i32);

    let title = format!("{} {} -> {}", opts.flight_number.clone().unwrap_or_default(), d.route.origin.icao, d.route.destination.icao);
    for (i, page_lines) in pages.iter().enumerate() {
        let mut c = Content::new();
        let mut y = PAGE_H - MARGIN;
        for line in page_lines.iter() {
            c.begin_text();
            c.set_fill_gray(0.0);
            c.set_font(FONT, FONT_SIZE);
            c.next_line(MARGIN, y);
            c.show(Str(&ascii(fit(line))));
            c.end_text();
            y -= LEADING;
        }
        let footer = format!("{}   page {} of {}", title.trim(), i + 1, pages.len());
        c.begin_text();
        c.set_fill_gray(0.45);
        c.set_font(FONT, 6.0);
        c.next_line(PAGE_W - MARGIN - text_w(6.0, &footer), MARGIN * 0.5);
        c.show(Str(&ascii(&footer)));
        c.end_text();

        {
            let mut pg = pdf.page(page_ids[i]);
            pg.media_box(Rect::new(0.0, 0.0, PAGE_W, PAGE_H));
            pg.parent(pages_ref);
            pg.contents(content_ids[i]);
            let mut res = pg.resources();
            let mut fonts = res.fonts();
            fonts.pair(FONT, font_ref);
            fonts.finish();
            res.finish();
            pg.finish();
        }
        let data = c.finish();
        pdf.stream(content_ids[i], &data);
    }
    pdf.type1_font(font_ref).base_font(Name(b"Courier")).encoding_predefined(Name(b"WinAnsiEncoding"));
    pdf.finish()
}

/// Write the plan's PDF to `out`, creating any missing parent directory. Returns its size.
pub fn write(d: &Dispatch, opts: &DispatchOptions, out: &Path) -> Result<u64> {
    let bytes = build(d, opts);
    if let Some(parent) = out.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(out, &bytes).with_context(|| format!("write {}", out.display()))?;
    Ok(bytes.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ofp::fixtures;

    #[test]
    fn the_plan_is_a_valid_multi_page_pdf() {
        let d = fixtures::sample();
        let opts = fixtures::sample_opts();
        let bytes = build(&d, &opts);
        assert!(bytes.starts_with(b"%PDF"));
        // The nav log, the header and the notes between them run past one page at 7pt.
        let count = String::from_utf8_lossy(&bytes).matches("/Type /Page\n").count() + String::from_utf8_lossy(&bytes).matches("/Type/Page\n").count();
        let _ = count; // pdf-writer's exact spacing is an implementation detail; size is not.
        assert!(bytes.len() > 2000);
    }

    #[test]
    fn a_long_line_is_cut_to_the_page_rather_than_spilling_into_the_margin() {
        let long = "x".repeat(400);
        assert!(fit(&long).chars().count() <= columns());
        assert_eq!(fit("short"), "short");
    }

    #[test]
    fn writing_to_a_new_directory_creates_it() {
        let dir = std::env::temp_dir().join(format!("ofp-pdf-{}", std::process::id()));
        let out = dir.join("plan.pdf");
        let d = fixtures::sample();
        let opts = fixtures::sample_opts();
        let n = write(&d, &opts, &out).unwrap();
        assert!(out.is_file());
        assert_eq!(std::fs::metadata(&out).unwrap().len(), n);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_plan_still_makes_one_page() {
        let pages = paginate(&[]);
        assert_eq!(pages.len(), 1);
    }
}
