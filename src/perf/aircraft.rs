//! The type table: everything `perf` knows about each airframe.
//!
//! `AircraftSpec` (`crate::dispatch`) is what the rest of the planner sees. `TypeData` is
//! what this crate needs beyond that to fly a profile — a wing, a drag polar and an engine
//! — kept local because `dispatch` is frozen and does not carry aerodynamic coefficients.
//!
//! The weights, limits and geometry are the figures an operator or a spotter would read off
//! the manufacturer's "Aircraft Characteristics — Airport and Maintenance Planning" document
//! or the type's EASA/FAA Type Certificate Data Sheet: publicly published, not reverse
//! engineered from a licensed model. The drag polar (`cd0`, `oswald_e`) cannot be read off
//! any public document — no manufacturer publishes one — so it is estimated here from the
//! wing geometry and a textbook Oswald efficiency (0.78-0.85 for a swept transport wing in
//! clean cruise configuration; Torenbeek, *Synthesis of Subsonic Airplane Design*), and the
//! zero-lift term is chosen so the resulting best lift-to-drag ratio falls where public
//! sources (manufacturer briefings, pilots' operating figures) usually place it for the
//! type. Engine thrust and cruise TSFC are the figures published for the type's usual engine
//! option, again from the manufacturer's or engine maker's own literature; where a distinct
//! public figure was not to hand the value was estimated and then calibrated so the fuel
//! flow the model predicts sits near widely reported block-fuel and cruise-fuel-flow figures
//! for the type — this is noted per row in `source`.

/// What `perf` knows about a type beyond `AircraftSpec`.
#[derive(Debug, Clone, Copy)]
pub struct TypeData {
    pub spec_icao_type: &'static str,
    pub name: &'static str,
    pub engine: &'static str,
    pub engines: u8,
    pub oew_kg: f64,
    pub mzfw_kg: f64,
    pub mtow_kg: f64,
    pub mlw_kg: f64,
    pub max_fuel_kg: f64,
    pub ceiling_ft: f64,
    pub mmo: f64,
    pub vmo_kt: f64,
    pub cruise_mach: f64,
    pub etops_minutes: Option<u32>,
    pub one_engine_tas_kt: Option<f64>,
    /// Reference wing area, square metres.
    pub wing_area_m2: f64,
    pub wingspan_m: f64,
    /// Zero-lift drag coefficient, clean cruise configuration.
    pub cd0: f64,
    /// Oswald span efficiency, for the induced-drag factor `k = 1 / (pi * e * AR)`.
    pub oswald_e: f64,
    /// The Mach number above which compressibility drag starts to rise sharply.
    pub mach_crit: f64,
    /// Sea-level static thrust per engine, newtons.
    pub thrust_sls_n: f64,
    /// Cruise thrust-specific fuel consumption at a reference Mach and altitude, kilograms
    /// of fuel per kilogram-force of thrust per hour — the units engine literature usually
    /// publishes it in.
    pub tsfc_cruise_kgf_h: f64,
    /// The type's own citation for the figures above.
    pub source: &'static str,
}

impl TypeData {
    /// The aspect ratio implied by the wing geometry.
    pub fn aspect_ratio(&self) -> f64 {
        self.wingspan_m * self.wingspan_m / self.wing_area_m2
    }

    /// The induced-drag factor `k`.
    pub fn induced_k(&self) -> f64 {
        1.0 / (std::f64::consts::PI * self.oswald_e * self.aspect_ratio())
    }

    /// This type's aerodynamics with the weights and limits of a caller-supplied spec
    /// substituted in — for when `PerfRequest.spec` has been tailored for a particular
    /// registration rather than being exactly what `perf::spec` handed back.
    pub fn with_spec(&self, spec: &crate::dispatch::AircraftSpec) -> TypeData {
        TypeData {
            engines: spec.engines,
            oew_kg: spec.oew_kg,
            mzfw_kg: spec.mzfw_kg,
            mtow_kg: spec.mtow_kg,
            mlw_kg: spec.mlw_kg,
            max_fuel_kg: spec.max_fuel_kg,
            ceiling_ft: spec.ceiling_ft,
            mmo: spec.mmo,
            vmo_kt: spec.vmo_kt,
            cruise_mach: spec.cruise_mach,
            etops_minutes: spec.etops_minutes,
            one_engine_tas_kt: spec.one_engine_tas_kt,
            ..*self
        }
    }

