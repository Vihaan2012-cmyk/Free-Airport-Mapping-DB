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
use std::sync::{Arc, Condvar, Mutex, OnceLock};

const BASE: &str = "https://copernicus-dem-30m.s3.amazonaws.com";
/// Enough for the header and the tile index of a GLO-30 file.
const HEADER_BYTES: u64 = 256 * 1024;
/// What is asked for first: a GLO-30 header and its tile index take about 1.3 KB, and the
/// whole quarter megabyte, a square at a time, was most of what a chart run downloaded.
const FIRST_BYTES: u64 = 16 * 1024;

/// How far into the file the header and its tile index reach, or None when the chain of
/// image directories leaves `d` before it ends.
fn header_extent(d: &[u8]) -> Option<usize> {
    if d.len() < 8 || !(&d[0..2] == b"II" || &d[0..2] == b"MM") {
        return None;
    }
    let r = Reader { d, little: &d[0..2] == b"II" };
    let mut end = 8;
    let mut at = r.u32(4) as usize;
    for _ in 0..16 {
        if at == 0 {
            return Some(end);
        }
        if at + 2 > d.len() {
            return None;
        }
        let n = r.u16(at) as usize;
        let after = at + 2 + n * 12;
        if after + 4 > d.len() {
            return None;
        }
        end = end.max(after + 4);
        for i in 0..n {
            let e = at + 2 + i * 12;
            let size = match r.u16(e + 2) {
                1 | 2 | 6 | 7 => 1,
                3 | 8 => 2,
                4 | 9 | 11 => 4,
                5 | 10 | 12 | 16 | 17 => 8,
                _ => 1,
            } * r.u32(e + 4) as usize;
            if size > 4 {
                end = end.max(r.u32(e + 8) as usize + size);
            }
        }
        at = r.u32(after) as usize;
    }
    Some(end)
}

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

/// Every level of detail the file holds, finest first.
///
/// These files carry reduced copies of themselves: a degree of ground is stored at 3600
/// pixels across, and again at 1800, 900 and 450. Reading the smallest copy that is
/// still finer than what was asked for turns the terrain around an airport from
/// megabytes into a few kilobytes, which is the difference between a chart taking two
/// seconds and taking a tenth of one.
fn parse_layouts(d: &[u8]) -> Result<Vec<Layout>> {
    let mut out = Vec::new();
    let mut at = {
        if d.len() < 8 {
            return Err(anyhow!("short header"));
        }
        let little = &d[0..2] == b"II";
        let r = Reader { d, little };
        if r.u16(2) != 42 {
            return Err(anyhow!("not a classic TIFF"));
        }
        r.u32(4) as usize
    };
    // The finest image places the ground; the reduced copies inherit that placement.
    let mut placement: Option<((f64, f64), (f64, f64), u32)> = None;
    while at != 0 && at + 2 <= d.len() && out.len() < 12 {
        let (layout, next) = parse_ifd(d, at, placement)?;
        if placement.is_none() {
            placement = Some((layout.origin, layout.scale, layout.width));
        }
        out.push(layout);
        at = next;
    }
    if out.is_empty() {
        return Err(anyhow!("no images in the file"));
    }
    Ok(out)
}

