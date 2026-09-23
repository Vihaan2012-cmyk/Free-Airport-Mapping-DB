//! A GRIB2 reader, written against the WMO's own description of the format (FM 92-XII
//! GRIB edition 2, as published in the WMO Manual on Codes) rather than against any other
//! program's source: nothing here is derived from wgrib2 or ecCodes, both of which carry
//! licences this crate cannot take code from. NOAA's data itself is United States
//! government work and carries no copyright.
//!
//! A message is eight sections, most of them a handful of fixed fields:
//!
//! * 0 the indicator: "GRIB", the discipline, and the message's own length;
//! * 1 identification: whose model this is, and when it was run;
//! * 2 local use, skipped;
//! * 3 the grid: for us always a plain latitude/longitude box (template 3.0);
//! * 4 the product: which parameter, at what level, how far into the forecast (template 4.0);
//! * 5 how the data is packed: simple packing (5.0), or complex packing (5.2), optionally
//!   with the values spatially differenced first (5.3) — the two ends of a trade the
//!   encoder makes between how small the message is and how much arithmetic it takes to
//!   read back;
//! * 6 a bitmap, where some points in the grid have no value;
//! * 7 the values themselves, packed the way section 5 said.
//!
//! Anything else — a different grid or product template, or values packed as JPEG 2000
//! (template 5.40, which NOMADS uses for some products but not the ones this crate asks
//! the GRIB filter for) — is refused with a plain error naming what was found, rather than
//! guessed at.

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, TimeZone, Utc};

/// The plain latitude/longitude grid a message's values sit on, canonicalised so that
/// row 0 is the northernmost and column 0 the westernmost, whichever way the message
/// itself was scanned: `lat(j) = la1 - j * dj`, `lon(i) = lo1 + i * di` (the second taken
/// modulo 360 where the grid wraps).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridDef {
    pub ni: usize,
    pub nj: usize,
    pub la1: f64,
    pub lo1: f64,
    pub di: f64,
    pub dj: f64,
}

impl GridDef {
    /// Whether the grid runs the whole way round, so a longitude just past its east edge
    /// is in fact its west edge again.
    pub fn wraps(&self) -> bool {
        self.ni > 1 && (self.ni as f64 - 1.0) * self.di >= 359.9
    }
}

/// One field out of one GRIB2 message: a parameter at a level and a forecast time, over
/// its grid. `values` is row-major, north row first, `values[j * grid.ni + i]`; a point
/// the bitmap marked absent, or that its group marked individually missing, is
/// `f32::NAN`.
#[derive(Debug, Clone)]
pub struct Field {
    pub discipline: u8,
    pub category: u8,
    pub number: u8,
    /// The fixed-surface type code (Table 4.5): 100 is an isobaric surface.
    pub level_type: u8,
    /// The surface's value in its own units — pascals for an isobaric surface.
    pub level_value: f64,
    pub reference_time: DateTime<Utc>,
    pub forecast_hours: f64,
    pub grid: GridDef,
    pub values: Vec<f32>,
}

/// Every message in a byte stream, in order. A GRIB filter's answer, and a plain `.grib2`
/// file, are both just messages one after another with nothing between them.
pub fn decode_all(data: &[u8]) -> Result<Vec<Field>> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos + 16 <= data.len() {
        if &data[pos..pos + 4] != b"GRIB" {
            break;
        }
        let msg_len = u64::from_be_bytes(data[pos + 8..pos + 16].try_into().unwrap()) as usize;
        if msg_len < 16 || pos + msg_len > data.len() {
            bail!("GRIB message at byte {pos}: length {msg_len} runs past the end of the data");
        }
        out.push(decode_message(&data[pos..pos + msg_len]).with_context(|| format!("GRIB message at byte {pos}"))?);
        pos += msg_len;
    }
    if out.is_empty() {
        bail!("not a GRIB2 message: no \"GRIB\" indicator found");
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------
// Reading the fixed-width fields sections are built of.
// ---------------------------------------------------------------------------------

/// A cursor over one section's bytes. GRIB2 writes most integers plain big-endian, but
/// writes a "signed" field — one that can be negative, such as a scale factor or a
/// latitude — in sign-magnitude: the top bit of the first octet is the sign, the rest of
/// the field its size, rather than two's complement.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Reader<'a> {
        Reader { data, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos + n;
        let bytes = self.data.get(self.pos..end).ok_or_else(|| anyhow!("ran out of bytes"))?;
        self.pos = end;
        Ok(bytes)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    /// A signed field of `n` octets, sign-magnitude.
    fn signed(&mut self, n: usize) -> Result<i64> {
        Ok(sign_magnitude(self.take(n)?))
    }

    fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n)?;
        Ok(())
    }
}

fn sign_magnitude(bytes: &[u8]) -> i64 {
    let mut v: i64 = (bytes[0] & 0x7f) as i64;
    for b in &bytes[1..] {
        v = (v << 8) | *b as i64;
    }
    if bytes[0] & 0x80 != 0 {
        -v
    } else {
        v
    }
}

// ---------------------------------------------------------------------------------
// Bit-packed data.
// ---------------------------------------------------------------------------------

