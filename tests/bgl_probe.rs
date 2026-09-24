//! What record types do FS2024's navigation BGLs actually hold, and do the airways carry
//! altitude limits? Answered by dumping ids and counts rather than guessed at.
use amdbgen::sources::msfs::{bgl, for_each_bgl};
use std::collections::BTreeMap;

#[test]
#[ignore]
fn what_the_nav_bgls_hold() {
    for (section, label) in [(3u32, "nav"), (8, "airport"), (12, "misc")] {
        let mut ids: BTreeMap<u16, (usize, usize)> = BTreeMap::new();
        let mut files = 0usize;
        for_each_bgl(&["nax", "nvx"], |_name, data| {
            files += 1;
            if files > 60 {
                return;
            }
            for r in bgl::section_records(&data, section) {
                let e = ids.entry(r.id).or_insert((0, 0));
                e.0 += 1;
                e.1 = e.1.max(r.end - r.start);
            }
        });
        if !ids.is_empty() {
            println!("section {section} ({label}): {:?}", ids);
        }
    }
}

#[test]
#[ignore]
fn every_section_in_a_nav_bgl() {
    let mut seen = false;
    for_each_bgl(&["nax"], |name, data| {
        if seen {
            return;
        }
        seen = true;
        println!("file {name}, {} bytes", data.len());
        let n = bgl::u32le(&data, 0x14) as usize;
        for i in 0..n {
            let base = 0x38 + i * 20;
            let section = bgl::u32le(&data, base);
            let n_sub = bgl::u32le(&data, base + 8);
            println!("  section 0x{section:02x} subsections {n_sub}");
            let recs = bgl::section_records(&data, section);
            let mut ids: BTreeMap<u16, usize> = BTreeMap::new();
            for r in &recs {
                *ids.entry(r.id).or_default() += 1;
            }
            println!("    records: {ids:?}");
        }
    });
}

#[test]
#[ignore]
fn waypoint_record_bytes() {
    let mut shown = 0usize;
    for_each_bgl(&["nax"], |_name, data| {
        if shown >= 6 {
            return;
        }
        for r in bgl::section_records(&data, 0x22) {
            if shown >= 6 {
                return;
            }
            let d = &data[r.start..r.end];
            if d.len() < 24 {
                continue;
            }
            // Only the ones long enough to be carrying route entries.
            if d.len() < 40 {
                continue;
            }
            println!("len={} ident={} region={}", d.len(), bgl::ident(bgl::u32le(d, 0x0c)), bgl::ident(bgl::u32le(d, 0x10)));
            for (i, chunk) in d.chunks(16).enumerate() {
                println!("  {:04x}: {:02x?}", i * 16, chunk);
            }
            shown += 1;
        }
    });
}

#[test]
#[ignore]
fn waypoint_record_sizes() {
    let mut sizes: BTreeMap<usize, usize> = BTreeMap::new();
    let mut files = 0usize;
    let mut first: Option<Vec<u8>> = None;
    for_each_bgl(&["nax"], |_name, data| {
        files += 1;
        if files > 40 {
            return;
        }
        for r in bgl::section_records(&data, 0x22) {
            *sizes.entry(r.end - r.start).or_default() += 1;
            let d = &data[r.start..r.end];
            if first.is_none() && d.len() > 30 {
                first = Some(d.to_vec());
            }
        }
    });
    println!("sizes seen: {sizes:?}");
    if let Some(d) = first {
        println!("longest-ish record, {} bytes:", d.len());
        for (i, chunk) in d.chunks(16).enumerate() {
            println!("  {:04x}: {:02x?}", i * 16, chunk);
        }
    }
}

#[test]
#[ignore]
fn every_prefix_and_section() {
    let mut prefixes: BTreeMap<String, usize> = BTreeMap::new();
    let mut sections: BTreeMap<u32, BTreeMap<u16, (usize, usize)>> = BTreeMap::new();
    let mut files = 0usize;
    for_each_bgl(&[""], |name, data| {
        files += 1;
        let sep = |c: char| c == '/' || c == std::path::MAIN_SEPARATOR;
        let base = name.rsplit(sep).next().unwrap_or(&name).to_ascii_lowercase();
        let p: String = base.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
        *prefixes.entry(p).or_default() += 1;
        if files > 120 {
            return;
        }
        if data.len() < 0x38 {
            return;
        }
        let n = bgl::u32le(&data, 0x14) as usize;
        for i in 0..n.min(40) {
            let b = 0x38 + i * 20;
            if b + 20 > data.len() {
                break;
            }
            let section = bgl::u32le(&data, b);
            let entry = sections.entry(section).or_default();
            for r in bgl::section_records(&data, section) {
                let e = entry.entry(r.id).or_insert((0, 0));
                e.0 += 1;
                e.1 = e.1.max(r.end - r.start);
            }
        }
    });
    println!("file prefixes: {prefixes:?}");
    for (s, ids) in &sections {
        println!("section 0x{s:02x}: {ids:?}");
    }
}

