//! Everything that is not pavement, lines, stands or routing: ARP, buildings and
//! other vertical structures, service roads, water, construction, deicing, bridges,
//! frequency areas, lighting objects, helipads, arresting systems, LAHSO.

use super::Ctx;
use crate::geom::ops::{self, add, scale, unit_from_heading};
use crate::ir::{AreaKind, LineKind};
use crate::model::codes::{source, status, surftype};
use crate::model::feature::opt;
use crate::model::{AmdbFeature, Layer};
use geo::{Area, Intersects};
use geo_types::{Coord, LineString, MultiPolygon, Point, Polygon};

pub fn aerodrome_reference_point(ctx: &mut Ctx) {
    let h = &ctx.src.header;
    let src = if h.arp.is_some() && ctx.src.sources.iter().any(|s| s == source::XPLANE) { source::XPLANE } else { source::DERIVED };
    let f = AmdbFeature::new(Layer::AerodromeReferencePoint, Point(Coord { x: 0.0, y: 0.0 }))
        .with("idarpt", h.icao.clone())
        .with("iata", opt(h.iata.clone()))
        .with("name", opt(h.name.clone()))
        .with("city", opt(h.city.clone()))
        .with("country", opt(h.country.clone()))
        .with("region", opt(h.region.clone()))
        .with("elev", opt(h.elevation_ft))
        .with("geound", serde_json::Value::Null)
        .with("magvar", opt(h.mag_var))
        .with("transalt", opt(h.transition_alt_ft))
        .with("translvl", opt(h.transition_level.clone()))
        .with("lat", ctx.frame.lat0)
        .with("lon", ctx.frame.lon0)
        .with("source", src);
    ctx.push(f);
}

fn poly_from(ctx: &Ctx, outer: &[Coord<f64>], holes: &[Vec<Coord<f64>>]) -> Option<Polygon<f64>> {
    let ext = ops::close_ring(outer.iter().map(|c| ctx.p(*c)).collect());
    if ext.0.len() < 4 {
        return None;
    }
    let ints: Vec<LineString<f64>> = holes.iter().map(|h| ops::close_ring(h.iter().map(|c| ctx.p(*c)).collect())).filter(|l| l.0.len() >= 4).collect();
    ops::tidy_polygon(&Polygon::new(ext, ints))
}

/// Airport extent: OSM aerodrome polygon(s) and X-Plane boundary, unioned with a
/// margin around all pavement so nearby structures are kept.
pub fn compute_extent(ctx: &mut Ctx) {
    let mut polys: Vec<Polygon<f64>> = Vec::new();
    for a in ctx.src.areas.iter().filter(|a| a.kind == AreaKind::Aerodrome) {
        if let Some(p) = poly_from(ctx, &a.outer, &a.holes) {
            polys.push(p);
        }
    }
    if let Some(b) = &ctx.src.boundary {
        if let Some(p) = super::conv::ring_to_polygon(&ctx.frame, b, &[]) {
            polys.push(p);
        }
    }
    let mut pts: Vec<Coord<f64>> = Vec::new();
    for p in ctx.pavement_mp.0.iter().chain(ctx.runway_mp.0.iter()) {
        pts.extend(p.exterior().0.iter().copied());
    }
    pts.extend(ctx.stands_local.iter().map(|s| s.pos));
    pts.extend(ctx.src.stands.iter().map(|s| ctx.p(s.pos)));
    if let Some(h) = ops::hull(&pts) {
        polys.extend(ops::buffer_polygon(&h, ctx.opts.extent_margin_m).0);
    }
    if polys.is_empty() {
        polys.push(ops::circle(Coord { x: 0.0, y: 0.0 }, 2500.0, 32));
    }
    ctx.extent = ops::union_all(&polys);
}