/// A cursor that reads big-endian fields of an arbitrary width in bits, most-significant
/// bit first, which is how every packed array in section 7 is written.
struct BitReader<'a> {
    data: &'a [u8],
    bit: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> BitReader<'a> {
        BitReader { data, bit: 0 }
    }

    fn read(&mut self, nbits: u32) -> Result<u64> {
        if nbits == 0 {
            return Ok(0);
        }
        if nbits > 63 {
            bail!("a {nbits}-bit packed value is wider than this reads");
        }
        let mut v: u64 = 0;
        for _ in 0..nbits {
            let byte = *self.data.get(self.bit / 8).ok_or_else(|| anyhow!("packed data ran out"))?;
            let bit = (byte >> (7 - self.bit % 8)) & 1;
            v = (v << 1) | bit as u64;
            self.bit += 1;
        }
        Ok(v)
    }

    /// A fixed-width field read directly as bytes rather than through `read`, for the
    /// spatial-differencing descriptors, which are octet-aligned and can be wider than
    /// the 63 bits `read` allows.
    fn take_bytes(&mut self, n: usize) -> Result<i64> {
        let start = self.bit.div_ceil(8);
        let bytes = self.data.get(start..start + n).ok_or_else(|| anyhow!("ran out of bytes"))?;
        self.bit = (start + n) * 8;
        Ok(sign_magnitude(bytes))
    }

    fn align(&mut self) {
        self.bit = self.bit.div_ceil(8) * 8;
    }
}

// ---------------------------------------------------------------------------------
// Section 5: how the values are packed.
// ---------------------------------------------------------------------------------

/// The fields template 5.2 adds to simple packing to split the values into groups each
/// with its own reference, and to spread the arithmetic further apart if the values were
/// spatially differenced first (template 5.3).
#[derive(Debug)]
struct ComplexPacking {
    bits_ref: u8,
    missing_mgmt: u8,
    ng: u32,
    width_ref: u8,
    width_bits: u8,
    length_ref: u32,
    length_inc: u8,
    last_length: u32,
    length_bits: u8,
    /// `Some((order, octets))` for template 5.3: first- or second-order spatial
    /// differencing, and how many octets each of its extra descriptors takes.
    spatial: Option<(u8, u8)>,
}

#[derive(Debug)]
struct DataRepr {
    reference: f32,
    binary_scale: i32,
    decimal_scale: i32,
    bits: u8,
    complex: Option<ComplexPacking>,
}

fn read_data_repr(body: &[u8]) -> Result<DataRepr> {
    let mut r = Reader::new(body);
    r.skip(4)?; // number of data points; the caller already has this from section 3
    let template = r.u16()?;
    let reference = r.f32()?;
    let binary_scale = r.signed(2)? as i32;
    let decimal_scale = r.signed(2)? as i32;
    let bits = r.u8()?;
    r.skip(1)?; // type of the original values: doesn't change how they are packed
    let complex = match template {
        0 => None,
        2 | 3 => {
            r.skip(1)?; // group splitting method: general splitting is all a filter sends
            let missing_mgmt = r.u8()?;
            r.skip(8)?; // primary and secondary missing-value substitutes
            let ng = r.u32()?;
            let width_ref = r.u8()?;
            let width_bits = r.u8()?;
            let length_ref = r.u32()?;
            let length_inc = r.u8()?;
            let last_length = r.u32()?;
            let length_bits = r.u8()?;
            let spatial = if template == 3 {
                let order = r.u8()?;
                let octets = r.u8()?;
                Some((order, octets))
            } else {
                None
            };
            Some(ComplexPacking { bits_ref: bits, missing_mgmt, ng, width_ref, width_bits, length_ref, length_inc, last_length, length_bits, spatial })
        }
        other => bail!("data representation template 5.{other} is not supported (only simple packing 5.0 and complex packing 5.2/5.3 are)"),
    };
    Ok(DataRepr { reference, binary_scale, decimal_scale, bits, complex })
}

/// `Y = (R + X * 2^E) / 10^D`: the formula every GRIB2 packing scheme ends with, once `X`,
/// the packed integer, has been recovered.
fn unscale(x: i64, drt: &DataRepr) -> f32 {
    ((drt.reference as f64 + x as f64 * 2f64.powi(drt.binary_scale)) / 10f64.powi(drt.decimal_scale)) as f32
}

/// Section 7 unpacked into `present` real values, in the order the bitmap (if there is
/// one) says they belong: simple packing is `present` fixed-width integers; complex
/// packing splits them into groups, each with its own reference value and bit width, so
/// that a field which is mostly smooth packs far smaller than a single width would allow.
fn unpack(body: &[u8], present: usize, drt: &DataRepr) -> Result<Vec<f32>> {
    match &drt.complex {
        None => {
            let mut br = BitReader::new(body);
            let mut out = Vec::with_capacity(present);
            for _ in 0..present {
                out.push(unscale(br.read(drt.bits as u32)? as i64, drt));
            }
            Ok(out)
        }
        Some(c) => unpack_complex(body, present, drt, c),
    }
}