#[test]
#[ignore]
fn a_waypoint_that_carries_routes() {
    let mut best: Option<Vec<u8>> = None;
    let mut sizes: BTreeMap<usize, usize> = BTreeMap::new();
    for_each_bgl(&[""], |_n, data| {
        for r in bgl::section_records(&data, 0x22) {
            let d = &data[r.start..r.end];
            *sizes.entry(d.len()).or_default() += 1;
            if best.as_ref().map(|b: &Vec<u8>| d.len() > b.len()).unwrap_or(true) {
                best = Some(d.to_vec());
            }
        }
    });
    println!("waypoint record sizes across every file: {sizes:?}");
    let d = best.expect("a waypoint with routes");
    println!("len={} ident={} region={}", d.len(), bgl::ident(bgl::u32le(&d, 0x0c)), bgl::ident(bgl::u32le(&d, 0x10)));
    println!("byte6={} byte7={} (type, nRoutes?)", d[6], d[7]);
    for (i, chunk) in d.chunks(16).enumerate() {
        println!("  {:04x}: {:02x?}  {}", i * 16, chunk, chunk.iter().map(|&b| if b.is_ascii_graphic() { b as char } else { '.' }).collect::<String>());
    }
}

#[test]
#[ignore]
fn route_entries_aligned() {
    let mut best: Option<Vec<u8>> = None;
    for_each_bgl(&[""], |_n, data| {
        for r in bgl::section_records(&data, 0x22) {
            let d = &data[r.start..r.end];
            if d.len() > 120 && d.len() < 200 && best.as_ref().map(|b: &Vec<u8>| d.len() > b.len()).unwrap_or(true) {
                best = Some(d.to_vec());
            }
        }
    });
    let d = best.expect("one");
    println!("id=0x{:04x} size={} type={} n={} magvar={}", u16::from_le_bytes([d[0], d[1]]), d.len(), d[6], d[7], bgl::f32le(&d, 0x10));
    println!("ident@0x14={} region@0x18={}", bgl::ident(bgl::u32le(&d, 0x14)), bgl::ident(bgl::u32le(&d, 0x18)));
    println!("lon={:.5} lat={:.5}", bgl::lon(bgl::u32le(&d, 0x08)), bgl::lat(bgl::u32le(&d, 0x0c)));
    let mut at = 0x1c;
    let mut k = 0;
    while at + 33 <= d.len() && k < 6 {
        let e = &d[at..at + 33];
        let name: String = e[1..9].iter().take_while(|&&b| b != 0).map(|&b| b as char).collect();
        let m = bgl::f32le(e, 29);
        println!("  entry {k} @0x{at:02x} type={} name={name:<6} tail_f32={m:.2} m = {:.0} ft", e[0], m * 3.28084);
        println!("      {:02x?}", e);
        at += 33;
        k += 1;
    }
}

#[test]
#[ignore]
fn airport_record_children_and_position() {
    let mut shown = 0usize;
    let mut ids: BTreeMap<u16, (usize, usize)> = BTreeMap::new();
    for_each_bgl(&[""], |_n, data| {
        for r in bgl::section_records(&data, 0x03) {
            if r.id != 0x56 || r.end - r.start < 0x44 {
                continue;
            }
            for c in bgl::records(&data, r.start + 0x44, r.end) {
                let e = ids.entry(c.id).or_insert((0, 0));
                e.0 += 1;
                e.1 = e.1.max(c.end - c.start);
            }
            if shown < 3 {
                let d = &data[r.start..r.end];
                println!(
                    "icao={} @0x0c lon={:.5} @0x10 lat={:.5} @0x14 f32={:.1} u32={} len={}",
                    bgl::ident(bgl::u32le(d, 0x28)),
                    bgl::lon(bgl::u32le(d, 0x0c)),
                    bgl::lat(bgl::u32le(d, 0x10)),
                    bgl::f32le(d, 0x14),
                    bgl::u32le(d, 0x14),
                    d.len()
                );
                shown += 1;
            }
        }
    });
    println!("airport child records: {ids:?}");
}

#[test]
#[ignore]
fn airport_child_18_bytes() {
    let mut shown = 0usize;
    for_each_bgl(&[""], |_n, data| {
        if shown >= 4 {
            return;
        }
        for r in bgl::section_records(&data, 0x03) {
            if r.id != 0x56 || r.end - r.start < 0x44 || shown >= 4 {
                continue;
            }
            let icao = bgl::ident(bgl::u32le(&data, r.start + 0x28));
            for c in bgl::records(&data, r.start + 0x44, r.end) {
                if c.id != 0x12 || shown >= 4 {
                    continue;
                }
                let d = &data[c.start..c.end];
                println!("{icao} child 0x12 len={}: {:02x?}", d.len(), d);
                println!("   lon@0x08={:.5} lat@0x0c={:.5} alt@0x10={} f32@0x14={:.2} f32@0x18={:.2} f32@0x1c={:.2}",
                    bgl::lon(bgl::u32le(d, 0x08)), bgl::lat(bgl::u32le(d, 0x0c)), bgl::u32le(d, 0x10),
                    bgl::f32le(d, 0x14), bgl::f32le(d, 0x18), bgl::f32le(d, 0x1c));
                shown += 1;
            }
        }
    });
}