fn parse_ifd(d: &[u8], ifd: usize, placement: Option<((f64, f64), (f64, f64), u32)>) -> Result<(Layout, usize)> {
    if d.len() < 8 {
        return Err(anyhow!("short header"));
    }
    let little = &d[0..2] == b"II";
    let r = Reader { d, little };
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
    let next = {
        let e = ifd + 2 + n * 12;
        if e + 4 <= d.len() {
            r.u32(e) as usize
        } else {
            0
        }
    };
    let one = |t: u16| tags.get(&t).and_then(|v| v.first().copied());
    let width = one(256).unwrap_or(0.0) as u32;
    // A reduced copy carries no placement of its own; it covers the same ground as the
    // full-sized image, with pixels as much larger as it is smaller.
    let (origin, scale) = match (tags.get(&33550), tags.get(&33922), placement) {
        (Some(pixel_scale), Some(tiepoint), _) if pixel_scale.len() >= 2 && tiepoint.len() >= 5 => ((tiepoint[3], tiepoint[4]), (pixel_scale[0], pixel_scale[1])),
        (_, _, Some((origin, scale, full_width))) if width > 0 => {
            let factor = full_width as f64 / width as f64;
            (origin, (scale.0 * factor, scale.1 * factor))
        }
        _ => return Err(anyhow!("no geographic placement in the file")),
    };
    let layout = Layout {
        width,
        height: one(257).unwrap_or(0.0) as u32,
        tile_w: one(322).unwrap_or(0.0) as u32,
        tile_h: one(323).unwrap_or(0.0) as u32,
        compression: one(259).unwrap_or(1.0) as u16,
        predictor: one(317).unwrap_or(1.0) as u16,
        bits: one(258).unwrap_or(32.0) as u16,
        sample_format: one(339).unwrap_or(1.0) as u16,
        tile_offsets: tags.get(&324).map(|v| v.iter().map(|x| *x as u64).collect()).unwrap_or_default(),
        tile_bytes: tags.get(&325).map(|v| v.iter().map(|x| *x as u64).collect()).unwrap_or_default(),
        origin,
        scale,
    };
    Ok((layout, next))
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

    /// Every height the model has over the first 3,000 feet of a runway, sorted.
    ///
    /// What a touchdown zone elevation should be taken from is not obvious: the survey
    /// is the highest point of that stretch of pavement, but this model is a model of the
    /// *surface*, so the highest sample over an airport is as likely to be a hangar roof
    /// or a line of trees beside the strip as the runway itself.
    pub fn touchdown_zone_samples(&self, thr_lat: f64, thr_lon: f64, bearing_deg: f64) -> Vec<f64> {
        const ZONE_M: f64 = 914.4; // 3,000 feet
        let b = bearing_deg.to_radians();
        let (m_lat, m_lon) = (111_320.0, 111_320.0 * thr_lat.to_radians().cos().max(0.05));
        let mut out = Vec::new();
        let mut steps = 0;
        while (steps as f64) * 30.0 <= ZONE_M {
            let along = steps as f64 * 30.0;
            for across in [-15.0, 0.0, 15.0] {
                let north = along * b.cos() - across * b.sin();
                let east = along * b.sin() + across * b.cos();
                if let Some(m) = self.height_at(thr_lat + north / m_lat, thr_lon + east / m_lon) {
                    out.push(m / 0.3048);
                }
            }
            steps += 1;
        }
        out.sort_by(f64::total_cmp);
        out
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
    /// Finest first.
    levels: Vec<Layout>,
}

impl Source {
    /// The smallest copy whose pixels are about the spacing asked for.
    ///
    /// A little coarser is fine and a great deal cheaper: the copies step by a factor of
    /// two, so insisting on strictly finer than asked for lands on one four times the
    /// size whenever the spacing falls just below a level.
    fn level_for(&self, step_deg: f64) -> &Layout {
        self.levels
            .iter()
            .filter(|l| l.scale.0 <= step_deg * 1.3 && !l.tile_offsets.is_empty() && l.tile_w > 0)
            .next_back()
            .unwrap_or(&self.levels[0])
    }
}

/// What a one-degree square has: the file's header, or nothing where the model has no
/// file at all, which is the open sea.
enum Square {
    Land(Arc<Source>),
    Sea,
}

/// Every square opened so far in this run, so a header is read and parsed once rather
/// than once a chart.
fn squares() -> &'static Mutex<HashMap<(i32, i32), Arc<Square>>> {
    static SQUARES: OnceLock<Mutex<HashMap<(i32, i32), Arc<Square>>>> = OnceLock::new();
    SQUARES.get_or_init(Default::default)
}

