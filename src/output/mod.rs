//! Writers: GeoJSON and Geobuf (PBF) per layer, plus manifest/index files.

pub mod geobuf;
pub mod geojson;
pub mod manifest;
pub mod chart;
pub mod preview;
pub mod xplane;

use crate::geom::LocalFrame;
use crate::model::{AmdbFeature, Layer, ALL_LAYERS};
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::Path;

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