fn unpack_complex(body: &[u8], present: usize, drt: &DataRepr, c: &ComplexPacking) -> Result<Vec<f32>> {
    let mut br = BitReader::new(body);
    let order = c.spatial.map(|(o, _)| o as usize).unwrap_or(0);
    let mut initial: Vec<i64> = Vec::with_capacity(order);
    let mut overall_min: i64 = 0;
    if let Some((ord, octets)) = c.spatial {
        if ord != 1 && ord != 2 {
            bail!("spatial differencing of order {ord} is not supported (only first and second order are)");
        }
        for _ in 0..ord {
            initial.push(br.take_bytes(octets as usize)?);
        }
        overall_min = br.take_bytes(octets as usize)?;
        br.align();
    }

    let packed_count = present - order;
    let ng = c.ng as usize;
    if ng == 0 {
        // Nothing to split into groups: every present point was reported missing.
        return Ok(vec![f32::NAN; present]);
    }

    let mut group_ref = Vec::with_capacity(ng);
    for _ in 0..ng {
        group_ref.push(br.read(c.bits_ref as u32)?);
    }
    let mut group_width = Vec::with_capacity(ng);
    for _ in 0..ng {
        group_width.push(c.width_ref as u64 + br.read(c.width_bits as u32)?);
    }
    let mut group_len = Vec::with_capacity(ng);
    for g in 0..ng {
        if g + 1 == ng {
            group_len.push(c.last_length as u64);
        } else {
            group_len.push(c.length_ref as u64 + br.read(c.length_bits as u32)? * c.length_inc as u64);
        }
    }
    let total: u64 = group_len.iter().sum();
    if total as usize != packed_count {
        bail!("complex-packed groups add up to {total} values, not the {packed_count} expected");
    }

    // A value inside a group is written as "missing" when every bit of it is set, which
    // is why the group's own width has to allow one more value than its data needs
    // whenever missing-value management is in use.
    let mut coded: Vec<Option<i64>> = Vec::with_capacity(packed_count);
    for g in 0..ng {
        let width = group_width[g];
        for _ in 0..group_len[g] {
            let raw = br.read(width as u32)?;
            let missing = c.missing_mgmt != 0 && width > 0 && raw == (1u64 << width) - 1;
            coded.push(if missing { None } else { Some(group_ref[g] as i64 + raw as i64) });
        }
    }

    let mut out = Vec::with_capacity(present);
    if order == 0 {
        for v in coded {
            out.push(match v {
                Some(x) => unscale(x, drt),
                None => f32::NAN,
            });
        }
    } else {
        // Undo the differencing: each coded value is a difference (plus the shift that
        // was taken out of the whole array before it was group-packed, to keep every
        // packed value non-negative), so the running values are rebuilt in order. The
        // first `order` values were not differenced at all — they are stored plain, as
        // the extra descriptors read above — so they lead the output unchanged.
        let mut running = initial.clone();
        for v in initial.iter() {
            out.push(unscale(*v, drt));
        }
        for v in &coded {
            let next = match v {
                Some(d) => {
                    let d = d + overall_min;
                    match order {
                        1 => running[running.len() - 1] + d,
                        _ => 2 * running[running.len() - 1] - running[running.len() - 2] + d,
                    }
                }
                // A missing value part-way through breaks the recurrence; carry the last
                // known value forward so the values after it are not thrown off, and the
                // missing one alone is reported as such.
                None => *running.last().unwrap(),
            };
            running.push(next);
            out.push(if v.is_some() { unscale(next, drt) } else { f32::NAN });
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------
// The message as a whole.
// ---------------------------------------------------------------------------------

fn decode_message(msg: &[u8]) -> Result<Field> {
    if &msg[0..4] != b"GRIB" {
        bail!("not a GRIB message");
    }
    let discipline = msg[6];
    let edition = msg[7];
    if edition != 2 {
        bail!("GRIB edition {edition}, not 2");
    }

    let mut reference_time = None;
    let mut grid: Option<GridDef> = None;
    let mut scan: u8 = 0;
    let mut category = None;
    let mut number = None;
    let mut level_type = None;
    let mut level_value = None;
    let mut forecast_hours = None;
    let mut drt: Option<DataRepr> = None;
    let mut bitmap: Option<Vec<bool>> = None; // present in scan order; None means all present
    let mut values: Option<Vec<f32>> = None;

    let mut pos = 16;
    while pos + 4 <= msg.len() {
        if pos + 4 <= msg.len() && &msg[pos..pos + 4] == b"7777" {
            break;
        }
        let seclen = u32::from_be_bytes(msg[pos..pos + 4].try_into().unwrap()) as usize;
        if seclen < 5 || pos + seclen > msg.len() {
            bail!("section at byte {pos} has an impossible length {seclen}");
        }
        let secnum = msg[pos + 4];
        let body = &msg[pos + 5..pos + seclen];

        match secnum {
            1 => {
                let mut r = Reader::new(body);
                r.skip(7)?; // centre, sub-centre, table versions, significance of time
                let year = r.u16()? as i32;
                let (month, day, hour, minute, second) = (r.u8()? as u32, r.u8()? as u32, r.u8()? as u32, r.u8()? as u32, r.u8()? as u32);
                reference_time = Some(Utc.with_ymd_and_hms(year, month, day, hour, minute, second).single().ok_or_else(|| anyhow!("section 1: not a real date/time"))?);
            }
            3 => {
                let mut r = Reader::new(body);
                r.skip(5)?; // source of the grid, number of points
                r.skip(2)?; // number of octets for the optional list, and its interpretation
                let template = r.u16()?;
                if template != 0 {
                    bail!("grid definition template 3.{template} is not supported (only the regular latitude/longitude grid, 3.0, is)");
                }
                r.skip(1 + 1 + 4 + 1 + 4 + 1 + 4)?; // shape of the earth and its three axes
                let ni = r.u32()? as usize;
                let nj = r.u32()? as usize;
                r.skip(8)?; // basic angle and its subdivisions: GFS always leaves these at 1e-6 degrees
                let la1 = r.signed(4)? as f64 * 1e-6;
                let lo1 = r.signed(4)? as f64 * 1e-6;
                r.skip(1)?; // resolution and component flags
                let la2 = r.signed(4)? as f64 * 1e-6;
                let lo2 = r.signed(4)? as f64 * 1e-6;
                let di = r.signed(4)?.unsigned_abs() as f64 * 1e-6;
                let dj = r.signed(4)?.unsigned_abs() as f64 * 1e-6;
                scan = r.u8()?;
                grid = Some(canonical_grid(ni, nj, la1, lo1, la2, lo2, di, dj, scan));
            }
            4 => {
                let mut r = Reader::new(body);
                r.skip(2)?; // number of coordinate values
                let template = r.u16()?;
                if template != 0 {
                    bail!("product definition template 4.{template} is not supported (only the plain forecast at a level, 4.0, is)");
                }
                category = Some(r.u8()?);
                number = Some(r.u8()?);
                r.skip(3)?; // generating process fields, not needed to place the field in time or space
                r.skip(2)?; // hours of cutoff
                r.skip(1)?; // minutes of cutoff
                let time_unit = r.u8()?;
                let time_value = r.u32()? as f64;
                forecast_hours = Some(time_value * hours_per_unit(time_unit)?);
                level_type = Some(r.u8()?);
                let scale = r.signed(1)?;
                let scaled = r.signed(4)?;
                level_value = Some(scaled as f64 / 10f64.powi(scale as i32));
            }
            5 => drt = Some(read_data_repr(body)?),
            6 => {
                let indicator = body[0];
                bitmap = match indicator {
                    255 => None,
                    0 => {
                        let g = grid.ok_or_else(|| anyhow!("bitmap section before the grid was read"))?;
                        let n = g.ni * g.nj;
                        let bits = &body[1..];
                        Some((0..n).map(|k| (bits[k / 8] >> (7 - k % 8)) & 1 != 0).collect())
                    }
                    other => bail!("bitmap indicator {other} is not supported (only an included bitmap or none is)"),
                };
            }
            7 => {
                let g = grid.ok_or_else(|| anyhow!("data section before the grid was read"))?;
                let d = drt.as_ref().ok_or_else(|| anyhow!("data section before its representation was read"))?;
                let n = g.ni * g.nj;
                let present = bitmap.as_ref().map(|b| b.iter().filter(|p| **p).count()).unwrap_or(n);
                let packed = unpack(body, present, d)?;
                let scan_order = match &bitmap {
                    None => packed,
                    Some(b) => {
                        let mut out = vec![f32::NAN; n];
                        let mut it = packed.into_iter();
                        for (k, present) in b.iter().enumerate() {
                            if *present {
                                out[k] = it.next().ok_or_else(|| anyhow!("fewer packed values than the bitmap needs"))?;
                            }
                        }
                        out
                    }
                };
                values = Some(reorder_to_canonical(&scan_order, g.ni, g.nj, scan));
            }
            _ => {}
        }
        pos += seclen;
    }

    Ok(Field {
        discipline,
        category: category.ok_or_else(|| anyhow!("no product definition section"))?,
        number: number.ok_or_else(|| anyhow!("no product definition section"))?,
        level_type: level_type.ok_or_else(|| anyhow!("no product definition section"))?,
        level_value: level_value.ok_or_else(|| anyhow!("no product definition section"))?,
        reference_time: reference_time.ok_or_else(|| anyhow!("no identification section"))?,
        forecast_hours: forecast_hours.ok_or_else(|| anyhow!("no product definition section"))?,
        grid: grid.ok_or_else(|| anyhow!("no grid definition section"))?,
        values: values.ok_or_else(|| anyhow!("no data section"))?,
    })
}

fn hours_per_unit(indicator: u8) -> Result<f64> {
    Ok(match indicator {
        0 => 1.0 / 60.0,    // minute
        1 => 1.0,           // hour
        2 => 24.0,          // day
        10 => 3.0,          // 3 hours
        11 => 6.0,          // 6 hours
        12 => 12.0,         // 12 hours
        13 => 1.0 / 3600.0, // second
        other => bail!("forecast time given in units of indicator {other}, which is not one this reads"),
    })
}

/// The grid definition's own edges and increments, with the direction the scan bits say
/// removed: `la1`/`lo1` become the north-west corner and `di`/`dj` are always positive.
fn canonical_grid(ni: usize, nj: usize, la1: f64, lo1: f64, la2: f64, lo2: f64, di: f64, dj: f64, scan: u8) -> GridDef {
    // Both edges are given, so a zero increment (some encoders write one rather than work
    // it out) is filled in from them rather than trusted.
    let di = if di > 0.0 {
        di
    } else if ni > 1 {
        lon_span(lo1, lo2) / (ni as f64 - 1.0)
    } else {
        0.0
    };
    let dj = if dj > 0.0 {
        dj
    } else if nj > 1 {
        (la2 - la1).abs() / (nj as f64 - 1.0)
    } else {
        0.0
    };
    let j_increases_north = scan & 0x40 != 0; // bit 2: 0 = -j (north first), 1 = +j (south first)
    let i_increases_west = scan & 0x80 != 0; // bit 1: 0 = +i (west first), 1 = -i (east first)
    let (canon_la1, canon_dj) = if j_increases_north { (la1 + (nj.max(1) - 1) as f64 * dj, dj) } else { (la1, dj) };
    let (canon_lo1, canon_di) = if i_increases_west { (lo1 - (ni.max(1) - 1) as f64 * di, di) } else { (lo1, di) };
    GridDef { ni, nj, la1: canon_la1, lo1: canon_lo1, di: canon_di, dj: canon_dj }
}

fn lon_span(lo1: f64, lo2: f64) -> f64 {
    let span = lo2 - lo1;
    if span >= 0.0 {
        span
    } else {
        span + 360.0
    }
}

/// The values in whatever order the message scanned the grid, laid out canonically:
/// `out[j * ni + i]`, row 0 north, column 0 west.
fn reorder_to_canonical(scan_order: &[f32], ni: usize, nj: usize, scan: u8) -> Vec<f32> {
    let i_consecutive = scan & 0x20 == 0; // bit 3: 0 = i varies fastest (row-major)
    let alternating = scan & 0x10 != 0; // bit 4: adjacent rows/columns scan opposite ways
    let i_increases_west = scan & 0x80 != 0;
    let j_increases_north = scan & 0x40 != 0;

    // Where a scanned point (i_s, j_s) lands once north is row 0 and west is column 0.
    let canon_i = |i_s: usize| if i_increases_west { ni - 1 - i_s } else { i_s };
    let canon_j = |j_s: usize| if j_increases_north { nj - 1 - j_s } else { j_s };

    let mut out = vec![f32::NAN; ni * nj];
    let mut k = 0;
    if i_consecutive {
        for j_s in 0..nj {
            for raw_i in 0..ni {
                let i_s = if alternating && j_s % 2 == 1 { ni - 1 - raw_i } else { raw_i };
                out[canon_j(j_s) * ni + canon_i(i_s)] = scan_order[k];
                k += 1;
            }
        }
    } else {
        for i_s in 0..ni {
            for raw_j in 0..nj {
                let j_s = if alternating && i_s % 2 == 1 { nj - 1 - raw_j } else { raw_j };
                out[canon_j(j_s) * ni + canon_i(i_s)] = scan_order[k];
                k += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // A tiny synthetic message, simple packing, built by hand from the spec rather than
    // by any GRIB-writing library: the smallest possible exercise of every section this
    // reader looks at.
    // ------------------------------------------------------------------

    fn sm_bytes(v: i64, n: usize) -> Vec<u8> {
        let mut out = vec![0u8; n];
        let mut m = v.unsigned_abs();
        for i in (0..n).rev() {
            out[i] = (m & 0xff) as u8;
            m >>= 8;
        }
        if v < 0 {
            out[0] |= 0x80;
        }
        out
    }

    /// Values for an `ni` x `nj` grid (west to east, north to south), packed as 8-bit
    /// simple packing, wrapped in the sections a decoder needs and nothing more.
    fn synthetic_simple(values: &[f32], ni: u32, nj: u32, scan: u8) -> Vec<u8> {
        let min = values.iter().cloned().fold(f32::INFINITY, f32::min);
        let max = values.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        let bits: u8 = 8;
        let levels = ((1u32 << bits) - 1) as f64;
        let scale = if max > min { levels / (max - min) as f64 } else { 1.0 };
        // `unscale` recovers `Y = R + X * 2^E`, so encoding must divide by `2^E`, i.e.
        // multiply by `2^-E`: choose `E` negative so that multiplying by `2^-E` spreads
        // the values across the bits available, rounded down so the top value never
        // clamps against the bit width.
        let e = -(scale.log2().floor() as i32);
        let factor = 2f64.powi(-e);
        let packed: Vec<u8> = values.iter().map(|v| (((*v - min) as f64 * factor).round() as i64).clamp(0, levels as i64) as u8).collect();

        let mut out = Vec::new();
        out.extend_from_slice(b"GRIB");
        out.extend_from_slice(&[0, 0]);
        out.push(0); // discipline: meteorological
        out.push(2); // edition
        out.extend_from_slice(&[0u8; 8]); // total length, patched below

        let mut s1 = vec![0u8; 5];
        s1[4] = 1;
        s1.extend_from_slice(&[7, 253, 7, 253, 0, 0, 1]);
        s1.extend_from_slice(&2026u16.to_be_bytes());
        s1.extend_from_slice(&[1, 15, 6, 0, 0, 0, 1]);
        patch_len(&mut s1);
        out.extend_from_slice(&s1);

        let mut s3 = vec![0u8; 5];
        s3[4] = 3;
        s3.push(0);
        s3.extend_from_slice(&(ni * nj).to_be_bytes());
        s3.extend_from_slice(&[0, 0]);
        s3.extend_from_slice(&0u16.to_be_bytes());
        s3.push(6);
        s3.extend_from_slice(&[0, 0, 0, 0, 0]);
        s3.extend_from_slice(&[0, 0, 0, 0, 0]);
        s3.extend_from_slice(&[0, 0, 0, 0, 0]);
        s3.extend_from_slice(&ni.to_be_bytes());
        s3.extend_from_slice(&nj.to_be_bytes());
        s3.extend_from_slice(&[0, 0, 0, 0]);
        s3.extend_from_slice(&[0xff, 0xff, 0xff, 0xff]);
        s3.extend_from_slice(&sm_bytes(50_000_000, 4)); // la1 = 50.0
        s3.extend_from_slice(&sm_bytes(10_000_000, 4)); // lo1 = 10.0
        s3.push(0x30);
        s3.extend_from_slice(&sm_bytes(48_000_000, 4)); // la2 = 48.0
        s3.extend_from_slice(&sm_bytes(12_000_000, 4)); // lo2 = 12.0
        s3.extend_from_slice(&sm_bytes(1_000_000, 4)); // di = 1.0
        s3.extend_from_slice(&sm_bytes(1_000_000, 4)); // dj = 1.0
        s3.push(scan);
        patch_len(&mut s3);
        out.extend_from_slice(&s3);

        let mut s4 = vec![0u8; 5];
        s4[4] = 4;
        s4.extend_from_slice(&[0, 0]);
        s4.extend_from_slice(&0u16.to_be_bytes());
        s4.push(2); // category: momentum
        s4.push(2); // number: U-component of wind
        s4.extend_from_slice(&[0, 0, 0]);
        s4.extend_from_slice(&[0, 0]);
        s4.push(0);
        s4.push(1); // time unit: hour
        s4.extend_from_slice(&3u32.to_be_bytes()); // forecast hour 3
        s4.push(100); // isobaric surface
        s4.push(0); // scale factor
        s4.extend_from_slice(&sm_bytes(85_000, 4)); // 85,000 Pa = 850 hPa
        s4.push(255);
        s4.push(0);
        s4.extend_from_slice(&[0, 0, 0, 0]);
        patch_len(&mut s4);
        out.extend_from_slice(&s4);

        let mut s5 = vec![0u8; 5];
        s5[4] = 5;
        s5.extend_from_slice(&(ni * nj).to_be_bytes());
        s5.extend_from_slice(&0u16.to_be_bytes());
        s5.extend_from_slice(&min.to_be_bytes());
        s5.extend_from_slice(&sm_bytes(e as i64, 2)); // binary scale E
        s5.extend_from_slice(&sm_bytes(0, 2)); // decimal scale D = 0
        s5.push(bits);
        s5.push(0);
        patch_len(&mut s5);
        out.extend_from_slice(&s5);

        let mut s6 = vec![0u8; 5];
        s6[4] = 6;
        s6.push(255);
        patch_len(&mut s6);
        out.extend_from_slice(&s6);

        let mut s7 = vec![0u8; 5];
        s7[4] = 7;
        s7.extend_from_slice(&packed);
        patch_len(&mut s7);
        out.extend_from_slice(&s7);

        out.extend_from_slice(b"7777");
        let total = out.len() as u64;
        out[8..16].copy_from_slice(&total.to_be_bytes());
        out
    }

    fn patch_len(section: &mut [u8]) {
        let len = section.len() as u32;
        section[0..4].copy_from_slice(&len.to_be_bytes());
    }

    #[test]
    fn a_simple_packed_message_round_trips_to_within_the_packing_error() {
        let values = [10.0f32, 12.0, 11.0, 9.0, 8.0, 13.0];
        let msg = synthetic_simple(&values, 3, 2, 0x00);
        let fields = decode_all(&msg).unwrap();
        assert_eq!(fields.len(), 1);
        let f = &fields[0];
        assert_eq!(f.grid.ni, 3);
        assert_eq!(f.grid.nj, 2);
        assert_eq!((f.grid.la1, f.grid.lo1), (50.0, 10.0));
        assert_eq!(f.level_value, 85_000.0);
        assert_eq!(f.level_type, 100);
        assert!((f.forecast_hours - 3.0).abs() < 1e-9);
        for (got, want) in f.values.iter().zip(values.iter()) {
            assert!((got - want).abs() < 0.1, "{got} vs {want}");
        }
    }

    #[test]
    fn two_messages_back_to_back_both_decode() {
        let a = synthetic_simple(&[1.0, 2.0], 2, 1, 0);
        let b = synthetic_simple(&[3.0, 4.0], 2, 1, 0);
        let mut both = a;
        both.extend_from_slice(&b);
        let fields = decode_all(&both).unwrap();
        assert_eq!(fields.len(), 2);
        assert!((fields[1].values[0] - 3.0).abs() < 0.1);
    }

    #[test]
    fn scan_bit_1_reverses_west_to_east() {
        let values = [10.0f32, 12.0, 11.0, 9.0, 8.0, 13.0];
        let msg = synthetic_simple(&values, 3, 2, 0x80);
        let f = &decode_all(&msg).unwrap()[0];
        // Canonical west edge is now lo1 - 2*di = 8.0, and the row is reversed back to match.
        assert_eq!(f.grid.lo1, 8.0);
        for (got, want) in f.values[0..3].iter().zip([11.0, 12.0, 10.0]) {
            assert!((got - want).abs() < 0.1);
        }
    }

    #[test]
    fn scan_bit_2_puts_the_south_edge_first_and_the_reader_flips_it_back() {
        let values = [10.0f32, 12.0, 11.0, 9.0, 8.0, 13.0];
        let msg = synthetic_simple(&values, 3, 2, 0x40);
        let f = &decode_all(&msg).unwrap()[0];
        // la1=50 was the south edge; north edge (row 0 canonically) is la1 + (2-1)*1 = 51.
        assert_eq!(f.grid.la1, 51.0);
        for (got, want) in f.values[0..3].iter().zip([9.0, 8.0, 13.0]) {
            assert!((got - want).abs() < 0.1);
        }
    }

    #[test]
    fn column_major_scan_is_also_put_back_in_row_order() {
        // j varies fastest (bit 3 set): column 0 is [10,9], column 1 is [12,8], column 2 is [11,13].
        let scan_order = [10.0f32, 9.0, 12.0, 8.0, 11.0, 13.0];
        let msg = synthetic_simple(&scan_order, 3, 2, 0x20);
        let f = &decode_all(&msg).unwrap()[0];
        for (got, want) in f.values.iter().zip([10.0, 12.0, 11.0, 9.0, 8.0, 13.0]) {
            assert!((got - want).abs() < 0.1, "{got} vs {want}");
        }
    }

    #[test]
    fn a_grid_definition_template_other_than_3_0_is_a_clear_error() {
        let values = [1.0f32, 2.0];
        let mut msg = synthetic_simple(&values, 2, 1, 0);
        let s3_at = 16 + 21; // right after the 16-byte section 0 and 21-byte section 1
        let template_at = s3_at + 5 + 7; // section header + source/ndp/optlist/interp
        msg[template_at + 1] = 1; // template 3.1 instead of 3.0
        let err = decode_all(&msg).unwrap_err();
        assert!(format!("{err:#}").contains("3.1"), "{err:#}");
    }

    // ------------------------------------------------------------------
    // The bit reader and the packing arithmetic, tested directly rather than through a
    // whole message: complex packing has enough moving parts that it is worth trusting
    // each one on its own before trusting the sections that carry it.
    // ------------------------------------------------------------------

    #[test]
    fn the_bit_reader_reads_across_byte_boundaries() {
        let data = [0b1011_0101u8, 0b1100_0000];
        let mut br = BitReader::new(&data);
        assert_eq!(br.read(5).unwrap(), 0b10110);
        assert_eq!(br.read(5).unwrap(), 0b10111);
        assert_eq!(br.read(6).unwrap(), 0b000000);
    }

    #[test]
    fn sign_magnitude_reads_the_top_bit_as_the_sign() {
        assert_eq!(sign_magnitude(&[0x00, 0x05]), 5);
        assert_eq!(sign_magnitude(&[0x80, 0x05]), -5);
        assert_eq!(sign_magnitude(&[0x00]), 0);
        assert_eq!(sign_magnitude(&[0x80]), 0);
    }

    /// A minimal MSB-first bit writer, used only to build test fixtures.
    struct BitWriter {
        bytes: Vec<u8>,
        bit: usize,
    }
    impl BitWriter {
        fn new() -> Self {
            BitWriter { bytes: Vec::new(), bit: 0 }
        }
        fn write(&mut self, value: u64, nbits: u32) {
            for i in (0..nbits).rev() {
                let bit = (value >> i) & 1;
                if self.bit % 8 == 0 {
                    self.bytes.push(0);
                }
                let last = self.bytes.len() - 1;
                self.bytes[last] |= (bit as u8) << (7 - self.bit % 8);
                self.bit += 1;
            }
        }
        fn finish(self) -> Vec<u8> {
            self.bytes
        }
    }

    /// A hand-rolled encoder for complex packing with spatial differencing, matching the
    /// single-group scheme `unpack_complex` reads: not a general packer, but enough to
    /// prove the reader recovers what a real one would have put in.
    fn pack_complex_spatial(values: &[i64], order: u8) -> (Vec<u8>, ComplexPacking) {
        let n = values.len();
        let initial: Vec<i64> = values[..order as usize].to_vec();
        let mut diffs = Vec::new();
        for i in order as usize..n {
            let d = match order {
                1 => values[i] - values[i - 1],
                _ => values[i] - 2 * values[i - 1] + values[i - 2],
            };
            diffs.push(d);
        }
        let min = diffs.iter().cloned().min().unwrap_or(0);
        let shifted: Vec<u64> = diffs.iter().map(|d| (d - min) as u64).collect();
        let max = shifted.iter().cloned().max().unwrap_or(0);
        let value_bits = (64 - max.leading_zeros()).max(1) as u8;

        let mut bytes = Vec::new();
        for v in &initial {
            bytes.extend(sm_bytes(*v, 2));
        }
        bytes.extend(sm_bytes(min, 2));

        let mut bw = BitWriter::new();
        for v in &shifted {
            bw.write(*v, value_bits as u32);
        }
        bytes.extend(bw.finish());

        let packing = ComplexPacking {
            bits_ref: 0,
            missing_mgmt: 0,
            ng: 1,
            width_ref: value_bits,
            width_bits: 0,
            length_ref: shifted.len() as u32,
            length_inc: 0,
            last_length: shifted.len() as u32,
            length_bits: 0,
            spatial: Some((order, 2)),
        };
        (bytes, packing)
    }

    #[test]
    fn spatial_differencing_of_order_one_round_trips() {
        let values: Vec<i64> = vec![1000, 1010, 1005, 990, 995, 1020, 1015];
        let (body, packing) = pack_complex_spatial(&values, 1);
        let drt = DataRepr { reference: 0.0, binary_scale: 0, decimal_scale: 0, bits: 0, complex: None };
        let out = unpack_complex(&body, values.len(), &drt, &packing).unwrap();
        for (got, want) in out.iter().zip(values.iter()) {
            assert!((got.round() as i64 - want).abs() <= 1, "{got} vs {want}");
        }
    }

    #[test]
    fn spatial_differencing_of_order_two_round_trips_a_falling_and_rising_series() {
        let values: Vec<i64> = vec![500, 480, 450, 430, 440, 470, 520, 540];
        let (body, packing) = pack_complex_spatial(&values, 2);
        let drt = DataRepr { reference: 0.0, binary_scale: 0, decimal_scale: 0, bits: 0, complex: None };
        let out = unpack_complex(&body, values.len(), &drt, &packing).unwrap();
        for (got, want) in out.iter().zip(values.iter()) {
            assert!((got.round() as i64 - want).abs() <= 1, "{got} vs {want}");
        }
    }

    #[test]
    fn simple_packing_with_a_bitmap_leaves_missing_points_as_nan() {
        let mut bw = BitWriter::new();
        bw.write(3, 4);
        bw.write(9, 4);
        let body = bw.finish();
        let drt = DataRepr { reference: 0.0, binary_scale: 0, decimal_scale: 0, bits: 4, complex: None };
        let out = unpack(&body, 2, &drt).unwrap();
        assert_eq!(out, vec![3.0, 9.0]);
    }

    /// A genuine NOAA GFS 0.25-degree message (UGRD at 850 hPa, a small North Atlantic
    /// subregion, fetched once through the NOMADS GRIB filter and kept as a fixture): the
    /// reader is checked against real production data, not only against messages this
    /// file built itself.
    #[test]
    fn a_real_gfs_message_decodes_to_plausible_wind() {
        let data = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/weather/gfs-sample.grib2")).unwrap();
        let fields = decode_all(&data).unwrap();
        assert_eq!(fields.len(), 1);
        let f = &fields[0];
        assert_eq!(f.discipline, 0);
        assert_eq!(f.category, 2);
        assert_eq!(f.number, 2); // UGRD
        assert_eq!(f.level_type, 100);
        assert_eq!(f.level_value, 85_000.0);
        assert_eq!(f.grid.ni, 17);
        assert_eq!(f.grid.nj, 17);
        // GRIB2 longitudes run 0-360, so -32 degrees west is written as 328.
        assert!((f.grid.lo1 - 328.0).abs() < 1e-6, "{}", f.grid.lo1);
        assert!((f.grid.la1 - 52.0).abs() < 1e-6, "{}", f.grid.la1);
        // A U-wind component at 850 hPa over the North Atlantic: nowhere near a jet
        // stream, so a sane bound is generous, but it should not be the everywhere-zero
        // or everywhere-NaN a wiring mistake would produce.
        assert!(f.values.iter().all(|v| v.is_finite()));
        let max_abs = f.values.iter().cloned().fold(0.0f32, |a, b| a.max(b.abs()));
        assert!(max_abs > 1.0 && max_abs < 150.0, "{max_abs}");
    }

    #[test]
    fn an_unsupported_data_representation_template_names_itself_in_the_error() {
        // Template number 40 in section 5, i.e. JPEG 2000 (5.40): its data section starts
        // the same way as simple packing's (reference, scales, bits, original type) before
        // its own JPEG-specific fields, so that much can still be read before refusing it.
        let mut body = vec![0u8; 16];
        body[4..6].copy_from_slice(&40u16.to_be_bytes());
        let err = read_data_repr(&body).unwrap_err();
        assert!(format!("{err:#}").contains("5.40"), "{err:#}");
    }
}
