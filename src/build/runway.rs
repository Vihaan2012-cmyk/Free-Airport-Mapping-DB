//! Runway layers: RunwayElement, RunwayThreshold, RunwayDisplacedArea, Blastpad,
//! Stopway, RunwayShoulder, PaintedCenterline, RunwayCenterlinePoint,
//! RunwayIntersection, plus Water for water runways and lighting points at ends.

use super::{Ctx, RwyGeom};
use crate::geom::ops::{self, add, scale, unit_from_heading};
use crate::model::codes::{lighting, source, status, surftype, thrtype};
use crate::model::feature::opt;
use crate::model::{AmdbFeature, Layer};
use geo::{Area, BooleanOps};
use geo_types::{LineString, MultiPolygon, Point, Polygon};
use serde_json::json;

fn ident_num(s: &str) -> Option<u32> {
    s.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().ok()
}

/// "09L/27R" style pair id, low number first.
pub fn pair_id(a: &str, b: &str) -> String {
    match (ident_num(a), ident_num(b)) {
        (Some(x), Some(y)) if y < x => format!("{b}/{a}"),
        _ => format!("{a}/{b}"),
    }
}

pub fn build(ctx: &mut Ctx) {
    // When X-Plane runways exist, OSM runway centrelines are only a cross-check.
    let has_xp = ctx.src.runways.iter().any(|r| r.source == source::XPLANE);
    let mut geoms = Vec::new();
    for (i, r) in ctx.src.runways.iter().enumerate() {
        if has_xp && r.source != source::XPLANE {
            continue;
        }
        let e0 = ctx.p(r.ends[0].pos);
        let e1 = ctx.p(r.ends[1].pos);
        let length = ops::dist(e0, e1);
        if length < 30.0 || r.width_m < 1.0 {
            ctx.warn(format!("runway {}/{} degenerate, skipped", r.ends[0].ident, r.ends[1].ident));
            continue;
        }
        let heading = ops::heading_deg(e0, e1);
        let u = unit_from_heading(heading);
        let t0 = add(e0, scale(u, r.ends[0].displaced_m.min(length / 2.0)));
        let t1 = add(e1, scale(u, -r.ends[1].displaced_m.min(length / 2.0)));
        let poly = ops::rect_between(e0, e1, r.width_m);
        geoms.push(RwyGeom {
            idrwy: pair_id(&r.ends[0].ident, &r.ends[1].ident),
            idents: [r.ends[0].ident.clone(), r.ends[1].ident.clone()],
            ends: [e0, e1],
            thresholds: [t0, t1],
            width: r.width_m,
            length,
            heading,
            poly,
            surface: r.surface,
            src_index: i,
        });
    }
    // Deduplicate OSM-only runways that describe the same pair twice.
    geoms.sort_by(|a, b| a.idrwy.cmp(&b.idrwy));
    geoms.dedup_by(|a, b| a.idrwy == b.idrwy && ops::dist(a.ends[0], b.ends[0]) < 50.0);

    // Intersections: pairwise polygon intersections, subtracted from the elements.
    let mut inter_polys: Vec<(Polygon<f64>, String, i64)> = Vec::new();
    for i in 0..geoms.len() {
        for j in (i + 1)..geoms.len() {
            let x = geoms[i].poly.intersection(&geoms[j].poly);
            for p in x.0 {
                if p.unsigned_area() > 1.0 {
                    inter_polys.push((p, format!("{}+{}", geoms[i].idrwy, geoms[j].idrwy), geoms[i].surface));
                }
            }
        }
    }
    let inter_mp = MultiPolygon(inter_polys.iter().map(|(p, _, _)| p.clone()).collect());

    for g in &geoms {
        let r = &ctx.src.runways[g.src_index];
        let elem = if inter_mp.0.is_empty() { MultiPolygon(vec![g.poly.clone()]) } else { g.poly.difference(&inter_mp) };
        let cl_lights = r.centerline_lights;
        for p in ops::tidy_multi(&elem) {
            let f = AmdbFeature::new(Layer::RunwayElement, p)
                .with("idrwy", g.idrwy.clone())
                .with("surftype", g.surface)
                .with("width", round1(g.width))
                .with("length", round1(g.length))
                .with("status", status::OPEN)
                .with("pcn", serde_json::Value::Null)
                .with("rwelight", cl_lights)
                .with("edgelght", r.edge_lights > 0)
                .with("brngtrue", round1(g.heading))
                .with("source", r.source);
            ctx.push(f);
        }
        // Thresholds / ends / displaced areas / blastpads / stopways per end.
        for k in 0..2 {
            let end = &r.ends[k];
            let e = g.ends[k];
            let t = g.thresholds[k];
            let dir_in = if k == 0 { g.heading } else { (g.heading + 180.0) % 360.0 }; // landing direction
            let u_in = unit_from_heading(dir_in);
            let displaced = ops::dist(e, t);
            let other_thr = g.thresholds[1 - k];
            let lda = ops::dist(t, other_thr);
            let tora = g.length;
            let stopway = r.stopway_m[k];
            // The visual glidepath this end has, if any. The label on the object is tried
            // first, and then where it stands and which way it points: a PAPI sits beside
            // the runway it serves, inside its length of the threshold, pointing along the
            // direction landed on. Many are labelled by the side of the runway the unit is
            // on — PAPI-5L and PAPI-5R are both runway 05 — so the label alone misses them.
            let is_glidepath = |l: &crate::ir::LightObject| matches!(l.kind, lighting::PAPI | lighting::VASI | lighting::APAPI);
            let vasis = ctx
                .src
                .lights
                .iter()
                .find(|l| is_glidepath(l) && l.runway.as_deref() == Some(end.ident.as_str()))
                .or_else(|| {
                    ctx.src.lights.iter().find(|l| {
                        if !is_glidepath(l) {
                            return false;
                        }
                        let Some(hdg) = l.heading_deg else { return false };
                        let off = ((hdg - dir_in + 540.0) % 360.0 - 180.0).abs();
                        off < 25.0 && ops::dist(l.pos, t) < g.length.max(400.0)
                    })
                })
                .map(|l| l.kind);
            let f = AmdbFeature::new(Layer::RunwayThreshold, Point(t))
                .with("idthr", end.ident.clone())
                .with("idrwy", g.idrwy.clone())
                .with("thrtype", if displaced > 0.5 { thrtype::DISPLACED } else { thrtype::THRESHOLD })
                .with("brngtrue", round1(dir_in))
                .with("brngmag", serde_json::Value::Null)
                .with("width", round1(g.width))
                .with("tora", round1(end.tora_m.unwrap_or(tora)))
                .with("toda", round1(end.toda_m.unwrap_or(tora)))
                .with("asda", round1(end.asda_m.unwrap_or(tora + stopway)))
                .with("lda", round1(end.lda_m.unwrap_or(lda)))
                .with("tdze", opt(end.tdze_ft.or(ctx.src.header.elevation_ft)))
                .with("rwymktyp", end.marking)
                .with("tohlight", end.approach_lights)
                .with("tdzlight", end.tdz_lights)
                .with("reil", end.reil)
                .with("vasis", opt(vasis))
                .with("status", status::OPEN)
                .with("source", r.source);
            ctx.push(f);
            // Runway end point (physical end) as a centreline point + threshold point.
            ctx.push(AmdbFeature::new(Layer::RunwayCenterlinePoint, Point(e)).with("idrwy", g.idrwy.clone()).with("idthr", end.ident.clone()).with("feattype", thrtype::END).with("brngtrue", round1(dir_in)).with("source", r.source));
            if displaced > 0.5 {
                ctx.push(AmdbFeature::new(Layer::RunwayCenterlinePoint, Point(t)).with("idrwy", g.idrwy.clone()).with("idthr", end.ident.clone()).with("feattype", thrtype::DISPLACED).with("brngtrue", round1(dir_in)).with("source", r.source));
                let da = ops::rect_along(e, dir_in, displaced, g.width);
                ctx.push(AmdbFeature::new(Layer::RunwayDisplacedArea, da).with("idrwy", g.idrwy.clone()).with("idthr", end.ident.clone()).with("surftype", g.surface).with("length", round1(displaced)).with("width", round1(g.width)).with("source", r.source));
            } else {
                ctx.push(AmdbFeature::new(Layer::RunwayCenterlinePoint, Point(t)).with("idrwy", g.idrwy.clone()).with("idthr", end.ident.clone()).with("feattype", thrtype::THRESHOLD).with("brngtrue", round1(dir_in)).with("source", r.source));
            }
            if end.blastpad_m > 0.5 {
                let bp = ops::rect_along(e, (dir_in + 180.0) % 360.0, end.blastpad_m, g.width);
                ctx.push(AmdbFeature::new(Layer::Blastpad, bp).with("idrwy", g.idrwy.clone()).with("idthr", end.ident.clone()).with("surftype", g.surface).with("length", round1(end.blastpad_m)).with("width", round1(g.width)).with("source", r.source));
            }
            if stopway > 0.5 {
                let sw = ops::rect_along(e, (dir_in + 180.0) % 360.0, stopway, g.width);
                ctx.push(AmdbFeature::new(Layer::Stopway, sw).with("idrwy", g.idrwy.clone()).with("idthr", end.ident.clone()).with("surftype", g.surface).with("length", round1(stopway)).with("width", round1(g.width)).with("source", source::FAA_NASR));
            }
            // Approach lighting / REIL as lighting points at the threshold.
            if end.approach_lights > 0 {
                ctx.push(AmdbFeature::new(Layer::AerodromeSurfaceLighting, Point(add(t, scale(u_in, -30.0)))).with("lstype", lighting::APPROACH).with("alstype", end.approach_lights).with("idrwy", g.idrwy.clone()).with("idthr", end.ident.clone()).with("brngtrue", round1(dir_in)).with("source", r.source));
            }
            if end.reil > 0 {
                let right = ops::right_of(u_in);
                for s in [-1.0, 1.0] {
                    ctx.push(AmdbFeature::new(Layer::AerodromeSurfaceLighting, Point(add(t, scale(right, s * (g.width / 2.0 + 12.0))))).with("lstype", lighting::REIL).with("idrwy", g.idrwy.clone()).with("idthr", end.ident.clone()).with("brngtrue", round1(dir_in)).with("source", r.source));
                }
            }
        }
        // Painted centreline between thresholds.
        let cl = LineString(vec![g.thresholds[0], g.thresholds[1]]);
        ctx.push(AmdbFeature::new(Layer::PaintedCenterline, cl).with("idrwy", g.idrwy.clone()).with("width", 0.9).with("length", round1(ops::dist(g.thresholds[0], g.thresholds[1]))).with("source", source::DERIVED));
        // Shoulders.
        if let Some(ss) = r.shoulder_surface {
            let w = r.shoulder_width_m.unwrap_or(ctx.opts.runway_shoulder_m);
            let right = ops::right_of(unit_from_heading(g.heading));
            for s in [-1.0, 1.0] {
                let off = scale(right, s * (g.width / 2.0 + w / 2.0));
                let sh = ops::rect_between(add(g.ends[0], off), add(g.ends[1], off), w);
                ctx.push(AmdbFeature::new(Layer::RunwayShoulder, sh).with("idrwy", g.idrwy.clone()).with("surftype", ss).with("width", round1(w)).with("source", source::DERIVED));
            }
        }
    }
    for (p, id, surf) in inter_polys {
        let c = ops::centroid(&p);
        ctx.push(AmdbFeature::new(Layer::RunwayCenterlinePoint, Point(c)).with("idrwy", id.clone()).with("idthr", serde_json::Value::Null).with("feattype", 4).with("brngtrue", serde_json::Value::Null).with("source", source::DERIVED));
        ctx.push(AmdbFeature::new(Layer::RunwayIntersection, p).with("idrwi", id.clone()).with("idrwy", id).with("surftype", surf).with("status", status::OPEN).with("source", source::DERIVED));
    }
    // Water runways.
    for w in &ctx.src.water_runways {
        let a = ctx.p(w.ends[0].1);
        let b = ctx.p(w.ends[1].1);
        if ops::dist(a, b) > 30.0 {
            let poly = ops::rect_between(a, b, w.width_m.max(10.0));
            let id = pair_id(&w.ends[0].0, &w.ends[1].0);
            ctx.push(AmdbFeature::new(Layer::Water, poly).with("feattype", 2).with("idrwy", id.clone()).with("name", json!(format!("Water runway {id}"))).with("surftype", surftype::WATER).with("source", source::XPLANE));
        }
    }
    // OSM runway *areas* (when present and no X-Plane runways) add nothing new here;
    // the OSM centreline already produced the element.
    let all: Vec<Polygon<f64>> = geoms.iter().map(|g| g.poly.clone()).collect();
    ctx.runway_mp = ops::union_all(&all);
    ctx.runways = geoms;
    if ctx.runways.is_empty() && ctx.src.helipads.is_empty() {
        ctx.warn("no runways in any source");
    }
}

pub fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pair_ids_are_ordered() {
        assert_eq!(pair_id("27", "09"), "09/27");
        assert_eq!(pair_id("16L", "34R"), "16L/34R");
        assert_eq!(pair_id("H1", "H2"), "H1/H2");
    }
}
