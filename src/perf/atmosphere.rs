//! The International Standard Atmosphere, and the conversions between calibrated airspeed,
//! true airspeed and Mach that every other part of `perf` is built on.
//!
//! The formulae here are the standard ones published in ICAO Doc 7488, "Manual of the ICAO
//! Standard Atmosphere", and in any general aerodynamics text (e.g. Anderson, *Introduction
//! to Flight*): nothing here comes from a licensed performance model.

/// Sea-level standard temperature, kelvin.
pub const T0_K: f64 = 288.15;
/// Sea-level standard pressure, pascals.
pub const P0_PA: f64 = 101_325.0;
/// Sea-level standard density, kg/m^3.
pub const RHO0_KGM3: f64 = 1.225;
/// The specific gas constant for dry air, J/(kg K).
pub const R_SPECIFIC: f64 = 287.052_87;
/// The ratio of specific heats for air.
pub const GAMMA: f64 = 1.4;
/// The height of the tropopause in the standard atmosphere, feet (11,000 m).
pub const TROPOPAUSE_FT: f64 = 36_089.24;
/// The speed of sound at sea level on a standard day, knots.
pub const A0_KT: f64 = 661.478_6;

const MS_TO_KT: f64 = 1.943_844_49;

/// The standard temperature at a pressure altitude, kelvin. `dispatch::isa_temp_c` gives the
/// same figure in Celsius; this just re-expresses it for the formulae below.
pub fn isa_temp_k(alt_ft: f64) -> f64 {
    crate::dispatch::isa_temp_c(alt_ft) + 273.15
}

/// The pressure at a pressure altitude, pascals: the standard atmosphere, which by
/// definition does not move with a non-standard day (that is what "pressure altitude"
/// means — an altimeter set to 1013 hPa reads it whatever the temperature).
pub fn pressure_pa(alt_ft: f64) -> f64 {
    if alt_ft <= TROPOPAUSE_FT {
        P0_PA * (1.0 - 6.875_585_6e-6 * alt_ft).powf(5.255_879_7)
    } else {
        let p11 = P0_PA * (1.0 - 6.875_585_6e-6 * TROPOPAUSE_FT).powf(5.255_879_7);
        p11 * (-4.806_346e-5 * (alt_ft - TROPOPAUSE_FT)).exp()
    }
}

/// The air density at a pressure altitude and an actual (not standard) temperature, kg/m^3.
pub fn density_kgm3(alt_ft: f64, temp_c: f64) -> f64 {
    pressure_pa(alt_ft) / (R_SPECIFIC * (temp_c + 273.15).max(1.0))
}

/// The speed of sound at an actual temperature, knots.
pub fn speed_of_sound_kt(temp_c: f64) -> f64 {
    let t_k = (temp_c + 273.15).max(1.0);
    (GAMMA * R_SPECIFIC * t_k).sqrt() * MS_TO_KT
}

/// Mach number from a true airspeed and the local temperature.
pub fn mach_from_tas(tas_kt: f64, temp_c: f64) -> f64 {
    tas_kt / speed_of_sound_kt(temp_c)
}

/// True airspeed from a Mach number and the local temperature, knots.
pub fn tas_from_mach(mach: f64, temp_c: f64) -> f64 {
    mach * speed_of_sound_kt(temp_c)
}

/// Mach number from calibrated airspeed, a pressure altitude and the local temperature: the
/// standard compressible-flow relation between impact pressure and Mach (the temperature
/// does not enter directly — Mach depends only on the pressure ratio — but is accepted here
/// for symmetry with the other conversions and because callers always have it to hand).
pub fn mach_from_cas(cas_kt: f64, alt_ft: f64, _temp_c: f64) -> f64 {
    let p = pressure_pa(alt_ft);
    let qc_over_p0 = (1.0 + 0.2 * (cas_kt / A0_KT).powi(2)).powf(3.5) - 1.0;
    let qc = qc_over_p0 * P0_PA;
    let ratio = qc / p + 1.0;
    (5.0 * (ratio.powf(2.0 / 7.0) - 1.0)).max(0.0).sqrt()
}

/// Calibrated airspeed from a Mach number and a pressure altitude, knots.
pub fn cas_from_mach(mach: f64, alt_ft: f64) -> f64 {
    let p = pressure_pa(alt_ft);
    let qc = p * ((1.0 + 0.2 * mach * mach).powf(3.5) - 1.0);
    let ratio = qc / P0_PA + 1.0;
    A0_KT * (5.0 * (ratio.powf(2.0 / 7.0) - 1.0)).max(0.0).sqrt()
}

