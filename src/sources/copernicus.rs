//! Terrain heights from the Copernicus DEM (GLO-30), the free global 30 m elevation
//! model, read straight from its public copy on AWS.
//!
//! The files are cloud-optimised GeoTIFFs: a header, then the image cut into tiles that
//! can be fetched one at a time. An airport needs a patch a few kilometres across, so
//! rather than download a whole 1°x1° file (tens of megabytes) this reads the header and
//! only the tiles the patch covers, a few hundred kilobytes.
//!
//! The model is a surface model: it includes buildings and trees, so it is right for
//! drawing terrain around an airport and wrong for taking runway elevations from.

use crate::cache::Cache;
use crate::sources::http::Http;
use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::io::Read;

const BASE: &str = "https://copernicus-dem-30m.s3.amazonaws.com";
/// Enough for the header and the tile index of a GLO-30 file.
const HEADER_BYTES: u64 = 256 * 1024;

/// The file covering a whole degree of latitude and longitude.
fn tile_name(lat_deg: i32, lon_deg: i32) -> String {
    let ns = if lat_deg < 0 { 'S' } else { 'N' };
    let ew = if lon_deg < 0 { 'W' } else { 'E' };
    format!("Copernicus_DSM_COG_10_{}{:02}_00_{}{:03}_00_DEM", ns, lat_deg.abs(), ew, lon_deg.abs())
}

/// What the GeoTIFF header says about the image.
#[derive(Debug, Clone)]
struct Layout {
    width: u32,
    height: u32,
    tile_w: u32,
    tile_h: u32,
    compression: u16,
    predictor: u16,
    bits: u16,
    sample_format: u16,
    tile_offsets: Vec<u64>,
    tile_bytes: Vec<u64>,
    /// Longitude and latitude of the top-left corner, and the degrees per pixel.
    origin: (f64, f64),
    scale: (f64, f64),
}

struct Reader<'a> {
    d: &'a [u8],
    little: bool,
}

impl<'a> Reader<'a> {
    fn u16(&self, at: usize) -> u16 {
        let b = [self.d[at], self.d[at + 1]];
        if self.little {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        }
    }
    fn u32(&self, at: usize) -> u32 {
        let b = [self.d[at], self.d[at + 1], self.d[at + 2], self.d[at + 3]];
        if self.little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        }
    }
    fn f64(&self, at: usize) -> f64 {
        let mut b = [0u8; 8];
        b.copy_from_slice(&self.d[at..at + 8]);
        if self.little {
            f64::from_le_bytes(b)
        } else {
            f64::from_be_bytes(b)
        }
    }
    /// One tag's values as numbers, following the pointer when they do not fit inline.
    fn values(&self, kind: u16, count: usize, at_value: usize) -> Vec<f64> {
        let size = match kind {
            1 | 2 | 6 | 7 => 1,
            3 | 8 => 2,
            4 | 9 | 11 => 4,
            5 | 10 | 12 | 16 | 17 => 8,
            _ => return Vec::new(),
        };
        let total = size * count;
        let base = if total <= 4 { at_value } else { self.u32(at_value) as usize };
        let mut out = Vec::new();
        for i in 0..count {
            let at = base + i * size;
            if at + size > self.d.len() {
                break;
            }
            out.push(match kind {
                3 => self.u16(at) as f64,
                4 => self.u32(at) as f64,
                12 => self.f64(at),
                16 => {
                    let lo = self.u32(at) as u64;
                    let hi = self.u32(at + 4) as u64;
                    (if self.little { (hi << 32) | lo } else { (lo << 32) | hi }) as f64
                }
                _ => self.u32(at) as f64,
            });
        }
        out
    }
}