#[test]
#[ignore]
fn what_atx_files_hold() {
    let mut sections: BTreeMap<u32, BTreeMap<u16, (usize, usize)>> = BTreeMap::new();
    let mut n = 0usize;
    for_each_bgl(&["atx"], |_name, data| {
        n += 1;
        if n > 40 || data.len() < 0x38 {
            return;
        }
        let count = bgl::u32le(&data, 0x14) as usize;
        for i in 0..count.min(40) {
            let b = 0x38 + i * 20;
            if b + 20 > data.len() {
                break;
            }
            let section = bgl::u32le(&data, b);
            let entry = sections.entry(section).or_default();
            for r in bgl::section_records(&data, section) {
                let e = entry.entry(r.id).or_insert((0, 0));
                e.0 += 1;
                e.1 = e.1.max(r.end - r.start);
            }
        }
    });
    println!("atx files seen: {n}");
    for (s, ids) in &sections {
        println!("  section 0x{s:02x}: {ids:?}");
    }
}

#[test]
#[ignore]
fn what_the_fs2024_archive_holds() {
    use amdbgen::sources::msfs::{fsarchive::FsArchive, nav_archives};
    let mut sections: BTreeMap<u32, BTreeMap<u16, (usize, usize)>> = BTreeMap::new();
    let mut n = 0usize;
    for path in nav_archives() {
        println!("archive {}", path.display());
        let a = FsArchive::open(&path).unwrap();
        a.for_each_with_prefix(&[""], |name, data| {
            n += 1;
            if n > 200 || data.len() < 0x38 {
                return;
            }
            if n <= 3 {
                println!("  {name} {} bytes magic={:08x}", data.len(), bgl::u32le(&data, 0));
            }
            let count = bgl::u32le(&data, 0x14) as usize;
            for i in 0..count.min(40) {
                let b = 0x38 + i * 20;
                if b + 20 > data.len() {
                    break;
                }
                let section = bgl::u32le(&data, b);
                let entry = sections.entry(section).or_default();
                for r in bgl::section_records(&data, section) {
                    let e = entry.entry(r.id).or_insert((0, 0));
                    e.0 += 1;
                    e.1 = e.1.max(r.end - r.start);
                }
            }
        })
        .unwrap();
    }
    println!("files scanned: {n}");
    for (s, ids) in &sections {
        println!("  section 0x{s:02x}: {ids:?}");
    }
}

#[test]
#[ignore]
fn fs2024_airport_children() {
    use amdbgen::sources::msfs::{fsarchive::FsArchive, nav_archives};
    let mut ids: BTreeMap<u16, (usize, usize)> = BTreeMap::new();
    let mut shown = 0usize;
    for path in nav_archives() {
        let a = FsArchive::open(&path).unwrap();
        a.for_each_with_prefix(&[""], |_n, data| {
            for r in bgl::section_records(&data, 0x03) {
                if r.id != 275 || r.end - r.start < 0x44 {
                    continue;
                }
                if shown < 3 {
                    println!(
                        "icao@0x28={} lon={:.4} lat={:.4} elev_mm={} len={}",
                        bgl::ident(bgl::u32le(&data, r.start + 0x28)),
                        bgl::lon(bgl::u32le(&data, r.start + 0x0c)),
                        bgl::lat(bgl::u32le(&data, r.start + 0x10)),
                        bgl::u32le(&data, r.start + 0x14),
                        r.end - r.start
                    );
                    shown += 1;
                }
                for c in bgl::records(&data, r.start + 0x44, r.end) {
                    let e = ids.entry(c.id).or_insert((0, 0));
                    e.0 += 1;
                    e.1 = e.1.max(c.end - c.start);
                }
            }
        })
        .unwrap();
    }
    println!("FS2024 airport child records: {ids:?}");
}

#[test]
#[ignore]
fn where_is_the_fs2024_icao() {
    use amdbgen::sources::msfs::{fsarchive::FsArchive, nav_archives};
    let mut shown = 0usize;
    for path in nav_archives() {
        let a = FsArchive::open(&path).unwrap();
        a.for_each_with_prefix(&[""], |_n, data| {
            if shown >= 4 {
                return;
            }
            for r in bgl::section_records(&data, 0x03) {
                if r.id != 275 || r.end - r.start < 0x60 || shown >= 4 {
                    continue;
                }
                let d = &data[r.start..r.end];
                let mut hits = Vec::new();
                for off in (0..0x60).step_by(4) {
                    let id = bgl::ident(bgl::u32le(d, off));
                    let looks_icao = id.len() == 4 && id.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
                    if looks_icao {
                        hits.push(format!("+0x{off:02x}={id}"));
                    }
                }
                println!("len={} candidates: {}", d.len(), hits.join("  "));
                println!("   first 0x60: {:02x?}", &d[..0x60]);
                shown += 1;
            }
        })
        .unwrap();
    }
}

