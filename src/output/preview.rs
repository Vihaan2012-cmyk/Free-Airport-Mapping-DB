//! Self-contained HTML previews of a built airport: an OANS-style moving-map viewer
//! and a Jeppesen-style airport diagram. The page templates live in `tools/` and are
//! embedded at compile time; the layers they draw are inlined so the file works offline.

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};
use std::path::Path;

const VIEWER_TEMPLATE: &str = include_str!("../../tools/oans_template.html");
const CHART_TEMPLATE: &str = include_str!("../../tools/jepp_template.html");
const DATA_HOOK: &str = "/*__DATA__*/null";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preview {
    /// OANS-style moving map (viewer.html).
    Viewer,
    /// Jeppesen-style airport diagram (chart.html).
    Chart,
}

impl Preview {
    pub fn default_file(self) -> &'static str {
        match self {
            Preview::Viewer => "viewer.html",
            Preview::Chart => "chart.html",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Preview::Viewer => "OANS-style viewer",
            Preview::Chart => "airport diagram",
        }
    }

    fn template(self) -> &'static str {
        match self {
            Preview::Viewer => VIEWER_TEMPLATE,
            Preview::Chart => CHART_TEMPLATE,
        }
    }

    fn layers(self) -> &'static [&'static str] {
        match self {
            Preview::Viewer => &[
                "aerodromereferencepoint", "runwayelement", "runwaydisplacedarea", "blastpad", "stopway",
                "runwayshoulder", "taxiwayelement", "taxiwayshoulder", "apronelement", "serviceroad",
                "verticalpolygonalstructure", "water", "constructionarea", "deicingarea",
                "taxiwayguidanceline", "standguidanceline", "runwayexitline", "taxiwayholdingposition",
                "taxiwayintersectionmarking", "paintedcenterline", "parkingstandlocation", "runwaythreshold",
                "runwaymarking", "aerodromesign", "hotspot", "bridgeside", "finalapproachandtakeoffarea",
            ],
            Preview::Chart => &[
                "aerodromereferencepoint", "runwayelement", "runwaydisplacedarea", "blastpad", "stopway",
                "taxiwayelement", "apronelement", "verticalpolygonalstructure", "water", "taxiwayguidanceline",
                "runwayexitline", "taxiwayholdingposition", "parkingstandlocation", "runwaythreshold",
                "frequencyarea", "hotspot", "verticalpointstructure", "constructionarea",
            ],
        }
    }

    /// Properties kept per layer (everything else is dropped to keep the page small).
    fn keep(self, layer: &str) -> &'static [&'static str] {
        match (self, layer) {
            (_, "runwayelement") => &["idrwy", "width", "length", "surftype"],
            (_, "taxiwayelement") => &["idlin"],
            (_, "apronelement") => &["idapron"],
            (_, "verticalpolygonalstructure") => &["plysttyp", "name"],
            (_, "taxiwayguidanceline") => &["idlin"],
            (_, "runwayexitline") => &["idlin", "idrwy"],
            (_, "taxiwayholdingposition") => &["idrwy", "catstop"],
            (_, "parkingstandlocation") => &["idstd", "brngtrue"],
            (_, "runwaythreshold") => &["idthr", "idrwy", "brngtrue", "tora", "lda", "width", "rwymktyp"],
            (_, "runwaymarking") => &["marktype", "text"],
            (_, "aerodromesign") => &["msgfront", "signtype", "signdir"],
            (_, "aerodromereferencepoint") => &["idarpt", "iata", "name", "city", "country", "elev", "lat", "lon", "transalt"],
            (_, "hotspot") => &["idhot"],
            (_, "frequencyarea") => &["frq", "station", "name"],
            (_, "verticalpointstructure") => &["pntsttyp", "name"],
            _ => &[],
        }
    }

    pub fn viewer_layers() -> &'static [&'static str] {
        Preview::Viewer.layers()
    }
}