fn parse_layout(d: &[u8]) -> Result<Layout> {
    if d.len() < 8 {
        return Err(anyhow!("short header"));
    }
    let little = &d[0..2] == b"II";
    let r = Reader { d, little };
    if r.u16(2) != 42 {
        return Err(anyhow!("not a classic TIFF"));
    }
    let ifd = r.u32(4) as usize;
    if ifd + 2 > d.len() {
        return Err(anyhow!("header cut short"));
    }
    let n = r.u16(ifd) as usize;
    let mut tags: HashMap<u16, Vec<f64>> = HashMap::new();
    for i in 0..n {
        let e = ifd + 2 + i * 12;
        if e + 12 > d.len() {
            break;
        }
        let tag = r.u16(e);
        let kind = r.u16(e + 2);
        let count = r.u32(e + 4) as usize;
        tags.insert(tag, r.values(kind, count, e + 8));
    }
    let one = |t: u16| tags.get(&t).and_then(|v| v.first().copied());
    let pixel_scale = tags.get(&33550).cloned().unwrap_or_default();
    let tiepoint = tags.get(&33922).cloned().unwrap_or_default();
    if pixel_scale.len() < 2 || tiepoint.len() < 5 {
        return Err(anyhow!("no geographic placement in the file"));
    }
    Ok(Layout {
        width: one(256).unwrap_or(0.0) as u32,
        height: one(257).unwrap_or(0.0) as u32,
        tile_w: one(322).unwrap_or(0.0) as u32,
        tile_h: one(323).unwrap_or(0.0) as u32,
        compression: one(259).unwrap_or(1.0) as u16,
        predictor: one(317).unwrap_or(1.0) as u16,
        bits: one(258).unwrap_or(32.0) as u16,
        sample_format: one(339).unwrap_or(1.0) as u16,
        tile_offsets: tags.get(&324).map(|v| v.iter().map(|x| *x as u64).collect()).unwrap_or_default(),
        tile_bytes: tags.get(&325).map(|v| v.iter().map(|x| *x as u64).collect()).unwrap_or_default(),
        origin: (tiepoint[3], tiepoint[4]),
        scale: (pixel_scale[0], pixel_scale[1]),
    })
}

fn inflate(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(data).read_to_end(&mut out).context("decompress tile")?;
    Ok(out)
}

/// Undo the differencing the file was written with, and hand back the heights.
///
/// Predictor 2 subtracts each sample from the one before it. Predictor 3, which the
/// Copernicus files use, stores a row's bytes grouped by significance (all the first
/// bytes of every height, then all the second, and so on) and differences them one byte
/// at a time. That packs far better, and means the heights only reappear once the row
/// is accumulated and put back in order.
fn heights_from(buf: &mut [u8], width: usize, height: usize, predictor: u16) -> Vec<f32> {
    const BPS: usize = 4;
    let mut out = Vec::with_capacity(width * height);
    for row in 0..height {
        let base = row * width * BPS;
        let end = base + width * BPS;
        if end > buf.len() {
            break;
        }
        match predictor {
            2 => {
                for s in 1..width {
                    let (p, h) = (base + (s - 1) * BPS, base + s * BPS);
                    let prev = f32::from_le_bytes(buf[p..p + BPS].try_into().unwrap());
                    let here = f32::from_le_bytes(buf[h..h + BPS].try_into().unwrap());
                    buf[h..h + BPS].copy_from_slice(&(prev + here).to_le_bytes());
                }
                out.extend(buf[base..end].chunks_exact(BPS).map(|c| f32::from_le_bytes(c.try_into().unwrap())));
            }
            3 => {
                // One byte at a time across the row, not one sample at a time.
                for i in base + 1..end {
                    buf[i] = buf[i].wrapping_add(buf[i - 1]);
                }
                // Bytes come out most significant first, so each sample reads big-endian.
                for s in 0..width {
                    let b = [buf[base + s], buf[base + width + s], buf[base + 2 * width + s], buf[base + 3 * width + s]];
                    out.push(f32::from_be_bytes(b));
                }
            }
            _ => out.extend(buf[base..end].chunks_exact(BPS).map(|c| f32::from_le_bytes(c.try_into().unwrap()))),
        }
    }
    out
}

/// A patch of terrain: heights in metres on a regular latitude/longitude grid.
#[derive(Debug, Clone)]
pub struct Patch {
    pub west: f64,
    pub north: f64,
    /// Degrees per sample.
    pub step_lon: f64,
    pub step_lat: f64,
    pub width: usize,
    pub height: usize,
    /// Row-major from the north-west corner; NaN where no data.
    pub heights: Vec<f32>,
}

impl Patch {
    pub fn at(&self, row: usize, col: usize) -> f32 {
        self.heights.get(row * self.width + col).copied().unwrap_or(f32::NAN)
    }

