//! Everything `ofp::dispatch` asks the other four parts for, behind one trait.
//!
//! The five parts of flight planning are written and reviewed in parallel, and on the
//! day this was written four of them were stubs that returned an error. `Providers` is
//! what makes that not matter: every call this crate's `route`, `weather` and `perf`
//! modules answer is named here once, `RealProviders` calls straight through to them, and
//! a fake in the tests answers instead — so the orchestration in `ofp::dispatch` can be
//! written and tested end to end before a single one of those modules works, and needs no
//! change at all once they do.
//!
//! `route::etops::Etops::new`, `route::oceanic::TrackRule::new` and `route::Graph::add`
//! are plain constructors over data already fetched, not outside calls, so they are not
//! here: only what reaches the network, the filesystem or the navigation database is.

use crate::dispatch::{Airport, AircraftSpec, Alternate, Bounds, CostModel, FiledRoute, Hazard, LatLon, Metar, PerfPlan, PerfRequest, RouteRequest, Taf, WindField};
use crate::route;
use crate::route::airspace::FirCrossing;
use crate::route::oceanic::Track;
use crate::route::rad::Rad;
use crate::{perf, weather};
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::sync::Arc;

/// Every outside call `ofp::dispatch` makes on the other four parts.
pub trait Providers: Send + Sync {
    fn airport(&self, icao: &str) -> Option<Airport>;
    fn airports_near(&self, at: LatLon, radius_nm: f64, min_runway_ft: f64) -> Vec<(Airport, f64)>;
    fn aircraft_spec(&self, icao_type: &str) -> Result<AircraftSpec>;
    /// The winds and temperatures aloft over a corridor: a run of rectangles covering the way
    /// from one airport to another, rather than one box round the two of them.
    fn wind_field(&self, corridor: &[Bounds], when: DateTime<Utc>) -> Result<Arc<dyn WindField>>;
    fn metar(&self, icao: &str) -> Result<Metar>;
    fn taf(&self, icao: &str) -> Result<Taf>;
    fn sigmets(&self, bounds: Bounds, when: DateTime<Utc>) -> Result<Vec<Hazard>>;
    fn conflict_zones(&self, when: DateTime<Utc>) -> Vec<Hazard>;
    fn notam_hazards(&self, bounds: Bounds, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<Hazard>>;
    fn choose_alternates(&self, destination: &Airport, eta: DateTime<Utc>, count: usize) -> Result<Vec<Alternate>>;
    fn rad(&self, when: DateTime<Utc>) -> Result<Rad>;
    fn oceanic_tracks(&self, when: DateTime<Utc>) -> Result<Vec<Track>>;
    /// The airway network, with any oceanic tracks folded in as one-way airways.
    fn network(&self, tracks: &[Track]) -> route::Graph;
    fn plan_routes(&self, graph: &route::Graph, req: &RouteRequest<'_>, most: usize) -> Result<Vec<FiledRoute>>;
    fn cost_model<'a>(&self, spec: &'a AircraftSpec, air: &'a dyn WindField, zfw_kg: f64, trip_nm: f64, cost_index: Option<f64>) -> Result<Box<dyn CostModel + 'a>>;
    fn perf_plan(&self, req: &PerfRequest<'_>) -> Result<PerfPlan>;
    fn fir_crossings(&self, route: &FiledRoute) -> Vec<FirCrossing>;
}

/// The real implementation: calls straight through to `route`, `weather` and `perf`.
pub struct RealProviders;

impl Providers for RealProviders {
    fn airport(&self, icao: &str) -> Option<Airport> {
        route::airports::airport(icao)
    }

    fn airports_near(&self, at: LatLon, radius_nm: f64, min_runway_ft: f64) -> Vec<(Airport, f64)> {
        route::airports::airports_near(at, radius_nm, min_runway_ft)
    }

    fn aircraft_spec(&self, icao_type: &str) -> Result<AircraftSpec> {
        perf::spec(icao_type)
    }

    fn wind_field(&self, corridor: &[Bounds], when: DateTime<Utc>) -> Result<Arc<dyn WindField>> {
        Ok(Arc::new(weather::Forecast::fetch_corridor(corridor, when)?))
    }

    fn metar(&self, icao: &str) -> Result<Metar> {
        weather::metar(icao)
    }

    fn taf(&self, icao: &str) -> Result<Taf> {
        weather::taf(icao)
    }

    fn sigmets(&self, bounds: Bounds, when: DateTime<Utc>) -> Result<Vec<Hazard>> {
        weather::sigmets(bounds, when)
    }

    fn conflict_zones(&self, when: DateTime<Utc>) -> Vec<Hazard> {
        route::airspace::conflict_zones(when)
    }

    fn notam_hazards(&self, bounds: Bounds, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<Hazard>> {
        route::airspace::notam_hazards(bounds, from, to)
    }

    fn choose_alternates(&self, destination: &Airport, eta: DateTime<Utc>, count: usize) -> Result<Vec<Alternate>> {
        weather::choose_alternates(destination, eta, count)
    }

    fn rad(&self, when: DateTime<Utc>) -> Result<Rad> {
        Rad::load(when)
    }

    fn oceanic_tracks(&self, when: DateTime<Utc>) -> Result<Vec<Track>> {
        route::oceanic::fetch_tracks(when)
    }

    fn network(&self, tracks: &[Track]) -> route::Graph {
        let mut g = route::Graph::from_navdata();
        route::oceanic::add_tracks(&mut g, tracks);
        g
    }

    fn plan_routes(&self, graph: &route::Graph, req: &RouteRequest<'_>, most: usize) -> Result<Vec<FiledRoute>> {
        route::plan_routes(graph, req, most)
    }

    fn cost_model<'a>(&self, spec: &'a AircraftSpec, air: &'a dyn WindField, zfw_kg: f64, trip_nm: f64, cost_index: Option<f64>) -> Result<Box<dyn CostModel + 'a>> {
        perf::cost_model(spec, air, zfw_kg, trip_nm, cost_index)
    }

    fn perf_plan(&self, req: &PerfRequest<'_>) -> Result<PerfPlan> {
        perf::plan(req)
    }

    fn fir_crossings(&self, route: &FiledRoute) -> Vec<FirCrossing> {
        route::airspace::fir_crossings(route)
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! A `Providers` with fixed, deterministic answers, so `ofp::dispatch` can be tested
    //! end to end without the network, the simulator's navigation data, or the other four
    //! parts being built yet. Two invented airports six hundred miles apart, one aircraft
    //! type, and a great-circle route between them with one intermediate fix.

    use super::*;
    use crate::dispatch::{self, Air, FuelBreakdown, PointKind, Waypoint, Weights};
    use std::sync::Mutex;

    pub const ORIGIN: &str = "LPPT";
    pub const DESTINATION: &str = "LEMD";
    pub const AIRCRAFT: &str = "A20N";

    fn origin() -> Airport {
        Airport { icao: ORIGIN.to_string(), name: "Lisbon".to_string(), pos: (38.7813, -9.1359), elevation_ft: 374.0 }
    }

    fn destination() -> Airport {
        Airport { icao: DESTINATION.to_string(), name: "Madrid-Barajas".to_string(), pos: (40.4936, -3.5668), elevation_ft: 2001.0 }
    }

    fn alternate_airport() -> Airport {
        Airport { icao: "LEBL".to_string(), name: "Barcelona".to_string(), pos: (41.2971, 2.0785), elevation_ft: 12.0 }
    }

    fn spec() -> AircraftSpec {
        AircraftSpec {
            icao_type: AIRCRAFT.to_string(),
            name: "A320neo".to_string(),
            engine: "LEAP-1A".to_string(),
            engines: 2,
            oew_kg: 42600.0,
            mzfw_kg: 62800.0,
            mtow_kg: 79000.0,
            mlw_kg: 67400.0,
            max_fuel_kg: 23859.0,
            ceiling_ft: 39800.0,
            mmo: 0.82,
            vmo_kt: 350.0,
            cruise_mach: 0.78,
            etops_minutes: None,
            one_engine_tas_kt: None,
        }
    }

    /// A route with one intermediate fix, at whatever level the request asks for.
    fn route_between(from: &Airport, to: &Airport, cruise_ft: f64, off_block: DateTime<Utc>) -> FiledRoute {
        let mid = dispatch::along(from.pos, to.pos, 0.5);
        FiledRoute {
            origin: from.clone(),
            destination: to.clone(),
            dep_runway: None,
            sid: None,
            sid_transition: None,
            star: None,
            star_transition: None,
            arr_runway: None,
            approach: None,
            points: vec![
                Waypoint::new(from.icao.clone(), from.pos, "DCT", PointKind::Airport),
                Waypoint::new("MIDPT", mid, "DCT", PointKind::Enroute),
                Waypoint::new(to.icao.clone(), to.pos, "", PointKind::Airport),
            ],
            cruise_ft,
            off_block,
        }
    }

    /// Fails every call whose name is in `fail`, so a test can exercise the "degrade to a
    /// warning" path for exactly the provider it cares about.
    #[derive(Default)]
    pub struct FakeProviders {
        pub fail: Mutex<Vec<&'static str>>,
        /// When set, `perf_plan` fails so `ofp::dispatch`'s own fallback is exercised.
        pub no_route: bool,
    }

    impl FakeProviders {
        pub fn new() -> FakeProviders {
            FakeProviders::default()
        }

        pub fn failing(names: &[&'static str]) -> FakeProviders {
            FakeProviders { fail: Mutex::new(names.to_vec()), no_route: false }
        }

        fn fails(&self, name: &'static str) -> bool {
            self.fail.lock().unwrap().contains(&name)
        }
    }

    impl Providers for FakeProviders {
        fn airport(&self, icao: &str) -> Option<Airport> {
            match icao.to_uppercase().as_str() {
                ORIGIN => Some(origin()),
                DESTINATION => Some(destination()),
                "LEBL" => Some(alternate_airport()),
                _ => None,
            }
        }

        fn airports_near(&self, at: LatLon, radius_nm: f64, _min_runway_ft: f64) -> Vec<(Airport, f64)> {
            [origin(), destination(), alternate_airport()].into_iter().map(|a| (a.clone(), dispatch::distance_nm(at, a.pos))).filter(|(_, d)| *d <= radius_nm).collect()
        }

        fn aircraft_spec(&self, icao_type: &str) -> Result<AircraftSpec> {
            if self.fails("aircraft_spec") || !icao_type.eq_ignore_ascii_case(AIRCRAFT) {
                anyhow::bail!("no such aircraft");
            }
            Ok(spec())
        }

        fn wind_field(&self, _corridor: &[Bounds], _when: DateTime<Utc>) -> Result<Arc<dyn WindField>> {
            if self.fails("wind_field") {
                anyhow::bail!("no forecast today");
            }
            struct Tail;
            impl WindField for Tail {
                fn air(&self, _at: LatLon, alt_ft: f64, _when: DateTime<Utc>) -> Air {
                    Air { wind_from_deg: 270.0, wind_kt: 40.0, temp_c: dispatch::isa_temp_c(alt_ft) }
                }
            }
            Ok(Arc::new(Tail))
        }

        fn metar(&self, icao: &str) -> Result<Metar> {
            if self.fails("metar") {
                anyhow::bail!("no METAR");
            }
            Ok(Metar { station: icao.to_string(), raw: format!("{icao} 010000Z 27008KT 9999 FEW030 18/10 Q1015"), wind_from_deg: Some(270.0), wind_kt: 8.0, ..Default::default() })
        }

        fn taf(&self, icao: &str) -> Result<Taf> {
            if self.fails("taf") {
                anyhow::bail!("no TAF");
            }
            Ok(Taf { station: icao.to_string(), raw: format!("{icao} 010000Z 0100/0206 27010KT 9999 FEW030"), ..Default::default() })
        }

        fn sigmets(&self, _bounds: Bounds, _when: DateTime<Utc>) -> Result<Vec<Hazard>> {
            if self.fails("sigmets") {
                anyhow::bail!("no SIGMET feed");
            }
            Ok(Vec::new())
        }

        fn conflict_zones(&self, _when: DateTime<Utc>) -> Vec<Hazard> {
            Vec::new()
        }

        fn notam_hazards(&self, _bounds: Bounds, _from: DateTime<Utc>, _to: DateTime<Utc>) -> Result<Vec<Hazard>> {
            if self.fails("notam_hazards") {
                anyhow::bail!("no NOTAM feed");
            }
            Ok(Vec::new())
        }

        fn choose_alternates(&self, _destination: &Airport, _eta: DateTime<Utc>, _count: usize) -> Result<Vec<Alternate>> {
            if self.fails("choose_alternates") {
                anyhow::bail!("no alternates worked out");
            }
            Ok(vec![Alternate { airport: alternate_airport(), distance_nm: 260.0, reason: "nearest suitable".to_string() }])
        }

        fn rad(&self, _when: DateTime<Utc>) -> Result<Rad> {
            anyhow::bail!("the RAD is not built yet")
        }

        fn oceanic_tracks(&self, _when: DateTime<Utc>) -> Result<Vec<Track>> {
            anyhow::bail!("oceanic tracks are not built yet")
        }

        fn network(&self, _tracks: &[Track]) -> route::Graph {
            route::Graph::default()
        }

        fn plan_routes(&self, _graph: &route::Graph, req: &RouteRequest<'_>, most: usize) -> Result<Vec<FiledRoute>> {
            if self.no_route || self.fails("plan_routes") {
                anyhow::bail!("no route");
            }
            let level = req.levels_ft.last().copied().unwrap_or(req.cruise_ft).max(req.cruise_ft);
            Ok(vec![route_between(req.origin, req.destination, level, req.off_block); most.max(1)])
        }

        fn cost_model<'a>(&self, spec: &'a AircraftSpec, air: &'a dyn WindField, _zfw_kg: f64, _trip_nm: f64, _cost_index: Option<f64>) -> Result<Box<dyn CostModel + 'a>> {
            if self.fails("cost_model") {
                anyhow::bail!("no performance model yet");
            }
            Ok(Box::new(dispatch::SimpleCost { tas_kt: 440.0, kg_per_hour: 2400.0, ceiling_ft: spec.ceiling_ft, air, max_tailwind_kt: 150.0 }))
        }

        fn perf_plan(&self, req: &PerfRequest<'_>) -> Result<PerfPlan> {
            if self.fails("perf_plan") {
                anyhow::bail!("aircraft performance is not built yet");
            }
            let nm = req.route.distance_nm();
            let block_kg = 4200.0;
            // A flat profile, but a profile: every renderer's navigation log, and every export
            // built from one, is only exercised if the fake hands back the points a real
            // performance model would. A plan with no profile prints no log, and a log that is
            // never printed in a test is a log whose columns nothing checks.
            let mut profile = Vec::new();
            let (mut dist_nm, mut minutes) = (0.0, 0.0);
            for (i, w) in req.route.points.iter().enumerate() {
                if i > 0 {
                    let prev = &req.route.points[i - 1];
                    dist_nm += dispatch::distance_nm(prev.pos, w.pos);
                    minutes = dist_nm / 440.0 * 60.0;
                }
                let used = block_kg * 0.85 * (dist_nm / nm.max(1.0));
                profile.push(dispatch::ProfilePoint {
                    ident: w.ident.clone(),
                    kind: if i == 1 { dispatch::ProfileKind::TopOfClimb } else { dispatch::ProfileKind::Waypoint },
                    pos: w.pos,
                    via: w.via.clone(),
                    alt_ft: req.route.cruise_ft,
                    dist_nm,
                    time_min: minutes,
                    fuel_used_kg: used,
                    fuel_remaining_kg: block_kg - used,
                    gross_kg: req.spec.oew_kg + req.payload_kg + block_kg - used,
                    track_true_deg: if i == 0 { 0.0 } else { dispatch::bearing_deg(req.route.points[i - 1].pos, w.pos) },
                    tas_kt: 440.0,
                    gs_kt: 428.0,
                    mach: 0.78,
                    air: dispatch::Air { wind_from_deg: 270.0, wind_kt: 25.0, temp_c: -48.0 },
                    mora_ft: Some(6_500.0),
                });
            }
            Ok(PerfPlan {
                profile,
                fuel: FuelBreakdown { taxi_kg: 200.0, trip_kg: block_kg * 0.85, contingency_kg: block_kg * 0.05, alternate_kg: 500.0, final_reserve_kg: 300.0, extra_kg: 0.0, tanker_kg: 0.0, takeoff_kg: block_kg, block_kg: block_kg + 200.0, landing_kg: block_kg * 0.15 + 300.0 },
                weights: Weights { oew_kg: req.spec.oew_kg, payload_kg: req.payload_kg, zfw_kg: req.spec.oew_kg + req.payload_kg, tow_kg: req.spec.oew_kg + req.payload_kg + block_kg, lw_kg: req.spec.oew_kg + req.payload_kg + block_kg * 0.15, max_zfw_kg: req.spec.mzfw_kg, max_tow_kg: req.spec.mtow_kg, max_lw_kg: req.spec.mlw_kg, limited_by: None },
                step_climbs: Vec::new(),
                avg_wind_kt: -12.0,
                avg_isa_dev: 3.0,
                equal_time_points: Vec::new(),
                no_return: None,
                alternate: None,
                warnings: vec![format!("fake performance model: a flat {nm:.0} nm estimate, not a flown profile")],
            })
        }

        fn fir_crossings(&self, _route: &FiledRoute) -> Vec<FirCrossing> {
            vec![FirCrossing { ident: "LPPC".to_string(), name: "Lisbon FIR".to_string(), entry: origin().pos, entry_nm: 0.0, exit_nm: 120.0 }, FirCrossing { ident: "LECM".to_string(), name: "Madrid FIR".to_string(), entry: destination().pos, entry_nm: 120.0, exit_nm: 260.0 }]
        }
    }
}