/// Downloads at once in a run. On a slow line a few finish while many, sharing it, each
/// run past the timeout, are tried again and fail again.
const DOWNLOADS_AT_ONCE: usize = 8;

/// Part of a file, through the run's own client: a tile is megabytes, and the callers'
/// two-minute limit is short for one on a slow line.
fn fetch_range(url: &str, start: u64, len: u64) -> Result<Vec<u8>> {
    static GATE: OnceLock<(Mutex<usize>, Condvar)> = OnceLock::new();
    static HTTP: OnceLock<Http> = OnceLock::new();
    let (busy, freed) = GATE.get_or_init(|| (Mutex::new(0), Condvar::new()));
    {
        let mut n = busy.lock().unwrap();
        while *n >= DOWNLOADS_AT_ONCE {
            n = freed.wait(n).unwrap();
        }
        *n += 1;
    }
    let got = HTTP.get_or_init(|| Http::new(600, 0)).get_range(url, start, len);
    *busy.lock().unwrap() -= 1;
    freed.notify_one();
    got
}

thread_local! {
    /// Squares and tiles this thread could not read, since it last asked.
    static MISSED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many squares and tiles this thread failed to read since it last asked: a chart
/// drawn meanwhile has ground missing from it, and should be drawn again.
pub fn take_missed() -> usize {
    MISSED.with(|m| m.replace(0))
}

fn missed() {
    MISSED.with(|m| m.set(m.get() + 1));
}

/// A square's header, read once a run. A square the model has no file for is remembered
/// as sea on disk too: asked again, the server only says the same, and a chart round an
/// island used to ask it about every square of sea on the page.
fn square(_http: &Http, cache: &Cache, lat_deg: i32, lon_deg: i32) -> Arc<Square> {
    if let Some(s) = squares().lock().unwrap().get(&(lat_deg, lon_deg)) {
        return s.clone();
    }
    let name = tile_name(lat_deg, lon_deg);
    let found = match open(cache, lat_deg, lon_deg) {
        Ok(Some(src)) => Square::Land(Arc::new(src)),
        Ok(None) => Square::Sea,
        Err(e) => {
            // Not remembered: a server that failed to answer may answer next time.
            log::warn!("Copernicus DEM {name}: {e:#}");
            missed();
            return Arc::new(Square::Sea);
        }
    };
    let found = Arc::new(found);
    squares().lock().unwrap().insert((lat_deg, lon_deg), found.clone());
    found
}

/// The file's header, or None where the model has no file (the server says 404, or 403
/// as S3 does for a name it has never had).
fn open(cache: &Cache, lat_deg: i32, lon_deg: i32) -> Result<Option<Source>> {
    let name = tile_name(lat_deg, lon_deg);
    let url = format!("{BASE}/{name}/{name}.tif");
    let head = cache.get_or_fetch_bytes(&format!("copernicus/{name}.header"), || {
        let first = match fetch_range(&url, 0, FIRST_BYTES) {
            Err(e) if format!("{e:#}").contains("HTTP 404") || format!("{e:#}").contains("HTTP 403") => return Ok(Vec::new()),
            other => other?,
        };
        match header_extent(&first) {
            Some(end) if end <= first.len() => Ok(first),
            _ => fetch_range(&url, 0, HEADER_BYTES),
        }
    })?;
    if head.is_empty() {
        return Ok(None);
    }
    let levels = parse_layouts(&head).with_context(|| format!("read the header of {name}"))?;
    let layout = &levels[0];
    if layout.tile_w == 0 || layout.tile_offsets.is_empty() {
        return Err(anyhow!("{name}: not tiled as expected"));
    }
    if layout.bits != 32 || layout.sample_format != 3 {
        return Err(anyhow!("{name}: heights are not 32-bit floats"));
    }
    Ok(Some(Source { url, levels }))
}

/// Tiles decoded in this run, shared between the threads drawing charts, up to a size:
/// the charts of one airport, and of its neighbours, read the same tiles over and over,
/// and inflating one from disk costs more than drawing a page.
struct Decoded {
    tiles: HashMap<(i32, i32, usize, usize), Arc<Vec<f32>>>,
    order: std::collections::VecDeque<(i32, i32, usize, usize)>,
    bytes: usize,
}

const DECODED_LIMIT: usize = 2 << 30;

fn decoded() -> &'static Mutex<Decoded> {
    static DECODED: OnceLock<Mutex<Decoded>> = OnceLock::new();
    DECODED.get_or_init(|| Mutex::new(Decoded { tiles: HashMap::new(), order: Default::default(), bytes: 0 }))
}

