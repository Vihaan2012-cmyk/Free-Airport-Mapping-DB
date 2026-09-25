//! Rewrites amdbgen's DO-272 output into the exact Navigraph AMDB schema, as defined
//! by Navigraph's own SDK (`@navigraph/amdb` types) and their `amdb-geo` library, so
//! every aircraft built on that API sees identical data.
//!
//! Per layer the property set is exactly the SDK's: `id` (number, unique per layer),
//! `feattype` (ER-009 number), `idarpt`, then the layer attributes with Navigraph's
//! enum numbering. Unknown values use the sentinel `-32767`, not-applicable `-32765`.
//! Geometry-derived members (`centroid`, `midpoint`, `longest_segment`) are added by
//! the server in the requested projection.

use crate::model::codes;
use crate::model::{AmdbFeature, Layer};
use serde_json::{json, Map, Value};

pub const UNKNOWN: i64 = -32767;
pub const NOT_APPLICABLE: i64 = -32765;

/// ER-009 feature-class numbers used by Navigraph (`FeatureType` in the SDK).
pub fn feattype(layer: Layer) -> Option<i64> {
    Some(match layer {
        Layer::RunwayElement => 0,
        Layer::RunwayIntersection => 1,
        Layer::RunwayThreshold => 2,
        Layer::RunwayMarking => 3,
        Layer::PaintedCenterline => 4,
        Layer::LandAndHoldShortOperationLocation => 5,
        Layer::ArrestingGearLocation => 6,
        Layer::RunwayShoulder => 7,
        Layer::Stopway => 8,
        Layer::RunwayDisplacedArea => 9,
        Layer::FinalApproachAndTakeOffArea => 11,
        Layer::TouchDownLiftOffArea => 12,
        Layer::HelipadThreshold => 13,
        Layer::TaxiwayElement => 14,
        Layer::TaxiwayShoulder => 15,
        Layer::TaxiwayGuidanceLine => 16,
        Layer::TaxiwayIntersectionMarking => 17,
        Layer::TaxiwayHoldingPosition => 18,
        Layer::RunwayExitLine => 19,
        Layer::FrequencyArea => 20,
        Layer::ApronElement => 21,
        Layer::StandGuidanceLine => 22,
        Layer::ParkingStandLocation => 23,
        Layer::ParkingStandArea => 24,
        Layer::DeicingArea => 25,
        Layer::AerodromeReferencePoint => 26,
        Layer::VerticalPolygonalStructure => 27,
        Layer::VerticalPointStructure => 28,
        Layer::VerticalLineStructure => 29,
        Layer::ConstructionArea => 30,
        Layer::Blastpad => 33,
        Layer::ServiceRoad => 34,
        Layer::Water => 35,
        Layer::Hotspot => 37,
        Layer::AsrnEdge => 39,
        Layer::AsrnNode => 40,
        _ => return None, // not part of the Navigraph AMDB API
    })
}

/// The 36 layers Navigraph serves, in their order.
pub const NAVIGRAPH_LAYERS: &[Layer] = &[
    Layer::AerodromeReferencePoint,
    Layer::ApronElement,
    Layer::ArrestingGearLocation,
    Layer::AsrnEdge,
    Layer::AsrnNode,
    Layer::Blastpad,
    Layer::ConstructionArea,
    Layer::DeicingArea,
    Layer::FinalApproachAndTakeOffArea,
    Layer::FrequencyArea,
    Layer::HelipadThreshold,
    Layer::Hotspot,
    Layer::LandAndHoldShortOperationLocation,
    Layer::PaintedCenterline,
    Layer::ParkingStandArea,
    Layer::ParkingStandLocation,
    Layer::RunwayDisplacedArea,
    Layer::RunwayElement,
    Layer::RunwayExitLine,
    Layer::RunwayIntersection,
    Layer::RunwayMarking,
    Layer::RunwayShoulder,
    Layer::RunwayThreshold,
    Layer::ServiceRoad,
    Layer::StandGuidanceLine,
    Layer::Stopway,
    Layer::TaxiwayElement,
    Layer::TaxiwayGuidanceLine,
    Layer::TaxiwayHoldingPosition,
    Layer::TaxiwayIntersectionMarking,
    Layer::TaxiwayShoulder,
    Layer::TouchDownLiftOffArea,
    Layer::VerticalLineStructure,
    Layer::VerticalPointStructure,
    Layer::VerticalPolygonalStructure,
    Layer::Water,
];

