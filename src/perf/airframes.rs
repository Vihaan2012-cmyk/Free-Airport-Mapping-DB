//! Airframes: the real weights of real aeroplanes, one tail at a time.
//!
//! The built-in performance data knows a type — what an A320 weighs, roughly — and a type is not
//! what anybody flies. Two 737-800s off the same line differ by a tonne of empty weight and by
//! whatever the operator fitted, and the figure that decides how much payload a flight can take
//! is the airframe's, not the type's.
//!
//! So this carries a table of airframes exported from SimBrief: four hundred and ninety-seven of
//! them over two hundred types, each with its registration, its engines, its empty weight and
//! its four limits. Selecting one gives a plan the weights it will actually be flown at.
//!
//! The weights in that file are in **pounds**, whatever its column headings say. Every one of
//! them: a 777-300ER reads 775,000, an A380 1,267,657, a 747-400 875,000. Taken at their word
//! they would make every aeroplane two and a fifth times too heavy, every payload impossible and
//! every plan wrong in the same direction — so they are converted on the way in, and a figure
//! that still looks like a spaceship after conversion is dropped rather than believed.

use crate::dispatch::AircraftSpec;

/// One aeroplane, as an operator holds it.
#[derive(Debug, Clone)]
pub struct Airframe {
    /// The type it is a variant of: A320, B77W.
    pub icao_type: String,
    /// What the airframe is called, which is usually the marketing name of the variant.
    pub name: String,
    pub registration: String,
    pub fin: String,
    pub engines: String,
    pub max_passengers: u32,
    pub oew_kg: f64,
    pub mzfw_kg: f64,
    pub mtow_kg: f64,
    pub mlw_kg: f64,
    pub max_fuel_kg: f64,
    /// The speeds the operator flies it at, as SimBrief writes them: "250/320/84".
    pub climb: String,
    pub cruise: String,
    pub descent: String,
}

impl Airframe {
    /// How this airframe is named in a list: the registration where there is one, because that
    /// is what a crew is given, with the variant beside it to say what it is.
    pub fn label(&self) -> String {
        match (self.registration.trim(), self.name.trim()) {
            ("", name) => name.to_string(),
            (reg, "") => reg.to_string(),
            (reg, name) => format!("{reg} — {name}"),
        }
    }

    /// The type's own performance with this airframe's weights over it.
    ///
    /// Only the weights: the speeds, the ceiling and the fuel flow belong to the type and are
    /// modelled, not tabulated, so an airframe that carried its own would be replacing measured
    /// figures with a line from a spreadsheet.
    pub fn onto(&self, spec: &AircraftSpec) -> AircraftSpec {
        let mut out = spec.clone();
        for (slot, value) in [
            (&mut out.oew_kg, self.oew_kg),
            (&mut out.mzfw_kg, self.mzfw_kg),
            (&mut out.mtow_kg, self.mtow_kg),
            (&mut out.mlw_kg, self.mlw_kg),
            (&mut out.max_fuel_kg, self.max_fuel_kg),
        ] {
            if value > 0.0 {
                *slot = value;
            }
        }
        if !self.engines.trim().is_empty() {
            out.engine = self.engines.trim().to_string();
        }
        out
    }
}

const LB_TO_KG: f64 = 0.453_592_37;

/// The heaviest aeroplane ever built has a maximum take-off weight of six hundred and forty
/// tonnes. A converted figure above that is not a weight, it is a column read wrongly, and it
/// is dropped rather than planned on.
const TOO_HEAVY_KG: f64 = 700_000.0;

fn table() -> &'static Vec<Airframe> {
    static TABLE: std::sync::OnceLock<Vec<Airframe>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| parse(include_str!("../../data/airframes.csv")))
}

fn parse(csv: &str) -> Vec<Airframe> {
    let mut lines = csv.lines();
    let Some(header) = lines.next() else { return Vec::new() };
    let columns: Vec<&str> = header.trim_start_matches('\u{feff}').split(',').map(str::trim).collect();
    let at = |name: &str| columns.iter().position(|c| c.eq_ignore_ascii_case(name));
    let (Some(c_type), Some(c_name)) = (at("Base_Aircraft_ICAO"), at("Airframe_Name")) else { return Vec::new() };

    let mut out = Vec::new();
    for line in lines {
        let f = split(line);
        let get = |i: Option<usize>| i.and_then(|i| f.get(i)).map(|s| s.trim().to_string()).unwrap_or_default();
        let weight = |i: Option<usize>| -> f64 {
            let kg = get(i).replace(',', "").parse::<f64>().unwrap_or(0.0) * LB_TO_KG;
            if kg.is_finite() && (0.0..TOO_HEAVY_KG).contains(&kg) { kg } else { 0.0 }
        };
        let icao_type = get(Some(c_type)).to_uppercase();
        if icao_type.is_empty() {
            continue;
        }
        out.push(Airframe {
            icao_type,
            name: get(Some(c_name)),
            registration: get(at("Registration")).to_uppercase(),
            fin: get(at("Fin_Number")),
            engines: get(at("Engines")),
            max_passengers: get(at("Max_Passengers")).parse().unwrap_or(0),
            oew_kg: weight(at("OEW_kgs")),
            mzfw_kg: weight(at("MZFW_kgs")),
            mtow_kg: weight(at("MTOW_kgs")),
            mlw_kg: weight(at("MLW_kgs")),
            max_fuel_kg: weight(at("Max_Fuel_kgs")),
            climb: get(at("Default_Climb")),
            cruise: get(at("Default_Cruise")),
            descent: get(at("Default_Descent")),
        });
    }
    out
}

