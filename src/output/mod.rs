//! Writers: GeoJSON and Geobuf (PBF) per layer, plus manifest/index files.

pub mod geobuf;
pub mod geojson;
pub mod manifest;
pub mod approach;
pub mod canvas;
pub mod charts_bulk;
pub mod chart;
pub mod preview;
pub mod routemap;
pub mod xplane;

use crate::geom::LocalFrame;
use crate::model::{AmdbFeature, Layer, ALL_LAYERS};
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::Path;

/// The letter underneath an accented one, for the Central European letters WinAnsi has
/// no glyph for. Indexed from U+0100.
const LATIN_A_BASE: [&str; 128] = [
    "A", "a", "A", "a", "A", "a", "C", "c",
    "C", "c", "C", "c", "C", "c", "D", "d",
    "D", "d", "E", "e", "E", "e", "E", "e",
    "E", "e", "E", "e", "G", "g", "G", "g",
    "G", "g", "G", "g", "H", "h", "H", "h",
    "I", "i", "I", "i", "I", "i", "I", "i",
    "I", "i", "IJ", "ij", "J", "j", "K", "k",
    "k", "L", "l", "L", "l", "L", "l", "L",
    "l", "L", "l", "N", "n", "N", "n", "N",
    "n", "n", "N", "n", "O", "o", "O", "o",
    "O", "o", "OE", "oe", "R", "r", "R", "r",
    "R", "r", "S", "s", "S", "s", "S", "s",
    "S", "s", "T", "t", "T", "t", "T", "t",
    "U", "u", "U", "u", "U", "u", "U", "u",
    "U", "u", "U", "u", "W", "w", "Y", "y",
    "Y", "Z", "z", "Z", "z", "Z", "z", "s",
];

/// What the built-in fonts can print of one character, as WinAnsi bytes.
///
/// WinAnsi is CP1252, so it already has the accented letters most of the world spells
/// its airports with: Funchal's NÉLIO MENDONÇA prints as itself. What it has no glyph
/// for — Łódź, Timişoara, the Central European letters — is written as the plain letter
/// underneath, which is how a chart abroad reads rather than a row of question marks.
pub fn winansi_char(c: char) -> &'static [u8] {
    /// One byte, held still so it can be returned by reference.
    fn one(b: u8) -> &'static [u8] {
        const T: [[u8; 1]; 256] = {
            let mut t = [[0u8; 1]; 256];
            let mut i = 0;
            while i < 256 {
                t[i] = [i as u8];
                i += 1;
            }
            t
        };
        &T[b as usize]
    }
    match c as u32 {
        0x20..=0x7e => one(c as u8),
        // Latin-1 is WinAnsi's upper half unchanged: the degree sign, the fractions, and
        // every accented letter from À to ÿ.
        0xa0..=0xff => one(c as u32 as u8),
        // The handful WinAnsi keeps in the range Latin-1 leaves empty.
        0x20ac => one(0x80),
        0x201a => one(0x82),
        0x192 => one(0x83),
        0x201e => one(0x84),
        0x2026 => one(0x85),
        0x2020 => one(0x86),
        0x2021 => one(0x87),
        0x2c6 => one(0x88),
        0x2030 => one(0x89),
        0x160 => one(0x8a),
        0x2039 => one(0x8b),
        0x152 => one(0x8c),
        0x17d => one(0x8e),
        0x2018 => one(0x91),
        0x2019 => one(0x92),
        0x201c => one(0x93),
        0x201d => one(0x94),
        0x2022 => one(0x95),
        0x2013 => one(0x96),
        0x2014 => one(0x97),
        0x2dc => one(0x98),
        0x2122 => one(0x99),
        0x161 => one(0x9a),
        0x203a => one(0x9b),
        0x153 => one(0x9c),
        0x17e => one(0x9e),
        0x178 => one(0x9f),
        0x100..=0x17f => LATIN_A_BASE[c as usize - 0x100].as_bytes(),
        _ => b"?",
    }
}

/// A whole string as WinAnsi bytes.
pub fn winansi(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for c in s.chars() {
        out.extend_from_slice(winansi_char(c));
    }
    out
}

/// The ASCII letter a character is measured as. Accented letters are as wide as the
/// letter underneath, which is close enough to set a line by.
pub fn width_char(c: char) -> char {
    match c as u32 {
        0x20..=0x7e => c,
        0x100..=0x17f => LATIN_A_BASE[c as usize - 0x100].chars().next().unwrap_or('n'),
        _ => match winansi_char(c).first() {
            // Latin-1's accented letters, back to their base for measuring.
            Some(&b) if (0xc0..=0xc5).contains(&b) => 'A',
            Some(&b) if b == 0xc7 => 'C',
            Some(&b) if (0xc8..=0xcb).contains(&b) => 'E',
            Some(&b) if (0xcc..=0xcf).contains(&b) => 'I',
            Some(&b) if (0xd2..=0xd6).contains(&b) || b == 0xd8 => 'O',
            Some(&b) if (0xd9..=0xdc).contains(&b) => 'U',
            Some(&b) if (0xe0..=0xe5).contains(&b) => 'a',
            Some(&b) if b == 0xe7 => 'c',
            Some(&b) if (0xe8..=0xeb).contains(&b) => 'e',
            Some(&b) if (0xec..=0xef).contains(&b) => 'i',
            Some(&b) if (0xf2..=0xf6).contains(&b) || b == 0xf8 => 'o',
            Some(&b) if (0xf9..=0xfc).contains(&b) => 'u',
            Some(&b) if b == 0xd1 => 'N',
            Some(&b) if b == 0xf1 => 'n',
            _ => 'n',
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Projection {
    /// EPSG:4326 lon/lat degrees.
    Wgs84,
    /// Azimuthal equidistant metres from the ARP (x east, y north).
    LocalMetres,
}

impl Projection {
    pub fn name(self) -> &'static str {
        match self {
            Projection::Wgs84 => "EPSG:4326",
            Projection::LocalMetres => "AEQD_ARP_METRES",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Formats {
    pub geojson: bool,
    pub pbf: bool,
}

/// Write every layer for one airport. Features are in the local frame; `frame`
/// converts them to WGS84 unless the projection keeps metres.
pub fn write_airport(
    dir: &Path,
    icao: &str,
    frame: &LocalFrame,
    features: &BTreeMap<Layer, Vec<AmdbFeature>>,
    projection: Projection,
    formats: Formats,
    layers: &[Layer],
) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("mkdir {}", dir.display()))?;
    let _ = ALL_LAYERS;
    for layer in layers {
        let feats = features.get(layer).map(Vec::as_slice).unwrap_or(&[]);
        let projected: Vec<AmdbFeature> = feats
            .iter()
            .map(|f| AmdbFeature {
                layer: f.layer,
                geom: match projection {
                    Projection::Wgs84 => frame.to_wgs84(&f.geom),
                    Projection::LocalMetres => f.geom.clone(),
                },
                props: f.props.clone(),
            })
            .collect();
        if formats.geojson {
            let text = geojson::feature_collection_string(icao, *layer, &projected, projection, frame);
            std::fs::write(dir.join(format!("{}.geojson", layer.name())), text)?;
        }
        if formats.pbf {
            let bytes = geobuf::encode(&projected, projection);
            std::fs::write(dir.join(format!("{}.pbf", layer.name())), bytes)?;
        }
    }
    Ok(())
}
