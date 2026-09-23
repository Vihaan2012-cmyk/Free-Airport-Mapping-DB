//! One drawing surface, two things to draw on.
//!
//! A chart is built by calling a couple of dozen operations — move somewhere, draw a line,
//! fill what has been drawn, set a grey, write some text — and until now those went
//! straight into a PDF content stream. That is the right thing for a chart somebody
//! prints, and the wrong thing for one an aircraft's electronic flight bag asks for,
//! because it asks for a picture.
//!
//! So the operations are named here instead, and there are two places to send them: the
//! PDF stream as before, and a bitmap. Nothing above this file knows which it is drawing
//! on, and the two cannot drift apart, because there is only one set of drawing code.

use ab_glyph::{Font, FontVec, PxScale, ScaleFont};
use anyhow::{anyhow, Context, Result};
use pdf_writer::types::LineCapStyle;
use pdf_writer::{Content, Name, Str};
use std::path::{Path, PathBuf};

/// What a chart is drawn with.
///
/// The names follow the PDF operators the drawing code was written against, because that
/// is what it says, and changing them would only have meant editing every call site to
/// say the same thing differently.
pub trait Canvas {
    fn move_to(&mut self, x: f32, y: f32);
    fn line_to(&mut self, x: f32, y: f32);
    fn cubic_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x3: f32, y3: f32);
    fn close_path(&mut self);
    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32);

    fn fill_nonzero(&mut self);
    fn fill_even_odd(&mut self);
    fn fill_even_odd_and_stroke(&mut self);
    fn stroke(&mut self);
    /// Throw away what has been drawn without painting it, which is how a clip is ended.
    fn end_path(&mut self);
    fn clip_nonzero(&mut self);

    fn set_fill_gray(&mut self, grey: f32);
    fn set_fill_rgb(&mut self, r: f32, g: f32, b: f32);
    fn set_stroke_gray(&mut self, grey: f32);
    fn set_line_width(&mut self, w: f32);
    fn set_dash(&mut self, pattern: &[f32], phase: f32);

    fn save_state(&mut self);
    fn restore_state(&mut self);

    /// Text on its baseline, in one of the two faces a chart is set in.
    fn text(&mut self, font: Name, size: f32, x: f32, y: f32, s: &str, grey: f32);
    /// The same, turned a quarter turn anticlockwise, for a label up the side of a band.
    fn text_turned(&mut self, font: Name, size: f32, x: f32, y: f32, s: &str, grey: f32);
}

/// True for the bold face. The two fonts are named F and B in the PDF's resources, and
/// that name is carried through the drawing code as the handle for which face to use.
fn is_bold(font: Name) -> bool {
    font.0 == b"B"
}

// ---------------------------------------------------------------------------------
// The PDF stream, which is what it always was.
// ---------------------------------------------------------------------------------

impl Canvas for Content {
    fn move_to(&mut self, x: f32, y: f32) {
        Content::move_to(self, x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        Content::line_to(self, x, y);
    }
    fn cubic_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x3: f32, y3: f32) {
        Content::cubic_to(self, x1, y1, x2, y2, x3, y3);
    }
    fn close_path(&mut self) {
        Content::close_path(self);
    }
    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        Content::rect(self, x, y, w, h);
    }
    fn fill_nonzero(&mut self) {
        Content::fill_nonzero(self);
    }
    fn fill_even_odd(&mut self) {
        Content::fill_even_odd(self);
    }
    fn fill_even_odd_and_stroke(&mut self) {
        Content::fill_even_odd_and_stroke(self);
    }
    fn stroke(&mut self) {
        Content::stroke(self);
    }
    fn end_path(&mut self) {
        Content::end_path(self);
    }
    fn clip_nonzero(&mut self) {
        Content::clip_nonzero(self);
    }
    fn set_fill_gray(&mut self, grey: f32) {
        Content::set_fill_gray(self, grey);
    }
    fn set_fill_rgb(&mut self, r: f32, g: f32, b: f32) {
        Content::set_fill_rgb(self, r, g, b);
    }
    fn set_stroke_gray(&mut self, grey: f32) {
        Content::set_stroke_gray(self, grey);
    }
    fn set_line_width(&mut self, w: f32) {
        Content::set_line_width(self, w);
    }
    fn set_dash(&mut self, pattern: &[f32], phase: f32) {
        Content::set_dash_pattern(self, pattern.iter().copied(), phase);
    }
    fn save_state(&mut self) {
        Content::save_state(self);
    }
    fn restore_state(&mut self) {
        Content::restore_state(self);
    }
    fn text(&mut self, font: Name, size: f32, x: f32, y: f32, s: &str, grey: f32) {
        self.begin_text();
        self.set_fill_gray(grey);
        self.set_font(font, size);
        self.next_line(x, y);
        self.show(Str(&super::winansi(s)));
        self.end_text();
    }
    fn text_turned(&mut self, font: Name, size: f32, x: f32, y: f32, s: &str, grey: f32) {
        self.begin_text();
        self.set_fill_gray(grey);
        self.set_font(font, size);
        // A quarter turn anticlockwise about the point given.
        self.set_text_matrix([0.0, 1.0, -1.0, 0.0, x, y]);
        self.show(Str(&super::winansi(s)));
        self.end_text();
    }
}