/// A comma-separated line, respecting quotes: an airframe's comment carries commas of its own.
fn split(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    for c in line.chars() {
        match c {
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut field)),
            _ => field.push(c),
        }
    }
    out.push(field);
    out
}

/// Every airframe of a type, registration first.
pub fn of_type(icao_type: &str) -> Vec<&'static Airframe> {
    let want = icao_type.trim().to_uppercase();
    let mut out: Vec<&Airframe> = table().iter().filter(|a| a.icao_type == want).collect();
    out.sort_by(|a, b| a.registration.cmp(&b.registration));
    out
}

/// One airframe by its registration, or by its registration and type together where a
/// registration is not unique.
pub fn by_registration(registration: &str) -> Option<&'static Airframe> {
    let want = registration.trim().to_uppercase();
    table().iter().find(|a| a.registration == want)
}

/// Every type the table has an airframe for.
pub fn types() -> Vec<&'static str> {
    let mut out: Vec<&str> = table().iter().map(|a| a.icao_type.as_str()).collect();
    out.sort_unstable();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table reads, and reads as many airframes as the file has.
    #[test]
    fn the_table_is_read() {
        assert!(table().len() > 400, "{} airframes", table().len());
        assert!(types().len() > 150, "{} types", types().len());
    }

    /// The weights are pounds in the file and kilograms here. A 777-300ER's maximum take-off
    /// weight is 351.5 tonnes; the file says 775,000, which is that in pounds. Read as
    /// kilograms it would be heavier than any aeroplane ever built, and every plan made on it
    /// would carry a payload no aeroplane could lift.
    #[test]
    fn the_weights_are_converted_from_pounds() {
        let a = of_type("B77W").into_iter().find(|a| a.mtow_kg > 0.0).expect("a 777-300ER");
        assert!((a.mtow_kg - 351_534.0).abs() < 2_000.0, "MTOW {:.0} kg", a.mtow_kg);
        assert!(a.oew_kg > 120_000.0 && a.oew_kg < 200_000.0, "OEW {:.0} kg", a.oew_kg);
        assert!(a.oew_kg < a.mzfw_kg && a.mzfw_kg <= a.mtow_kg, "the limits are in order");
    }

    /// Every airframe that gives a weight gives one an aeroplane could have.
    #[test]
    fn no_airframe_is_heavier_than_an_aeroplane() {
        for a in table() {
            for (what, kg) in [("OEW", a.oew_kg), ("MZFW", a.mzfw_kg), ("MTOW", a.mtow_kg), ("MLW", a.mlw_kg), ("fuel", a.max_fuel_kg)] {
                assert!(kg >= 0.0 && kg < TOO_HEAVY_KG, "{} {} {what} {kg}", a.icao_type, a.registration);
            }
            if a.mtow_kg > 0.0 && a.mzfw_kg > 0.0 {
                assert!(a.mzfw_kg <= a.mtow_kg, "{} {}: MZFW over MTOW", a.icao_type, a.registration);
            }
        }
    }

    /// An airframe lends a type its weights and leaves the modelled performance alone.
    #[test]
    fn an_airframe_replaces_the_weights_and_nothing_else() {
        let a = of_type("B77W").into_iter().find(|a| a.mtow_kg > 0.0).expect("a 777-300ER");
        let base = AircraftSpec {
            icao_type: "B77W".into(),
            name: "test".into(),
            engine: "old".into(),
            engines: 2,
            oew_kg: 1.0,
            mzfw_kg: 2.0,
            mtow_kg: 3.0,
            mlw_kg: 4.0,
            max_fuel_kg: 5.0,
            ceiling_ft: 43_100.0,
            mmo: 0.89,
            vmo_kt: 330.0,
            cruise_mach: 0.84,
            etops_minutes: Some(330),
            one_engine_tas_kt: Some(310.0),
        };
        let out = a.onto(&base);
        assert_eq!(out.mtow_kg, a.mtow_kg);
        assert_eq!(out.oew_kg, a.oew_kg);
        assert_eq!(out.ceiling_ft, base.ceiling_ft, "the ceiling is the type's");
        assert_eq!(out.cruise_mach, base.cruise_mach, "so is the cruise");
        assert_eq!(out.etops_minutes, base.etops_minutes);
    }

    /// A line with a comma inside a quoted field is one field, not two.
    #[test]
    fn quoted_commas_do_not_split_a_line() {
        let f = split(r#"A,B,"C, with comma",D"#);
        assert_eq!(f, vec!["A", "B", "C, with comma", "D"]);
    }
}