/// Find where FS2024 keeps an airport's identifier, by matching records to FS2020's by
/// position — the one field both layouts agree on — and then searching the FS2024 record for
/// the packed form of the identifier FS2020 gives.
#[test]
#[ignore]
fn find_the_fs2024_icao_offset() {
    use amdbgen::sources::msfs::{fsarchive::FsArchive, nav_archives};
    // FS2020's airports, by rounded position.
    let mut known: BTreeMap<(i64, i64), String> = BTreeMap::new();
    for_each_bgl(&[""], |_n, data| {
        for r in bgl::section_records(&data, 0x03) {
            if r.id != 0x56 || r.end - r.start < 0x44 {
                continue;
            }
            let icao = bgl::ident(bgl::u32le(&data, r.start + 0x28));
            if icao.len() < 3 {
                continue;
            }
            let lat = bgl::lat(bgl::u32le(&data, r.start + 0x10));
            let lon = bgl::lon(bgl::u32le(&data, r.start + 0x0c));
            known.insert(((lat * 1000.0) as i64, (lon * 1000.0) as i64), icao);
        }
    });
    println!("FS2020 airports indexed by position: {}", known.len());

    let mut hits: BTreeMap<usize, usize> = BTreeMap::new();
    let mut checked = 0usize;
    for path in nav_archives() {
        let a = FsArchive::open(&path).unwrap();
        a.for_each_with_prefix(&[""], |_n, data| {
            if checked >= 400 {
                return;
            }
            for r in bgl::section_records(&data, 0x03) {
                if r.id != 275 || r.end - r.start < 0x80 || checked >= 400 {
                    continue;
                }
                let d = &data[r.start..r.end];
                let lat = bgl::lat(bgl::u32le(d, 0x10));
                let lon = bgl::lon(bgl::u32le(d, 0x0c));
                let Some(want) = known.get(&((lat * 1000.0) as i64, (lon * 1000.0) as i64)) else { continue };
                checked += 1;
                for off in 0..d.len().min(0x120) {
                    if off + 4 > d.len() {
                        break;
                    }
                    if &bgl::ident(bgl::u32le(d, off)) == want {
                        *hits.entry(off).or_default() += 1;
                    }
                }
            }
        })
        .unwrap();
    }
    println!("records matched to FS2020 by position: {checked}");
    let mut best: Vec<(usize, usize)> = hits.into_iter().collect();
    best.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    for (off, n) in best.iter().take(8) {
        println!("  identifier found at +0x{off:02x} in {n} of {checked} records");
    }
}

/// Where do an FS2024 airport record's children begin, and is its identifier stored as text?
/// The child stream is found by trying each offset and keeping the one whose records tile
/// exactly to the end of the record — a wrong offset runs off the end or stops short.
#[test]
#[ignore]
fn fs2024_airport_children_offset_and_text_ident() {
    use amdbgen::sources::msfs::{fsarchive::FsArchive, nav_archives};
    let mut known: BTreeMap<(i64, i64), String> = BTreeMap::new();
    for_each_bgl(&[""], |_n, data| {
        for r in bgl::section_records(&data, 0x03) {
            if r.id != 0x56 || r.end - r.start < 0x44 {
                continue;
            }
            let icao = bgl::ident(bgl::u32le(&data, r.start + 0x28));
            if icao.len() >= 3 {
                known.insert(((bgl::lat(bgl::u32le(&data, r.start + 0x10)) * 1000.0) as i64, (bgl::lon(bgl::u32le(&data, r.start + 0x0c)) * 1000.0) as i64), icao);
            }
        }
    });
    let mut child_at: BTreeMap<usize, usize> = BTreeMap::new();
    let mut text_at: BTreeMap<usize, usize> = BTreeMap::new();
    let mut checked = 0usize;
    let mut sample = true;
    for path in nav_archives() {
        let a = FsArchive::open(&path).unwrap();
        a.for_each_with_prefix(&[""], |_n, data| {
            if checked >= 300 {
                return;
            }
            for r in bgl::section_records(&data, 0x03) {
                if r.id != 275 || r.end - r.start < 0x80 || checked >= 300 {
                    continue;
                }
                let d = &data[r.start..r.end];
                let lat = bgl::lat(bgl::u32le(d, 0x10));
                let lon = bgl::lon(bgl::u32le(d, 0x0c));
                let Some(want) = known.get(&((lat * 1000.0) as i64, (lon * 1000.0) as i64)) else { continue };
                checked += 1;
                // The identifier as plain text, anywhere in the record.
                let bytes = want.as_bytes();
                for off in 0..d.len().saturating_sub(bytes.len()) {
                    if &d[off..off + bytes.len()] == bytes {
                        *text_at.entry(off).or_default() += 1;
                    }
                }
                // Where a child stream tiles exactly to the end.
                for off in (0x20..0x140).step_by(2) {
                    if off + 6 > d.len() {
                        break;
                    }
                    let mut at = off;
                    let mut ok = 0;
                    while at + 6 <= d.len() {
                        let size = bgl::u32le(d, at + 2) as usize;
                        if size < 6 || at + size > d.len() {
                            break;
                        }
                        at += size;
                        ok += 1;
                    }
                    if at == d.len() && ok >= 2 {
                        *child_at.entry(off).or_default() += 1;
                    }
                }
                if sample {
                    sample = false;
                    println!("sample {want} at {lat:.4},{lon:.4}, {} bytes", d.len());
                }
            }
        })
        .unwrap();
    }
    println!("records checked: {checked}");
    let top = |m: BTreeMap<usize, usize>, label: &str| {
        let mut v: Vec<(usize, usize)> = m.into_iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        for (off, n) in v.iter().take(6) {
            println!("  {label} +0x{off:02x} in {n} of {checked}");
        }
    };
    top(text_at, "identifier as text at");
    top(child_at, "children tile from");
}

