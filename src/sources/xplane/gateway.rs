//! The X-Plane Scenery Gateway's airport data, from its copy on Hugging Face. Laminar does
//! not allow products outside X-Plane to use the Gateway's own servers, so nothing here goes
//! near them. The data itself is GPL, and `amdb-bridge export-gateway-source` writes this
//! copy from the Global Airports pack that ships with X-Plane:
//!
//! * `airports.json`  every airport: `{airports: [{icao, name, lat, lon, elevation_ft}]}`
//! * `<AB>.dat.gz`    the apt.dat of every airport whose code begins with AB

use crate::cache::Cache;
use crate::sources::http::Http;
use anyhow::Result;
use std::io::BufReader;

const SOURCE: &str = "https://huggingface.co/datasets/Royale2072/free-airport-mapping-db/resolve/main/gateway/source";

/// The file an airport's apt.dat is in: the first two characters of its code.
pub fn group_of(icao: &str) -> String {
    icao.chars().take(2).map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' }).collect()
}

/// The list of airports, as JSON.
pub fn airport_list(http: &Http, cache: &Cache) -> Result<String> {
    cache.get_or_fetch_text("gateway/source/airports.json", || http.get_text(&format!("{SOURCE}/airports.json")))
}

/// The apt.dat text of `icao`, `Ok(None)` when the Gateway's data does not have it.
pub fn fetch_aptdat(http: &Http, cache: &Cache, icao: &str) -> Result<Option<String>> {
    let icao = icao.to_uppercase();
    let group = group_of(&icao);
    let fetched = cache.get_or_fetch_bytes(&format!("gateway/source/{group}.dat.gz"), || {
        crate::term::step(Some(&icao), &format!("GET {SOURCE}/{group}.dat.gz"));
        http.get_bytes(&format!("{SOURCE}/{group}.dat.gz"))
    });
    let gz = match fetched {
        Ok(gz) => gz,
        // No airport's code begins this way.
        Err(e) if format!("{e:#}").contains("HTTP 404") => return Ok(None),
        Err(e) => return Err(e),
    };
    super::local::airport_block(BufReader::new(flate2::read::GzDecoder::new(gz.as_slice())), &icao)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_gives_its_group_whatever_it_holds() {
        assert_eq!(group_of("kjfk"), "KJ");
        assert_eq!(group_of("5TE"), "5T");
        assert_eq!(group_of("X"), "X");
        assert_eq!(group_of("A/B"), "A_");
    }

    #[test]
    fn an_airport_is_found_in_its_group_and_no_other_is() {
        let text = "I\n1200 x\n\n1 13 0 0 AAAA First\n100 a\n\n17 0 0 0 AAAB Second\n102 b\n\n99\n";
        let block = super::super::local::airport_block(text.as_bytes(), "aaab").unwrap().unwrap();
        assert!(block.contains("AAAB Second") && block.contains("102 b") && !block.contains("AAAA"));
        assert!(super::super::local::airport_block(text.as_bytes(), "AAAC").unwrap().is_none());
    }
}
