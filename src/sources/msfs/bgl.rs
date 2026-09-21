//! The little of the BGL container format needed to reach airport records.
//!
//! A BGL is a header, a table of sections, and for each section a table of subsections
//! that point at streams of records. Every record is an id, a length, and a payload,
//! and records nest, so one walk serves for all of them.

/// Section holding airport records.
pub const SECTION_AIRPORT: u32 = 0x03;
/// Section holding waypoints, which is where the fixes a procedure names live.
pub const SECTION_WAYPOINT: u32 = 0x22;
pub const REC_WAYPOINT: u16 = 0x22;
/// Record ids. MSFS numbers the airport record differently from the older simulators.
pub const REC_AIRPORT: u16 = 0x56;
pub const REC_NAME: u16 = 0x19;

const MAGIC: u32 = 0x1992_0201;

fn u16le(d: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([d[at], d[at + 1]])
}

pub fn u32le(d: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]])
}

pub fn f32le(d: &[u8], at: usize) -> f32 {
    f32::from_le_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]])
}

/// One record: its id, where its payload starts and where it ends.
#[derive(Debug, Clone, Copy)]
pub struct Record {
    pub id: u16,
    pub start: usize,
    pub end: usize,
}

/// Records laid end to end between `from` and `to`. A length that runs past the end
/// means the stream is not what we think it is, so it stops rather than guessing.
pub fn records(d: &[u8], from: usize, to: usize) -> Vec<Record> {
    let mut out = Vec::new();
    let mut at = from;
    while at + 6 <= to.min(d.len()) {
        let id = u16le(d, at);
        let size = u32le(d, at + 2) as usize;
        if size < 6 || at + size > to {
            break;
        }
        out.push(Record { id, start: at, end: at + size });
        at += size;
    }
    out
}

/// Every record of one section, following its subsection table.
pub fn section_records(d: &[u8], section: u32) -> Vec<Record> {
    let mut out = Vec::new();
    if d.len() < 0x38 || u32le(d, 0) != MAGIC {
        return out;
    }
    let n_sections = u32le(d, 0x14) as usize;
    for i in 0..n_sections {
        let base = 0x38 + i * 20;
        if base + 20 > d.len() {
            break;
        }
        if u32le(d, base) != section {
            continue;
        }
        let n_sub = u32le(d, base + 8) as usize;
        let sub_off = u32le(d, base + 12) as usize;
        for j in 0..n_sub {
            let b = sub_off + j * 16;
            if b + 16 > d.len() {
                break;
            }
            let off = u32le(d, b + 8) as usize;
            let size = u32le(d, b + 12) as usize;
            if off + size <= d.len() {
                out.extend(records(d, off, off + size));
            }
        }
    }
    out
}

/// The 32-bit packed form used for idents (airports, waypoints, runways).
pub fn ident(mut v: u32) -> String {
    const CH: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    if v == 0 {
        return String::new();
    }
    v >>= 5;
    let mut out = Vec::new();
    while v > 1 {
        let r = (v % 38) as usize;
        v /= 38;
        out.push(if r < 2 { b' ' } else { CH[r - 2] });
    }
    out.reverse();
    String::from_utf8_lossy(&out).trim().to_string()
}

/// Longitude and latitude as BGL stores them.
pub fn lon(v: u32) -> f64 {
    v as f64 * (360.0 / (3.0 * 0x1000_0000 as f64)) - 180.0
}

pub fn lat(v: u32) -> f64 {
    90.0 - v as f64 * (180.0 / (2.0 * 0x1000_0000 as f64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idents_decode() {
        // Values taken from the simulator's own files.
        assert_eq!(ident(0x321e_1245), "AKUNA");
        assert_eq!(ident(0), "");
    }

    #[test]
    fn a_truncated_stream_stops_instead_of_guessing() {
        let mut d = vec![0u8; 20];
        d[0] = 0x56; // id
        d[2] = 0xFF; // a length far past the end
        d[3] = 0xFF;
        assert!(records(&d, 0, d.len()).is_empty());
    }
}