#[test]
#[ignore]
fn fs2024_children_with_the_right_offset() {
    use amdbgen::sources::msfs::{fsarchive::FsArchive, nav_archives};
    let mut ids: BTreeMap<u16, (usize, usize)> = BTreeMap::new();
    let mut shown = 0usize;
    for path in nav_archives() {
        let a = FsArchive::open(&path).unwrap();
        a.for_each_with_prefix(&[""], |_n, data| {
            for r in bgl::section_records(&data, 0x03) {
                if r.id != 275 || r.end - r.start < 0x80 {
                    continue;
                }
                let d = &data[r.start..r.end];
                let ident: String = d[0x6f..].iter().take(5).take_while(|&&b| b.is_ascii_alphanumeric()).map(|&b| b as char).collect();
                if shown < 4 && d.len() > 4000 {
                    println!("{ident}: {} bytes, children:", d.len());
                    for c in bgl::records(d, 0x5c, d.len()) {
                        println!("   id 0x{:04x} ({}) len {}", c.id, c.id, c.end - c.start);
                    }
                    shown += 1;
                }
                for c in bgl::records(d, 0x5c, d.len()) {
                    let e = ids.entry(c.id).or_insert((0, 0));
                    e.0 += 1;
                    e.1 = e.1.max(c.end - c.start);
                }
            }
        })
        .unwrap();
    }
    let mut v: Vec<(u16, (usize, usize))> = ids.into_iter().collect();
    v.sort_by_key(|(_, (n, _))| std::cmp::Reverse(*n));
    println!("FS2024 airport children, commonest first:");
    for (id, (n, max)) in v.iter().take(14) {
        println!("  id 0x{id:04x} ({id}) count {n} max {max}");
    }
}

#[test]
#[ignore]
fn fs2024_big_airport_children() {
    use amdbgen::sources::msfs::{fsarchive::FsArchive, nav_archives};
    for path in nav_archives() {
        let a = FsArchive::open(&path).unwrap();
        let mut done = false;
        a.for_each_with_prefix(&[""], |_n, data| {
            if done {
                return;
            }
            for r in bgl::section_records(&data, 0x03) {
                if r.id != 275 || r.end - r.start < 0x80 {
                    continue;
                }
                let d = &data[r.start..r.end];
                let ident: String = d[0x6f..].iter().take(5).take_while(|&&b| b.is_ascii_alphanumeric()).map(|&b| b as char).collect();
                if !matches!(ident.as_str(), "KJFK" | "EGLL" | "KLAX" | "EDDF") {
                    continue;
                }
                let mut ids: BTreeMap<u16, (usize, usize)> = BTreeMap::new();
                for c in bgl::records(d, 0x5c, d.len()) {
                    let e = ids.entry(c.id).or_insert((0, 0));
                    e.0 += 1;
                    e.1 = e.1.max(c.end - c.start);
                }
                println!("{ident}: {} bytes", d.len());
                for (id, (n, max)) in &ids {
                    println!("   id 0x{id:04x} ({id}) count {n} max {max}");
                }
                // A runway child, in full.
                if let Some(c) = bgl::records(d, 0x5c, d.len()).into_iter().find(|c| c.id == 0x11a) {
                    println!("   runway child, {} bytes: {:02x?}", c.end - c.start, &d[c.start..c.end]);
                    println!("     text: {}", d[c.start..c.end].iter().map(|&b| if b.is_ascii_graphic() { b as char } else { '.' }).collect::<String>());
                }
                done = true;
                return;
            }
        })
        .unwrap();
    }
}

/// Where do an FS2024 procedure record's own children (its leg lists and transitions) begin,
/// and what ids do they carry? Found the same way as the airport's: try each offset, keep the
/// one whose records tile exactly to the end.
#[test]
#[ignore]
fn fs2024_procedure_children() {
    use amdbgen::sources::msfs::{fsarchive::FsArchive, nav_archives};
    let mut tally: BTreeMap<(u16, usize), BTreeMap<u16, usize>> = BTreeMap::new();
    let mut offsets: BTreeMap<u16, BTreeMap<usize, usize>> = BTreeMap::new();
    let mut n = 0usize;
    for path in nav_archives() {
        let a = FsArchive::open(&path).unwrap();
        a.for_each_with_prefix(&[""], |_x, data| {
            if n >= 60 {
                return;
            }
            for r in bgl::section_records(&data, 0x03) {
                if r.id != 275 || r.end - r.start < 0x200 || n >= 60 {
                    continue;
                }
                let d = &data[r.start..r.end];
                for c in bgl::records(d, 0x5c, d.len()) {
                    if !matches!(c.id, 0x42 | 0x48 | 0x111) {
                        continue;
                    }
                    n += 1;
                    let body = &d[c.start..c.end];
                    for off in (0x08..0x40).step_by(2) {
                        if off + 6 > body.len() {
                            break;
                        }
                        let mut at = off;
                        let mut ok = 0;
                        while at + 6 <= body.len() {
                            let size = bgl::u32le(body, at + 2) as usize;
                            if size < 6 || at + size > body.len() {
                                break;
                            }
                            at += size;
                            ok += 1;
                        }
                        if at == body.len() && ok >= 1 {
                            *offsets.entry(c.id).or_default().entry(off).or_default() += 1;
                            for cc in bgl::records(body, off, body.len()) {
                                *tally.entry((c.id, off)).or_default().entry(cc.id).or_default() += 1;
                            }
                        }
                    }
                }
            }
        })
        .unwrap();
    }
    for (id, offs) in &offsets {
        let mut v: Vec<(usize, usize)> = offs.iter().map(|(a, b)| (*a, *b)).collect();
        v.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
        println!("record 0x{id:03x}: children tile from {:?}", &v[..v.len().min(3)]);
        if let Some((best, _)) = v.first() {
            if let Some(ids) = tally.get(&(*id, *best)) {
                println!("    at +0x{best:02x} the child ids are {ids:?}");
            }
        }
    }
}