    /// Lowest and highest height in the patch, ignoring gaps.
    pub fn range(&self) -> (f32, f32) {
        let mut lo = f32::INFINITY;
        let mut hi = f32::NEG_INFINITY;
        for h in self.heights.iter().copied().filter(|h| h.is_finite()) {
            lo = lo.min(h);
            hi = hi.max(h);
        }
        (lo, hi)
    }

    /// The height in metres at a point, interpolated between the four samples around
    /// it. `None` outside the patch or where the model has a gap.
    pub fn height_at(&self, lat: f64, lon: f64) -> Option<f64> {
        let fr = (self.north - lat) / self.step_lat;
        let fc = (lon - self.west) / self.step_lon;
        if fr < 0.0 || fc < 0.0 {
            return None;
        }
        let (r0, c0) = (fr.floor() as usize, fc.floor() as usize);
        if r0 + 1 >= self.height || c0 + 1 >= self.width {
            return None;
        }
        let (tr, tc) = (fr - r0 as f64, fc - c0 as f64);
        let corners = [
            (self.at(r0, c0) as f64, (1.0 - tr) * (1.0 - tc)),
            (self.at(r0, c0 + 1) as f64, (1.0 - tr) * tc),
            (self.at(r0 + 1, c0) as f64, tr * (1.0 - tc)),
            (self.at(r0 + 1, c0 + 1) as f64, tr * tc),
        ];
        let (sum, weight) = corners.iter().filter(|(h, _)| h.is_finite()).fold((0.0, 0.0), |(s, w), (h, k)| (s + h * k, w + k));
        (weight > 0.0).then(|| sum / weight)
    }

    /// The highest point of the first 3,000 feet of a runway from its threshold,
    /// which is what a touchdown zone elevation is. `bearing_deg` is the direction the
    /// runway points, so the strip is walked up the pavement rather than across it.
    pub fn touchdown_zone_ft(&self, thr_lat: f64, thr_lon: f64, bearing_deg: f64) -> Option<f64> {
        const ZONE_M: f64 = 914.4; // 3,000 feet
        let b = bearing_deg.to_radians();
        let (m_lat, m_lon) = (111_320.0, 111_320.0 * thr_lat.to_radians().cos().max(0.05));
        let mut highest = f64::NEG_INFINITY;
        let mut steps = 0;
        while (steps as f64) * 30.0 <= ZONE_M {
            let along = steps as f64 * 30.0;
            for across in [-15.0, 0.0, 15.0] {
                let north = along * b.cos() - across * b.sin();
                let east = along * b.sin() + across * b.cos();
                if let Some(m) = self.height_at(thr_lat + north / m_lat, thr_lon + east / m_lon) {
                    highest = highest.max(m / 0.3048);
                }
            }
            steps += 1;
        }
        highest.is_finite().then(|| highest.round())
    }

    pub fn position(&self, row: usize, col: usize) -> (f64, f64) {
        (self.north - row as f64 * self.step_lat, self.west + col as f64 * self.step_lon)
    }
}

struct Source {
    url: String,
    layout: Layout,
}

fn open(http: &Http, cache: &Cache, lat_deg: i32, lon_deg: i32) -> Result<Source> {
    let name = tile_name(lat_deg, lon_deg);
    let url = format!("{BASE}/{name}/{name}.tif");
    let head = cache.get_or_fetch_bytes(&format!("copernicus/{name}.header"), || http.get_range(&url, 0, HEADER_BYTES))?;
    let layout = parse_layout(&head).with_context(|| format!("read the header of {name}"))?;
    if layout.tile_w == 0 || layout.tile_offsets.is_empty() {
        return Err(anyhow!("{name}: not tiled as expected"));
    }
    if layout.bits != 32 || layout.sample_format != 3 {
        return Err(anyhow!("{name}: heights are not 32-bit floats"));
    }
    Ok(Source { url, layout })
}

fn tile(http: &Http, cache: &Cache, src: &Source, name: &str, index: usize) -> Result<Vec<f32>> {
    let l = &src.layout;
    let (off, len) = (l.tile_offsets[index], l.tile_bytes[index]);
    let raw = cache.get_or_fetch_bytes(&format!("copernicus/{name}/{index}.tile"), || http.get_range(&src.url, off, len))?;
    let mut bytes = match l.compression {
        1 => raw,
        8 | 32946 => inflate(&raw)?,
        other => return Err(anyhow!("tile compression {other} is not supported")),
    };
    Ok(heights_from(&mut bytes, l.tile_w as usize, l.tile_h as usize, l.predictor))
}

