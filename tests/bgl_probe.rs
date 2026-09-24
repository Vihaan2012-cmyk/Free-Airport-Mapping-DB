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