/// True airspeed from calibrated airspeed, a pressure altitude and the local temperature.
pub fn tas_from_cas(cas_kt: f64, alt_ft: f64, temp_c: f64) -> f64 {
    tas_from_mach(mach_from_cas(cas_kt, alt_ft, temp_c), temp_c)
}

/// Calibrated airspeed from a true airspeed, a pressure altitude and the local temperature.
pub fn cas_from_tas(tas_kt: f64, alt_ft: f64, temp_c: f64) -> f64 {
    cas_from_mach(mach_from_tas(tas_kt, temp_c), alt_ft)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reference figures are the published ICAO standard-atmosphere table (Doc 7488).
    #[test]
    fn sea_level_is_the_definition() {
        assert!((pressure_pa(0.0) - P0_PA).abs() < 1.0);
        assert!((density_kgm3(0.0, 15.0) - RHO0_KGM3).abs() < 0.001);
        assert!((speed_of_sound_kt(15.0) - A0_KT).abs() < 0.1);
    }

    #[test]
    fn ten_thousand_feet_matches_the_table() {
        // ICAO standard atmosphere: 10,000 ft -> 696.8 hPa, -4.8 C, 0.9046 kg/m^3.
        assert!((pressure_pa(10_000.0) - 69_680.0).abs() < 150.0);
        assert!((isa_temp_k(10_000.0) - 268.34).abs() < 0.1);
        assert!((density_kgm3(10_000.0, isa_temp_k(10_000.0) - 273.15) - 0.9046).abs() < 0.01);
    }

    #[test]
    fn the_tropopause_matches_the_table() {
        // 36,089 ft: 226.32 hPa, -56.5 C.
        assert!((pressure_pa(TROPOPAUSE_FT) - 22_632.0).abs() < 50.0);
        assert!((isa_temp_k(TROPOPAUSE_FT) - 216.65).abs() < 0.1);
    }

    #[test]
    fn thirty_five_thousand_feet_matches_the_table() {
        // A commonly quoted cruise-level figure: 238.4 hPa, -54.3 C.
        assert!((pressure_pa(35_000.0) - 23_842.0).abs() < 80.0);
    }

    #[test]
    fn the_two_formulae_meet_exactly_at_the_tropopause() {
        // The below- and above-tropopause formulae are different curves; what must hold is
        // that they agree at the seam, not that pressure stops falling with altitude there.
        let below = pressure_pa(TROPOPAUSE_FT);
        let above = pressure_pa(TROPOPAUSE_FT + 0.001);
        assert!((below - above).abs() < 0.1, "a kink at the tropopause: {below} vs {above}");
    }

    #[test]
    fn mach_and_cas_round_trip() {
        for alt in [0.0, 10_000.0, 25_000.0, 35_000.0, 41_000.0] {
            for mach in [0.3, 0.5, 0.78, 0.85] {
                let cas = cas_from_mach(mach, alt);
                let back = mach_from_cas(cas, alt, isa_temp_k(alt) - 273.15);
                assert!((back - mach).abs() < 1e-6, "alt {alt} mach {mach} -> cas {cas} -> {back}");
            }
        }
    }

    #[test]
    fn tas_exceeds_cas_with_height_on_a_standard_day() {
        let low = tas_from_cas(280.0, 5_000.0, isa_temp_k(5_000.0) - 273.15);
        let high = tas_from_cas(280.0, 35_000.0, isa_temp_k(35_000.0) - 273.15);
        assert!(high > low, "TAS should grow with altitude for the same CAS: {low} vs {high}");
        // A widely quoted rule of thumb: about 2% TAS gain per 1,000 ft, so 280 KCAS at
        // FL350 should sit somewhere around 460-490 KTAS for a jet.
        assert!((430.0..520.0).contains(&high), "{high}");
    }

    #[test]
    fn mach_078_at_fl350_is_roughly_the_known_figure() {
        // M0.78 at FL350 is textbook-quoted as about 447-450 KTAS.
        let tas = tas_from_mach(0.78, isa_temp_k(35_000.0) - 273.15);
        assert!((440.0..460.0).contains(&tas), "{tas}");
    }
}
