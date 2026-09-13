//! DO-272 code lists used as numeric attribute values, plus a machine-readable legend
//! (`codes.json`) written next to the output so plugin authors can decode them.

use serde_json::{json, Value};

/// `surftype` — surface type.
pub mod surftype {
    pub const UNKNOWN: i64 = 0;
    pub const CONCRETE_GROOVED: i64 = 1;
    pub const CONCRETE: i64 = 2;
    pub const ASPHALT_GROOVED: i64 = 3;
    pub const ASPHALT: i64 = 4;
    pub const SAND_DIRT: i64 = 5;
    pub const BARE_EARTH: i64 = 6;
    pub const SNOW_ICE: i64 = 7;
    pub const WATER: i64 = 8;
    pub const GRASS: i64 = 9;
    pub const AGGREGATE_SEAL: i64 = 10;
    pub const GRAVEL: i64 = 11;
    pub const MACADAM: i64 = 12;
    pub const METAL: i64 = 13;
    pub const MATS: i64 = 14;
    pub const BRICK_PAVERS: i64 = 15;

    /// Map an X-Plane apt.dat surface code to DO-272 `surftype`.
    pub fn from_xplane(code: i64) -> i64 {
        match code {
            1 | 20..=38 => ASPHALT,
            2 | 50..=57 => CONCRETE,
            3 => GRASS,
            4 => BARE_EARTH,
            5 => GRAVEL,
            12 => SAND_DIRT,
            13 => WATER,
            14 => SNOW_ICE,
            _ => UNKNOWN,
        }
    }

    /// Map an OSM `surface=*` value to `surftype`.
    pub fn from_osm(v: &str) -> i64 {
        match v.to_ascii_lowercase().as_str() {
            "asphalt" | "paved" | "tarmac" => ASPHALT,
            "concrete" | "concrete:plates" | "concrete:lanes" => CONCRETE,
            "grass" | "turf" => GRASS,
            "gravel" | "fine_gravel" | "compacted" | "pebblestone" => GRAVEL,
            "dirt" | "earth" | "ground" | "unpaved" | "mud" => BARE_EARTH,
            "sand" => SAND_DIRT,
            "water" => WATER,
            "snow" | "ice" => SNOW_ICE,
            "metal" | "metal_grid" => METAL,
            "paving_stones" | "sett" | "cobblestone" => BRICK_PAVERS,
            _ => UNKNOWN,
        }
    }
}

/// `status` — operational status.
pub mod status {
    pub const UNKNOWN: i64 = 0;
    pub const OPEN: i64 = 1;
    pub const CLOSED: i64 = 2;
    pub const CONSTRUCTION: i64 = 3;
}

/// `rwymktyp` — runway marking type.
pub mod rwymktyp {
    pub const UNKNOWN: i64 = 0;
    pub const NONE: i64 = 1;
    pub const BASIC: i64 = 2;
    pub const NON_PRECISION: i64 = 3;
    pub const PRECISION: i64 = 4;

    pub fn from_xplane(code: i64) -> i64 {
        match code {
            0 => NONE,
            1 => BASIC,
            2 => NON_PRECISION,
            3 => PRECISION,
            4 | 5 | 6 | 7 => BASIC, // UK-style / other: treat as basic set
            _ => UNKNOWN,
        }
    }
}

/// `marktype` — runway marking element type (RunwayMarking layer).
pub mod marktype {
    pub const THRESHOLD: i64 = 1;
    pub const DESIGNATION: i64 = 2;
    pub const CENTERLINE: i64 = 3;
    pub const AIMING_POINT: i64 = 4;
    pub const TOUCHDOWN_ZONE: i64 = 5;
    pub const SIDE_STRIPE: i64 = 6;
    pub const DISPLACED_ARROW: i64 = 7;
    pub const CHEVRON: i64 = 8;
}

/// `catstop` — holding position category.
pub mod catstop {
    pub const UNKNOWN: i64 = 0;
    pub const CAT_I: i64 = 1;
    pub const CAT_II: i64 = 2;
    pub const CAT_III: i64 = 3;
    pub const CAT_II_III: i64 = 4;
    pub const NO_ILS: i64 = 5;
}

