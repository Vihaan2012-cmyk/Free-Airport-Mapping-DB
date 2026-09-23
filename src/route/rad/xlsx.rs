//! A minimal `.xlsx` reader: enough to read the RAD workbook and nothing more. An
//! `.xlsx` is a zip of XML parts; only three kinds of part matter here — the workbook's
//! sheet list, the shared string table every text cell indexes into, and each
//! worksheet's rows — so this reads only those, with no formulas, no styles and no
//! formatting.

use anyhow::{Context, Result};
use quick_xml::events::{BytesStart, Event};
use std::collections::HashMap;
use std::io::Read;

/// One worksheet: its name, and its rows in order, each cell by its column (so a blank
/// cell a sheet left out still leaves a gap rather than shifting everything after it
/// left).
#[derive(Debug, Clone, Default)]
pub(crate) struct Sheet {
    pub name: String,
    pub rows: Vec<Vec<String>>,
}

impl Sheet {
    pub(crate) fn cell(&self, row: usize, col: usize) -> &str {
        self.rows.get(row).and_then(|r| r.get(col)).map(String::as_str).unwrap_or("")
    }

    /// The header row turned into a lookup by name, so a column is found by what it is
    /// called rather than by a position that moves when Eurocontrol adds one.
    pub(crate) fn header(&self) -> Header {
        let mut idx = HashMap::new();
        if let Some(row) = self.rows.first() {
            for (i, cell) in row.iter().enumerate() {
                idx.entry(normalise_header(cell)).or_insert(i);
            }
        }
        Header { idx }
    }
}

/// A sheet's header row, so the rest of the row is read by column name.
pub(crate) struct Header {
    idx: HashMap<String, usize>,
}

impl Header {
    pub(crate) fn col(&self, name: &str) -> Option<usize> {
        self.idx.get(&normalise_header(name)).copied()
    }
}

/// A header as the RAD writes it, `"Change\nInd."`, folded to how it is asked for,
/// `"change ind."`: the line breaks Excel wraps a heading with are not part of its name.
fn normalise_header(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Every worksheet of a workbook, in the order it lists them.
pub(crate) struct Workbook {
    pub sheets: Vec<Sheet>,
}

impl Workbook {
    pub(crate) fn sheet(&self, name: &str) -> Option<&Sheet> {
        self.sheets.iter().find(|s| s.name.eq_ignore_ascii_case(name))
    }

    pub(crate) fn read(bytes: &[u8]) -> Result<Workbook> {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).context("open the workbook as a zip (an .xlsx is one)")?;
        let workbook_xml = read_entry(&mut zip, "xl/workbook.xml").context("xl/workbook.xml: this is not an .xlsx, or not one in the shape Excel writes")?;
        let rels_xml = read_entry(&mut zip, "xl/_rels/workbook.xml.rels").context("xl/_rels/workbook.xml.rels")?;
        let shared = read_entry(&mut zip, "xl/sharedStrings.xml").map(|x| shared_strings(&x)).unwrap_or_default();
        let targets = parse_rels(&rels_xml);
        let mut sheets = Vec::new();
        for (name, rid) in parse_sheet_list(&workbook_xml) {
            let Some(target) = targets.get(&rid) else { continue };
            let Ok(xml) = read_entry(&mut zip, &format!("xl/{target}")) else { continue };
            sheets.push(Sheet { name, rows: parse_sheet(&xml, &shared) });
        }
        if sheets.is_empty() {
            anyhow::bail!("the workbook has no worksheet this could read");
        }
        Ok(Workbook { sheets })
    }
}

fn read_entry<R: Read + std::io::Seek>(zip: &mut zip::ZipArchive<R>, name: &str) -> Result<String> {
    let mut member = zip.by_name(name).with_context(|| format!("{name}: not in the zip"))?;
    let mut text = String::new();
    member.read_to_string(&mut text).with_context(|| format!("{name}: not UTF-8 text"))?;
    Ok(text)
}

