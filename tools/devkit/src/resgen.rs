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
use serde_json::{Map, Value, json};

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
    #[serde(default)]
    theme: BTreeMap<String, ThemeEntry>,
}

/// `fonts` names entries of `font`, several making a chain; a bundled base also
/// defines them, in its own `font`. `icons` maps a name of `Icons` to an SVG.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ThemeEntry {
    base: Option<String>,
    #[serde(default)]
    fonts: BTreeMap<String, FontChoice>,
    #[serde(default)]
    icons: IconMap,
    #[serde(default)]
    icon_size: BTreeMap<String, u16>,
    #[serde(default)]
    colors: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(default)]
    vars: Map<String, Value>,
    #[serde(default)]
    widgets: Map<String, Value>,
    #[serde(default)]
    keyboards: Map<String, Value>,
    #[serde(default)]
    motion: Map<String, Value>,
    #[serde(default)]
    font: BTreeMap<String, FontEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyboardEntry {
    height: i16,
    layers: Vec<LayerEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LayerEntry {
    id: String,
    columns: u8,
    layout: Vec<Vec<Value>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyEntry {
    icon: Option<String>,
    label: Option<String>,
    action: Option<String>,
    #[serde(default)]
    once: bool,
    double: Option<String>,
    insert: Option<String>,
    custom: Option<u16>,
    #[serde(default = "default_key_width")]
    width: f64,
    role: Option<String>,
    #[serde(default)]
    repeat: bool,
    font: Option<String>,
    style: Option<String>,
}

#[derive(Deserialize, Clone)]
#[serde(untagged)]
enum FontChoice {
    One(String),
    Chain(Vec<String>),
}

impl FontChoice {
    fn names(&self) -> Vec<String> {
        match self {
            Self::One(name) => vec![name.clone()],
            Self::Chain(names) => names.clone(),
        }
    }
}

const THEME_DIR: &str = "themes";
const THEME_FONTS: [&str; 3] = ["title", "body", "caption"];
const THEME_ICONS: [&str; 9] = ["back", "more", "close", "check", "chevron", "expand", "shift", "backspace", "enter"];
const THEME_COLORS: [&str; 5] = ["text", "primary", "surface", "background", "divider"];
const THEME_KEYBOARDS: [&str; 2] = ["text", "number"];
const THEME_ANIMATED: [&str; 3] = ["color", "opacity", "position"];
const THEME_STATES: [&str; 6] = ["pressed", "hovered", "focused", "selected", "disabled", "checked"];
const DEFAULT_ICON_SIZE: u16 = 40;
const THEME_KEYS: &str = "keys";
const KEY_QUARTERS: f64 = 4.0;

#[derive(Deserialize, Clone)]
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

fn default_key_width() -> f64 {
    1.0
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImageEntry {
    file: String,
    format: String,
    width: Option<u32>,
    height: Option<u32>,
}

/// A theme after merging its base, ready for code generation.
struct Theme {
    fonts: BTreeMap<String, Vec<String>>,
    /// Each name of `Icons`, with the font it went to and its own symbol.
    icons: BTreeMap<String, (String, String)>,
    light: BTreeMap<String, String>,
    dark: Option<BTreeMap<String, String>>,
    widgets: BTreeMap<String, BTreeMap<String, Token>>,
    /// Rust of each style, by where it is defined.
    styles: BTreeMap<String, String>,
    keyboards: BTreeMap<String, ThemeKeyboard>,
    motion: BTreeMap<String, Token>,
    unused_vars: Vec<String>,
    unused_keys: Vec<String>,
}

struct ThemeKeyboard {
    height: i16,
    layers: Vec<ThemeLayer>,
}

struct ThemeLayer {
    columns: u8,
    rows: Vec<Vec<ThemeKey>>,
}

/// Rust of a key. `icon` is left a name, as code points come after the themes.
struct ThemeKey {
    icon: Option<String>,
    label: String,
    action: String,
    options: String,
}

impl ThemeKey {
    fn insert(text: &str) -> Self {
        Self {
            icon: None,
            label: format!("devkit_ui::keyboard::KeyLabel::Text({text:?})"),
            action: format!("devkit_ui::keyboard::KeyAction::Insert({text:?})"),
            options: String::new(),
        }
    }

    fn spacer(width: f64) -> Self {
        Self {
            icon: None,
            label: "devkit_ui::keyboard::KeyLabel::None".to_string(),
            action: "devkit_ui::keyboard::KeyAction::None".to_string(),
            options: format!(".width({width:?})"),
        }
    }
}

/// What a value of `vars` or `widgets` comes to.
#[derive(Clone)]
enum Token {
    Number(i64),
    /// A `ColorRole`.
    Color(String),
    Font(String),
    Style(String),
    /// A value of an enum, or a bool.
    Literal(String),
}

impl Token {
    fn code(&self, symbol: &str) -> String {
        match self {
            Self::Number(number) => number.to_string(),
            Self::Color(code) | Self::Literal(code) => code.clone(),
            Self::Font(slot) => format!("&{}", chain_symbol(symbol, slot)),
            Self::Style(path) => format!("&{}", style_symbol(symbol, path)),
        }
    }
}

fn chain_symbol(symbol: &str, slot: &str) -> String {
    format!("{symbol}_{}_CHAIN", slot.to_uppercase())
}

fn style_symbol(symbol: &str, path: &str) -> String {
    format!("{symbol}_{}_STYLE", path.replace('.', "_").to_uppercase())
}

fn keyboard_symbol(symbol: &str, keyboard: &str) -> String {
    format!("{symbol}_{}_KEYBOARD", keyboard.to_uppercase())
}

/// Sets what `from` has, going into objects, so a style changes key by key.
fn merge(into: &mut Map<String, Value>, from: Map<String, Value>) {
    for (key, value) in from {
        match (into.get_mut(&key), value) {
            (Some(Value::Object(old)), Value::Object(new)) => merge(old, new),
            (_, value) => {
                into.insert(key, value);
            }
        }
    }
}

fn is_enum_value(text: &str) -> bool {
    let is_name = |name: &str| name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) && is_ident(name);
    text.split_once('.').is_some_and(|(ty, value)| is_name(ty) && is_name(value))
}

/// Follows the references of one theme down to their values.
struct Resolver<'a> {
    theme: &'a str,
    symbol: String,
    root: Value,
    colors: &'a BTreeMap<String, String>,
    styles: BTreeMap<String, String>,
    used_vars: BTreeSet<String>,
    used_fonts: BTreeSet<String>,
    used_keys: BTreeSet<String>,
    following: Vec<String>,
}

impl Resolver<'_> {
    fn error(&self, path: &str, message: impl std::fmt::Display) -> String {
        format!("theme.{}.{path}: {message}", self.theme)
    }

    fn value(&mut self, path: &str, value: &Value) -> Result<Token, String> {
        match value {
            Value::Number(number) => number.as_i64().map(Token::Number).ok_or_else(|| self.error(path, "not an integer")),
            Value::Bool(flag) => Ok(Token::Literal(flag.to_string())),
            Value::String(text) => self.text(path, text),
            Value::Object(object) => self.style(path, object),
            _ => Err(self.error(path, "not a value")),
        }
    }

    fn text(&mut self, path: &str, text: &str) -> Result<Token, String> {
        if text.starts_with('#') {
            let [r, g, b, a] = parse_color(text).ok_or_else(|| self.error(path, format!("{text} is not #rrggbb or #rrggbbaa")))?;
            return Ok(Token::Color(format!("devkit_ui::ColorRole::Fixed(devkit_gfx::Color::rgba({r}, {g}, {b}, {a}))")));
        }
        if let Some(color) = text.strip_prefix("color.") {
            if THEME_COLORS.contains(&color) {
                return Ok(Token::Color(format!("devkit_ui::ColorRole::{}", type_name(color))));
            }
            if !self.colors.contains_key(color) {
                return Err(self.error(path, format!("no color {color}")));
            }
            return Ok(Token::Color(format!("colors::{}", color.to_uppercase())));
        }
        if let Some(font) = text.strip_prefix("font.") {
            self.used_fonts.insert(font.to_string());
            return Ok(Token::Font(font.to_string()));
        }
        if is_enum_value(text) {
            return Ok(Token::Literal(format!("devkit_ui::{}", text.replace('.', "::"))));
        }
        self.enter(path, text)?;
        let target = self.find(path, text)?;
        let token = self.value(text, &target);
        self.following.pop();
        token
    }

    fn enter(&mut self, path: &str, text: &str) -> Result<(), String> {
        if self.following.iter().any(|followed| followed == text) {
            return Err(self.error(path, format!("{text} refers to itself")));
        }
        self.following.push(text.to_string());
        Ok(())
    }

    fn find(&mut self, path: &str, text: &str) -> Result<Value, String> {
        let mut names = text.split('.');
        if !matches!(names.next(), Some("vars" | "widgets")) {
            return Err(self.error(path, format!("{text} is not a value")));
        }
        if let Some(var) = text.strip_prefix("vars.") {
            self.used_vars.insert(var.split('.').next().unwrap_or(var).to_string());
        }
        let mut current = self.root.get(text.split('.').next().unwrap_or(text)).cloned().unwrap_or(Value::Null);
        for name in names {
            let fields = match &current {
                Value::Object(object) => self.flat(text, object)?,
                _ => Map::new(),
            };
            current = fields.get(name).cloned().ok_or_else(|| self.error(path, format!("no {text}")))?;
        }
        Ok(current)
    }

    /// `object` with what it extends under it.
    fn flat(&mut self, path: &str, object: &Map<String, Value>) -> Result<Map<String, Value>, String> {
        let references: Vec<&Value> = match object.get("extends") {
            None => Vec::new(),
            Some(Value::Array(list)) => list.iter().collect(),
            Some(one) => vec![one],
        };
        let mut flat = Map::new();
        for reference in references {
            let Some(text) = reference.as_str() else { return Err(self.error(path, "extends takes references")) };
            self.enter(path, text)?;
            let Value::Object(target) = self.find(path, text)? else { return Err(self.error(path, format!("{text} is not a style"))) };
            merge(&mut flat, self.flat(text, &target)?);
            self.following.pop();
        }
        merge(&mut flat, object.clone());
        flat.remove("extends");
        Ok(flat)
    }

    fn style(&mut self, path: &str, object: &Map<String, Value>) -> Result<Token, String> {
        if !self.styles.contains_key(path) {
            let flat = self.flat(path, object)?;
            let font = match flat.get("font").map(|font| self.value(path, font)).transpose()? {
                Some(token @ Token::Font(_)) => format!("Some({})", token.code(&self.symbol)),
                Some(_) => return Err(self.error(path, "font is not a font")),
                None => "None".to_string(),
            };
            let base = self.props(path, &flat)?;
            let mut states = Vec::new();
            for (state, value) in flat.get("states").and_then(Value::as_object).into_iter().flatten() {
                let change = match value {
                    Value::String(text) => self.referred(path, text)?,
                    Value::Object(object) => self.flat(path, object)?,
                    _ => return Err(self.error(path, format!("states.{state}: not a style"))),
                };
                if change.contains_key("font") || change.contains_key("states") {
                    return Err(self.error(path, format!("states.{state}: only values of a node change with a state")));
                }
                states.push(format!("({}, {})", state_code(state).ok_or_else(|| self.error(path, format!("no state {state}")))?, self.props(path, &change)?));
            }
            let code = format!("devkit_ui::StateStyle {{ font: {font}, base: {base}, states: &[{}] }}", states.join(", "));
            self.styles.insert(path.to_string(), code);
        }
        Ok(Token::Style(path.to_string()))
    }

    fn keyboard(&mut self, path: &str, value: &Value, keys: &Map<String, Value>) -> Result<ThemeKeyboard, String> {
        let entry: KeyboardEntry = serde_json::from_value(value.clone()).map_err(|e| self.error(path, e))?;
        if entry.layers.is_empty() {
            return Err(self.error(path, "no layers"));
        }
        let Token::Number(gap) = self.text(path, "widgets.keyboard.gap")? else { return Err(self.error(path, "widgets.keyboard.gap is not a number")) };
        let ids: Vec<&str> = entry.layers.iter().map(|layer| layer.id.as_str()).collect();
        let mut layers = Vec::new();
        for (index, layer) in entry.layers.iter().enumerate() {
            let path = format!("{path}.layers.{}", layer.id);
            if ids[..index].contains(&layer.id.as_str()) {
                return Err(self.error(&path, "the id is used twice"));
            }
            let row_count = layer.layout.len() as i64;
            if row_count == 0 || (i64::from(entry.height) - gap) / row_count - gap <= 0 {
                return Err(self.error(&path, format!("height {} has no room for {row_count} rows", entry.height)));
            }
            let mut rows = Vec::new();
            for items in &layer.layout {
                let mut quarters = 0.0;
                let mut row = Vec::new();
                for item in items {
                    let (key, width) = self.layer_key(&path, item, keys, &ids)?;
                    if width <= 0.0 || (width * KEY_QUARTERS).fract() != 0.0 {
                        return Err(self.error(&path, format!("{item}: key widths go by quarters")));
                    }
                    quarters += width * KEY_QUARTERS;
                    row.push(key);
                }
                if quarters > f64::from(layer.columns) * KEY_QUARTERS {
                    return Err(self.error(&path, format!("a row is wider than {} columns", layer.columns)));
                }
                rows.push(row);
            }
            layers.push(ThemeLayer { columns: layer.columns, rows });
        }
        Ok(ThemeKeyboard { height: entry.height, layers })
    }

    /// A key of a row, with its width.
    fn layer_key(&mut self, path: &str, item: &Value, keys: &Map<String, Value>, ids: &[&str]) -> Result<(ThemeKey, f64), String> {
        match item {
            Value::Number(width) => {
                let width = width.as_f64().ok_or_else(|| self.error(path, format!("{item}: not a width")))?;
                Ok((ThemeKey::spacer(width), width))
            }
            Value::String(name) if keys.contains_key(name) => {
                let fields = self.key_fields(path, name, keys)?;
                let entry: KeyEntry = serde_json::from_value(Value::Object(fields)).map_err(|e| self.error(path, format!("{name}: {e}")))?;
                Ok((self.key(path, name, &entry, ids)?, entry.width))
            }
            Value::String(text) => Ok((ThemeKey::insert(text), default_key_width())),
            _ => Err(self.error(path, format!("{item}: not a key"))),
        }
    }

    /// The key `name` with what it extends under it.
    fn key_fields(&mut self, path: &str, name: &str, keys: &Map<String, Value>) -> Result<Map<String, Value>, String> {
        let Some(Value::Object(key)) = keys.get(name) else { return Err(self.error(path, format!("no key {name}"))) };
        self.used_keys.insert(name.to_string());
        let mut fields = Map::new();
        if let Some(parent) = key.get("extends") {
            let Some(parent) = parent.as_str() else { return Err(self.error(path, format!("{name}: extends takes a key"))) };
            self.enter(path, parent)?;
            fields = self.key_fields(path, parent, keys)?;
            self.following.pop();
        }
        merge(&mut fields, key.clone());
        fields.remove("extends");
        Ok(fields)
    }

    fn key(&mut self, path: &str, name: &str, key: &KeyEntry, ids: &[&str]) -> Result<ThemeKey, String> {
        let error = |resolver: &Self, message: &str| resolver.error(path, format!("{name}: {message}"));
        if key.icon.is_some() && key.label.is_some() {
            return Err(error(self, "needs at most one of 'icon' or 'label'"));
        }
        if key.icon.as_deref().is_some_and(|icon| !THEME_ICONS.contains(&icon)) {
            return Err(error(self, "no such icon"));
        }
        let layer = key.action.as_deref().and_then(|action| action.strip_prefix("layer."));
        if layer.is_none() && (key.once || key.double.is_some()) {
            return Err(error(self, "'once' and 'double' are for layer changes"));
        }
        let action = match (&key.action, &key.insert, key.custom) {
            (Some(action), None, None) => match layer {
                Some(layer) => {
                    let mut code = format!("devkit_ui::keyboard::KeyAction::layer({})", self.layer_id(path, ids, layer)?);
                    if key.once {
                        code += ".once()";
                    }
                    if let Some(double) = &key.double {
                        code += &format!(".double({})", self.layer_id(path, ids, double)?);
                    }
                    code
                }
                None => {
                    let Token::Literal(input) = self.text(path, action)? else { return Err(error(self, "action is no value of TextInput")) };
                    format!("devkit_ui::keyboard::KeyAction::Press({input})")
                }
            },
            (None, Some(text), None) => format!("devkit_ui::keyboard::KeyAction::Insert({text:?})"),
            (None, None, Some(id)) => format!("devkit_ui::keyboard::KeyAction::Custom({id})"),
            _ => return Err(error(self, "needs exactly one of 'action', 'insert' or 'custom'")),
        };
        let mut options = String::new();
        if key.width != default_key_width() {
            options += &format!(".width({:?})", key.width);
        }
        if let Some(role) = &key.role {
            options += &format!(".role({})", self.text(path, role)?.code(&self.symbol));
        }
        if key.repeat {
            options += ".repeat()";
        }
        if let Some(font) = &key.font {
            let token @ Token::Font(_) = self.text(path, font)? else { return Err(error(self, "font is not a font")) };
            options += &format!(".font({})", token.code(&self.symbol));
        }
        if let Some(style) = &key.style {
            let token @ Token::Style(_) = self.text(path, style)? else { return Err(error(self, "style is not a style")) };
            options += &format!(".style({})", token.code(&self.symbol));
        }
        let label = key.label.as_ref().map_or("devkit_ui::keyboard::KeyLabel::None".to_string(), |text| format!("devkit_ui::keyboard::KeyLabel::Text({text:?})"));
        Ok(ThemeKey { icon: key.icon.clone(), label, action, options })
    }

    fn layer_id(&self, path: &str, ids: &[&str], name: &str) -> Result<usize, String> {
        ids.iter().position(|id| *id == name).ok_or_else(|| self.error(path, format!("no layer {name}")))
    }

    fn referred(&mut self, path: &str, text: &str) -> Result<Map<String, Value>, String> {
        self.enter(path, text)?;
        let Value::Object(object) = self.find(path, text)? else { return Err(self.error(path, format!("{text} is not a style"))) };
        let flat = self.flat(text, &object);
        self.following.pop();
        flat
    }

    fn props(&mut self, path: &str, style: &Map<String, Value>) -> Result<String, String> {
        let mut props = Vec::new();
        for (key, value) in style {
            if matches!(key.as_str(), "font" | "states") {
                continue;
            }
            if key == "animate" {
                props.push(self.animate(path, value)?);
                continue;
            }
            let values: Vec<&Value> = match value {
                Value::Array(list) => list.iter().collect(),
                one => vec![one],
            };
            let mut arguments = Vec::new();
            for value in values {
                match self.value(path, value)? {
                    Token::Style(_) => return Err(self.error(path, format!("{key}: a style is no value of a style"))),
                    token => arguments.push(token.code(&self.symbol)),
                }
            }
            props.push(format!("devkit_ui::Prop::{}({})", type_name(key), arguments.join(", ")));
        }
        Ok(format!("&[{}]", props.join(", ")))
    }

    /// `["color|position", 150]`, or `[]` for nothing, directly or through a
    /// reference.
    fn animate(&mut self, path: &str, value: &Value) -> Result<String, String> {
        let followed = match value {
            Value::String(text) => {
                self.enter(path, text)?;
                let target = self.find(path, text);
                self.following.pop();
                target?
            }
            other => other.clone(),
        };
        let arguments = match followed.as_array().map(Vec::as_slice) {
            Some([]) => "devkit_ui::Animated::empty(), 0".to_string(),
            Some([Value::String(names), ms]) => {
                let mut bits = Vec::new();
                for name in names.split('|') {
                    if !THEME_ANIMATED.contains(&name) {
                        return Err(self.error(path, format!("animate: no {name}")));
                    }
                    bits.push(format!("devkit_ui::Animated::{}.bits()", name.to_uppercase()));
                }
                let Token::Number(ms) = self.value(path, ms)? else { return Err(self.error(path, "animate: milliseconds are not a number")) };
                format!("devkit_ui::Animated::from_bits_retain({}), {ms}", bits.join(" | "))
            }
            _ => return Err(self.error(path, "animate takes [names, milliseconds] or []")),
        };
        Ok(format!("devkit_ui::Prop::Animate({arguments})"))
    }
}