pub fn build(ctx: &mut Ctx) {
    let extent = ctx.extent.clone();
    let pavement_all = ops::mp_union(&ctx.pavement_mp, &ctx.runway_mp);
    // DO-272 capture rule (as Navigraph applies it): structures and roads within 90 m of a
    // runway or 50 m of any other movement area. Terminals and towers are always kept.
    let stand_polys: Vec<Polygon<f64>> = ctx.layer(Layer::ParkingStandArea).iter().filter_map(|f| if let geo_types::Geometry::Polygon(p) = &f.geom { Some(p.clone()) } else { None }).collect();
    let movement = ops::mp_union(&ctx.pavement_mp, &ops::union_all(&stand_polys));
    let near = ops::mp_union(&ops::buffer_multi(&movement, 50.0), &ops::buffer_multi(&ctx.runway_mp, 90.0));
    let near = if near.0.is_empty() { extent.clone() } else { near };
    let keep_poly = |p: &Polygon<f64>| near.0.iter().any(|e| e.intersects(p));
    let keep_line = |l: &LineString<f64>| near.0.iter().any(|e| e.intersects(l));
    let keep_pt = |c: Coord<f64>| ops::mp_contains_point(&near, c);

    // Buildings.
    for b in &ctx.src.buildings {
        let Some(p) = poly_from(ctx, &b.outer, &b.holes) else { continue };
        let landmark = matches!(b.kind, crate::model::codes::plysttyp::TERMINAL | crate::model::codes::plysttyp::CONTROL_TOWER | crate::model::codes::plysttyp::HANGAR);
        if !keep_poly(&p) && !(landmark && extent.0.iter().any(|e| e.intersects(&p))) {
            continue;
        }
        // Only landmarks carry a name on the map (Navigraph labels terminals, towers,
        // hangars); cargo sheds and offices would just clutter the display.
        ctx.push(AmdbFeature::new(Layer::VerticalPolygonalStructure, p).with("plysttyp", b.kind).with("name", opt(if landmark { b.name.clone() } else { None })).with("height", opt(b.height_m)).with("levels", opt(b.levels)).with("elev", serde_json::Value::Null).with("material", serde_json::Value::Null).with("source", b.source));
    }
    // Point structures.
    for s in &ctx.src.point_structures {
        let c = ctx.p(s.pos);
        if !keep_pt(c) {
            continue;
        }
        ctx.push(AmdbFeature::new(Layer::VerticalPointStructure, Point(c)).with("pntsttyp", s.kind).with("name", opt(s.name.clone())).with("height", opt(s.height_m)).with("elev", serde_json::Value::Null).with("lighting", serde_json::Value::Null).with("source", s.source));
    }
    // Line structures.
    for l in &ctx.src.line_structures {
        let ls = ctx.frame.fwd_line(&l.pts);
        if ls.0.len() < 2 || !keep_line(&ls) {
            continue;
        }
        ctx.push(AmdbFeature::new(Layer::VerticalLineStructure, ls).with("linsttyp", l.kind).with("name", opt(l.name.clone())).with("height", opt(l.height_m)).with("elev", serde_json::Value::Null).with("source", l.source));
    }
    // Service roads from OSM centrelines: buffered, clipped to the extent, minus pavement.
    let has_osm_roads = ctx.src.semantic_lines.iter().any(|l| l.kind == LineKind::RoadCenter);
    if has_osm_roads {
        for l in ctx.src.semantic_lines.iter().filter(|l| l.kind == LineKind::RoadCenter) {
            let ls = ctx.frame.fwd_line(&l.pts);
            if ls.0.len() < 2 || !keep_line(&ls) {
                continue;
            }
            let w = l.width_m.unwrap_or(5.0);
            let mut mp = ops::buffer_line(&ls, w / 2.0);
            mp = ops::mp_intersection(&mp, &near);
            mp = ops::mp_difference(&mp, &pavement_all);
            for p in ops::tidy_multi(&mp) {
                if p.unsigned_area() < 4.0 {
                    continue;
                }
                ctx.push(AmdbFeature::new(Layer::ServiceRoad, p).with("name", opt(l.name.clone())).with("surftype", surftype::ASPHALT).with("width", w).with("bridge", l.bridge).with("source", l.source));
            }
        }
    }
    // Areas: water, construction, deicing, stopways/blastpads from OSM.
    let mut deicing_polys: Vec<(Polygon<f64>, Option<String>)> = Vec::new();
    for a in &ctx.src.areas {
        let layer = match a.kind {
            AreaKind::Water => Layer::Water,
            AreaKind::Construction => Layer::ConstructionArea,
            AreaKind::Deicing => Layer::DeicingArea,
            AreaKind::Stopway => Layer::Stopway,
            AreaKind::Blastpad => Layer::Blastpad,
            AreaKind::Hotspot => Layer::Hotspot,
            _ => continue,
        };
        let Some(p) = poly_from(ctx, &a.outer, &a.holes) else { continue };
        if !keep_poly(&p) {
            continue;
        }
        let parts: Vec<Polygon<f64>> = match layer {
            Layer::Water => ops::tidy_multi(&ops::mp_intersection(&MultiPolygon(vec![p.clone()]), &extent)),
            // OSM construction sites often still cover pavement the scenery already has
            // in use; the pavement wins and the site keeps only what lies outside it.
            Layer::ConstructionArea => {
                let before = p.unsigned_area();
                let rest = ops::tidy_multi(&ops::mp_difference(&MultiPolygon(vec![p.clone()]), &ctx.pavement_mp));
                let after: f64 = rest.iter().map(|q| q.unsigned_area()).sum();
                if before > 0.0 && after / before < 0.25 { vec![] } else { rest.into_iter().filter(|q| q.unsigned_area() > 400.0).collect() }
            }
            _ => vec![p],
        };
        for p in parts {
            match layer {
                Layer::Water => ctx.push(AmdbFeature::new(layer, p).with("feattype", 1).with("idrwy", serde_json::Value::Null).with("name", opt(a.name.clone())).with("surftype", surftype::WATER).with("source", a.source)),
                Layer::ConstructionArea => ctx.push(AmdbFeature::new(layer, p).with("name", opt(a.name.clone())).with("status", status::CONSTRUCTION).with("pendate", serde_json::Value::Null).with("source", a.source)),
                Layer::DeicingArea => {
                    deicing_polys.push((p.clone(), a.name.clone()));
                    ctx.push(AmdbFeature::new(layer, p).with("idapron", opt(a.name.clone())).with("deicegrp", serde_json::Value::Null).with("surftype", opt(a.surface)).with("status", status::OPEN).with("source", a.source));
                }
                // `reference` carries the published caution text where a source has one.
                Layer::Hotspot => ctx.push(AmdbFeature::new(layer, p).with("idhot", opt(a.name.clone())).with("name", opt(a.name.clone())).with("description", opt(a.reference.clone())).with("source", a.source)),
                _ => ctx.push(AmdbFeature::new(layer, p).with("idrwy", opt(a.reference.clone())).with("idthr", serde_json::Value::Null).with("surftype", opt(a.surface)).with("length", serde_json::Value::Null).with("width", serde_json::Value::Null).with("source", a.source)),
            }
        }
    }
    // Deicing groups: clusters of deicing areas within 60 m of each other.
    if !deicing_polys.is_empty() {
        let buffered: Vec<Polygon<f64>> = deicing_polys.iter().flat_map(|(p, _)| ops::buffer_polygon(p, 30.0).0).collect();
        let groups = ops::union_all(&buffered);
        for (gi, gp) in groups.0.iter().enumerate() {
            let members: Vec<&(Polygon<f64>, Option<String>)> = deicing_polys.iter().filter(|(p, _)| gp.intersects(p)).collect();
            let hull_pts: Vec<Coord<f64>> = members.iter().flat_map(|(p, _)| p.exterior().0.iter().copied()).collect();
            let Some(h) = ops::hull(&hull_pts) else { continue };
            let name = members.iter().find_map(|(_, n)| n.clone()).unwrap_or_else(|| format!("DEICE {}", gi + 1));
            ctx.push(AmdbFeature::new(Layer::DeicingGroup, h).with("deicegrp", name).with("members", members.len() as i64).with("source", source::DERIVED));
        }
        // Back-fill group ids on the areas.
        let groups2 = groups.clone();
        if let Some(areas) = ctx.out.get_mut(&Layer::DeicingArea) {
            for a in areas.iter_mut() {
                if let geo_types::Geometry::Polygon(p) = &a.geom {
                    if let Some(gi) = groups2.0.iter().position(|g| g.intersects(p)) {
                        a.set("deicegrp", format!("DEICE {}", gi + 1));
                    }
                }
            }
        }
    }
    // Bridges: the two sides of each bridged taxiway centreline.
    for l in ctx.src.semantic_lines.iter().filter(|l| l.bridge && l.kind == LineKind::TaxiCenterline) {
        let ls = ctx.frame.fwd_line(&l.pts);
        if ls.0.len() < 2 {
            continue;
        }
        let w = l.width_m.unwrap_or(ctx.opts.default_taxiway_width_m) + 6.0;
        for (side, s) in [(1, -1.0), (2, 1.0)] {
            let off = offset_line(&ls, s * w / 2.0);
            ctx.push(AmdbFeature::new(Layer::BridgeSide, off).with("idlin", opt(l.name.clone())).with("side", side).with("name", opt(l.name.clone())).with("source", l.source));
        }
    }
    // Frequency areas: one extent polygon per ATC frequency.
    let freq_poly: Option<Polygon<f64>> = {
        let b = ctx.src.boundary.as_ref().and_then(|b| super::conv::ring_to_polygon(&ctx.frame, b, &[]));
        b.or_else(|| extent.0.iter().max_by(|a, b| a.unsigned_area().partial_cmp(&b.unsigned_area()).unwrap()).cloned())
    };
    if let Some(fp) = freq_poly {
        for f in &ctx.src.frequencies {
            ctx.push(AmdbFeature::new(Layer::FrequencyArea, fp.clone()).with("frq", (f.mhz * 1000.0).round() / 1000.0).with("station", f.station).with("name", f.name.clone()).with("source", source::XPLANE));
        }
    }
    // Lighting objects (PAPI/VASI/beacons/guard lights).
    for l in &ctx.src.lights {
        let c = ctx.p(l.pos);
        let idrwy = l.runway.as_ref().and_then(|r| ctx.runways.iter().find(|g| g.idents.iter().any(|i| i.eq_ignore_ascii_case(r))).map(|g| g.idrwy.clone()));
        ctx.push(AmdbFeature::new(Layer::AerodromeSurfaceLighting, Point(c)).with("lstype", l.kind).with("brngtrue", opt(l.heading_deg)).with("angle", opt(l.glideslope_deg)).with("idthr", opt(l.runway.clone())).with("idrwy", opt(idrwy)).with("name", opt(l.name.clone())).with("source", l.source));
    }
    // Helipads.
    for h in &ctx.src.helipads {
        let c = ctx.p(h.pos);
        let (len, wid) = if h.length_m > 1.0 && h.width_m > 1.0 { (h.length_m, h.width_m) } else { (25.0, 25.0) };
        let osm_area = ctx.src.areas.iter().find(|a| a.kind == AreaKind::Helipad && a.name == Some(h.ident.clone())).and_then(|a| poly_from(ctx, &a.outer, &a.holes));
        let fato = osm_area.unwrap_or_else(|| ops::rect_centered(c, h.heading_deg, len, wid));
        let tlof = ops::rect_centered(c, h.heading_deg, len * 0.6, wid * 0.6);
        let thr = add(c, scale(unit_from_heading(h.heading_deg), -len / 2.0));
        ctx.push(AmdbFeature::new(Layer::FinalApproachAndTakeOffArea, fato).with("ident", h.ident.clone()).with("surftype", h.surface).with("length", len).with("width", wid).with("brngtrue", h.heading_deg).with("source", h.source));
        ctx.push(AmdbFeature::new(Layer::TouchDownLiftOffArea, tlof).with("ident", h.ident.clone()).with("surftype", h.surface).with("length", len * 0.6).with("width", wid * 0.6).with("brngtrue", h.heading_deg).with("source", source::DERIVED));
        ctx.push(AmdbFeature::new(Layer::HelipadThreshold, Point(thr)).with("ident", h.ident.clone()).with("brngtrue", h.heading_deg).with("elev", opt(ctx.src.header.elevation_ft)).with("source", source::DERIVED));
    }
    // Arresting systems and LAHSO lines across the runway.
    let runways = ctx.runways.clone();
    for a in &ctx.src.arresting {
        let Some((g, k)) = runways.iter().find_map(|g| g.idents.iter().position(|i| i.eq_ignore_ascii_case(&a.runway_end)).map(|k| (g, k))) else { continue };
        let dir_in = if k == 0 { g.heading } else { (g.heading + 180.0) % 360.0 };
        let u = unit_from_heading(dir_in);
        let r = ops::right_of(u);
        let d = a.distance_m.unwrap_or(0.0);
        let c = add(g.ends[k], scale(u, d));
        let line = LineString(vec![add(c, scale(r, -g.width / 2.0)), add(c, scale(r, g.width / 2.0))]);
        ctx.push(AmdbFeature::new(Layer::ArrestingGearLocation, line).with("idrwy", g.idrwy.clone()).with("idthr", g.idents[k].clone()).with("feattype", a.kind.clone()).with("distance", opt(a.distance_m)).with("source", a.source));
        let emas = a.kind.to_ascii_uppercase().contains("EMAS");
        let poly = if emas { ops::rect_along(g.ends[k], (dir_in + 180.0) % 360.0, 120.0, g.width) } else { ops::rect_along(add(c, scale(u, -10.0)), dir_in, 20.0, g.width + 10.0) };
        ctx.push(AmdbFeature::new(Layer::ArrestingSystemLocation, poly).with("idrwy", g.idrwy.clone()).with("idthr", g.idents[k].clone()).with("astype", a.kind.clone()).with("aslength", if emas { 120.0 } else { 20.0 }).with("aslwidth", g.width).with("source", source::DERIVED));
    }
    for l in &ctx.src.lahso {
        let Some((g, k)) = runways.iter().find_map(|g| g.idents.iter().position(|i| i.eq_ignore_ascii_case(&l.runway_end)).map(|k| (g, k))) else { continue };
        let dir_in = if k == 0 { g.heading } else { (g.heading + 180.0) % 360.0 };
        let u = unit_from_heading(dir_in);
        let r = ops::right_of(u);
        let c = add(g.thresholds[k], scale(u, l.available_m.min(g.length)));
        let line = LineString(vec![add(c, scale(r, -g.width / 2.0)), add(c, scale(r, g.width / 2.0))]);
        ctx.push(AmdbFeature::new(Layer::LandAndHoldShortOperationLocation, line).with("idrwy", g.idrwy.clone()).with("idthr", g.idents[k].clone()).with("lahsotyp", 1).with("lda", super::runway::round1(l.available_m)).with("idcross", opt(l.intersecting.clone())).with("source", l.source));
    }
}

/// Offset a polyline sideways by `d` metres (positive = right of travel).
pub fn offset_line(ls: &LineString<f64>, d: f64) -> LineString<f64> {
    let n = ls.0.len();
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let prev = if i == 0 { ls.0[0] } else { ls.0[i - 1] };
        let next = if i == n - 1 { ls.0[n - 1] } else { ls.0[i + 1] };
        let len = ops::dist(prev, next).max(1e-9);
        let u = Coord { x: (next.x - prev.x) / len, y: (next.y - prev.y) / len };
        let r = ops::right_of(u);
        out.push(add(ls.0[i], scale(r, d)));
    }
    LineString(out)
}
