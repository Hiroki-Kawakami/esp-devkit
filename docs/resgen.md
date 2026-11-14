# resgen (fonts, icons, images, themes)

`devkit-resgen` (`tools/devkit_resgen`) turns a JSON definition plus source assets
(SVG, TTF/OTF, PNG) into resource binaries and the Rust code that refers to
them, for `libs/devkit_gfx` and `libs/devkit_ui`, at build time. Only the
sources are checked in.

## Usage

```cmake
devkit_rust_crate(ROOT "lib.rs" RESOURCES resources/resources.json)
```

```rust
mod resources {
    include!(concat!(env!("OUT_DIR"), "/resources.rs"));
}

static BODY: FontChain = FontChain::new(&[&resources::BODY_24, &resources::JP_24]);
let icon: char = resources::icons::FOLDER;
```

- Every entry becomes a `static` named in upper case, a `devkit_gfx::Font` or
  `devkit_gfx::Image`, embedded with `include_bytes!` at 64 byte alignment. A
  binary that does not parse fails the build. The crate needs `devkit_gfx`.
- `icons` has one `char` per icon name, shared by every icon font.
- `devkit-resgen` is the flake's `resgen` package, on `PATH` in the devShell and
  in `mkFwImage`.

## Build integration

- `devkit-resgen inputs` runs at configure time and lists each entry's input
  files and the bundled theme files; the JSON, those theme files and the tool
  are `CMAKE_CONFIGURE_DEPENDS`.
- Each font/image is its own custom command, so Ninja runs them in parallel and
  an SVG edit only regenerates the fonts that use it.
- Outputs are only written when their content changes.
- The commands carry the absolute path of the `devkit-resgen` found on `PATH`,
  so a change to the tool reruns every entry.

## Definition

Plain JSON (no comments). Paths are relative to the JSON file. `font` and
`image` keys must be identifiers, and a name cannot be both a font and an image.

```json
{
  "font": {
    "icon_40": {
      "size": 40,
      "icon": { "settings": "icons/settings.svg", "info": "icons/sdcard.svg" }
    },
    "body_24": { "size": 24, "font": "@montserrat" },
    "jp_24": {
      "size": 24,
      "font": "@noto-sans-jp",
      "variation": { "wght": 500 },
      "glyph_file": "@jp_glyphs"
    }
  },
  "image": {
    "picture": { "file": "picture.png", "format": "RGB565", "width": 320 }
  }
}
```

Font keys:

| key | |
|---|---|
| `size` | pixel size (required) |
| `bpp` | 1, 2 or 4; default 4 |
| `icon` | `{ name: file.svg }` — makes an icon font (exclusive with `font`) |
| `font` | TTF/OTF file, or `@name` for a font bundled in `tools/devkit_resgen/fonts` (`@montserrat`, `@noto-sans-jp`) — makes a text font (exclusive with `icon`) |
| `glyph` | array of strings; the union of their characters is included |
| `glyph_file` | a file listing the characters to include, `#` lines ignored, or `@name` for a list bundled in `tools/devkit_resgen/fonts` (`@jp_glyphs`); unioned with `glyph` |
| `variation` | variable-font axis values, e.g. `{"wght": 700}`; omitted axes use the font default |
| `compress` | `false` stores every glyph raw. Default `true` (RLE per glyph, raw when that is smaller) |

Fonts have no fallback of their own; chains are built at runtime with
`FontChain`.

Image keys: `file` (PNG) and `format` are required. `format` is one of
`RGB565`, `BGR888`, `ARGB8888`, `A8`, `L8`. `width`/`height`: neither keeps the
source size, one scales the other by aspect ratio, both stretch.

## Icon fonts

- Each icon name gets one code point from U+E000–U+EFFF. The same name in several
  fonts shares the code point, so `icons::INFO` works with every size; the file
  may differ per font (a hand-tuned small variant, say).
- The SVG is rendered with resvg at `size` px tall (width by viewBox aspect)
  and only the alpha channel is kept, so `currentColor` strokes/fills work and
  the text color applies. Tabler and Lucide SVGs render as-is.
- The line height is `size` and the baseline is the bottom of the icon, so an
  icon reached through a `FontChain` from a text font sits on the text baseline.

## Text fonts

- Rasterized with FreeType, light hinting (`FT_LOAD_TARGET_LIGHT`).
- The line height and baseline come from the face's ascender/descender, not
  from the included glyphs, so changing the `glyph` subset never moves layout.