// ---------------------------------------------------------------------------------
// The bitmap.
// ---------------------------------------------------------------------------------

/// The graphics state, which is saved and restored around a clip the same way the PDF's
/// is. Only the parts a chart actually changes are kept.
#[derive(Clone)]
struct State {
    fill: tiny_skia::Color,
    stroke: tiny_skia::Color,
    width: f32,
    dash: Option<tiny_skia::StrokeDash>,
    clip: Option<tiny_skia::Mask>,
}

/// A chart painted into pixels.
///
/// The page is in points with the origin at the bottom left, the way PDF has it, and the
/// bitmap has its origin at the top left, so every y is turned over on the way in. Scale
/// is points to pixels: three gives about 216 to the inch, which is what an electronic
/// flight bag wants of an A4 page.
pub struct Raster {
    pixmap: tiny_skia::Pixmap,
    scale: f32,
    height_pt: f32,
    path: tiny_skia::PathBuilder,
    state: State,
    stack: Vec<State>,
    regular: FontVec,
    bold: FontVec,
}

impl Raster {
    pub fn new(width_pt: f32, height_pt: f32, scale: f32) -> Result<Raster> {
        let (w, h) = ((width_pt * scale).ceil() as u32, (height_pt * scale).ceil() as u32);
        let mut pixmap = tiny_skia::Pixmap::new(w.max(1), h.max(1)).ok_or_else(|| anyhow!("{w}x{h} is not a size a picture can be"))?;
        // Paper. A chart drawn on nothing would come out on whatever is behind it.
        pixmap.fill(tiny_skia::Color::WHITE);
        let (regular, bold) = faces()?;
        Ok(Raster {
            pixmap,
            scale,
            height_pt,
            path: tiny_skia::PathBuilder::new(),
            state: State {
                fill: tiny_skia::Color::BLACK,
                stroke: tiny_skia::Color::BLACK,
                width: 1.0,
                dash: None,
                clip: None,
            },
            stack: Vec::new(),
            regular,
            bold,
        })
    }

    pub fn write_png(&self, out: &Path) -> Result<()> {
        self.pixmap.save_png(out).with_context(|| format!("write {}", out.display()))?;
        Ok(())
    }

    pub fn png_bytes(&self) -> Result<Vec<u8>> {
        Ok(self.pixmap.encode_png()?)
    }

    /// Page point to pixel. The y axis is the whole of the difference.
    fn at(&self, x: f32, y: f32) -> (f32, f32) {
        (x * self.scale, (self.height_pt - y) * self.scale)
    }

    fn take_path(&mut self) -> Option<tiny_skia::Path> {
        std::mem::take(&mut self.path).finish()
    }