/// `feattype` for TaxiwayElement.
pub mod twy_feattype {
    pub const UNKNOWN: i64 = 0;
    pub const PARALLEL: i64 = 1;
    pub const RAPID_EXIT: i64 = 2;
    pub const EXIT: i64 = 3;
    pub const TURNAROUND: i64 = 4;
    pub const APRON_TAXIWAY: i64 = 5;
    pub const STAND_TAXILANE: i64 = 6;
    pub const BRIDGE: i64 = 7;
}

/// `feattype` for ApronElement.
pub mod apron_feattype {
    pub const UNKNOWN: i64 = 0;
    pub const PARKING: i64 = 1;
    pub const CARGO: i64 = 2;
    pub const GA: i64 = 3;
    pub const MAINTENANCE: i64 = 4;
    pub const MILITARY: i64 = 5;
    pub const DEICING: i64 = 6;
    pub const FUEL: i64 = 7;
    pub const HELICOPTER: i64 = 8;
}

/// `nodetype` for AsrnNode.
pub mod nodetype {
    pub const UNKNOWN: i64 = 0;
    pub const TAXIWAY: i64 = 1;
    pub const RUNWAY: i64 = 2;
    pub const STAND: i64 = 3;
    pub const PARKING: i64 = 4;
    pub const HOLDING_POSITION: i64 = 5;
    pub const RUNWAY_EXIT: i64 = 6;
    pub const DEICING: i64 = 7;
    pub const HELIPAD: i64 = 8;
}

/// `edgetype` for AsrnEdge.
pub mod edgetype {
    pub const UNKNOWN: i64 = 0;
    pub const TAXIWAY: i64 = 1;
    pub const RUNWAY: i64 = 2;
    pub const STAND: i64 = 3;
    pub const PARKING: i64 = 4;
    pub const RUNWAY_EXIT: i64 = 5;
    pub const DEICING: i64 = 6;
}

/// `direc` — direction of use.
pub mod direc {
    pub const UNKNOWN: i64 = 0;
    pub const BIDIRECTIONAL: i64 = 1;
    pub const FORWARD: i64 = 2; // start -> end only
    pub const BACKWARD: i64 = 3;
}

/// `signtype` for AerodromeSign.
pub mod signtype {
    pub const UNKNOWN: i64 = 0;
    pub const MANDATORY: i64 = 1;
    pub const INFORMATION_DIRECTION: i64 = 2;
    pub const LOCATION: i64 = 3;
    pub const DISTANCE_REMAINING: i64 = 4;
    pub const DESTINATION: i64 = 5;
}

/// `lstype` for AerodromeSurfaceLighting.
pub mod lighting {
    pub const UNKNOWN: i64 = 0;
    pub const PAPI: i64 = 1;
    pub const VASI: i64 = 2;
    pub const APAPI: i64 = 3;
    pub const RUNWAY_GUARD: i64 = 4;
    pub const BEACON: i64 = 5;
    pub const WINDSOCK: i64 = 6;
    pub const REIL: i64 = 7;
    pub const APPROACH: i64 = 8;
}

/// `plysttyp` — polygonal structure type.
pub mod plysttyp {
    pub const UNKNOWN: i64 = 0;
    pub const TERMINAL: i64 = 1;
    pub const HANGAR: i64 = 2;
    pub const CONTROL_TOWER: i64 = 3;
    pub const BUILDING: i64 = 4;
    pub const FUEL_FARM: i64 = 5;
    pub const PARKING_GARAGE: i64 = 6;
    pub const INDUSTRIAL: i64 = 7;
}

/// `pntsttyp` — point structure type.
pub mod pntsttyp {
    pub const UNKNOWN: i64 = 0;
    pub const TOWER: i64 = 1;
    pub const MAST: i64 = 2;
    pub const CHIMNEY: i64 = 3;
    pub const TREE: i64 = 4;
    pub const POLE: i64 = 5;
    pub const WINDSOCK: i64 = 6;
    pub const ANTENNA: i64 = 7;
    pub const NAVAID: i64 = 8;
    pub const TANK: i64 = 9;
}

/// `linsttyp` — line structure type.
pub mod linsttyp {
    pub const UNKNOWN: i64 = 0;
    pub const FENCE: i64 = 1;
    pub const WALL: i64 = 2;
    pub const POWER_LINE: i64 = 3;
    pub const CABLE: i64 = 4;
    pub const HEDGE: i64 = 5;
}