    pub fn to_spec(&self) -> crate::dispatch::AircraftSpec {
        crate::dispatch::AircraftSpec {
            icao_type: self.spec_icao_type.to_string(),
            name: self.name.to_string(),
            engine: self.engine.to_string(),
            engines: self.engines,
            oew_kg: self.oew_kg,
            mzfw_kg: self.mzfw_kg,
            mtow_kg: self.mtow_kg,
            mlw_kg: self.mlw_kg,
            max_fuel_kg: self.max_fuel_kg,
            ceiling_ft: self.ceiling_ft,
            mmo: self.mmo,
            vmo_kt: self.vmo_kt,
            cruise_mach: self.cruise_mach,
            etops_minutes: self.etops_minutes,
            one_engine_tas_kt: self.one_engine_tas_kt,
        }
    }
}

const LBF_TO_N: f64 = 4.448_222;

/// One row of the table, grouped so a mistyped figure lands in the field it looks like
/// rather than silently becoming a different one.
#[allow(clippy::too_many_arguments)]
fn row(
    icao_type: &'static str,
    name: &'static str,
    engine: (&'static str, u8, f64, f64),
    weights: (f64, f64, f64, f64, f64),
    limits: (f64, f64, f64, f64),
    etops: (Option<u32>, Option<f64>),
    geom: (f64, f64, f64, f64, f64),
    source: &'static str,
) -> TypeData {
    let (engine_name, engines, thrust_lbf, tsfc) = engine;
    let (oew_kg, mzfw_kg, mtow_kg, mlw_kg, max_fuel_kg) = weights;
    let (ceiling_ft, mmo, vmo_kt, cruise_mach) = limits;
    let (etops_minutes, one_engine_tas_kt) = etops;
    let (wing_area_m2, wingspan_m, cd0, oswald_e, mach_crit) = geom;
    TypeData {
        spec_icao_type: icao_type,
        name,
        engine: engine_name,
        engines,
        oew_kg,
        mzfw_kg,
        mtow_kg,
        mlw_kg,
        max_fuel_kg,
        ceiling_ft,
        mmo,
        vmo_kt,
        cruise_mach,
        etops_minutes,
        one_engine_tas_kt,
        wing_area_m2,
        wingspan_m,
        cd0,
        oswald_e,
        mach_crit,
        thrust_sls_n: thrust_lbf * LBF_TO_N,
        tsfc_cruise_kgf_h: tsfc,
        source,
    }
}

/// Every type this planner flies. Figures are drawn from the manufacturers' Aircraft
/// Characteristics documents and EASA/FAA type certificate data sheets for the weights,
/// limits and geometry; the drag polar and cruise TSFC are estimated as described on
/// `TypeData` and calibrated against publicly reported cruise fuel flows and block fuel.
fn all() -> Vec<TypeData> {
    vec![
        row("A20N", "Airbus A320neo", ("LEAP-1A", 2, 27_000.0, 0.53),
            (44_300.0, 62_800.0, 79_000.0, 67_400.0, 23_723.0),
            (39_800.0, 0.82, 350.0, 0.78), (None, None),
            (122.6, 35.8, 0.0270, 0.82, 0.79),
            "Airbus A320 Family Aircraft Characteristics — Airport and Maintenance Planning; CFM LEAP-1A cruise SFC from CFM public data; drag polar estimated, calibrated to reported ~2,300 kg/h cruise burn"),
        row("A21N", "Airbus A321neo", ("LEAP-1A", 2, 33_000.0, 0.53),
            (50_100.0, 74_000.0, 97_000.0, 79_200.0, 23_723.0),
            (39_800.0, 0.82, 350.0, 0.78), (None, None),
            (122.6, 35.8, 0.0210, 0.82, 0.79),
            "Airbus A320 Family Aircraft Characteristics; LEAP-1A public SFC; drag polar estimated, calibrated to reported ~2,600 kg/h cruise burn"),
        row("A319", "Airbus A319", ("CFM56-5B", 2, 22_000.0, 0.63),
            (40_800.0, 57_000.0, 75_500.0, 61_000.0, 24_210.0),
            (39_100.0, 0.82, 350.0, 0.78), (None, None),
            (122.6, 34.1, 0.0220, 0.80, 0.79),
            "Airbus A320 Family Aircraft Characteristics; CFM56-5B public SFC; drag polar estimated"),
        row("A320", "Airbus A320", ("CFM56-5B", 2, 27_000.0, 0.63),
            (42_600.0, 64_500.0, 78_000.0, 66_000.0, 24_210.0),
            (39_100.0, 0.82, 350.0, 0.78), (None, None),
            (122.6, 34.1, 0.0220, 0.80, 0.79),
            "Airbus A320 Family Aircraft Characteristics; CFM56-5B public SFC; calibrated to widely reported ~2,400 kg/h cruise burn"),
        row("A321", "Airbus A321", ("CFM56-5B", 2, 33_000.0, 0.63),
            (48_500.0, 73_800.0, 93_500.0, 77_800.0, 24_050.0),
            (39_100.0, 0.82, 350.0, 0.78), (None, None),
            (122.6, 34.1, 0.0225, 0.80, 0.79),
            "Airbus A320 Family Aircraft Characteristics; CFM56-5B public SFC; drag polar estimated"),
        row("A332", "Airbus A330-200", ("Trent 772B", 2, 71_100.0, 0.57),
            (120_200.0, 170_000.0, 230_000.0, 182_000.0, 97_170.0),
            (41_000.0, 0.86, 330.0, 0.82), (Some(180), Some(300.0)),
            (361.6, 60.3, 0.0195, 0.82, 0.83),
            "Airbus A330 Aircraft Characteristics; Rolls-Royce Trent 700 public SFC; drag polar estimated, calibrated to reported ~5,500 kg/h cruise burn"),
        row("A333", "Airbus A330-300", ("Trent 772B", 2, 72_000.0, 0.57),
            (126_200.0, 175_000.0, 242_000.0, 187_000.0, 97_170.0),
            (41_000.0, 0.86, 330.0, 0.82), (Some(180), Some(300.0)),
            (361.6, 60.3, 0.0198, 0.82, 0.83),
            "Airbus A330 Aircraft Characteristics; Trent 700 public SFC; drag polar estimated"),
        row("A339", "Airbus A330-900", ("Trent 7000", 2, 72_000.0, 0.51),
            (132_000.0, 187_000.0, 242_000.0, 191_000.0, 109_000.0),
            (41_450.0, 0.86, 330.0, 0.82), (Some(285), Some(300.0)),
            (361.6, 64.0, 0.0190, 0.83, 0.83),
            "Airbus A330neo Aircraft Characteristics; Trent 7000 public SFC; drag polar estimated, calibrated to reported ~5,000 kg/h cruise burn"),
        row("A359", "Airbus A350-900", ("Trent XWB-84", 2, 84_200.0, 0.52),
            (142_400.0, 207_000.0, 280_000.0, 205_000.0, 109_000.0),
            (43_100.0, 0.89, 340.0, 0.85), (Some(370), Some(320.0)),
            (443.0, 64.75, 0.0180, 0.84, 0.86),
            "Airbus A350 Aircraft Characteristics; Rolls-Royce Trent XWB public SFC; drag polar estimated, calibrated to reported ~5,700-6,200 kg/h cruise burn"),
        row("A35K", "Airbus A350-1000", ("Trent XWB-97", 2, 97_000.0, 0.53),
            (155_300.0, 223_000.0, 319_000.0, 236_000.0, 133_000.0),
            (43_100.0, 0.89, 340.0, 0.85), (Some(370), Some(320.0)),
            (443.0, 64.75, 0.0185, 0.84, 0.86),
            "Airbus A350 Aircraft Characteristics; Trent XWB-97 public SFC; drag polar estimated"),
        row("A388", "Airbus A380-800", ("Trent 970", 4, 70_000.0, 0.58),
            (277_000.0, 361_000.0, 575_000.0, 394_000.0, 254_000.0),
            (43_100.0, 0.89, 330.0, 0.85), (None, None),
            (845.0, 79.75, 0.0132, 0.85, 0.85),
            "Airbus A380 Aircraft Characteristics; Trent 900/GP7200 public SFC; drag polar estimated, calibrated to widely reported ~11,000-12,000 kg/h cruise burn"),
        row("BCS1", "Airbus A220-100", ("PW1500G", 2, 21_000.0, 0.51),
            (35_800.0, 51_256.0, 60_781.0, 51_256.0, 13_100.0),
            (41_000.0, 0.82, 320.0, 0.78), (Some(180), Some(260.0)),
            (112.3, 35.1, 0.0195, 0.83, 0.79),
            "Airbus A220 Aircraft Characteristics; Pratt & Whitney PW1500G public SFC; drag polar estimated, calibrated to reported ~1,700 kg/h cruise burn"),
        row("BCS3", "Airbus A220-300", ("PW1500G", 2, 23_300.0, 0.51),
            (39_500.0, 55_492.0, 67_812.0, 55_700.0, 13_100.0),
            (41_000.0, 0.82, 320.0, 0.78), (Some(180), Some(260.0)),
            (112.3, 35.1, 0.0200, 0.83, 0.79),
            "Airbus A220 Aircraft Characteristics; PW1500G public SFC; drag polar estimated"),
        row("B737", "Boeing 737-700", ("CFM56-7B22", 2, 22_700.0, 0.63),
            (38_147.0, 53_298.0, 70_080.0, 58_060.0, 20_894.0),
            (41_000.0, 0.82, 340.0, 0.78), (None, None),
            (124.6, 34.3, 0.0225, 0.80, 0.79),
            "Boeing 737 Airplane Characteristics for Airport Planning; CFM56-7B public SFC; drag polar estimated"),
        row("B738", "Boeing 737-800", ("CFM56-7B26", 2, 26_300.0, 0.63),
            (41_413.0, 61_688.0, 79_016.0, 66_360.0, 20_894.0),
            (41_000.0, 0.82, 340.0, 0.78), (None, None),
            (124.6, 35.8, 0.0225, 0.80, 0.79),
            "Boeing 737 Airplane Characteristics for Airport Planning; CFM56-7B public SFC; calibrated to widely reported ~2,500 kg/h cruise burn"),
        row("B739", "Boeing 737-900ER", ("CFM56-7B27", 2, 27_300.0, 0.63),
            (44_676.0, 64_950.0, 85_139.0, 71_350.0, 20_894.0),
            (41_000.0, 0.82, 340.0, 0.78), (None, None),
            (124.6, 35.8, 0.0230, 0.80, 0.79),
            "Boeing 737 Airplane Characteristics for Airport Planning; CFM56-7B public SFC; drag polar estimated"),
        row("B38M", "Boeing 737 MAX 8", ("LEAP-1B", 2, 29_300.0, 0.53),
            (45_070.0, 65_950.0, 82_191.0, 69_308.0, 23_000.0),
            (41_000.0, 0.82, 340.0, 0.79), (None, None),
            (127.0, 35.9, 0.0210, 0.82, 0.80),
            "Boeing 737 MAX Airplane Characteristics for Airport Planning; CFM LEAP-1B public SFC; drag polar estimated, calibrated to reported ~2,150 kg/h cruise burn"),
        row("B39M", "Boeing 737 MAX 9", ("LEAP-1B", 2, 30_200.0, 0.53),
            (46_980.0, 67_268.0, 88_314.0, 71_214.0, 23_000.0),
            (41_000.0, 0.82, 340.0, 0.79), (None, None),
            (127.0, 35.9, 0.0215, 0.82, 0.80),
            "Boeing 737 MAX Airplane Characteristics for Airport Planning; LEAP-1B public SFC; drag polar estimated"),
        row("B772", "Boeing 777-200", ("GE90-77B", 2, 77_000.0, 0.56),
            (135_000.0, 176_000.0, 247_200.0, 201_850.0, 117_000.0),
            (43_100.0, 0.89, 330.0, 0.84), (Some(180), Some(320.0)),
            (427.8, 60.9, 0.0190, 0.82, 0.85),
            "Boeing 777 Airplane Characteristics for Airport Planning; GE90-77B public SFC; drag polar estimated, calibrated to reported ~6,900-7,200 kg/h cruise burn"),
        row("B77L", "Boeing 777-200LR", ("GE90-110B1", 2, 110_000.0, 0.54),
            (145_150.0, 195_043.0, 347_450.0, 223_168.0, 202_287.0),
            (43_100.0, 0.89, 330.0, 0.84), (Some(330), Some(320.0)),
            (427.8, 64.8, 0.0192, 0.82, 0.85),
            "Boeing 777 Airplane Characteristics for Airport Planning; GE90-110B1 public SFC; drag polar estimated"),
        row("B77W", "Boeing 777-300ER", ("GE90-115B", 2, 115_300.0, 0.55),
            (167_829.0, 237_683.0, 351_530.0, 251_290.0, 145_150.0),
            (43_100.0, 0.89, 330.0, 0.84), (Some(330), Some(320.0)),
            (427.8, 64.8, 0.0198, 0.82, 0.85),
            "Boeing 777 Airplane Characteristics for Airport Planning; GE90-115B public SFC; calibrated to widely reported ~7,500-7,900 kg/h cruise burn"),
        row("B788", "Boeing 787-8", ("GEnx-1B64", 2, 64_000.0, 0.51),
            (119_950.0, 161_000.0, 227_930.0, 172_365.0, 126_206.0),
            (43_100.0, 0.90, 365.0, 0.85), (Some(330), Some(320.0)),
            (325.0, 60.1, 0.0175, 0.84, 0.86),
            "Boeing 787 Airplane Characteristics for Airport Planning; GE GEnx-1B public SFC; drag polar estimated, calibrated to reported ~5,400 kg/h cruise burn"),
        row("B789", "Boeing 787-9", ("GEnx-1B70", 2, 74_100.0, 0.50),
            (128_850.0, 181_000.0, 254_011.0, 192_777.0, 126_372.0),
            (43_100.0, 0.90, 365.0, 0.85), (Some(330), Some(320.0)),
            (325.0, 60.1, 0.0178, 0.84, 0.86),
            "Boeing 787 Airplane Characteristics for Airport Planning; GEnx-1B public SFC; drag polar estimated, calibrated to reported ~5,900-6,100 kg/h cruise burn"),
        row("B78X", "Boeing 787-10", ("GEnx-1B76", 2, 76_100.0, 0.50),
            (135_550.0, 192_000.0, 254_011.0, 201_848.0, 126_206.0),
            (43_100.0, 0.90, 365.0, 0.85), (Some(330), Some(320.0)),
            (325.0, 60.1, 0.0182, 0.84, 0.86),
            "Boeing 787 Airplane Characteristics for Airport Planning; GEnx-1B public SFC; drag polar estimated"),
        row("B744", "Boeing 747-400", ("CF6-80C2", 4, 56_700.0, 0.63),
            (180_800.0, 242_700.0, 396_800.0, 285_800.0, 173_700.0),
            (45_100.0, 0.92, 365.0, 0.855), (None, None),
            (541.2, 64.4, 0.0200, 0.80, 0.86),
            "Boeing 747 Airplane Characteristics for Airport Planning; GE CF6-80C2 public SFC; drag polar estimated, calibrated to widely reported ~10,500 kg/h cruise burn"),
        row("B748", "Boeing 747-8", ("GEnx-2B67", 4, 66_500.0, 0.53),
            (220_100.0, 306_200.0, 447_700.0, 312_980.0, 226_000.0),
            (43_100.0, 0.92, 365.0, 0.855), (None, None),
            (554.0, 68.4, 0.0195, 0.81, 0.86),
            "Boeing 747-8 Airplane Characteristics for Airport Planning; GE GEnx-2B public SFC; drag polar estimated"),
        row("B763", "Boeing 767-300ER", ("CF6-80C2", 2, 60_000.0, 0.61),
            (90_010.0, 126_100.0, 186_880.0, 145_150.0, 63_216.0),
            (43_100.0, 0.86, 360.0, 0.80), (Some(180), Some(280.0)),
            (283.3, 47.6, 0.0210, 0.80, 0.81),
            "Boeing 767 Airplane Characteristics for Airport Planning; GE CF6-80C2 public SFC; drag polar estimated, calibrated to reported ~5,300 kg/h cruise burn"),
        row("E190", "Embraer E190", ("CF34-10E", 2, 18_500.0, 0.65),
            (28_080.0, 40_870.0, 50_790.0, 43_000.0, 13_048.0),
            (41_000.0, 0.82, 340.0, 0.78), (None, None),
            (92.5, 28.72, 0.0230, 0.80, 0.79),
            "Embraer E-Jet Airport Planning Manual; General Electric CF34-10E public SFC; drag polar estimated"),
        row("E195", "Embraer E195", ("CF34-10E", 2, 20_100.0, 0.65),
            (29_550.0, 44_500.0, 52_290.0, 45_810.0, 13_048.0),
            (41_000.0, 0.82, 340.0, 0.78), (None, None),
            (92.5, 28.72, 0.0235, 0.80, 0.79),
            "Embraer E-Jet Airport Planning Manual; CF34-10E public SFC; drag polar estimated"),
        row("CRJ9", "Bombardier CRJ900", ("CF34-8C5", 2, 9_220.0, 0.68),
            (21_590.0, 32_999.0, 38_330.0, 34_065.0, 9_583.0),
            (41_000.0, 0.85, 340.0, 0.78), (None, None),
            (70.6, 23.2, 0.0245, 0.78, 0.80),
            "Bombardier CRJ900 Airport Planning Manual; GE CF34-8C5 public SFC; drag polar estimated"),
        row("AT76", "ATR 72-600", ("PW127M", 2, 4_724.0, 0.35),
            (13_311.0, 20_800.0, 22_800.0, 22_350.0, 5_000.0),
            (25_000.0, 0.55, 250.0, 0.45), (None, None),
            (61.0, 27.05, 0.0280, 0.78, 0.60),
            "ATR 72-600 Airport Planning document; Pratt & Whitney Canada PW127M power converted to an equivalent static thrust; drag polar and TSFC estimated and calibrated to reported ~550 kg/h total cruise burn"),
        row("DH8D", "De Havilland Dash 8 Q400", ("PW150A", 2, 9_712.0, 0.32),
            (17_185.0, 27_500.0, 29_257.0, 28_009.0, 5_308.0),
            (27_000.0, 0.58, 360.0, 0.52), (None, None),
            (63.1, 28.42, 0.0270, 0.78, 0.62),
            "Q400 Airport Planning document; Pratt & Whitney Canada PW150A power converted to an equivalent static thrust; drag polar and TSFC estimated and calibrated to reported ~800 kg/h total cruise burn"),
    ]
}

/// The type table, built once.
fn table() -> &'static Vec<TypeData> {
    static TABLE: std::sync::OnceLock<Vec<TypeData>> = std::sync::OnceLock::new();
    TABLE.get_or_init(all)
}