/// Are FS2024's renumbered records leg lists? A leg list is a count and then fixed 72-byte
/// legs, so its length minus its header should divide by 72.
#[test]
#[ignore]
fn fs2024_leg_lists() {
    use amdbgen::sources::msfs::{fsarchive::FsArchive, nav_archives};
    let mut shapes: BTreeMap<u16, BTreeMap<String, usize>> = BTreeMap::new();
    let mut n = 0usize;
    for path in nav_archives() {
        let a = FsArchive::open(&path).unwrap();
        a.for_each_with_prefix(&[""], |_x, data| {
            if n >= 400 {
                return;
            }
            for r in bgl::section_records(&data, 0x03) {
                if r.id != 275 || r.end - r.start < 0x200 || n >= 400 {
                    continue;
                }
                let d = &data[r.start..r.end];
                for c in bgl::records(d, 0x5c, d.len()) {
                    if !matches!(c.id, 0x42 | 0x48 | 0x111) {
                        continue;
                    }
                    let body = &d[c.start..c.end];
                    let at = if c.id == 0x111 { 0x2c } else { 0x14 };
                    if at + 6 > body.len() {
                        continue;
                    }
                    for cc in bgl::records(body, at, body.len()) {
                        n += 1;
                        let len = cc.end - cc.start;
                        // Try a few plausible header sizes and see which leaves a multiple of 72.
                        let fits: Vec<usize> = [6usize, 8, 0x0c, 0x10, 0x14, 0x1c, 0x20].into_iter().filter(|h| len > *h && (len - h) % 72 == 0).collect();
                        let count_at_6 = if len > 8 { bgl::u32le(body, cc.start + 6) & 0xffff } else { 0 };
                        shapes
                            .entry(cc.id)
                            .or_default()
                            .entry(format!("header_candidates={fits:?} u16@+6={count_at_6}"))
                            .and_modify(|v| *v += 1)
                            .or_insert(1);
                    }
                }
            }
        })
        .unwrap();
    }
    for (id, v) in &shapes {
        let mut rows: Vec<(&String, &usize)> = v.iter().collect();
        rows.sort_by_key(|(_, c)| std::cmp::Reverse(**c));
        println!("child 0x{id:03x} ({id}):");
        for (k, c) in rows.iter().take(3) {
            println!("    {k}  ({c} times)");
        }
    }
}

#[test]
#[ignore]
fn inside_fs2024_sid_and_star() {
    use amdbgen::sources::msfs::{fsarchive::FsArchive, nav_archives};
    let mut done = 0usize;
    for path in nav_archives() {
        let a = FsArchive::open(&path).unwrap();
        a.for_each_with_prefix(&[""], |_x, data| {
            if done >= 2 {
                return;
            }
            for r in bgl::section_records(&data, 0x03) {
                if r.id != 275 || r.end - r.start < 0x4000 || done >= 2 {
                    continue;
                }
                let d = &data[r.start..r.end];
                let ident: String = d[0x6f..].iter().take(5).take_while(|&&b| b.is_ascii_alphanumeric()).map(|&b| b as char).collect();
                for c in bgl::records(d, 0x5c, d.len()) {
                    if !matches!(c.id, 0x42 | 0x48) || done >= 2 {
                        continue;
                    }
                    let body = &d[c.start..c.end];
                    println!("{ident} {} record 0x{:02x}, {} bytes", if c.id == 0x42 { "SID " } else { "STAR" }, c.id, body.len());
                    println!("   header: {:02x?}", &body[..0x20.min(body.len())]);
                    for lvl1 in bgl::records(body, 0x14, body.len()) {
                        let n1 = lvl1.end - lvl1.start;
                        println!("   child 0x{:03x} len {n1} count@+6={}", lvl1.id, u16::from_le_bytes([body[lvl1.start + 6], body[lvl1.start + 7]]));
                        for off in [0x08usize, 0x0c, 0x10, 0x14, 0x18] {
                            if lvl1.start + off + 6 > lvl1.end {
                                continue;
                            }
                            let kids: Vec<String> = bgl::records(body, lvl1.start + off, lvl1.end).iter().map(|k| format!("0x{:03x}/{}", k.id, k.end - k.start)).collect();
                            if !kids.is_empty() {
                                println!("       from +0x{off:02x}: {}", kids.join(" "));
                            }
                        }
                    }
                    done += 1;
                }
            }
        })
        .unwrap();
    }
}