/// Layer name as the API spells it. The SDK response type uses `touchdownliftoffarea`;
/// the FlyByWire client requests `touchdownliftofarea`. Both are accepted on input; the
/// response echoes whichever spelling was requested (see `layer_from_client_name`).
pub fn layer_from_client_name(name: &str) -> Option<Layer> {
    let n = name.trim().to_ascii_lowercase();
    if n == "touchdownliftofarea" {
        return Some(Layer::TouchDownLiftOffArea);
    }
    Layer::from_name(&n)
}

fn pad_designator(s: &str) -> String {
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    let suffix: String = s.chars().skip_while(|c| c.is_ascii_digit()).collect();
    if digits.len() == 1 {
        format!("0{digits}{suffix}")
    } else {
        s.to_string()
    }
}

/// `07L/25R` -> `07L.25R`; intersections `07L/25R+18/36` -> `07L.25R_18.36`.
pub fn client_idrwy(s: &str) -> String {
    s.split('+').map(|pair| pair.split('/').map(pad_designator).collect::<Vec<_>>().join(".")).collect::<Vec<_>>().join("_")
}

/// The two ends of a runway identifier: `09L/27R` -> `["09L", "27R"]`, each zero-padded
/// the way the API spells thresholds. An intersection (`09L/27R+18/36`) gives the ends of
/// the first runway named, which is the one a node on it belongs to.
fn runway_ends(idrwy: &str) -> Vec<String> {
    idrwy.split('+').next().unwrap_or("").split(['/', '.']).map(str::trim).filter(|e| !e.is_empty()).map(pad_designator).collect()
}

/// Where each runway threshold is, so a node on a runway can name the one it is nearest.
///
/// The SDK declares `idthr` on a runway node as a string, and a null fails validation in
/// newer clients, which takes the whole routing network down with it. Filling it in is
/// therefore necessary -- but the value has to be right as well as present. The field
/// names the threshold the node is measured from, and a runway exit is only useful to the
/// landing that uses it: an exit two thirds of the way down 09L/27R belongs to 27R, and
/// naming 09L there is a number that passes validation and misleads. Taking the first end
/// of the pair would be the further threshold for well over half of them -- sixteen of
/// Heathrow's twenty-eight, thirty-six of Kennedy's sixty-one -- so the node's own
/// position decides it.
#[derive(Default)]
pub struct Thresholds(Vec<(String, geo_types::Coord<f64>)>);

impl Thresholds {
    /// Read them from a built airport, in the same local metre frame the features are put
    /// into, so a distance is a distance.
    pub fn read(dir: &std::path::Path, frame: &crate::geom::LocalFrame, is_wgs84: bool) -> Thresholds {
        let p = dir.join(format!("{}.geojson", Layer::RunwayThreshold.name()));
        let Ok(text) = std::fs::read_to_string(&p) else { return Thresholds::default() };
        let Ok((_, feats)) = crate::output::geojson::parse_feature_collection(&text, Layer::RunwayThreshold) else {
            return Thresholds::default();
        };
        let mut out = Vec::new();
        for f in feats {
            let Some(id) = s(&f.props, "idthr") else { continue };
            let geo_types::Geometry::Point(pt) = &f.geom else { continue };
            let c = if is_wgs84 { frame.forward(pt.0.x, pt.0.y) } else { pt.0 };
            out.push((pad_designator(&id), c));
        }
        Thresholds(out)
    }