    fn paint(&self, colour: tiny_skia::Color) -> tiny_skia::Paint<'static> {
        let mut p = tiny_skia::Paint::default();
        p.set_color(colour);
        p.anti_alias = true;
        p
    }

    fn fill_path(&mut self, rule: tiny_skia::FillRule) {
        let Some(path) = self.take_path() else { return };
        let paint = self.paint(self.state.fill);
        let clip = self.state.clip.clone();
        self.pixmap.fill_path(&path, &paint, rule, tiny_skia::Transform::identity(), clip.as_ref());
    }

    fn stroke_path_with(&mut self, path: &tiny_skia::Path) {
        let mut stroke = tiny_skia::Stroke {
            // A width is given in points and the picture is drawn larger than that.
            width: (self.state.width * self.scale).max(0.1),
            line_cap: tiny_skia::LineCap::Butt,
            ..Default::default()
        };
        stroke.dash = self.state.dash.clone();
        let paint = self.paint(self.state.stroke);
        let clip = self.state.clip.clone();
        self.pixmap.stroke_path(path, &paint, &stroke, tiny_skia::Transform::identity(), clip.as_ref());
    }

    fn face(&self, font: Name) -> &FontVec {
        if is_bold(font) {
            &self.bold
        } else {
            &self.regular
        }
    }

    /// Lay a string out and paint its glyphs. `turned` writes it up the page.
    fn write(&mut self, font: Name, size: f32, x: f32, y: f32, s: &str, grey: f32, turned: bool) {
        let colour = tiny_skia::Color::from_rgba(grey, grey, grey, 1.0).unwrap_or(tiny_skia::Color::BLACK);
        let px = size * self.scale;
        let (ox, oy) = self.at(x, y);
        // Collect first: the font is borrowed from self and the pixmap is written to it.
        let mut glyphs: Vec<(ab_glyph::OutlinedGlyph, f32)> = Vec::new();
        {
            let face = self.face(font);
            let scaled = face.as_scaled(PxScale::from(px));
            let mut pen = 0.0f32;
            let mut previous: Option<ab_glyph::GlyphId> = None;
            for ch in s.chars() {
                let id = face.glyph_id(ch);
                if let Some(prev) = previous {
                    pen += scaled.kern(prev, id);
                }
                let glyph = id.with_scale_and_position(px, ab_glyph::point(0.0, 0.0));
                if let Some(outlined) = face.outline_glyph(glyph) {
                    glyphs.push((outlined, pen));
                }
                pen += scaled.h_advance(id);
                previous = Some(id);
            }
        }
        let width = self.pixmap.width() as i32;
        let height = self.pixmap.height() as i32;
        let data = self.pixmap.pixels_mut();
        for (outlined, pen) in glyphs {
            let bounds = outlined.px_bounds();
            outlined.draw(|gx, gy, coverage| {
                if coverage <= 0.003 {
                    return;
                }
                // Where this pixel of the glyph lands on the page. Turned text runs up,
                // so its x on the page comes from the glyph's y and the other way about.
                let (dx, dy) = (bounds.min.x + gx as f32, bounds.min.y + gy as f32);
                let (fx, fy) = if turned { (ox + dy, oy - pen - dx) } else { (ox + pen + dx, oy + dy) };
                let (ix, iy) = (fx.round() as i32, fy.round() as i32);
                if ix < 0 || iy < 0 || ix >= width || iy >= height {
                    return;
                }
                let index = (iy * width + ix) as usize;
                let under = data[index].demultiply();
                let a = coverage.clamp(0.0, 1.0);
                let mix = |over: f32, under: u8| (over * 255.0 * a + under as f32 * (1.0 - a)) as u8;
                data[index] = tiny_skia::ColorU8::from_rgba(
                    mix(colour.red(), under.red()),
                    mix(colour.green(), under.green()),
                    mix(colour.blue(), under.blue()),
                    255,
                )
                .premultiply();
            });
        }
    }
}