fn state_code(state: &str) -> Option<String> {
    let mut bits = Vec::new();
    for name in state.split('|') {
        if !THEME_STATES.contains(&name) {
            return None;
        }
        bits.push(format!("devkit_ui::State::{}.bits()", name.to_uppercase()));
    }
    Some(format!("devkit_ui::State::from_bits_retain({})", bits.join(" | ")))
}

fn type_name(name: &str) -> String {
    name.split('_')
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or(String::new(), |first| first.to_ascii_uppercase().to_string() + chars.as_str())
        })
        .collect()
}

struct Context {
    definition: Definition,
    base: PathBuf,
    fonts_dir: PathBuf,
    icons: BTreeMap<String, u32>,
    themes: BTreeMap<String, Theme>,
    /// Definitions of bundled themes, which the build depends on.
    theme_files: Vec<PathBuf>,
}

impl Context {
    fn load(path: &Path, fonts_dir: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut definition: Definition = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let base = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let (themes, theme_files) = merge_themes(&mut definition, &base, fonts_dir)?;
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
        Ok(Self { definition, base, fonts_dir: fonts_dir.to_path_buf(), icons, themes, theme_files })
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
            // Theme icons are absolute already, and join leaves them so.
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

/// Puts each theme's fonts and icons into the definition, under names of its
/// own, leaving out what the app's theme replaces.
fn merge_themes(definition: &mut Definition, base: &Path, fonts_dir: &Path) -> Result<(BTreeMap<String, Theme>, Vec<PathBuf>), String> {
    let mut themes = BTreeMap::new();
    let mut files = Vec::new();
    for (name, entry) in std::mem::take(&mut definition.theme) {
        if !is_ident(&name) {
            return Err(format!("theme {name}: not an identifier"));
        }
        let lower = name.to_lowercase();
        let mut merged = ThemeEntry::default();
        let mut bundled = PathBuf::new();
        if let Some(reference) = &entry.base {
            let bundle = reference.strip_prefix('@').ok_or_else(|| format!("theme.{name}.base: must start with @"))?;
            bundled = fonts_dir.parent().unwrap_or(fonts_dir).join(THEME_DIR);
            let path = bundled.join(format!("{bundle}.json"));
            let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            merged = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            files.push(path);
        }
        for file in merged.icons.values_mut() {
            *file = bundled.join(&*file).to_string_lossy().into_owned();
        }
        for (icon, file) in entry.icons {
            merged.icons.insert(icon, base.join(file).to_string_lossy().into_owned());
        }
        merged.icon_size.extend(entry.icon_size);
        merged.fonts.extend(entry.fonts);
        for (mode, colors) in entry.colors {
            merged.colors.entry(mode).or_default().extend(colors);
        }
        merge(&mut merged.vars, entry.vars);
        merge(&mut merged.widgets, entry.widgets);
        merge(&mut merged.keyboards, entry.keyboards);
        merge(&mut merged.motion, entry.motion);
        if !entry.font.is_empty() {
            return Err(format!("theme.{name}.font: fonts go in the top level 'font'"));
        }

        let light = merged.colors.remove("light").ok_or_else(|| format!("theme.{name}.colors.light: missing"))?;
        let dark = merged.colors.remove("dark");
        if let Some(mode) = merged.colors.keys().next() {
            return Err(format!("theme.{name}.colors.{mode}: only light and dark exist"));
        }
        check_palette(&name, &light)?;
        if let Some(dark) = &dark {
            check_palette(&name, dark)?;
            check_same_colors(&format!("theme.{name}.colors.dark"), &light, dark)?;
        }

        let mut resolver = Resolver {
            theme: &name,
            symbol: name.to_uppercase(),
            root: json!({ "vars": merged.vars, "widgets": merged.widgets }),
            colors: &light,
            styles: BTreeMap::new(),
            used_vars: BTreeSet::new(),
            used_fonts: BTreeSet::new(),
            used_keys: BTreeSet::new(),
            following: Vec::new(),
        };
        let mut widgets = BTreeMap::new();
        for (widget, fields) in &merged.widgets {
            let Value::Object(fields) = fields else { return Err(format!("theme.{name}.widgets.{widget}: not an object")) };
            let mut tokens = BTreeMap::new();
            for (field, value) in fields {
                tokens.insert(field.clone(), resolver.value(&format!("widgets.{widget}.{field}"), value)?);
            }
            widgets.insert(widget.clone(), tokens);
        }
        let keys = match merged.keyboards.get(THEME_KEYS) {
            None => Map::new(),
            Some(Value::Object(keys)) => keys.clone(),
            Some(_) => return Err(format!("theme.{name}.keyboards.{THEME_KEYS}: not an object")),
        };
        for keyboard in THEME_KEYBOARDS {
            if !merged.keyboards.contains_key(keyboard) {
                return Err(format!("theme.{name}.keyboards.{keyboard}: missing"));
            }
        }
        let mut keyboards = BTreeMap::new();
        for (keyboard, value) in merged.keyboards.iter().filter(|(keyboard, _)| *keyboard != THEME_KEYS) {
            if !is_ident(keyboard) {
                return Err(format!("theme.{name}.keyboards.{keyboard}: not an identifier"));
            }
            keyboards.insert(keyboard.clone(), resolver.keyboard(&format!("keyboards.{keyboard}"), value, &keys)?);
        }
        if merged.motion.is_empty() {
            return Err(format!("theme.{name}.motion: missing"));
        }
        let mut motion = BTreeMap::new();
        for (field, value) in &merged.motion {
            motion.insert(field.clone(), resolver.value(&format!("motion.{field}"), value)?);
        }
        let Resolver { styles, used_vars, used_fonts, used_keys, .. } = resolver;
        let unused_vars = merged.vars.keys().filter(|var| !used_vars.contains(*var)).cloned().collect();
        let unused_keys = keys.keys().filter(|key| !used_keys.contains(*key)).cloned().collect();

        let mut fonts = BTreeMap::new();
        for slot in THEME_FONTS.iter().copied().chain(used_fonts.iter().map(String::as_str)) {
            let choice = merged.fonts.get(slot).ok_or_else(|| format!("theme.{name}.fonts.{slot}: missing"))?;
            let mut chain = Vec::new();
            for font in choice.names() {
                if let Some(bundled_font) = merged.font.get(&font) {
                    let own = format!("{lower}_{font}");
                    definition.font.insert(own.clone(), bundled_font.clone());
                    chain.push(own);
                } else if definition.font.contains_key(&font) {
                    chain.push(font);
                } else {
                    return Err(format!("theme.{name}.fonts.{slot}: no font {font}"));
                }
            }
            fonts.insert(slot.to_string(), chain);
        }

        let mut sets: BTreeMap<u16, IconMap> = BTreeMap::new();
        let mut icons = BTreeMap::new();
        for icon in THEME_ICONS {
            let file = merged.icons.get(icon).ok_or_else(|| format!("theme.{name}.icons.{icon}: missing"))?;
            let size = merged.icon_size.get(icon).copied().unwrap_or(DEFAULT_ICON_SIZE);
            let symbol = format!("{lower}_{icon}");
            sets.entry(size).or_default().insert(symbol.clone(), file.clone());
            icons.insert(icon.to_string(), (format!("{lower}_icons_{size}"), symbol));
        }
        for (size, set) in sets {
            files.extend(set.values().map(PathBuf::from));
            let font = FontEntry {
                size,
                bpp: default_bpp(),
                icon: Some(set),
                font: None,
                glyph: Vec::new(),
                glyph_file: None,
                variation: BTreeMap::new(),
                compress: default_compress(),
            };
            definition.font.insert(format!("{lower}_icons_{size}"), font);
        }

        themes.insert(name, Theme { fonts, icons, light, dark, widgets, styles, keyboards, motion, unused_vars, unused_keys });
    }
    if let Some((_, first)) = themes.first_key_value() {
        for (name, theme) in &themes {
            check_same_colors(&format!("theme.{name}.colors.light"), &first.light, &theme.light)?;
        }
    }
    Ok((themes, files))
}

fn custom_colors(colors: &BTreeMap<String, String>) -> impl Iterator<Item = &String> {
    colors.keys().filter(|color| !THEME_COLORS.contains(&color.as_str()))
}

fn check_palette(theme: &str, colors: &BTreeMap<String, String>) -> Result<(), String> {
    for color in THEME_COLORS {
        if !colors.contains_key(color) {
            return Err(format!("theme.{theme}.colors.{color}: missing"));
        }
    }
    for (color, value) in colors {
        if !is_ident(color) {
            return Err(format!("theme.{theme}.colors.{color}: not an identifier"));
        }
        parse_color(value).ok_or_else(|| format!("theme.{theme}.colors.{color}: {value} is not #rrggbb or #rrggbbaa"))?;
    }
    if custom_colors(colors).count() > usize::from(u8::MAX) + 1 {
        return Err(format!("theme.{theme}.colors: too many colors"));
    }
    Ok(())
}

fn check_same_colors(path: &str, expected: &BTreeMap<String, String>, colors: &BTreeMap<String, String>) -> Result<(), String> {
    match expected.keys().chain(colors.keys()).find(|color| !expected.contains_key(*color) || !colors.contains_key(*color)) {
        Some(color) => Err(format!("{path}.{color}: every palette needs the same colors")),
        None => Ok(()),
    }
}

fn parse_color(text: &str) -> Option<[u8; 4]> {
    let digits = text.strip_prefix('#')?;
    let byte = |at: usize| u8::from_str_radix(digits.get(at..at + 2)?, 16).ok();
    match digits.len() {
        6 => Some([byte(0)?, byte(2)?, byte(4)?, 255]),
        8 => Some([byte(0)?, byte(2)?, byte(4)?, byte(6)?]),
        _ => None,
    }
}

fn palette_code(colors: &BTreeMap<String, String>) -> String {
    let code = |color: &str| {
        let [r, g, b, a] = parse_color(&colors[color]).unwrap_or([0; 4]);
        format!("devkit_gfx::Color::rgba({r}, {g}, {b}, {a})")
    };
    let fields: Vec<String> = THEME_COLORS.iter().map(|color| format!("{color}: {}", code(color))).collect();
    let custom: Vec<String> = custom_colors(colors).map(|color| code(color)).collect();
    format!("devkit_ui::Palette {{ {}, custom: &[{}] }}", fields.join(", "), custom.join(", "))
}

fn icon_code(context: &Context, theme: &Theme, icon: &str) -> String {
    let (font, glyph) = &theme.icons[icon];
    let codepoint = context.icons[glyph];
    format!("devkit_ui::Icon {{ font: &{}_CHAIN, glyph: '\\u{{{codepoint:x}}}' }}", font.to_uppercase())
}

fn key_code(context: &Context, theme: &Theme, key: &ThemeKey) -> String {
    let label = key.icon.as_ref().map_or(key.label.clone(), |icon| format!("devkit_ui::keyboard::KeyLabel::Fixed({})", icon_code(context, theme, icon)));
    format!("devkit_ui::keyboard::Key::action({label}, {}){}", key.action, key.options)
}

fn keyboard_code(context: &Context, theme: &Theme, keyboard: &ThemeKeyboard) -> String {
    let layers: Vec<String> = keyboard
        .layers
        .iter()
        .map(|layer| {
            let rows: Vec<String> = layer
                .rows
                .iter()
                .map(|row| {
                    let keys: Vec<String> = row.iter().map(|key| key_code(context, theme, key)).collect();
                    format!("&[{}]", keys.join(", "))
                })
                .collect();
            format!("devkit_ui::keyboard::Layer {{ columns: {}, rows: &[{}] }}", layer.columns, rows.join(", "))
        })
        .collect();
    format!("devkit_ui::keyboard::Keyboard {{ height: {}, layers: &[{}] }}", keyboard.height, layers.join(", "))
}

fn theme_code(context: &Context, name: &str, theme: &Theme) -> String {
    let symbol = name.to_uppercase();
    let mut text = String::new();
    for (slot, fonts) in &theme.fonts {
        let fonts: Vec<String> = fonts.iter().map(|f| format!("&{}", f.to_uppercase())).collect();
        text += &format!(
            "static {}: devkit_gfx::FontChain = devkit_gfx::FontChain::new(&[{}]);\n",
            chain_symbol(&symbol, slot),
            fonts.join(", ")
        );
    }
    let mut sets: Vec<&String> = theme.icons.values().map(|(font, _)| font).collect();
    sets.sort();
    sets.dedup();
    for set in sets {
        text += &format!(
            "static {}_CHAIN: devkit_gfx::FontChain = devkit_gfx::FontChain::new(&[&{}]);\n",
            set.to_uppercase(),
            set.to_uppercase()
        );
    }
    for (path, style) in &theme.styles {
        text += &format!("static {}: devkit_ui::StateStyle = {style};\n", style_symbol(&symbol, path));
    }
    for (keyboard, value) in &theme.keyboards {
        text += &format!("pub static {}: devkit_ui::keyboard::Keyboard = {};\n", keyboard_symbol(&symbol, keyboard), keyboard_code(context, theme, value));
    }
    let widgets: Vec<String> = theme
        .widgets
        .iter()
        .map(|(widget, fields)| {
            let fields: Vec<String> = fields.iter().map(|(field, token)| format!("{field}: {}", token.code(&symbol))).collect();
            format!("{widget}: devkit_ui::{}Theme {{ {} }}", type_name(widget), fields.join(", "))
        })
        .collect();
    let icons: Vec<String> = THEME_ICONS
        .iter()
        .map(|icon| format!("{icon}: {}", icon_code(context, theme, icon)))
        .collect();
    let dark = theme.dark.as_ref().map_or("None".to_string(), |d| format!("Some({})", palette_code(d)));
    let motion: Vec<String> = theme.motion.iter().map(|(field, token)| format!("{field}: {}", token.code(&symbol))).collect();
    text += &format!(
        "pub static {symbol}: devkit_ui::Theme = devkit_ui::Theme {{\n    \
         light: {},\n    dark: {dark},\n    \
         fonts: devkit_ui::Fonts {{ title: &{}, body: &{}, caption: &{} }},\n    \
         icons: devkit_ui::Icons {{ {} }},\n    \
         widgets: devkit_ui::Widgets {{ {} }},\n    \
         keyboards: devkit_ui::Keyboards {{ text: &{}, number: &{} }},\n    \
         motion: devkit_ui::Motion {{ {} }},\n}};\n\n",
        palette_code(&theme.light),
        chain_symbol(&symbol, "title"),
        chain_symbol(&symbol, "body"),
        chain_symbol(&symbol, "caption"),
        icons.join(", "),
        widgets.join(", "),
        keyboard_symbol(&symbol, "text"),
        keyboard_symbol(&symbol, "number"),
        motion.join(", "),
    );
    text
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
    let themes: Vec<String> = context.theme_files.iter().map(|p| format!("\"{}\"", p.display())).collect();
    text += &format!("set(RESGEN_THEME_FILES {})\n", themes.join(" "));
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
    for (name, theme) in &context.themes {
        for var in &theme.unused_vars {
            eprintln!("warning: theme.{name}.vars.{var}: not used");
        }
        for key in &theme.unused_keys {
            eprintln!("warning: theme.{name}.keyboards.keys.{key}: not used");
        }
        text += &theme_code(context, name, theme);
    }
    if let Some(theme) = context.themes.values().next() {
        text += "pub mod colors {\n";
        for (index, color) in custom_colors(&theme.light).enumerate() {
            text += &format!("    pub const {}: devkit_ui::ColorRole = devkit_ui::ColorRole::Custom({index});\n", color.to_uppercase());
        }
        text += "}\n";
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