fn attr(e: &BytesStart, name: &str) -> Option<String> {
    e.attributes().flatten().find(|a| a.key.as_ref() == name).map(|a| a.normalized_value(quick_xml::XmlVersion::Implicit1_0).map(|v| v.into_owned()).unwrap_or_else(|_| a.value.to_string()))
}

/// The sheets a workbook lists, in order: each one's name and the relationship id that
/// says which part of the zip holds it.
fn parse_sheet_list(xml: &str) -> Vec<(String, String)> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut out = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) if e.name().as_ref() == "sheet" => {
                if let (Some(name), Some(rid)) = (attr(e, "name"), attr(e, "r:id")) {
                    out.push((name, rid));
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

/// A relationship id to the zip path it points at: `rId2` to `worksheets/sheet2.xml`.
fn parse_rels(xml: &str) -> HashMap<String, String> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut out = HashMap::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) if e.name().as_ref() == "Relationship" => {
                if let (Some(id), Some(target)) = (attr(e, "Id"), attr(e, "Target")) {
                    out.insert(id, target);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

/// The shared string table every `t="s"` cell indexes into, in order.
fn shared_strings(xml: &str) -> Vec<String> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_text = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => match e.name().as_ref() {
                "si" => current.clear(),
                "t" => in_text = true,
                _ => {}
            },
            Ok(Event::Text(t)) if in_text => current.push_str(t.xml_content(quick_xml::XmlVersion::Implicit1_0).as_ref()),
            Ok(Event::End(ref e)) => match e.name().as_ref() {
                "t" => in_text = false,
                "si" => out.push(std::mem::take(&mut current)),
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

/// A cell reference, `"AB12"`, turned into a zero-based column: the letters before the
/// row number, base 26.
fn col_index(cell_ref: &str) -> usize {
    let mut idx: usize = 0;
    for c in cell_ref.chars().take_while(|c| c.is_ascii_alphabetic()) {
        idx = idx * 26 + (c.to_ascii_uppercase() as usize - 'A' as usize + 1);
    }
    idx.saturating_sub(1)
}

fn place(row: &mut Vec<Option<String>>, col: usize, value: String) {
    if row.len() <= col {
        row.resize(col + 1, None);
    }
    row[col] = Some(value);
}

/// One worksheet's rows: each `<row>` of `<c>` cells, a cell's value coming from `<v>`
/// (a shared string index, or the value itself) or, for an inline string, `<is><t>`.
fn parse_sheet(xml: &str, shared: &[String]) -> Vec<Vec<String>> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut rows = Vec::new();
    let mut current: Vec<Option<String>> = Vec::new();
    let mut cell_col = 0usize;
    let mut cell_type = String::new();
    let mut cell_text = String::new();
    let mut in_value = false;
    loop {
        let ev = reader.read_event();
        match ev {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let empty = matches!(ev, Ok(Event::Empty(_)));
                match e.name().as_ref() {
                    "row" => current.clear(),
                    "c" => {
                        cell_col = attr(e, "r").map(|r| col_index(&r)).unwrap_or(0);
                        cell_type = attr(e, "t").unwrap_or_default();
                        cell_text.clear();
                        if empty {
                            place(&mut current, cell_col, String::new());
                        }
                    }
                    "v" | "t" => in_value = true,
                    _ => {}
                }
            }
            Ok(Event::Text(t)) if in_value => cell_text.push_str(t.xml_content(quick_xml::XmlVersion::Implicit1_0).as_ref()),
            Ok(Event::End(ref e)) => match e.name().as_ref() {
                "v" | "t" => in_value = false,
                "c" => {
                    let value = if cell_type == "s" { cell_text.trim().parse::<usize>().ok().and_then(|i| shared.get(i)).cloned().unwrap_or_default() } else { std::mem::take(&mut cell_text) };
                    place(&mut current, cell_col, value);
                }
                "row" => rows.push(std::mem::take(&mut current).into_iter().map(Option::unwrap_or_default).collect()),
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    rows
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;

    /// A workbook built from scratch: one sheet per `(name, rows)`, cells written as
    /// inline strings so the test needs no shared string table. Good enough to read
    /// back what this module writes, which is all a round-trip test needs.
    pub(crate) fn build(sheets: &[(&str, &[&[&str]])]) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            let sheet_tags: Vec<String> = sheets.iter().enumerate().map(|(i, (name, _))| format!(r#"<sheet name="{}" sheetId="{}" r:id="rId{}"/>"#, name, i + 1, i + 1)).collect();
            let workbook = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets>{}</sheets></workbook>"#,
                sheet_tags.join("")
            );
            zip.start_file("xl/workbook.xml", opts).unwrap();
            zip.write_all(workbook.as_bytes()).unwrap();

            let rel_tags: Vec<String> = sheets.iter().enumerate().map(|(i, _)| format!(r#"<Relationship Id="rId{}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet{}.xml"/>"#, i + 1, i + 1)).collect();
            let rels = format!(r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{}</Relationships>"#, rel_tags.join(""));
            zip.start_file("xl/_rels/workbook.xml.rels", opts).unwrap();
            zip.write_all(rels.as_bytes()).unwrap();

            for (i, (_, rows)) in sheets.iter().enumerate() {
                let mut xml = String::from(r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>"#);
                for (r, row) in rows.iter().enumerate() {
                    xml.push_str(&format!(r#"<row r="{}">"#, r + 1));
                    for (c, cell) in row.iter().enumerate() {
                        let col = column_letters(c);
                        let escaped = cell.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
                        xml.push_str(&format!(r#"<c r="{}{}" t="inlineStr"><is><t>{}</t></is></c>"#, col, r + 1, escaped));
                    }
                    xml.push_str("</row>");
                }
                xml.push_str("</sheetData></worksheet>");
                zip.start_file(format!("xl/worksheets/sheet{}.xml", i + 1), opts).unwrap();
                zip.write_all(xml.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        buf.into_inner()
    }

    fn column_letters(mut i: usize) -> String {
        let mut s = Vec::new();
        loop {
            s.push(b'A' + (i % 26) as u8);
            if i < 26 {
                break;
            }
            i = i / 26 - 1;
        }
        s.reverse();
        String::from_utf8(s).unwrap()
    }

    #[test]
    fn a_workbook_reads_back_its_sheets_names_and_gaps() {
        let bytes = build(&[("Annex 1", &[&["ID", "Definition"], &["A", "(EGLL, EGKK)"]]), ("Annex 2B", &[&["ID", "", "Utilization"], &["X1", "", "NOT AVBL"]])]);
        let wb = Workbook::read(&bytes).unwrap();
        assert_eq!(wb.sheets.len(), 2);
        let a1 = wb.sheet("Annex 1").unwrap();
        assert_eq!(a1.cell(1, 0), "A");
        assert_eq!(a1.cell(1, 1), "(EGLL, EGKK)");
        let a2b = wb.sheet("annex 2b").unwrap();
        let h = a2b.header();
        assert_eq!(h.col("ID"), Some(0));
        assert_eq!(h.col("Utilization"), Some(2));
        assert_eq!(a2b.cell(1, h.col("Utilization").unwrap()), "NOT AVBL");
        // The blank column between ID and Utilization really is blank, not a shift.
        assert_eq!(a2b.cell(1, 1), "");
    }

    #[test]
    fn a_header_with_a_line_break_is_still_found_by_its_words() {
        let bytes = build(&[("Sheet1", &[&["Change\nInd.", "Valid\nFrom"], &["AMD", "1 JAN 2026"]])]);
        let wb = Workbook::read(&bytes).unwrap();
        let h = wb.sheets[0].header();
        assert_eq!(h.col("Change Ind."), Some(0));
        assert_eq!(h.col("Valid From"), Some(1));
    }

    #[test]
    fn not_a_zip_is_a_clear_error() {
        assert!(Workbook::read(b"not a zip at all").is_err());
    }
}