/// Render the browser map from in-memory bridge responses, so it also works when the
/// bridge is configured not to retain generated airport files.
pub fn render_viewer(manifest: &crate::output::manifest::Manifest, collections: &Map<String, Value>) -> Result<String> {
    let mut layers = Map::new();
    for name in Preview::Viewer.layers() {
        let Some(fc) = collections.get(*name) else { continue };
        let keep = Preview::Viewer.keep(name);
        let slim: Vec<Value> = fc.get("features").and_then(Value::as_array).map(|fs| {
            fs.iter().filter_map(|f| {
                let g = f.get("geometry")?.clone();
                if g.is_null() { return None; }
                let props = f.get("properties").and_then(Value::as_object);
                let mut p = Map::new();
                if let Some(props) = props {
                    for k in keep {
                        if let Some(v) = props.get(*k) { if !v.is_null() { p.insert((*k).to_string(), v.clone()); } }
                    }
                }
                Some(json!({"g":g,"p":p}))
            }).collect()
        }).unwrap_or_default();
        layers.insert((*name).to_string(), Value::Array(slim));
    }
    let payload = serde_json::to_string(&json!({"manifest":manifest,"layers":layers}))?;
    Ok(VIEWER_TEMPLATE.replacen(DATA_HOOK, &payload, 1))
}

/// Render `dir` (a built airport folder with manifest.json + <layer>.geojson) to `out`.
/// Returns the size of the written file in bytes.
pub fn write(dir: &Path, kind: Preview, out: &Path) -> Result<u64> {
    let manifest_path = dir.join("manifest.json");
    let manifest: Value = serde_json::from_str(&std::fs::read_to_string(&manifest_path).with_context(|| format!("read {} (is {} a built airport?)", manifest_path.display(), dir.display()))?)
        .context("parse manifest.json")?;
    let mut layers = Map::new();
    for name in kind.layers() {
        let Some(text) = crate::output::layers::read(dir, name) else { continue };
        let fc: Value = serde_json::from_str(&text).with_context(|| format!("parse layer {name} of {}", dir.display()))?;
        let keep = kind.keep(name);
        let slim: Vec<Value> = fc
            .get("features")
            .and_then(Value::as_array)
            .map(|fs| {
                fs.iter()
                    .filter_map(|f| {
                        let g = f.get("geometry")?.clone();
                        let props = f.get("properties").and_then(Value::as_object);
                        let mut p = Map::new();
                        if let Some(props) = props {
                            for k in keep {
                                if let Some(v) = props.get(*k) {
                                    if !v.is_null() {
                                        p.insert((*k).to_string(), v.clone());
                                    }
                                }
                            }
                        }
                        Some(json!({"g": g, "p": p}))
                    })
                    .collect()
            })
            .unwrap_or_default();
        layers.insert((*name).to_string(), Value::Array(slim));
    }
    let payload = serde_json::to_string(&json!({"manifest": manifest, "layers": layers}))?;
    let html = kind.template().replacen(DATA_HOOK, &payload, 1);
    if let Some(parent) = out.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(out, &html).with_context(|| format!("write {}", out.display()))?;
    Ok(html.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_have_the_data_hook() {
        assert!(VIEWER_TEMPLATE.contains(DATA_HOOK));
        assert!(CHART_TEMPLATE.contains(DATA_HOOK));
    }

    #[test]
    fn renders_a_minimal_airport() {
        let dir = std::env::temp_dir().join(format!("amdbgen-preview-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.json"), r#"{"icao":"TEST","arp":[0,0],"layers":{}}"#).unwrap();
        std::fs::write(dir.join("runwayelement.geojson"), r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,0]]]},"properties":{"idrwy":"09/27","secret":1}}]}"#).unwrap();
        let out = dir.join("chart.html");
        let n = write(&dir, Preview::Chart, &out).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        assert!(n > 1000);
        assert!(html.contains(r#""idrwy":"09/27""#));
        assert!(!html.contains("secret"));
        assert!(!html.contains(DATA_HOOK));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