    /// Which of `among` is nearest to `at`. None when none of them is known here, which
    /// leaves the caller to fall back rather than invent a threshold.
    fn nearest(&self, at: geo_types::Coord<f64>, among: &[String]) -> Option<String> {
        among
            .iter()
            .filter_map(|e| self.0.iter().find(|(id, _)| id == e).map(|(id, c)| (id.clone(), (c.x - at.x).hypot(c.y - at.y))))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(id, _)| id)
    }
}

// ---- enum translators (amdbgen code -> Navigraph code) ------------------------------

fn surftype(v: Option<i64>) -> i64 {
    match v.unwrap_or(0) {
        c @ 1..=11 => c,
        12 => 17, // macadam
        13 => 13, // metal -> pierced steel planks
        14 => 22, // mats -> landing mats
        15 => 16, // brick
        _ => UNKNOWN,
    }
}

fn gsurftyp(v: Option<i64>) -> i64 {
    match v.unwrap_or(0) {
        1 | 2 => 1,
        3 | 4 => 2,
        5 => 3,
        6 => 4,
        7 => 5,
        8 => 6,
        9 => 7,
        11 => 8,
        13 => 9,
        15 => 11,
        12 => 12,
        14 => 17,
        _ => UNKNOWN,
    }
}

fn status(v: Option<i64>) -> i64 {
    match v.unwrap_or(codes::status::OPEN) {
        codes::status::CLOSED | codes::status::CONSTRUCTION => 0,
        _ => 1,
    }
}

fn catstop(v: Option<i64>) -> i64 {
    match v.unwrap_or(0) {
        codes::catstop::CAT_I => 1,
        codes::catstop::CAT_II | codes::catstop::CAT_III | codes::catstop::CAT_II_III => 2,
        codes::catstop::NO_ILS => 0,
        _ => UNKNOWN,
    }
}

fn direc(v: Option<i64>) -> i64 {
    match v.unwrap_or(0) {
        codes::direc::BIDIRECTIONAL => 0,
        codes::direc::FORWARD => 1,
        codes::direc::BACKWARD => 2,
        _ => UNKNOWN,
    }
}

fn style(v: Option<i64>) -> i64 {
    match v.unwrap_or(1) {
        2 => 1,
        3 => 2,
        _ => 0,
    }
}

fn vasis(v: Option<i64>) -> i64 {
    match v {
        Some(codes::lighting::PAPI) => 1,
        Some(codes::lighting::APAPI) => 2,
        Some(codes::lighting::VASI) => 3,
        _ => 0,
    }
}

fn plysttyp(v: Option<i64>) -> i64 {
    match v.unwrap_or(4) {
        1 => 1,
        2 => 2,
        3 => 3,
        5 => 5,
        _ => 4,
    }
}

fn pntsttyp(v: Option<i64>) -> i64 {
    match v.unwrap_or(0) {
        codes::pntsttyp::TOWER | codes::pntsttyp::MAST | codes::pntsttyp::ANTENNA => 3,
        codes::pntsttyp::CHIMNEY => 1,
        codes::pntsttyp::TREE => 5,
        codes::pntsttyp::POLE => 6,
        codes::pntsttyp::WINDSOCK => 4,
        codes::pntsttyp::NAVAID => 9,
        _ => UNKNOWN,
    }
}

fn linsttyp(v: Option<i64>) -> i64 {
    match v.unwrap_or(0) {
        codes::linsttyp::POWER_LINE => 1,
        codes::linsttyp::CABLE => 2,
        codes::linsttyp::HEDGE => 3,
        codes::linsttyp::WALL => 4,
        _ => UNKNOWN,
    }
}

fn nodetype(v: Option<i64>) -> i64 {
    match v.unwrap_or(0) {
        codes::nodetype::TAXIWAY => 0,
        codes::nodetype::HOLDING_POSITION => 1,
        codes::nodetype::RUNWAY | codes::nodetype::RUNWAY_EXIT => 3,
        codes::nodetype::PARKING => 7,
        codes::nodetype::STAND => 9,
        codes::nodetype::DEICING => 8,
        _ => 0,
    }
}

