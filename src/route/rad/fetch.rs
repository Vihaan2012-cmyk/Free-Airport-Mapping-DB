//! Getting hold of the workbook: EUROCONTROL's Network Manager publishes the RAD, every
//! AIRAC cycle and every amendment within it, as one Excel workbook — one tab per annex
//! — from `nm.eurocontrol.int/RAD/`, free, with no login. It calls itself a "Rolling RAD
//! Document": every row carries its own Valid From/Valid Until dates, so one downloaded
//! edition already carries the amendments due in cycles still to come, and there is no
//! need to work out which AIRAC's own file to ask for — the current edition is enough
//! for any `when` this crate is likely to be asked about.

use crate::cache::Cache;
use crate::sources::http::Http;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

const INDEX_URL: &str = "https://www.nm.eurocontrol.int/RAD/";
const HOW_TO_GET_IT: &str = "download the current edition (an .xlsx, no login required) from https://www.nm.eurocontrol.int/RAD/ and either set the AMDB_RAD_FILE environment variable to its path, or pass it to Rad::from_path";

/// The workbook's bytes: from `AMDB_RAD_FILE` if it is set — a file, or a folder to
/// find one in — otherwise downloaded from Eurocontrol and cached.
pub(crate) fn workbook_bytes(http: &Http, cache: &Cache) -> Result<Vec<u8>> {
    if let Ok(path) = std::env::var("AMDB_RAD_FILE") {
        return read_path(Path::new(&path));
    }
    fetch(http, cache)
}

/// A path given directly, by `Rad::from_path` or by `AMDB_RAD_FILE`: the file itself,
/// or the first `.xlsx` in a folder.
pub(crate) fn read_path(path: &Path) -> Result<Vec<u8>> {
    let file = if path.is_dir() {
        std::fs::read_dir(path)
            .with_context(|| format!("reading the folder {}", path.display()))?
            .flatten()
            .map(|e| e.path())
            .find(|f| f.extension().is_some_and(|e| e.eq_ignore_ascii_case("xlsx")))
            .ok_or_else(|| anyhow::anyhow!("{} has no .xlsx in it; {HOW_TO_GET_IT}", path.display()))?
    } else {
        path.to_path_buf()
    };
    std::fs::read(&file).with_context(|| format!("reading {}; {HOW_TO_GET_IT}", file.display()))
}

fn fetch(http: &Http, cache: &Cache) -> Result<Vec<u8>> {
    let url = discover_url(http, cache).with_context(|| format!("finding the current RAD workbook on {INDEX_URL}; {HOW_TO_GET_IT}"))?;
    let name = url.rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or("RAD.xlsx");
    cache.get_or_fetch_bytes(&format!("rad/{name}"), || http.get_bytes(&url)).with_context(|| format!("downloading {url}; if Eurocontrol now asks for a login here, {HOW_TO_GET_IT}"))
}

/// The current edition's link, scraped off the RAD's own index page: it is not
/// published at a predictable URL — a version number Eurocontrol assigns is folded into
/// the file name — so the page that lists it is read first.
fn discover_url(http: &Http, cache: &Cache) -> Result<String> {
    let mut index_cache = cache.clone();
    index_cache.max_age = Some(std::time::Duration::from_secs(6 * 3600));
    let html = index_cache.get_or_fetch_text("rad/index.html", || http.get_text(INDEX_URL))?;
    let marker = "assets/AIRAC-RAD_DATA/CURRENT_AIRAC/RAD_";
    let start = html.find(marker).ok_or_else(|| anyhow::anyhow!("no link to the current edition found on the page"))?;
    let rest = &html[start..];
    let end = rest.find(['"', '\'', ' ', ')']).unwrap_or(rest.len());
    Ok(format!("{INDEX_URL}{}", &rest[..end]))
}

/// Where this crate keeps its own copy of the workbook between runs: refetched a few
/// times a day, since Eurocontrol amends the rolling document within a cycle, not only
/// at the start of one.
pub(crate) fn default_cache() -> Cache {
    let base = std::env::var("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."));
    let mut cache = Cache::new(Some(base.join("amdbgen").join("rad")), false, false);
    cache.max_age = Some(std::time::Duration::from_secs(6 * 3600));
    cache
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_with_no_xlsx_says_where_to_get_one() {
        let dir = std::env::temp_dir().join(format!("amdbgen-rad-fetch-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let err = read_path(&dir).unwrap_err();
        assert!(format!("{err:#}").contains("nm.eurocontrol.int"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_file_says_where_to_get_one() {
        let err = read_path(Path::new("no such file at all.xlsx")).unwrap_err();
        assert!(format!("{err:#}").contains("nm.eurocontrol.int"));
    }

    #[test]
    fn the_current_edition_link_is_found_on_the_index_page() {
        let html = r#"<a href="assets/AIRAC-RAD_DATA/CURRENT_AIRAC/RAD_2609_v1_21.xlsx">Latest</a>"#;
        let start = html.find("assets/AIRAC-RAD_DATA/CURRENT_AIRAC/RAD_").unwrap();
        let rest = &html[start..];
        let end = rest.find(['"', '\'', ' ', ')']).unwrap();
        assert_eq!(&rest[..end], "assets/AIRAC-RAD_DATA/CURRENT_AIRAC/RAD_2609_v1_21.xlsx");
    }
}