- Characters in `glyph`/`glyph_file` that the font lacks are skipped with a
  build warning.
- No kerning. Glyphs are RLE compressed per glyph unless `compress` is `false`.
- The font also stores the maximum ascent/descent of the included glyphs, so a
  chain can grow the primary font's line box just enough that no glyph is
  clipped, instead of adopting a fallback face's own line metrics.

## Images

Formats without alpha drop the source alpha instead of compositing it.

## Themes

A `theme` entry generates a `devkit_ui::Theme`, under its name in
`resources.rs`.

```json
{
  "theme": {
    "TOUCH": {
      "base": "@touch",
      "fonts": { "body": ["body_24", "jp_24"] },
      "icons": { "back": "icons/my-back.svg" },
      "colors": { "light": { "primary": "#2196f3" }, "dark": { "primary": "#64b5f6" } },
      "vars": { "text_secondary": "color.muted" },
      "widgets": {
        "unit": { "radius": 16 },
        "list_item": {
          "root": { "height": 96, "padding_x": "widgets.unit.padding" },
          "meta": { "font": "font.caption", "foreground": "vars.text_secondary" }
        }
      },
      "motion": { "transition": "Transition.None" }
    }
  }
}
```

```rust
let config = AppConfig::new(&resources::TOUCH);
```

- `base` is a theme bundled in `tools/devkit_resgen/themes`, with its icons relative to
  that directory, so themes can share them: `tabler` holds the Tabler icons. What
  the entry sets replaces it, key by key; without a base every key is required.
- `fonts` sets `title`, `body` and `caption`, each a font of the definition or a
  list making a chain. Other names are for `font.x` values. Fonts of the base go
  in only if the theme still uses them.
- `icons` maps the names of `Icons` to SVG files, and `icon_size` their sizes in
  pixels, 40 by default. Icons of one size share a font, and a replaced file is
  left out. Any other name adds an icon for keys and `icon.x`; every theme has
  the same names.
- `colors` has `light` and optionally `dark`, each names as `#rrggbb` or
  `#rrggbbaa`. The names of `Palette` are required, and any other name adds a
  color, `resources::colors::MUTED` for `muted`. `light` and `dark` of every
  theme have the same names.
- `widgets` maps a widget to the fields of its type: `list_item` is
  `ListItemTheme`, `unit` is `UnitTheme`, the base values with no widget of their
  own. The compiler checks the names and types in the generated `resources.rs`.
  What the entry sets replaces the base key by key, inside styles too.
- `vars` are names of the theme's own, for any value, and never reach the
  firmware. One that nothing uses gives a warning.
- `motion` has the fields of `Motion`: `transition`, and a duration for each
  kind of transition. What it leaves out keeps `Motion::DEFAULT`.
- `keyboards` has the keyboards of the theme, described below.
- Values in `vars`, `widgets` and `motion` are:

  | value | is |
  |---|---|
  | `12`, `true` | a number, a bool |
  | `"#rrggbb"`, `"#rrggbbaa"` | a color for both modes (`ColorRole::Fixed`) |
  | `"color.x"` | a name of `colors` (a `ColorRole`) |
  | `"font.x"` | a name of `fonts` |
  | `"icon.x"` | a name of `icons` (an `Icon`), for a widget's own value but not in a style |
  | `"Align.Center"` | a value of a `devkit_ui` enum |
  | `"vars.x"`, `"widgets.y.z"` | that value |
  | an object | a `StateStyle` |

- In a style, each key is a `Prop` of that name, `padding_x` for `Prop::PaddingX`,
  and an array gives several values: `"border": ["color.divider", 2]`. `font` is
  for text. `states` maps a state to what changes in it, an object or a reference
  to one, which can hold no `font`. `pressed`, `hovered`, `focused`, `selected`,
  `disabled` and `checked` exist, and `selected|pressed` takes both. `extends` is a
  style or a list of them to start from.

  `corners` names the corners `radius` rounds, as `"top_left|top_right"`, with
  `top`, `bottom`, `left`, `right` and `all` for two or four. `border_sides` names
  the sides `border` runs along, as `"left"`, `"top|bottom"` or `"all"`. Both can
  be references.

  `animate` eases changes on the way to that style, as `["color|position", 150]`
  with `color`, `opacity` and `position` to choose from and the milliseconds.
  `[]` eases nothing. It can be a reference, so one value in `vars` turns a whole
  theme's motion off.

  ```json
  {
    "vars": { "dimmed": { "opacity": 102 } },
    "widgets": {
      "button": {
        "root": { "height": 72, "radius": 14, "states": { "disabled": "vars.dimmed" } },
        "primary": {
          "extends": "widgets.button.root",
          "background": "color.primary",
          "states": { "pressed": { "background": "color.primary_pressed" } }
        }
      }
    }
  }
  ```