fn edgetype(v: Option<i64>) -> i64 {
    match v.unwrap_or(0) {
        codes::edgetype::TAXIWAY => 0,
        codes::edgetype::RUNWAY => 1,
        codes::edgetype::RUNWAY_EXIT => 2,
        codes::edgetype::PARKING => 3,
        codes::edgetype::DEICING => 5,
        codes::edgetype::STAND => 7,
        _ => 0,
    }
}

fn availability(v: Option<bool>) -> i64 {
    match v {
        Some(true) => 1,
        Some(false) => 0,
        None => UNKNOWN,
    }
}

// ---- helpers --------------------------------------------------------------------------

fn s(p: &Map<String, Value>, k: &str) -> Option<String> {
    p.get(k).and_then(Value::as_str).map(str::to_string)
}

fn i(p: &Map<String, Value>, k: &str) -> Option<i64> {
    p.get(k).and_then(Value::as_i64)
}

fn f(p: &Map<String, Value>, k: &str) -> Option<f64> {
    p.get(k).and_then(Value::as_f64)
}

fn b(p: &Map<String, Value>, k: &str) -> Option<bool> {
    p.get(k).and_then(Value::as_bool)
}

fn opt_str(v: Option<String>) -> Value {
    v.map(Value::from).unwrap_or(Value::Null)
}

fn num_or(v: Option<f64>, default: i64) -> Value {
    v.map(Value::from).unwrap_or(Value::from(default))
}

fn ft_to_m(v: Option<f64>) -> Value {
    v.map(|x| Value::from((x * 0.3048 * 100.0).round() / 100.0)).unwrap_or(Value::from(UNKNOWN))
}

fn idrwy(p: &Map<String, Value>) -> Value {
    opt_str(s(p, "idrwy").map(|v| client_idrwy(&v)))
}

fn idthr(p: &Map<String, Value>) -> Value {
    opt_str(s(p, "idthr").map(|v| pad_designator(&v)))
}