/// A decoded tile, from memory when this run has read it before.
fn shared_tile(cache: &Cache, src: &Source, l: &Layout, name: &str, key: (i32, i32, usize, usize)) -> Result<Arc<Vec<f32>>> {
    if let Some(t) = decoded().lock().unwrap().tiles.get(&key) {
        return Ok(t.clone());
    }
    let t = Arc::new(tile(cache, src, l, name, key.2, key.3)?);
    let mut d = decoded().lock().unwrap();
    if d.tiles.insert(key, t.clone()).is_none() {
        d.order.push_back(key);
        d.bytes += t.len() * 4;
        // The oldest go first: the run moves on from an airport and does not come back.
        while d.bytes > DECODED_LIMIT {
            let Some(old) = d.order.pop_front() else {
                break;
            };
            if let Some(gone) = d.tiles.remove(&old) {
                d.bytes -= gone.len() * 4;
            }
        }
    }
    Ok(t)
}

fn tile(cache: &Cache, src: &Source, l: &Layout, name: &str, level: usize, index: usize) -> Result<Vec<f32>> {
    let (off, len) = (l.tile_offsets[index], l.tile_bytes[index]);
    let key = if level == 0 { format!("copernicus/{name}/{index}.tile") } else { format!("copernicus/{name}/L{level}-{index}.tile") };
    let raw = cache.get_or_fetch_bytes(&key, || fetch_range(&src.url, off, len))?;
    let mut bytes = match l.compression {
        1 => raw,
        8 | 32946 => inflate(&raw)?,
        other => return Err(anyhow!("tile compression {other} is not supported")),
    };
    Ok(heights_from(&mut bytes, l.tile_w as usize, l.tile_h as usize, l.predictor))
}

/// Whether the square a point is in has been read before (its header is on disk), so
/// the terrain round it needs little or nothing downloaded.
pub fn has_square(cache: &Cache, lat: f64, lon: f64) -> bool {
    cache.path(&format!("copernicus/{}.header", tile_name(lat.floor() as i32, lon.floor() as i32))).is_some_and(|p| p.is_file())
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
    Ok(sample(http, cache, west, north, step_lat, step_lon, width, height))
}

/// Terrain heights across one exact one-degree-by-one-degree tile, addressed by its
/// south-west corner — the shape a grid MORA quadrangle needs.
///
/// `patch` above draws a square *in kilometres* around a point, which is right for a
/// chart (an airport wants a patch a few kilometres wide regardless of latitude) but
/// wrong here: converting a fixed km radius to degrees of longitude divides by
/// `cos(latitude)`, so near the poles it would balloon into tens of degrees of longitude
/// to chase a one-degree cell. Working in exact degrees instead means each MORA cell
/// costs exactly the tile it sits on, at every latitude.
pub fn cell(http: &Http, cache: &Cache, lat_deg: i32, lon_deg: i32, step_m: f64) -> Result<Patch> {
    let (south, north) = (lat_deg as f64, lat_deg as f64 + 1.0);
    let (west, east) = (lon_deg as f64, lon_deg as f64 + 1.0);
    let mid_lat = south + 0.5;
    let step_lat = step_m / 111_320.0;
    let step_lon = step_lat / mid_lat.to_radians().cos().max(0.05);
    let width = (((east - west) / step_lon).round() as usize).max(2);
    let height = (((north - south) / step_lat).round() as usize).max(2);
    Ok(sample(http, cache, west, north, step_lat, step_lon, width, height))
}