#[test]
#[ignore]
fn fs2024_airway_entry_layout() {
    use amdbgen::sources::msfs::{fsarchive::FsArchive, nav_archives};
    let mut shown = 0usize;
    for path in nav_archives() {
        let a = FsArchive::open(&path).unwrap();
        a.for_each_with_prefix(&[""], |_x, data| {
            if shown >= 2 {
                return;
            }
            for r in bgl::section_records(&data, 0x22) {
                let len = r.end - r.start;
                if r.id != 0x108 || len < 200 || len > 400 || shown >= 2 {
                    continue;
                }
                let d = &data[r.start..r.end];
                let n = d[7] as usize;
                println!("len={len} n={n} (len-28)/n={:.2}", (len - 28) as f64 / n.max(1) as f64);
                for stride in [49usize] {
                    let mut at = 28;
                    let mut k = 0;
                    while at + stride <= len && k < 5 {
                        let e = &d[at..at + stride];
                        let text: String = e.iter().map(|&b| if b.is_ascii_graphic() { b as char } else { '.' }).collect();
                        println!("  entry {k} @{at}: {text}");
                        println!("     {:02x?}", e);
                        // Any f32 in the entry that converts to a round hundred of feet.
                        let alts: Vec<String> = (0..stride.saturating_sub(4))
                            .filter_map(|o| {
                                let v = bgl::f32le(e, o) as f64;
                                let ft = v * 3.280_839_895;
                                (v > 100.0 && v < 20000.0 && (ft / 100.0 - (ft / 100.0).round()).abs() < 0.02).then(|| format!("+{o}={:.0}ft", ft))
                            })
                            .collect();
                        println!("     round altitudes: {}", alts.join(" "));
                        at += stride;
                        k += 1;
                    }
                }
                shown += 1;
            }
        })
        .unwrap();
    }
}

#[test]
#[ignore]
fn fs2024_airway_entry_idents() {
    use amdbgen::sources::msfs::{fsarchive::FsArchive, nav_archives};
    let mut tally: BTreeMap<usize, usize> = BTreeMap::new();
    let mut n = 0usize;
    for path in nav_archives() {
        let a = FsArchive::open(&path).unwrap();
        a.for_each_with_prefix(&[""], |_x, data| {
            if n >= 2000 {
                return;
            }
            for r in bgl::section_records(&data, 0x22) {
                let len = r.end - r.start;
                if r.id != 0x108 || len < 28 + 49 {
                    continue;
                }
                let d = &data[r.start..r.end];
                let mut at = 28;
                while at + 49 <= len && n < 2000 {
                    let e = &d[at..at + 49];
                    n += 1;
                    for off in 0..45 {
                        let id = bgl::ident(bgl::u32le(e, off));
                        let plausible = (3..=5).contains(&id.len()) && id.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
                        if plausible {
                            *tally.entry(off).or_default() += 1;
                        }
                    }
                    at += 49;
                }
            }
        })
        .unwrap();
    }
    let mut v: Vec<(usize, usize)> = tally.into_iter().collect();
    v.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
    println!("entries scanned: {n}");
    for (off, c) in v.iter().take(10) {
        println!("  plausible ident at +{off} in {c} of {n}");
    }
}

#[test]
#[ignore]
fn where_is_the_fs2024_fix_ident() {
    use amdbgen::sources::msfs::{fsarchive::FsArchive, nav_archives};
    let mut known: BTreeMap<(i64, i64), String> = BTreeMap::new();
    for_each_bgl(&[""], |_n, data| {
        for r in bgl::section_records(&data, 0x22) {
            if r.id != 0x22 || r.end - r.start < 28 {
                continue;
            }
            let d = &data[r.start..r.end];
            let id = bgl::ident(bgl::u32le(d, 0x14));
            if id.len() >= 3 {
                known.insert(((bgl::lat(bgl::u32le(d, 0x0c)) * 2000.0) as i64, (bgl::lon(bgl::u32le(d, 0x08)) * 2000.0) as i64), id);
            }
        }
    });
    println!("FS2020 fixes indexed: {}", known.len());
    let mut hits: BTreeMap<usize, usize> = BTreeMap::new();
    let mut checked = 0usize;
    for path in nav_archives() {
        let a = FsArchive::open(&path).unwrap();
        a.for_each_with_prefix(&[""], |_x, data| {
            if checked >= 1500 {
                return;
            }
            for r in bgl::section_records(&data, 0x22) {
                if r.id != 0x108 || r.end - r.start < 28 || checked >= 1500 {
                    continue;
                }
                let d = &data[r.start..r.end];
                for (lat_at, lon_at) in [(0x0cusize, 0x08usize)] {
                    let lat = bgl::lat(bgl::u32le(d, lat_at));
                    let lon = bgl::lon(bgl::u32le(d, lon_at));
                    let Some(want) = known.get(&((lat * 2000.0) as i64, (lon * 2000.0) as i64)) else { continue };
                    checked += 1;
                    let bytes = want.as_bytes();
                    for off in 0..d.len().saturating_sub(bytes.len()) {
                        if &d[off..off + bytes.len()] == bytes {
                            *hits.entry(off).or_default() += 1;
                        }
                    }
                }
            }
        })
        .unwrap();
    }
    println!("FS2024 fixes matched by position: {checked}");
    let mut v: Vec<(usize, usize)> = hits.into_iter().collect();
    v.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
    for (off, c) in v.iter().take(6) {
        println!("  identifier at +0x{off:02x} in {c} of {checked}");
    }
}