/// Convert one feature's properties in place to the Navigraph schema. `seq` is the
/// running number within the layer. Returns false for layers Navigraph does not serve.
pub fn convert(feat: &mut AmdbFeature, seq: usize, thresholds: &Thresholds) -> bool {
    let layer = feat.layer;
    let Some(ft) = feattype(layer) else { return false };
    let here = match &feat.geom {
        geo_types::Geometry::Point(pt) => Some(pt.0),
        _ => None,
    };
    let p = std::mem::take(&mut feat.props);
    let idarpt = s(&p, "idarpt").unwrap_or_default();
    let mut o = Map::new();
    o.insert("id".into(), Value::from(ft as u64 * 1_000_000 + seq as u64 + 1));
    o.insert("feattype".into(), Value::from(ft));
    o.insert("idarpt".into(), Value::from(idarpt));
    let mut put = |k: &str, v: Value| {
        o.insert(k.to_string(), v);
    };
    match layer {
        Layer::AerodromeReferencePoint => {
            put("name", opt_str(s(&p, "name")));
            put("iata", opt_str(s(&p, "iata")));
            put("elev", ft_to_m(f(&p, "elev")));
        }
        Layer::RunwayElement => {
            put("idrwy", idrwy(&p));
            put("pcn", Value::Null);
            put("restacn", Value::Null);
            put("width", num_or(f(&p, "width"), UNKNOWN));
            put("length", num_or(f(&p, "length"), UNKNOWN));
            put("surftype", Value::from(surftype(i(&p, "surftype"))));
        }
        Layer::RunwayIntersection => {
            put("idrwi", opt_str(s(&p, "idrwi").map(|v| client_idrwy(&v))));
            put("pcn", Value::Null);
            put("restacn", Value::Null);
            put("surftype", Value::from(surftype(i(&p, "surftype"))));
        }
        Layer::RunwayThreshold => {
            put("idthr", idthr(&p));
            put("status", Value::from(status(i(&p, "status"))));
            put("tdze", ft_to_m(f(&p, "tdze")));
            put("tdzslope", Value::from(0.0));
            put("brngtrue", num_or(f(&p, "brngtrue"), UNKNOWN));
            put("brngmag", num_or(f(&p, "brngmag").or(f(&p, "brngtrue")), UNKNOWN));
            put("rwyslope", Value::from(0.0));
            put("tora", num_or(f(&p, "tora"), UNKNOWN));
            put("toda", num_or(f(&p, "toda"), UNKNOWN));
            put("asda", num_or(f(&p, "asda"), UNKNOWN));
            put("lda", num_or(f(&p, "lda"), UNKNOWN));
            put("vasis", Value::from(vasis(i(&p, "vasis"))));
            put("cat", Value::from(UNKNOWN));
            put("ellipse", Value::from(UNKNOWN));
            put("geound", Value::from(UNKNOWN));
            put("thrtype", Value::from(if i(&p, "thrtype") == Some(codes::thrtype::DISPLACED) { 1 } else { 0 }));
        }
        Layer::RunwayMarking => put("idrwy", idrwy(&p)),
        Layer::PaintedCenterline => put("idrwy", idrwy(&p)),
        Layer::LandAndHoldShortOperationLocation => {
            put("idthr", idthr(&p));
            put("idp", opt_str(s(&p, "idcross").map(|v| client_idrwy(&v))));
        }
        Layer::ArrestingGearLocation => {
            put("idthr", idthr(&p));
            put("status", Value::from(1));
        }
        Layer::RunwayShoulder => {
            put("idrwy", idrwy(&p));
            put("status", Value::from(1));
            put("gsurftyp", Value::from(gsurftyp(i(&p, "surftype"))));
        }
        Layer::Stopway => {
            put("idthr", idthr(&p));
            put("status", Value::from(1));
            put("surftype", Value::from(surftype(i(&p, "surftype"))));
        }
        Layer::RunwayDisplacedArea => {
            put("idthr", idthr(&p));
            put("status", Value::from(1));
            put("pcn", Value::Null);
            put("restacn", Value::Null);
            put("surftype", Value::from(surftype(i(&p, "surftype"))));
        }
        Layer::Blastpad => put("idthr", idthr(&p)),
        Layer::FinalApproachAndTakeOffArea | Layer::TouchDownLiftOffArea => {
            put("idrwy", opt_str(s(&p, "ident")));
            put("surftype", Value::from(surftype(i(&p, "surftype"))));
        }
        Layer::HelipadThreshold => {
            put("idthr", opt_str(s(&p, "ident")));
            put("status", Value::from(1));
            put("ellipse", Value::from(UNKNOWN));
            put("geound", Value::from(UNKNOWN));
        }
        Layer::TaxiwayElement => {
            put("idlin", opt_str(s(&p, "idlin")));
            put("idapron", Value::Null);
            put("gsurftyp", Value::from(gsurftyp(i(&p, "surftype"))));
            put("bridge", Value::from(if b(&p, "bridge") == Some(true) { 2 } else { 0 }));
            put("pcn", Value::Null);
            put("restacn", Value::Null);
            put("status", Value::from(status(i(&p, "status"))));
        }
        Layer::TaxiwayShoulder => {
            put("status", Value::from(1));
            put("gsurftyp", Value::from(gsurftyp(i(&p, "surftype"))));
        }
        Layer::TaxiwayGuidanceLine => {
            put("idlin", opt_str(s(&p, "idlin")));
            put("status", Value::from(1));
            put("wingspan", num_or(f(&p, "wingspan"), UNKNOWN));
            put("maxspeed", Value::from(UNKNOWN));
            put("color", Value::from(0));
            put("style", Value::from(style(i(&p, "style"))));
            put("direc", Value::from(direc(i(&p, "direc"))));
        }
        Layer::TaxiwayIntersectionMarking => put("idlin", opt_str(s(&p, "idlin"))),
        Layer::TaxiwayHoldingPosition => {
            put("idlin", opt_str(s(&p, "idlin")));
            put("catstop", Value::from(catstop(i(&p, "catstop"))));
            put("status", Value::from(1));
            put("idp", idrwy(&p));
        }
        Layer::RunwayExitLine => {
            put("idlin", opt_str(s(&p, "idlin")));
            put("status", Value::from(1));
            put("color", Value::from(0));
            put("style", Value::from(0));
            put("direc", Value::from(0));
        }
        Layer::FrequencyArea => {
            put("frq", num_or(f(&p, "frq"), UNKNOWN));
            put("station", opt_str(s(&p, "name").filter(|n| !n.is_empty())));
        }
        Layer::ApronElement => {
            put("status", Value::from(status(i(&p, "status"))));
            put("pcn", Value::Null);
            put("restacn", Value::Null);
            put("gsurftyp", Value::from(gsurftyp(i(&p, "surftype"))));
            put("idapron", opt_str(s(&p, "idapron")));
        }
        Layer::StandGuidanceLine => {
            put("idstd", opt_str(s(&p, "idstd")));
            put("color", Value::from(0));
            put("style", Value::from(style(i(&p, "style"))));
            put("direc", Value::from(0));
            put("wingspan", Value::from(UNKNOWN));
            put("status", Value::from(1));
            put("termref", Value::Null);
        }
        Layer::ParkingStandLocation => {
            put("idstd", opt_str(s(&p, "idstd")));
            put("acn", opt_str(s(&p, "acft").map(|a| a.replace('|', "."))));
            put("termref", opt_str(s(&p, "idapron")));
        }
        Layer::ParkingStandArea => {
            put("idstd", opt_str(s(&p, "idstd")));
            put("pcn", Value::Null);
            put("restacn", Value::Null);
            put("gsurftyp", Value::from(gsurftyp(i(&p, "surftype"))));
            put("jetway", Value::from(availability(b(&p, "jetway"))));
            put("fuel", Value::Null);
            put("towing", Value::from(UNKNOWN));
            put("gndpower", Value::from(UNKNOWN));
            put("idapron", opt_str(s(&p, "idapron")));
            put("termref", opt_str(s(&p, "idapron")));
        }
        Layer::DeicingArea => {
            put("status", Value::from(1));
            put("restacn", Value::Null);
            put("gsurftyp", Value::from(gsurftyp(i(&p, "surftype"))));
            put("idbase", opt_str(s(&p, "idapron")));
            put("ident", opt_str(s(&p, "idapron").or_else(|| s(&p, "deicegrp"))));
        }
        Layer::ServiceRoad => {
            put("gsurftyp", Value::from(gsurftyp(i(&p, "surftype"))));
            put("featbase", Value::from(0));
            put("idbase", Value::Null);
        }
        Layer::ConstructionArea => {
            put("pstdate", Value::from("0001-00-00"));
            put("pendate", Value::from("0001-00-00"));
            put("piocdate", Value::from("0001-00-00"));
        }
        Layer::Water => {}
        Layer::Hotspot => put("idhot", opt_str(s(&p, "idhot").or_else(|| s(&p, "name")))),
        Layer::VerticalPolygonalStructure => {
            put("plysttyp", Value::from(plysttyp(i(&p, "plysttyp"))));
            put("height", num_or(f(&p, "height"), UNKNOWN));
            put("elev", Value::from(UNKNOWN));
            put("material", Value::from(UNKNOWN));
            put("ident", opt_str(s(&p, "name")));
        }
        Layer::VerticalPointStructure => {
            put("pntsttyp", Value::from(pntsttyp(i(&p, "pntsttyp"))));
            put("marking", Value::from(UNKNOWN));
            put("lighting", Value::from(UNKNOWN));
            put("radius", Value::from(NOT_APPLICABLE));
            put("height", num_or(f(&p, "height"), UNKNOWN));
            put("elev", Value::from(UNKNOWN));
            put("material", Value::from(UNKNOWN));
        }
        Layer::VerticalLineStructure => {
            put("linsttyp", Value::from(linsttyp(i(&p, "linsttyp"))));
            put("marking", Value::from(UNKNOWN));
            put("lighting", Value::from(UNKNOWN));
            put("radius", Value::from(NOT_APPLICABLE));
            put("height", num_or(f(&p, "height"), UNKNOWN));
            put("elev", Value::from(UNKNOWN));
            put("material", Value::from(UNKNOWN));
        }
        Layer::AsrnEdge => {
            put("edgetype", Value::from(edgetype(i(&p, "edgetype"))));
            put("idnetwrk", opt_str(s(&p, "idlin").or_else(|| s(&p, "idrwy").map(|v| client_idrwy(&v))).or_else(|| s(&p, "idstd"))));
            put("node1ref", Value::from(i(&p, "stnode").map(|n| 40_000_000 + n + 1).unwrap_or(UNKNOWN)));
            put("node2ref", Value::from(i(&p, "ennode").map(|n| 40_000_000 + n + 1).unwrap_or(UNKNOWN)));
            put("direc", Value::from(direc(i(&p, "direc"))));
            put("edgederv", Value::from(0));
            put("edgelen", num_or(f(&p, "edgelen"), UNKNOWN));
            put("curvatur", Value::from(0.0));
            put("idapron", Value::Null);
            put("pcn", Value::Null);
            put("wingspan", num_or(f(&p, "wingspan"), UNKNOWN));
            put("restacn", Value::Null);
            put("idbase", Value::Null);
        }
        Layer::AsrnNode => {
            let nt = nodetype(i(&p, "nodetype"));
            put("nodetype", Value::from(nt));
            put("idnetwrk", opt_str(s(&p, "idstd").or_else(|| s(&p, "idrwy").map(|v| client_idrwy(&v))).or_else(|| s(&p, "name"))));
            match nt {
                1 => {
                    put("catstop", Value::from(UNKNOWN));
                    put("featref", Value::from(UNKNOWN));
                }
                // A runway or runway-exit node names the threshold it is measured
                // from. The SDK wants a string here; see [`Thresholds`] for why it has
                // to be the near one rather than simply the first.
                3 => {
                    let ends = s(&p, "idrwy").map(|r| runway_ends(&r)).unwrap_or_default();
                    let near = here.and_then(|at| thresholds.nearest(at, &ends));
                    put("idthr", Value::from(near.or_else(|| ends.first().cloned()).unwrap_or_default()));
                }
                5 | 9 => put("termref", Value::Null),
                _ => {}
            }
            // Keep the node id addressable by edges: ids are seq-based like everything else,
            // and edges use the same numbering (40_000_000 + index + 1).
            if let Some(n) = i(&p, "nodeid") {
                o.insert("id".into(), Value::from(40_000_000 + n + 1));
            }
        }
        _ => return false,
    }
    feat.props = o;
    true
}