/// A type by its ICAO designator, case-insensitive.
pub fn lookup(icao_type: &str) -> Option<TypeData> {
    let key = icao_type.to_uppercase();
    table().iter().find(|t| t.spec_icao_type == key).copied()
}

/// Every type designator this table knows.
pub fn known_types() -> Vec<&'static str> {
    table().iter().map(|t| t.spec_icao_type).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_required_type_is_present() {
        for t in ["A20N", "A21N", "A319", "A320", "A321", "A332", "A333", "A339", "A359", "A35K", "A388", "BCS1", "BCS3", "B737", "B738", "B739", "B38M", "B39M", "B772", "B77L", "B77W", "B788", "B789", "B78X", "B744", "B748", "B763", "E190", "E195", "CRJ9", "AT76", "DH8D"] {
            assert!(lookup(t).is_some(), "missing {t}");
        }
    }

    #[test]
    fn weights_are_internally_consistent() {
        for t in table() {
            assert!(t.oew_kg < t.mzfw_kg, "{}: oew >= mzfw", t.spec_icao_type);
            assert!(t.mzfw_kg < t.mtow_kg, "{}: mzfw >= mtow", t.spec_icao_type);
            assert!(t.mlw_kg <= t.mtow_kg, "{}: mlw > mtow", t.spec_icao_type);
            assert!(t.max_fuel_kg > 0.0, "{}: no fuel capacity", t.spec_icao_type);
            // The tanks should hold enough, on top of maximum payload, to reach MTOW: an
            // operator can trade payload for fuel up to that combined limit.
            assert!(t.mzfw_kg + t.max_fuel_kg >= t.mtow_kg, "{}: not enough fuel capacity to top up from MZFW to MTOW", t.spec_icao_type);
            assert!(t.induced_k() > 0.0 && t.induced_k() < 0.2, "{}: implausible induced drag factor {}", t.spec_icao_type, t.induced_k());
        }
    }

    #[test]
    fn four_engine_types_carry_no_etops() {
        for t in table() {
            if t.engines == 4 {
                assert!(t.etops_minutes.is_none() && t.one_engine_tas_kt.is_none(), "{}", t.spec_icao_type);
            }
        }
    }

    #[test]
    fn lookup_is_case_insensitive() {
        assert!(lookup("a320").is_some());
    }
}