#[test]
#[ignore]
fn compare_matched_fix_records() {
    use amdbgen::sources::msfs::{fsarchive::FsArchive, nav_archives};
    let mut known: BTreeMap<(i64, i64), (String, Vec<u8>)> = BTreeMap::new();
    for_each_bgl(&[""], |_n, data| {
        for r in bgl::section_records(&data, 0x22) {
            if r.id != 0x22 || r.end - r.start < 28 {
                continue;
            }
            let d = &data[r.start..r.end];
            let id = bgl::ident(bgl::u32le(d, 0x14));
            if id.len() >= 4 {
                known.insert(((bgl::lat(bgl::u32le(d, 0x0c)) * 2000.0) as i64, (bgl::lon(bgl::u32le(d, 0x08)) * 2000.0) as i64), (id, d[..28].to_vec()));
            }
        }
    });
    let mut shown = 0usize;
    for path in nav_archives() {
        let a = FsArchive::open(&path).unwrap();
        a.for_each_with_prefix(&[""], |_x, data| {
            if shown >= 5 {
                return;
            }
            for r in bgl::section_records(&data, 0x22) {
                if r.id != 0x108 || r.end - r.start < 28 || shown >= 5 {
                    continue;
                }
                let d = &data[r.start..r.end];
                let key = ((bgl::lat(bgl::u32le(d, 0x0c)) * 2000.0) as i64, (bgl::lon(bgl::u32le(d, 0x08)) * 2000.0) as i64);
                let Some((id, old)) = known.get(&key) else { continue };
                println!("{id}:");
                println!("   FS2020 header: {:02x?}", old);
                println!("   FS2024 header: {:02x?}", &d[..28]);
                println!("   FS2020 u32@0x14={:08x} decodes {}", bgl::u32le(old, 0x14), bgl::ident(bgl::u32le(old, 0x14)));
                println!("   FS2024 u32@0x14={:08x} @0x18={:08x} @0x10={:08x}", bgl::u32le(d, 0x14), bgl::u32le(d, 0x18), bgl::u32le(d, 0x10));
                shown += 1;
            }
        })
        .unwrap();
    }
}

/// Decode a packed identifier with a given number of tag bits shifted off first.
fn ident_shift(mut v: u32, shift: u32) -> String {
    const CH: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    if v == 0 {
        return String::new();
    }
    v >>= shift;
    let mut out = Vec::new();
    while v > 1 {
        let r = (v % 38) as usize;
        v /= 38;
        out.push(if r < 2 { b' ' } else { CH[r - 2] });
    }
    out.reverse();
    String::from_utf8_lossy(&out).trim().to_string()
}

#[test]
#[ignore]
fn which_shift_decodes_fs2024_idents() {
    use amdbgen::sources::msfs::{fsarchive::FsArchive, nav_archives};
    let mut known: BTreeMap<(i64, i64), String> = BTreeMap::new();
    for_each_bgl(&[""], |_n, data| {
        for r in bgl::section_records(&data, 0x22) {
            if r.id != 0x22 || r.end - r.start < 28 {
                continue;
            }
            let d = &data[r.start..r.end];
            let id = bgl::ident(bgl::u32le(d, 0x14));
            if id.len() >= 4 {
                known.insert(((bgl::lat(bgl::u32le(d, 0x0c)) * 2000.0) as i64, (bgl::lon(bgl::u32le(d, 0x08)) * 2000.0) as i64), id);
            }
        }
    });
    let mut hits: BTreeMap<u32, usize> = BTreeMap::new();
    let mut checked = 0usize;
    for path in nav_archives() {
        let a = FsArchive::open(&path).unwrap();
        a.for_each_with_prefix(&[""], |_x, data| {
            if checked >= 4000 {
                return;
            }
            for r in bgl::section_records(&data, 0x22) {
                if r.id != 0x108 || r.end - r.start < 28 || checked >= 4000 {
                    continue;
                }
                let d = &data[r.start..r.end];
                let Some(want) = known.get(&((bgl::lat(bgl::u32le(d, 0x0c)) * 2000.0) as i64, (bgl::lon(bgl::u32le(d, 0x08)) * 2000.0) as i64)) else { continue };
                checked += 1;
                for shift in 0..10u32 {
                    if &ident_shift(bgl::u32le(d, 0x14), shift) == want {
                        *hits.entry(shift).or_default() += 1;
                    }
                }
            }
        })
        .unwrap();
    }
    println!("checked {checked} matched fixes");
    let mut v: Vec<(u32, usize)> = hits.into_iter().collect();
    v.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
    for (shift, c) in v.iter().take(4) {
        println!("  shifting {shift} bits decodes {c} of {checked}");
    }
}