/// Search-result row, as `AmdbSearchResponse`.
pub fn search_row(idarpt: &str, iata: Option<&str>, name: &str, lat: f64, lon: f64, elev_ft: Option<f64>) -> Value {
    json!({
        "idarpt": idarpt,
        "iata": iata,
        "elev": elev_ft.map(|e| (e * 0.3048 * 100.0).round() / 100.0).unwrap_or(0.0),
        "name": name,
        "coordinates": {"lat": lat, "lon": lon},
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `idthr` names the near threshold, against the real built airports.
    ///
    /// The value has to be present -- a null takes the whole routing network down in
    /// newer clients -- but it also has to be right: it names the threshold the node is
    /// measured from, and a runway exit is only useful to the landing that uses it.
    /// Taking the first end of the pair would name the further threshold for sixteen of
    /// Heathrow's twenty-eight exit nodes and thirty-six of Kennedy's sixty-one, so this
    /// checks the answer against the distance rather than against itself.
    ///
    /// `cargo test --release -- --ignored idthr_names_the_near_threshold`
    #[test]
    #[ignore]
    fn idthr_names_the_near_threshold() {
        use crate::model::Layer;
        for icao in ["EGLL", "WSSS", "KJFK"] {
            let dir = std::path::PathBuf::from("out").join(icao);
            if !dir.join("manifest.json").is_file() {
                continue;
            }
            let manifest: crate::output::manifest::Manifest = serde_json::from_str(&std::fs::read_to_string(dir.join("manifest.json")).unwrap()).unwrap();
            let frame = crate::geom::LocalFrame::new(manifest.arp[0], manifest.arp[1]);
            let is_wgs84 = manifest.projection.starts_with("EPSG");
            let thresholds = Thresholds::read(&dir, &frame, is_wgs84);
            assert!(!thresholds.0.is_empty(), "{icao} has no thresholds to be near");

            let text = std::fs::read_to_string(dir.join(format!("{}.geojson", Layer::AsrnNode.name()))).unwrap();
            let (_, feats) = crate::output::geojson::parse_feature_collection(&text, Layer::AsrnNode).unwrap();
            let (mut checked, mut differs) = (0, 0);
            for (i, mut f) in feats.into_iter().enumerate() {
                if is_wgs84 {
                    f.geom = crate::bridge::store::project_to_local(&frame, &f.geom);
                }
                let raw = f.props.clone();
                let at = match &f.geom {
                    geo_types::Geometry::Point(pt) => pt.0,
                    _ => continue,
                };
                assert!(convert(&mut f, i, &thresholds));
                if f.props["nodetype"] != 3 {
                    continue;
                }
                let got = f.props["idthr"].as_str().expect("idthr is a string, never null");
                assert!(!got.is_empty(), "{icao}: a runway node with no threshold at all");
                let ends = runway_ends(&s(&raw, "idrwy").unwrap_or_default());
                // Whatever it named must be the nearest of that runway's ends.
                let dist = |e: &String| thresholds.0.iter().find(|(id, _)| id == e).map(|(_, c)| (c.x - at.x).hypot(c.y - at.y));
                if let Some(best) = ends.iter().filter_map(dist).min_by(|a, b| a.total_cmp(b)) {
                    let named = dist(&got.to_string()).expect("the threshold it named is one we know");
                    assert!((named - best).abs() < 1e-6, "{icao}: named {got} at {named:.0} m when {best:.0} m was nearer");
                    checked += 1;
                    if ends.first().map(|e| e != got).unwrap_or(false) {
                        differs += 1;
                    }
                }
            }
            assert!(checked > 0, "{icao}: no runway nodes to check");
            println!("{icao}: {checked} runway nodes, {differs} of them would have been wrong by taking the first end");
        }
    }
    use geo_types::Point;

    #[test]
    fn maps_to_navigraph_schema() {
        assert_eq!(client_idrwy("7/25"), "07.25");
        assert_eq!(client_idrwy("07L/25R+18/36"), "07L.25R_18.36");
        let mut t = AmdbFeature::new(Layer::RunwayThreshold, Point::new(0.0, 0.0)).with("idarpt", "KJFK").with("idthr", "4L").with("tora", 3000.0).with("tdze", 13.0).with("thrtype", 2).with("vasis", 1);
        assert!(convert(&mut t, 0, &Thresholds::default()));
        assert_eq!(t.props["feattype"], 2);
        assert_eq!(t.props["id"], 2_000_001);
        assert_eq!(t.props["idthr"], "04L");
        assert_eq!(t.props["thrtype"], 1);
        assert_eq!(t.props["vasis"], 1);
        assert_eq!(t.props["tdze"], 3.96);
        assert_eq!(t.props["cat"], UNKNOWN);
        assert!(t.props.get("source").is_none());
        let mut e = AmdbFeature::new(Layer::TaxiwayElement, Point::new(0.0, 0.0)).with("idarpt", "KJFK").with("idlin", "A").with("surftype", 4).with("bridge", true).with("status", 2);
        assert!(convert(&mut e, 5, &Thresholds::default()));
        assert_eq!(e.props["gsurftyp"], 2);
        assert_eq!(e.props["bridge"], 2);
        assert_eq!(e.props["status"], 0);
        let mut sign = AmdbFeature::new(Layer::AerodromeSign, Point::new(0.0, 0.0));
        assert!(!convert(&mut sign, 0, &Thresholds::default()));
        assert_eq!(NAVIGRAPH_LAYERS.len(), 36);
    }
}
