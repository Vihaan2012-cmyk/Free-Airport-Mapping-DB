//! Validation: geometry kind per layer, finite coordinates, ring sizes, unique ids,
//! ASRN edge/node consistency, thresholds on runway surfaces.

use crate::model::{AmdbFeature, GeomKind, Layer};
use geo::{Contains, Intersects};
use geo_types::{Coord, Geometry, Point};
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Default, Clone)]
pub struct Report {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub dropped: usize,
}

fn coords(g: &Geometry<f64>) -> Vec<Coord<f64>> {
    match g {
        Geometry::Point(p) => vec![p.0],
        Geometry::LineString(l) => l.0.clone(),
        Geometry::Polygon(p) => p.exterior().0.iter().chain(p.interiors().iter().flat_map(|r| r.0.iter())).copied().collect(),
        Geometry::MultiPoint(m) => m.0.iter().map(|p| p.0).collect(),
        Geometry::MultiLineString(m) => m.0.iter().flat_map(|l| l.0.iter().copied()).collect(),
        Geometry::MultiPolygon(m) => m.0.iter().flat_map(|p| coords(&Geometry::Polygon(p.clone()))).collect(),
        _ => vec![],
    }
}

fn kind_ok(layer: Layer, g: &Geometry<f64>) -> bool {
    match (layer.kind(), g) {
        (GeomKind::Point, Geometry::Point(_)) | (GeomKind::Point, Geometry::MultiPoint(_)) => true,
        (GeomKind::Curve, Geometry::LineString(l)) => l.0.len() >= 2,
        (GeomKind::Curve, Geometry::MultiLineString(m)) => m.0.iter().all(|l| l.0.len() >= 2),
        (GeomKind::Surface, Geometry::Polygon(p)) => p.exterior().0.len() >= 4,
        (GeomKind::Surface, Geometry::MultiPolygon(m)) => m.0.iter().all(|p| p.exterior().0.len() >= 4),
        _ => false,
    }
}

/// Validate and drop broken features in place. Cross-layer checks only warn.
pub fn validate_and_fix(features: &mut BTreeMap<Layer, Vec<AmdbFeature>>) -> Report {
    let mut rep = Report::default();
    for (layer, feats) in features.iter_mut() {
        let before = feats.len();
        feats.retain(|f| {
            if !kind_ok(*layer, &f.geom) {
                rep.errors.push(format!("{}: geometry kind mismatch or degenerate (id {:?})", layer.name(), f.get_str("id")));
                return false;
            }
            if coords(&f.geom).iter().any(|c| !c.x.is_finite() || !c.y.is_finite()) {
                rep.errors.push(format!("{}: non-finite coordinate (id {:?})", layer.name(), f.get_str("id")));
                return false;
            }
            true
        });
        rep.dropped += before - feats.len();
        let mut ids = HashSet::new();
        for f in feats.iter() {
            if let Some(id) = f.props.get("id") {
                if !ids.insert(id.to_string()) {
                    rep.errors.push(format!("{}: duplicate id {id}", layer.name()));
                }
            } else {
                rep.errors.push(format!("{}: feature without id", layer.name()));
            }
        }
    }
    // ASRN consistency.
    let node_ids: HashSet<i64> = features.get(&Layer::AsrnNode).map(|v| v.iter().filter_map(|f| f.props.get("nodeid").and_then(|v| v.as_i64())).collect()).unwrap_or_default();
    if let Some(edges) = features.get(&Layer::AsrnEdge) {
        for e in edges {
            for k in ["stnode", "ennode"] {
                if let Some(n) = e.props.get(k).and_then(|v| v.as_i64()) {
                    if !node_ids.contains(&n) {
                        rep.errors.push(format!("asrnedge {:?}: {k} {n} references a missing node", e.get_str("id")));
                    }
                }
            }
        }
    }
    // Thresholds must sit on a runway element or displaced area.
    let surfaces: Vec<Geometry<f64>> = features.get(&Layer::RunwayElement).into_iter().chain(features.get(&Layer::RunwayDisplacedArea)).chain(features.get(&Layer::RunwayIntersection)).flat_map(|v| v.iter().map(|f| f.geom.clone())).collect();
    if let Some(thr) = features.get(&Layer::RunwayThreshold) {
        for t in thr {
            if let Geometry::Point(p) = &t.geom {
                let on = surfaces.iter().any(|s| match s {
                    Geometry::Polygon(poly) => poly.contains(p) || poly.intersects(&Point(p.0)) || crate::geom::ops::point_line_dist(p.0, poly.exterior()) < 1.0,
                    _ => false,
                });
                if !on {
                    rep.warnings.push(format!("threshold {:?} is not on a runway surface", t.get_str("idthr")));
                }
            }
        }
    }
    rep
}

/// Validate an already written airport directory (GeoJSON files).
pub fn validate_dir(dir: &std::path::Path) -> anyhow::Result<Report> {
    let mut feats: BTreeMap<Layer, Vec<AmdbFeature>> = BTreeMap::new();
    let mut missing = Vec::new();
    // The manifest says which layers this folder was built with (a profile may omit some).
    let expected: Vec<Layer> = std::fs::read_to_string(dir.join("manifest.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|m| m.get("layers")?.as_object().map(|o| o.keys().filter_map(|k| Layer::from_name(k)).collect()))
        .unwrap_or_else(|| crate::model::ALL_LAYERS.to_vec());
    for l in &expected {
        match crate::output::layers::read(dir, l.name()) {
            Some(t) => {
                let (_, v) = crate::output::geojson::parse_feature_collection(&t, *l)?;
                feats.insert(*l, v);
            }
            None => missing.push(l.name().to_string()),
        }
    }
    let mut rep = validate_and_fix(&mut feats);
    for m in missing {
        rep.errors.push(format!("missing layer file {m}.geojson"));
    }
    Ok(rep)
}