impl Canvas for Raster {
    fn move_to(&mut self, x: f32, y: f32) {
        let (px, py) = self.at(x, y);
        self.path.move_to(px, py);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let (px, py) = self.at(x, y);
        // A line before any move is a line from nowhere; PDF readers ignore it and so do we.
        if self.path.is_empty() {
            self.path.move_to(px, py);
        } else {
            self.path.line_to(px, py);
        }
    }
    fn cubic_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x3: f32, y3: f32) {
        let (a, b, c) = (self.at(x1, y1), self.at(x2, y2), self.at(x3, y3));
        if self.path.is_empty() {
            self.path.move_to(a.0, a.1);
        }
        self.path.cubic_to(a.0, a.1, b.0, b.1, c.0, c.1);
    }
    fn close_path(&mut self) {
        self.path.close();
    }
    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        // The page's rectangle grows upwards from its corner; the picture's grows down.
        let (px, py) = self.at(x, y + h);
        if let Some(r) = tiny_skia::Rect::from_xywh(px, py, (w * self.scale).max(0.01), (h * self.scale).max(0.01)) {
            self.path.push_rect(r);
        }
    }
    fn fill_nonzero(&mut self) {
        self.fill_path(tiny_skia::FillRule::Winding);
    }
    fn fill_even_odd(&mut self) {
        self.fill_path(tiny_skia::FillRule::EvenOdd);
    }
    fn fill_even_odd_and_stroke(&mut self) {
        let Some(path) = self.take_path() else { return };
        let paint = self.paint(self.state.fill);
        let clip = self.state.clip.clone();
        self.pixmap.fill_path(&path, &paint, tiny_skia::FillRule::EvenOdd, tiny_skia::Transform::identity(), clip.as_ref());
        self.stroke_path_with(&path);
    }
    fn stroke(&mut self) {
        let Some(path) = self.take_path() else { return };
        self.stroke_path_with(&path);
    }
    fn end_path(&mut self) {
        let _ = self.take_path();
    }
    fn clip_nonzero(&mut self) {
        let Some(path) = self.take_path() else { return };
        let mut mask = tiny_skia::Mask::new(self.pixmap.width(), self.pixmap.height()).unwrap_or_else(|| tiny_skia::Mask::new(1, 1).unwrap());
        mask.fill_path(&path, tiny_skia::FillRule::Winding, true, tiny_skia::Transform::identity());
        // A clip inside a clip shows only what both allow.
        if let Some(outer) = &self.state.clip {
            let (inner, outer) = (mask.data().to_vec(), outer.data());
            for (i, byte) in mask.data_mut().iter_mut().enumerate() {
                *byte = ((inner[i] as u16 * outer[i] as u16) / 255) as u8;
            }
        }
        self.state.clip = Some(mask);
    }
    fn set_fill_gray(&mut self, grey: f32) {
        self.state.fill = tiny_skia::Color::from_rgba(grey, grey, grey, 1.0).unwrap_or(tiny_skia::Color::BLACK);
    }
    fn set_fill_rgb(&mut self, r: f32, g: f32, b: f32) {
        self.state.fill = tiny_skia::Color::from_rgba(r, g, b, 1.0).unwrap_or(tiny_skia::Color::BLACK);
    }
    fn set_stroke_gray(&mut self, grey: f32) {
        self.state.stroke = tiny_skia::Color::from_rgba(grey, grey, grey, 1.0).unwrap_or(tiny_skia::Color::BLACK);
    }
    fn set_line_width(&mut self, w: f32) {
        self.state.width = w;
    }
    fn set_dash(&mut self, pattern: &[f32], phase: f32) {
        let scaled: Vec<f32> = pattern.iter().map(|d| d * self.scale).collect();
        self.state.dash = tiny_skia::StrokeDash::new(scaled, phase * self.scale);
    }
    fn save_state(&mut self) {
        self.stack.push(self.state.clone());
    }
    fn restore_state(&mut self) {
        if let Some(s) = self.stack.pop() {
            self.state = s;
        }
    }
    fn text(&mut self, font: Name, size: f32, x: f32, y: f32, s: &str, grey: f32) {
        self.write(font, size, x, y, s, grey, false);
    }
    fn text_turned(&mut self, font: Name, size: f32, x: f32, y: f32, s: &str, grey: f32) {
        self.write(font, size, x, y, s, grey, true);
    }
}

/// The two faces a chart is set in, found on this machine.
///
/// A PDF names Helvetica and lets the reader find it; a picture has to have the letters
/// themselves. Arial carries Helvetica's metrics exactly, which is why the width tables
/// the layout is measured with still hold, and it is on every Windows machine already —
/// so it is read from where it is installed rather than shipped by us. Liberation Sans
/// and DejaVu stand in on a machine without it.
fn faces() -> Result<(FontVec, FontVec)> {
    let windows = std::env::var("WINDIR").unwrap_or_else(|_| "C:/Windows".into());
    let candidates: [(PathBuf, PathBuf); 4] = [
        (PathBuf::from(format!("{windows}/Fonts/arial.ttf")), PathBuf::from(format!("{windows}/Fonts/arialbd.ttf"))),
        (
            PathBuf::from("/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf"),
            PathBuf::from("/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf"),
        ),
        (
            PathBuf::from("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"),
            PathBuf::from("/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"),
        ),
        (PathBuf::from("/System/Library/Fonts/Supplemental/Arial.ttf"), PathBuf::from("/System/Library/Fonts/Supplemental/Arial Bold.ttf")),
    ];
    for (regular, bold) in candidates {
        let (Ok(r), Ok(b)) = (std::fs::read(&regular), std::fs::read(&bold)) else { continue };
        let (Ok(r), Ok(b)) = (FontVec::try_from_vec(r), FontVec::try_from_vec(b)) else { continue };
        return Ok((r, b));
    }
    Err(anyhow!(
        "no sans-serif font was found on this computer to draw a picture with; the PDF chart needs none and still works"
    ))
}

/// Kept so the PDF writer's line-cap type stays used where it is needed.
pub const BUTT: LineCapStyle = LineCapStyle::ButtCap;