- A theme can be generated for each entry, to switch between them.

## Keyboards

`keyboards` holds a theme's layouts. `text` and `number` are required and go into
`Theme::keyboards`. Any other name is a layout an app gives to `.keyboard`, as
`resources::TOUCH_PORT_KEYBOARD` for `port` in `TOUCH`.

```json
{
  "keyboards": {
    "keys": {
      "⌫": { "icon": "backspace", "action": "TextInput.Backspace", "width": 1.5, "role": "KeyRole.Special", "repeat": true },
      "⌫1": { "extends": "⌫", "width": 1 },
      "⇧": { "icon": "shift", "action": "layer.upper", "once": true, "double": "caps", "width": 1.5 },
      "OK": { "label": "OK", "custom": 1, "role": "KeyRole.Primary" }
    },
    "number": {
      "height": 360,
      "layers": [
        { "id": "main", "columns": 3, "layout": [["1", "2", "3"], [".", "0", "⌫1"]] }
      ]
    }
  }
}
```

- `height` is the whole panel's, gaps included. It stays through layer changes,
  and each layer's rows share it evenly.
- `max_height` caps `height` at that share of the screen's height, as `0.33`
  for a third, so a landscape screen gets a shorter panel. Default 1.
- `layers` starts on the first entry. `columns` is the width of a full row, and a
  row may be narrower: it stays on the left. The keys of a row are
  - a name of `keys`,
  - a number, for empty space that wide,
  - any other string, for a key typing it.
- `keys` are shared by the keyboards of the theme, and their names reach neither
  the firmware nor the code. One that nothing uses gives a warning.

  | field | is |
  |---|---|
  | `icon` | an icon of the theme |
  | `label` | text on the key; with no `icon` or `label` the key is blank |
  | `action` | a `TextInput` value to send, or `layer.x` to change to the layer with that `id` |
  | `once` | of `layer.x`: back to the previous layer after one character |
  | `double` | of `layer.x`: the `id` a double tap goes to |
  | `insert` | text to type |
  | `custom` | a number reaching the input as `TextInput::Custom` |
  | `width` | in columns, by quarters; default 1 |
  | `role` | a `KeyRole`; default `KeyRole.Normal` |
  | `repeat` | `true` repeats while held |
  | `font` | `font.x` for the label |
  | `style` | a reference to a style, which replaces the role's |
  | `extends` | a name of `keys` to start from |

  A key has one of `action`, `insert` and `custom`.
- What the entry sets replaces the base key by key, so `keys` change one at a
  time. `layers` is an array and is replaced whole.

## Binary format

All values are little endian; `libs/devkit_gfx/src/format.rs` is the definition,
shared with the tool.

A font is a 40 byte header (`DKFN`, version, `bpp`, flags, pixel size, line
metrics, then the glyph count and the offsets of the tables), a sorted `u32`
code point table, a 12 byte record per glyph and the glyph data. A record's
`bitmap` is the offset into the data, with bit 31 set when the glyph is RLE
compressed. Glyphs that do not get smaller are stored raw, as packed `bpp` bit
rows with no row padding.

The RLE is the scheme LVGL uses for its own compressed fonts (`decompress()` and
`rle_next()` in `lv_font_fmt_txt.c`): every row is XORed with the row above it,
a value costs `bpp` bits, a value equal to the previous one switches to repeat
mode where each further repeat is a single 1 bit, a 0 bit ends the run, and the
11th repeat is followed by a 6 bit count. The count runs out on a value — not on
a repeat — and the decoder does *not* re-enter repeat mode on that value;
encoders that get this wrong produce streams that are one pixel short per long
run. The data carries two zero bytes of slack at the end because the decoder
reads a 24 bit window.

An image is a 20 byte header (`DKIM`, version, format, size, stride in pixels,
data length), with the pixels at offset 64.