/// `thrtype` — threshold type.
pub mod thrtype {
    pub const THRESHOLD: i64 = 1;
    pub const DISPLACED: i64 = 2;
    pub const END: i64 = 3;
}

/// `station` types for FrequencyArea.
pub mod station {
    pub const RECORDED: i64 = 1;
    pub const UNICOM: i64 = 2;
    pub const CLEARANCE: i64 = 3;
    pub const GROUND: i64 = 4;
    pub const TOWER: i64 = 5;
    pub const APPROACH: i64 = 6;
    pub const DEPARTURE: i64 = 7;
}

/// `source` — provenance of a feature.
pub mod source {
    pub const XPLANE: &str = "xplane";
    pub const OSM: &str = "osm";
    pub const OURAIRPORTS: &str = "ourairports";
    pub const APTMETA: &str = "aptmeta";
    pub const FAA_NASR: &str = "faa_nasr";
    /// FAA open airport-mapping layers (hotspots and DO-272 pavement, US only).
    pub const FAA_AMDB: &str = "faa_amdb";
    pub const DERIVED: &str = "derived";
    pub const OVERRIDE: &str = "override";
}

/// Legend written as `codes.json` at the output root.
pub fn legend() -> Value {
    json!({
        "surftype": {"0":"unknown","1":"concrete grooved","2":"concrete","3":"asphalt grooved","4":"asphalt","5":"sand/dirt","6":"bare earth","7":"snow/ice","8":"water","9":"grass","10":"aggregate friction seal coat","11":"gravel","12":"macadam","13":"metal","14":"mats","15":"brick/pavers"},
        "status": {"0":"unknown","1":"open","2":"closed","3":"under construction"},
        "rwymktyp": {"0":"unknown","1":"none","2":"basic","3":"non-precision","4":"precision"},
        "marktype": {"1":"threshold bar","2":"designation","3":"centerline stripe","4":"aiming point","5":"touchdown zone","6":"side stripe","7":"displaced threshold arrow","8":"chevron"},
        "catstop": {"0":"unknown","1":"CAT I","2":"CAT II","3":"CAT III","4":"CAT II/III","5":"no ILS"},
        "feattype": {
            "taxiwayelement": {"0":"unknown","1":"parallel","2":"rapid exit","3":"exit","4":"turnaround","5":"apron taxiway","6":"stand taxilane","7":"bridge"},
            "apronelement": {"0":"unknown","1":"parking","2":"cargo","3":"general aviation","4":"maintenance","5":"military","6":"deicing","7":"fuel","8":"helicopter"}
        },
        "nodetype": {"0":"unknown","1":"taxiway","2":"runway","3":"stand","4":"parking","5":"holding position","6":"runway exit","7":"deicing","8":"helipad"},
        "edgetype": {"0":"unknown","1":"taxiway","2":"runway","3":"stand","4":"parking","5":"runway exit","6":"deicing"},
        "direc": {"0":"unknown","1":"bidirectional","2":"forward (start to end)","3":"backward"},
        "signtype": {"0":"unknown","1":"mandatory","2":"information/direction","3":"location","4":"distance remaining","5":"destination"},
        "lstype": {"0":"unknown","1":"PAPI","2":"VASI","3":"APAPI","4":"runway guard","5":"beacon","6":"windsock","7":"REIL","8":"approach"},
        "plysttyp": {"0":"unknown","1":"terminal","2":"hangar","3":"control tower","4":"building","5":"fuel farm","6":"parking garage","7":"industrial"},
        "pntsttyp": {"0":"unknown","1":"tower","2":"mast","3":"chimney","4":"tree","5":"pole","6":"windsock","7":"antenna","8":"navaid","9":"tank"},
        "linsttyp": {"0":"unknown","1":"fence","2":"wall","3":"power line","4":"cable","5":"hedge"},
        "thrtype": {"1":"threshold","2":"displaced threshold","3":"runway end"},
        "station": {"1":"recorded (ATIS/AWOS)","2":"unicom/CTAF","3":"clearance delivery","4":"ground","5":"tower","6":"approach","7":"departure"},
        "source": ["xplane","osm","ourairports","aptmeta","faa_nasr","derived","override"]
    })
}
