/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Turns a JSON definition plus source assets into resource binaries and the
 * Rust code that refers to them. See docs/resgen.md.
 */

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::c_void;
use std::fs;
use std::path::{Path, PathBuf};

use freetype::{Library, face::LoadFlag};
use resvg::tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};
use resvg::usvg::{Options, Tree};
use serde::Deserialize;

// Shared with the firmware side, which uses parts the host does not.
#[allow(dead_code)]
#[path = "../../../libs/devkit_gfx/src/format.rs"]
mod format;

use format::{FontHeader, GlyphRecord, ImageFormat, ImageHeader};

const ICON_FIRST: u32 = 0xE000;
const ICON_LAST: u32 = 0xEFFF;
const BIN_DIR: &str = "resgen";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Definition {
    #[serde(default)]
    font: BTreeMap<String, FontEntry>,
    #[serde(default)]
    image: BTreeMap<String, ImageEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FontEntry {
    size: u16,
    #[serde(default = "default_bpp")]
    bpp: u8,
    icon: Option<IconMap>,
    font: Option<String>,
    #[serde(default)]
    glyph: Vec<String>,
    glyph_file: Option<String>,
    #[serde(default)]
    variation: BTreeMap<String, f64>,
    #[serde(default = "default_compress")]
    compress: bool,
}

type IconMap = BTreeMap<String, String>;

fn default_bpp() -> u8 {
    4
}

fn default_compress() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImageEntry {
    file: String,
    format: String,
    width: Option<u32>,
    height: Option<u32>,
}

struct Context {
    definition: Definition,
    base: PathBuf,
    fonts_dir: PathBuf,
    icons: BTreeMap<String, u32>,
}

impl Context {
    fn load(path: &Path, fonts_dir: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let definition: Definition = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        for name in definition.font.keys().chain(definition.image.keys()) {
            if !is_ident(name) {
                return Err(format!("{name}: not an identifier"));
            }
            if definition.font.contains_key(name) && definition.image.contains_key(name) {
                return Err(format!("{name}: used by both a font and an image"));
            }
        }
        for (name, font) in &definition.font {
            if !matches!(font.bpp, 1 | 2 | 4) {
                return Err(format!("font.{name}.bpp: must be 1, 2 or 4"));
            }
            if font.icon.is_some() == font.font.is_some() {
                return Err(format!("font.{name}: needs exactly one of 'icon' or 'font'"));
            }
        }
        let mut icons = BTreeMap::new();
        for font in definition.font.values() {
            for icon in font.icon.iter().flatten().map(|(k, _)| k) {
                if !is_ident(icon) {
                    return Err(format!("icon {icon}: not an identifier"));
                }
                let next = ICON_FIRST + icons.len() as u32;
                icons.entry(icon.clone()).or_insert(next);
            }
        }
        if ICON_FIRST + icons.len() as u32 > ICON_LAST + 1 {
            return Err("too many icons".into());
        }
        let base = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        Ok(Self { definition, base, fonts_dir: fonts_dir.to_path_buf(), icons })
    }

    /// `@name` is a file bundled with resgen, with its extension left out.
    fn file(&self, relative: &str, extension: &str) -> PathBuf {
        match relative.strip_prefix('@') {
            Some(name) => self.fonts_dir.join(format!("{name}.{extension}")),
            None => self.base.join(relative),
        }
    }

    fn inputs(&self, name: &str) -> Vec<PathBuf> {
        if let Some(font) = self.definition.font.get(name) {
            let mut files: Vec<PathBuf> = font.icon.iter().flatten().map(|(_, f)| self.base.join(f)).collect();
            files.extend(font.font.iter().map(|f| self.file(f, "ttf")));
            files.extend(font.glyph_file.iter().map(|f| self.file(f, "txt")));
            return files;
        }
        vec![self.base.join(&self.definition.image[name].file)]
    }

    fn names(&self) -> impl Iterator<Item = &String> {
        self.definition.font.keys().chain(self.definition.image.keys())
    }
}

fn is_ident(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn write_if_changed(path: &Path, data: &[u8]) -> Result<(), String> {
    if fs::read(path).is_ok_and(|old| old == data) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    fs::write(path, data).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn command(args: &[String]) -> Result<(), String> {
    const USAGE: &str = "usage: devkit resgen <inputs|index|entry> <definition> <fonts dir> <out> [entry]";
    let [action, definition, fonts_dir, out, rest @ ..] = args else { return Err(USAGE.into()) };
    let context = Context::load(Path::new(definition), Path::new(fonts_dir))?;
    let out = Path::new(out);
    match (action.as_str(), rest) {
        ("inputs", []) => write_inputs(&context, out),
        ("index", []) => write_index(&context, out),
        ("entry", [name]) => write_entry(&context, name, out),
        _ => Err(USAGE.into()),
    }
}

fn write_inputs(context: &Context, out: &Path) -> Result<(), String> {
    let names: Vec<&str> = context.names().map(String::as_str).collect();
    let mut text = format!("set(RESGEN_ENTRIES {})\n", names.join(" "));
    for name in names {
        let inputs: Vec<String> =
            context.inputs(name).iter().map(|p| format!("\"{}\"", p.display())).collect();
        text += &format!("set(RESGEN_INPUTS_{name} {})\n", inputs.join(" "));
    }
    write_if_changed(out, text.as_bytes())
}

fn write_index(context: &Context, out: &Path) -> Result<(), String> {
    let mut text = String::from(
        "#[repr(C, align(64))]\nstruct ResourceData<T: ?Sized>(T);\n\n",
    );
    for name in context.names() {
        let symbol = name.to_uppercase();
        let path = format!("{BIN_DIR}/{name}.bin");
        let (ty, parse) = if context.definition.font.contains_key(name) {
            ("devkit_gfx::Font", "devkit_gfx::Font::parse")
        } else {
            ("devkit_gfx::Image", "devkit_gfx::Image::parse")
        };
        text += &format!(
            "static {symbol}_DATA: ResourceData<[u8; include_bytes!(\"{path}\").len()]> =\n    \
             ResourceData(*include_bytes!(\"{path}\"));\n\
             pub static {symbol}: {ty} = {parse}(&{symbol}_DATA.0);\n\n"
        );
    }
    text += "pub mod icons {\n";
    for (icon, codepoint) in &context.icons {
        text += &format!("    pub const {}: char = '\\u{{{codepoint:x}}}';\n", icon.to_uppercase());
    }
    text += "}\n";
    write_if_changed(&out.join("resources.rs"), text.as_bytes())
}

fn write_entry(context: &Context, name: &str, out: &Path) -> Result<(), String> {
    let data = if let Some(font) = context.definition.font.get(name) {
        match &font.icon {
            Some(icons) => icon_font(context, name, font, icons)?,
            None => text_font(context, name, font)?,
        }
    } else if let Some(image) = context.definition.image.get(name) {
        image_data(context, name, image)?
    } else {
        return Err(format!("no entry {name}"));
    };
    write_if_changed(&out.join(BIN_DIR).join(format!("{name}.bin")), &data)
}

struct Glyph {
    codepoint: u32,
    advance: u16,
    levels: Vec<u8>,
    width: usize,
    height: usize,
    ofs_x: i32,
    ofs_y: i32,
}

struct Metrics {
    line_height: i16,
    base_line: i16,
    underline_position: i8,
    underline_thickness: u8,
}

/// `alpha` is `width` x `height`; (`left`, `bottom`) is the lower left corner
/// relative to the origin. Empty rows and columns are trimmed.
fn make_glyph(
    codepoint: u32,
    advance: u16,
    alpha: &[u8],
    (width, height): (usize, usize),
    (left, bottom): (i32, i32),
    bpp: u8,
) -> Glyph {
    let max = (1u32 << bpp) - 1;
    let levels: Vec<u8> = alpha.iter().map(|&a| ((u32::from(a) * max + 127) / 255) as u8).collect();
    let mut x0 = width;
    let mut x1 = 0;
    let mut y0 = height;
    let mut y1 = 0;
    for y in 0..height {
        for x in 0..width {
            if levels[y * width + x] != 0 {
                x0 = x0.min(x);
                x1 = x1.max(x + 1);
                y0 = y0.min(y);
                y1 = y1.max(y + 1);
            }
        }
    }
    if x1 == 0 {
        return Glyph { codepoint, advance, levels: Vec::new(), width: 0, height: 0, ofs_x: 0, ofs_y: 0 };
    }
    let mut trimmed = Vec::with_capacity((x1 - x0) * (y1 - y0));
    for y in y0..y1 {
        trimmed.extend_from_slice(&levels[y * width + x0..y * width + x1]);
    }
    Glyph {
        codepoint,
        advance,
        levels: trimmed,
        width: x1 - x0,
        height: y1 - y0,
        ofs_x: left + x0 as i32,
        ofs_y: bottom + (height - y1) as i32,
    }
}

fn encode_font(name: &str, entry: &FontEntry, mut glyphs: Vec<Glyph>, metrics: &Metrics) -> Result<Vec<u8>, String> {
    glyphs.sort_by_key(|g| g.codepoint);
    let mut records = Vec::with_capacity(glyphs.len() * format::GLYPH_SIZE);
    let mut data = Vec::new();
    for g in &glyphs {
        let too_large = g.width > 255 || g.height > 255;
        let off_range = i8::try_from(g.ofs_x).is_err() || i8::try_from(g.ofs_y).is_err();
        if too_large || off_range {
            return Err(format!("font.{name}: U+{:04X} does not fit a glyph record", g.codepoint));
        }
        let mut bitmap = data.len() as u32;
        if !g.levels.is_empty() {
            let raw = format::pack_levels(&g.levels, entry.bpp);
            let rle = entry.compress.then(|| {
                format::rle_encode(&format::prefilter_rows(&g.levels, g.width), entry.bpp)
            });
            match rle.filter(|rle| rle.len() < raw.len()) {
                Some(rle) => {
                    bitmap |= format::GLYPH_COMPRESSED;
                    data.extend_from_slice(&rle);
                }
                None => data.extend_from_slice(&raw),
            }
        }
        GlyphRecord {
            bitmap,
            advance: g.advance,
            width: g.width as u8,
            height: g.height as u8,
            ofs_x: g.ofs_x as i8,
            ofs_y: g.ofs_y as i8,
        }
        .write(&mut records);
    }
    data.extend_from_slice(&[0; format::FONT_DATA_SLACK]);

    let codepoints = format::FONT_HEADER_SIZE as u32;
    let table = codepoints + glyphs.len() as u32 * 4;
    let header = FontHeader {
        bpp: entry.bpp,
        flags: if entry.compress { format::FONT_PREFILTER } else { 0 },
        px: entry.size,
        line_height: metrics.line_height,
        base_line: metrics.base_line,
        max_ascent: glyphs.iter().map(|g| (g.height as i32 + g.ofs_y) as i16).max().unwrap_or(0),
        max_descent: glyphs.iter().map(|g| (-g.ofs_y) as i16).max().unwrap_or(0),
        underline_position: metrics.underline_position,
        underline_thickness: metrics.underline_thickness,
        glyph_count: glyphs.len() as u32,
        codepoints,
        glyphs: table,
        data: table + records.len() as u32,
        data_len: data.len() as u32,
    };
    let mut out = Vec::with_capacity(header.data as usize + data.len());
    header.write(&mut out);
    for g in &glyphs {
        out.extend_from_slice(&g.codepoint.to_le_bytes());
    }
    out.extend_from_slice(&records);
    out.extend_from_slice(&data);
    Ok(out)
}

fn load_svg(path: &Path) -> Result<Tree, String> {
    let data = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Tree::from_data(&data, &Options::default()).map_err(|e| format!("{}: {e}", path.display()))
}

fn icon_font(context: &Context, name: &str, entry: &FontEntry, icons: &IconMap) -> Result<Vec<u8>, String> {
    let size = u32::from(entry.size);
    let mut glyphs = Vec::with_capacity(icons.len());
    for (icon, file) in icons {
        let tree = load_svg(&context.base.join(file))?;
        let width = ((size as f32 * tree.size().width() / tree.size().height()).round() as u32).max(1);
        let mut pixmap = Pixmap::new(width, size).ok_or_else(|| format!("{icon}: empty icon"))?;
        let transform =
            Transform::from_scale(width as f32 / tree.size().width(), size as f32 / tree.size().height());
        resvg::render(&tree, transform, &mut pixmap.as_mut());
        let alpha: Vec<u8> = pixmap.data().chunks_exact(4).map(|p| p[3]).collect();
        glyphs.push(make_glyph(
            context.icons[icon],
            (width * 16) as u16,
            &alpha,
            (width as usize, size as usize),
            (0, 0),
            entry.bpp,
        ));
    }
    let metrics = Metrics {
        line_height: entry.size as i16,
        base_line: 0,
        underline_position: 0,
        underline_thickness: 1,
    };
    encode_font(name, entry, glyphs, &metrics)
}

fn glyph_set(context: &Context, name: &str, entry: &FontEntry) -> Result<BTreeSet<u32>, String> {
    let mut chars: BTreeSet<char> = entry.glyph.iter().flat_map(|s| s.chars()).collect();
    if let Some(file) = &entry.glyph_file {
        let path = context.file(file, "txt");
        let text = fs::read_to_string(&path).map_err(|e| format!("font.{name}.glyph_file: {}: {e}", path.display()))?;
        chars.extend(text.lines().filter(|l| !l.starts_with('#')).flat_map(str::chars));
    }
    Ok(chars.into_iter().filter(|c| !matches!(c, '\r' | '\n' | '\t')).map(u32::from).collect())
}

#[repr(C)]
struct VarAxis {
    name: *const i8,
    minimum: std::ffi::c_long,
    default: std::ffi::c_long,
    maximum: std::ffi::c_long,
    tag: std::ffi::c_ulong,
    strid: u32,
}

#[repr(C)]
struct MmVar {
    num_axis: u32,
    num_designs: u32,
    num_namedstyles: u32,
    axis: *mut VarAxis,
    namedstyle: *mut c_void,
}

unsafe extern "C" {
    fn FT_Get_MM_Var(face: *mut c_void, master: *mut *mut MmVar) -> i32;
    fn FT_Done_MM_Var(library: *mut c_void, master: *mut MmVar) -> i32;
    fn FT_Set_Var_Design_Coordinates(face: *mut c_void, count: u32, coords: *const std::ffi::c_long) -> i32;
}

fn set_variation(
    library: &Library,
    face: &mut freetype::Face,
    where_: &str,
    variation: &BTreeMap<String, f64>,
) -> Result<(), String> {
    if variation.is_empty() {
        return Ok(());
    }
    let raw = face.raw_mut() as *mut _ as *mut c_void;
    let mut master = std::ptr::null_mut();
    if unsafe { FT_Get_MM_Var(raw, &mut master) } != 0 {
        return Err(format!("{where_}: not a variable font"));
    }
    let axes = unsafe { std::slice::from_raw_parts((*master).axis, (*master).num_axis as usize) };
    let mut coords = Vec::with_capacity(axes.len());
    let mut known = 0;
    for axis in axes {
        let tag: String = axis.tag.to_be_bytes()[4..].iter().map(|&b| b as char).collect();
        match variation.get(&tag) {
            Some(value) => {
                coords.push((value * 65536.0) as std::ffi::c_long);
                known += 1;
            }
            None => coords.push(axis.default),
        }
    }
    let status = unsafe { FT_Set_Var_Design_Coordinates(raw, coords.len() as u32, coords.as_ptr()) };
    unsafe { FT_Done_MM_Var(library.raw() as *mut _ as *mut c_void, master) };
    if status != 0 || known != variation.len() {
        return Err(format!("{where_}.variation: axes do not match the font"));
    }
    Ok(())
}

fn text_font(context: &Context, name: &str, entry: &FontEntry) -> Result<Vec<u8>, String> {
    let where_ = format!("font.{name}");
    let path = context.file(entry.font.as_deref().unwrap_or_default(), "ttf");
    let library = Library::init().map_err(|e| format!("freetype: {e}"))?;
    let mut face = library.new_face(&path, 0).map_err(|e| format!("{where_}: {}: {e}", path.display()))?;
    set_variation(&library, &mut face, &where_, &entry.variation)?;
    face.set_pixel_sizes(0, u32::from(entry.size)).map_err(|e| format!("{where_}: {e}"))?;

    let mut codepoints = glyph_set(context, name, entry)?;
    if codepoints.is_empty() {
        codepoints = face.chars().map(|(cp, _)| cp as u32).filter(|&cp| cp >= 0x20 && !(0x7F..=0x9F).contains(&cp)).collect();
    }
    let mut glyphs = Vec::with_capacity(codepoints.len());
    let mut missing = Vec::new();
    for cp in codepoints {
        if face.get_char_index(cp as usize).is_none() {
            missing.push(cp);
            continue;
        }
        face.load_char(cp as usize, LoadFlag::RENDER | LoadFlag::TARGET_LIGHT).map_err(|e| format!("{where_}: U+{cp:04X}: {e}"))?;
        let slot = face.glyph();
        let bitmap = slot.bitmap();
        let (width, rows, pitch) = (bitmap.width() as usize, bitmap.rows() as usize, bitmap.pitch() as usize);
        let mut alpha = Vec::with_capacity(width * rows);
        if rows > 0 {
            for row in bitmap.buffer().chunks(pitch).take(rows) {
                alpha.extend_from_slice(&row[..width]);
            }
        }
        let advance = ((slot.advance().x + 2) / 4) as u16;
        glyphs.push(make_glyph(
            cp,
            advance,
            &alpha,
            (width, rows),
            (slot.bitmap_left(), slot.bitmap_top() - rows as i32),
            entry.bpp,
        ));
    }
    if !missing.is_empty() {
        let shown: Vec<String> = missing.iter().take(20).map(|cp| format!("U+{cp:04X}")).collect();
        eprintln!("resgen: warning: {where_}: {} glyphs not in {}: {}", missing.len(), path.display(), shown.join(" "));
    }

    let size = face.size_metrics().ok_or_else(|| format!("{where_}: no size metrics"))?;
    let ascender = (size.ascender as f32 / 64.0).ceil() as i16;
    let descender = (size.descender as f32 / 64.0).floor() as i16;
    let scale = f32::from(entry.size) / face.em_size() as f32;
    let metrics = Metrics {
        line_height: ascender - descender,
        base_line: -descender,
        underline_position: (f32::from(face.underline_position()) * scale).round() as i8,
        underline_thickness: ((f32::from(face.underline_thickness()) * scale).round() as u8).max(1),
    };
    encode_font(name, entry, glyphs, &metrics)
}

fn image_data(context: &Context, name: &str, entry: &ImageEntry) -> Result<Vec<u8>, String> {
    let path = context.base.join(&entry.file);
    let format = match entry.format.as_str() {
        "RGB565" => ImageFormat::Rgb565,
        "RGB888" => ImageFormat::Rgb888,
        "ARGB8888" => ImageFormat::Argb8888,
        "A8" => ImageFormat::A8,
        "L8" => ImageFormat::L8,
        other => return Err(format!("image.{name}.format: unknown format {other}")),
    };
    let source = Pixmap::load_png(&path).map_err(|e| format!("image.{name}: {}: {e}", path.display()))?;
    let (sw, sh) = (source.width(), source.height());
    let (width, height) = match (entry.width, entry.height) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, (w * sh).div_ceil(sw).max(1)),
        (None, Some(h)) => ((h * sw).div_ceil(sh).max(1), h),
        (None, None) => (sw, sh),
    };
    let mut scaled = Pixmap::new(width, height).ok_or_else(|| format!("image.{name}: empty size"))?;
    let paint = PixmapPaint { quality: FilterQuality::Bicubic, ..PixmapPaint::default() };
    let transform = Transform::from_scale(width as f32 / sw as f32, height as f32 / sh as f32);
    scaled.draw_pixmap(0, 0, source.as_ref(), &paint, transform, None);

    let mut pixels = Vec::with_capacity((width * height) as usize * format.bytes());
    for p in scaled.pixels() {
        let c = p.demultiply();
        let (r, g, b, a) = (c.red(), c.green(), c.blue(), c.alpha());
        match format {
            ImageFormat::Rgb565 => {
                let v = u16::from(r >> 3) << 11 | u16::from(g >> 2) << 5 | u16::from(b >> 3);
                pixels.extend_from_slice(&v.to_le_bytes());
            }
            ImageFormat::Rgb888 => pixels.extend_from_slice(&[b, g, r]),
            ImageFormat::Argb8888 => pixels.extend_from_slice(&[b, g, r, a]),
            ImageFormat::A8 => pixels.push(a),
            ImageFormat::L8 => pixels.push(((u32::from(r) * 77 + u32::from(g) * 150 + u32::from(b) * 29) >> 8) as u8),
        }
    }
    let mut out = Vec::with_capacity(format::IMAGE_DATA_OFFSET + pixels.len());
    ImageHeader {
        format,
        width: width as u16,
        height: height as u16,
        stride: width * format.bytes() as u32,
        data_len: pixels.len() as u32,
    }
    .write(&mut out);
    out.resize(format::IMAGE_DATA_OFFSET, 0);
    out.extend_from_slice(&pixels);
    Ok(out)
}