/// The shared fetch loop behind `patch` and `cell`: walk a grid from its north-west
/// corner, opening (and caching) whichever one-degree source tile and decimated level
/// each sample falls in.
fn sample(http: &Http, cache: &Cache, west: f64, north: f64, step_lat: f64, step_lon: f64, width: usize, height: usize) -> Patch {
    // The model does not change: what is on disk is kept for good, whatever the cache
    // passed in keeps other things for. A day's limit had every tile of a run a week old
    // fetched again.
    let mut forever = cache.clone();
    forever.max_age = None;
    let cache = &forever;
    let mut heights = vec![f32::NAN; width * height];
    // The file, and which of its copies answers this spacing: both settled once per
    // one-degree square rather than once per sample, and the tiles kept here as well as
    // in the run's shared store, which is locked to be read.
    let mut sources: HashMap<(i32, i32), Option<(String, Arc<Source>, usize)>> = HashMap::new();
    let mut tiles: HashMap<(i32, i32, usize, usize), Arc<Vec<f32>>> = HashMap::new();
    let mut failed: std::collections::HashSet<(i32, i32, usize, usize)> = std::collections::HashSet::new();

    for row in 0..height {
        let plat = north - row as f64 * step_lat;
        for col in 0..width {
            let plon = west + col as f64 * step_lon;
            let key = (plat.floor() as i32, plon.floor() as i32);
            let entry = sources.entry(key).or_insert_with(|| match &*square(http, cache, key.0, key.1) {
                Square::Land(s) => {
                    // The smallest copy of the ground still about the spacing asked for: a
                    // chart wants a quarter of a mile, not thirty metres.
                    let level = s.levels.iter().position(|c| std::ptr::eq(c, s.level_for(step_lat))).unwrap_or(0);
                    Some((tile_name(key.0, key.1), s.clone(), level))
                }
                Square::Sea => None,
            });
            let Some((name, src, level)) = entry else {
                continue;
            };
            let level = *level;
            let l = &src.levels[level];
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
            // A tile that could not be read is not asked for again for every sample in it:
            // each ask is a download tried four times.
            if failed.contains(&(key.0, key.1, level, index)) {
                continue;
            }
            let data = match tiles.entry((key.0, key.1, level, index)) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(v) => match shared_tile(cache, src, l, name, (key.0, key.1, level, index)) {
                    Ok(t) => v.insert(t),
                    Err(e) => {
                        log::warn!("Copernicus DEM {name} tile {index}: {e:#}");
                        missed();
                        failed.insert((key.0, key.1, level, index));
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
    Patch { west, north, step_lon, step_lat, width, height, heights }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_header_says_how_far_it_reaches() {
        // Little-endian TIFF, one directory at 8 with one entry: 4 longs at offset 26.
        let mut d = vec![0u8; 64];
        d[0..4].copy_from_slice(b"II* ");
        d[4..8].copy_from_slice(&8u32.to_le_bytes());
        d[8..10].copy_from_slice(&1u16.to_le_bytes());
        d[10..12].copy_from_slice(&324u16.to_le_bytes());
        d[12..14].copy_from_slice(&4u16.to_le_bytes());
        d[14..18].copy_from_slice(&4u32.to_le_bytes());
        d[18..22].copy_from_slice(&26u32.to_le_bytes());
        assert_eq!(header_extent(&d), Some(42));
        assert_eq!(header_extent(&d[..20]), None, "the directory runs past what was read");
        d[22..26].copy_from_slice(&1000u32.to_le_bytes());
        assert_eq!(header_extent(&d), None, "a next directory beyond what was read");
    }

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