/// Terrain heights around a point. `radius_km` is half the width of the square patch,
/// and `step_m` how far apart the samples are.
pub fn patch(http: &Http, cache: &Cache, lat: f64, lon: f64, radius_km: f64, step_m: f64) -> Result<Patch> {
    let deg_lat = radius_km * 1000.0 / 111_320.0;
    let deg_lon = deg_lat / lat.to_radians().cos().max(0.05);
    let (north, south) = (lat + deg_lat, lat - deg_lat);
    let (west, east) = (lon - deg_lon, lon + deg_lon);
    let step_lat = step_m / 111_320.0;
    let step_lon = step_lat / lat.to_radians().cos().max(0.05);
    let width = (((east - west) / step_lon).round() as usize).max(2);
    let height = (((north - south) / step_lat).round() as usize).max(2);

    let mut heights = vec![f32::NAN; width * height];
    let mut sources: HashMap<(i32, i32), Option<(String, Source)>> = HashMap::new();
    let mut tiles: HashMap<(i32, i32, usize), Vec<f32>> = HashMap::new();

    for row in 0..height {
        let plat = north - row as f64 * step_lat;
        for col in 0..width {
            let plon = west + col as f64 * step_lon;
            let key = (plat.floor() as i32, plon.floor() as i32);
            let entry = sources.entry(key).or_insert_with(|| {
                let name = tile_name(key.0, key.1);
                match open(http, cache, key.0, key.1) {
                    Ok(s) => Some((name, s)),
                    Err(e) => {
                        log::warn!("Copernicus DEM {name}: {e:#}");
                        None
                    }
                }
            });
            let Some((name, src)) = entry else { continue };
            let l = &src.layout;
            // Pixel in the file, then which tile holds it.
            let px = ((plon - l.origin.0) / l.scale.0).floor();
            let py = ((l.origin.1 - plat) / l.scale.1).floor();
            if px < 0.0 || py < 0.0 || px >= l.width as f64 || py >= l.height as f64 {
                continue;
            }
            let (px, py) = (px as u32, py as u32);
            let across = l.width.div_ceil(l.tile_w);
            let index = ((py / l.tile_h) * across + px / l.tile_w) as usize;
            if index >= l.tile_offsets.len() {
                continue;
            }
            let data = match tiles.entry((key.0, key.1, index)) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(v) => match tile(http, cache, src, name, index) {
                    Ok(t) => v.insert(t),
                    Err(e) => {
                        log::warn!("Copernicus DEM {name} tile {index}: {e:#}");
                        continue;
                    }
                },
            };
            let within = ((py % l.tile_h) * l.tile_w + (px % l.tile_w)) as usize;
            if let Some(h) = data.get(within) {
                // The model marks gaps with a very negative number.
                heights[row * width + col] = if *h < -9000.0 { f32::NAN } else { *h };
            }
        }
    }
    Ok(Patch { west, north, step_lon, step_lat, width, height, heights })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiles_are_named_by_their_corner() {
        assert_eq!(tile_name(51, -1), "Copernicus_DSM_COG_10_N51_00_W001_00_DEM");
        assert_eq!(tile_name(-34, 151), "Copernicus_DSM_COG_10_S34_00_E151_00_DEM");
    }

    #[test]
    fn sample_differencing_is_undone() {
        let mut buf = Vec::new();
        for v in [10.0f32, 1.0, 1.0, -2.0] {
            buf.extend_from_slice(&v.to_le_bytes());
        }
        assert_eq!(heights_from(&mut buf, 4, 1, 2), vec![10.0, 11.0, 12.0, 10.0]);
    }

    #[test]
    fn byte_grouped_differencing_is_undone() {
        // Write two heights the way predictor 3 stores them, then read them back.
        let wanted = [575.25f32, 2100.5];
        let mut planes = vec![0u8; 8];
        for (s, v) in wanted.iter().enumerate() {
            for (b, byte) in v.to_be_bytes().iter().enumerate() {
                planes[b * wanted.len() + s] = *byte;
            }
        }
        for i in (1..planes.len()).rev() {
            planes[i] = planes[i].wrapping_sub(planes[i - 1]);
        }
        assert_eq!(heights_from(&mut planes, 2, 1, 3), wanted.to_vec());
    }
}
