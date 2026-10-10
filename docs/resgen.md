# resgen (fonts, icons, images)

`tools/resgen/resgen.py` turns a JSON definition plus source assets (SVG, TTF,
PNG/JPEG) into LVGL v9 C sources at build time. Only the sources are checked in;
the generated `.c`/`.h` live under the build directory.

## Usage

`devkit.cmake` includes `tools/resgen/resgen.cmake`, so any component can call
`resgen_add_resources()`:

```cmake
idf_component_register(SRCS ${SRCS} INCLUDE_DIRS "." REQUIRES ui_framework)

if(NOT CMAKE_SCRIPT_MODE_FILE)
    resgen_add_resources(TARGET ${COMPONENT_LIB} DEFINITION resources/resources.json)
endif()
```

The guard is required: ESP-IDF's requirement scan includes component
CMakeLists in script mode, where `devkit.cmake` has not been included. The
component needs `ui_framework` if the definition has a `pack` font or uses
[partition mode](#partition-mode).

Code then includes `resources.h`:

```cpp
#include "resources.h"
lv_obj_set_style_text_font(label, &icon_36, 0);
lv_label_set_text(label, ICON_SETTINGS);
lv_image_set_src(img, &picture);
```

Python runs from `$RESGEN_PYTHON` (freetype-py, pillow, resvg-py, fonttools),
set by the esp-devkit devShell and `mkFwImage`. It is a separate interpreter
from ESP-IDF's `IDF_PYTHON_ENV_PATH` so the two environments never collide on
`python3`.

The generator can also be run by hand, e.g. to inspect output:

```sh
nix develop -c sh -c '$RESGEN_PYTHON <esp-devkit>/tools/resgen/resgen.py all <definition> /tmp/out'
```

`all --partition <label>` writes the partition mode output instead.

## Partition mode

By default every bitmap is a `const` array in the generated `.c` files, so it
lands in the app image and moves whenever code or other constants change,
which defeats differential flashing. Passing a partition label moves the bulk
data out of the app:

```cmake
resgen_add_resources(TARGET ${COMPONENT_LIB} DEFINITION resources/resources.json PARTITION resources)
```

```csv
resources, data, undefined, , 4M,
```

- The partition is looked up by label; the subtype is not checked. A missing
  partition fails the configure, a `resources.bin` larger than it fails the
  build.
- `resources.bin` is registered with `esp_partition_register_target(...
  FLASH_IN_PROJECT)`, so `idf.py flash`, `merge-bin` and `<label>-flash` write
  it; `idf.py app-flash` does not.
- The blob holds glyph bitmaps, image pixels and a pack's `glyphs`,
  `codepoints` and `data`, each 64 byte aligned. An `lv_font_t` keeps
  `glyph_dsc` in the app: its bitfield layout depends on
  `CONFIG_LV_FONT_FMT_TXT_LARGE` and the compiler. `cmaps` stay too, since
  they hold pointers and the mapped blob is read-only.
- The structs that point into the blob (`lv_font_fmt_txt_dsc_t`,
  `lv_image_dsc_t`, `resgen_font_pack_t`) are non-`const` RAM objects, so
  images and packs are declared without `const` in `resources.h`. Symbols and
  usage are otherwise the same as in the default mode.
- `ui_framework`'s `lvgl_port_init()` calls `resgen_resources_init()`, a weak
  no-op that the generated `resources.c` overrides: it maps the partition (on
  the simulator, reads the build's `resources.bin`) and points the structs at
  it. No resource may be read before `lvgl_port_init()`. The override sits in
  the object every entry references, so it is linked whenever any resource is.
- The blob starts with a magic, a version, the payload size and its SHA-256,
  and `resources.c` embeds the same header. Firmware that finds anything else
  in the partition logs `resgen: partition '<label>' does not match this
  firmware` and aborts.
- The mapping is permanent and costs flash MMU pages for the whole blob; on a
  SoC with a small MMU window, stay with the default mode.
- One component per firmware can use partition mode (`resources.c` defines
  global symbols).

## Definition

Plain JSON (no comments). Paths are relative to the JSON file. Every `font`
and `image` key becomes a global C symbol, so keys must be C identifiers and a
name cannot be both a font and an image.

```json
{
  "font": {
    "icon_36": {
      "size": 36,
      "icon": { "ICON_SETTINGS": "settings.svg", "ICON_INFO": "sdcard.svg" }
    },
    "icon_24": {
      "size": 24,
      "bpp": 2,
      "icon": { "ICON_INFO": "sdcard-24.svg" }
    },
    "noto_sans_24": {
      "size": 24,
      "font": "NotoSansJP.ttf",
      "variation": { "wght": 500 },
      "glyph": ["abcdefghijklmnopqrstuvwxyz", "ABCDEFGHIJKLMNOPQRSTUVWXYZ"],
      "fallback": "icon_24"
    }
  },
  "image": {
    "picture": { "file": "picture.jpg", "format": "RGB565", "width": 320 }
  }
}
```

Font keys:

| key | |
|---|---|
| `size` | pixel size (required) |
| `bpp` | 1, 2, 4 or 8; default 4 |
| `icon` | `{ NAME: file.svg }` — makes an icon font (exclusive with `font`) |
| `font` | TTF/OTF file — makes a text font (exclusive with `icon`) |
| `glyph` | array of strings; the union of their characters is included. Omitted: every mapped glyph in the font |
| `glyph_file` | a file listing the characters to include, `#` lines ignored; unioned with `glyph` |
| `variation` | variable-font axis values, e.g. `{"wght": 700}`; omitted axes use the font default |
| `fallback` | a generated font name or any `lv_font_t` symbol (e.g. `lv_font_montserrat_24`) |
| `pack` | `true` emits a `resgen_font_pack_t` instead of an `lv_font_t` (exclusive with `fallback`); see [Font packs](#font-packs) |
| `compress` | pack only; `false` stores every glyph raw. Default `true` (RLE per glyph, raw when that is smaller) |

Image keys: `file` and `format` are required. `format` is one of `RGB565`,
`RGB565A8`, `RGB888`, `XRGB8888`, `ARGB8888`, `L8`, `A8` (`A8` needs a source
with an alpha channel). `width`/`height`: neither keeps the source size, one
scales the other by aspect ratio, both stretch.

## Icon fonts

- Each icon name gets one code point, assigned in order of first appearance
  across all icon fonts starting at U+E000. The same name in several fonts
  shares the code point, so `ICON_INFO` works with every size; the file may
  differ per font (a hand-tuned small variant, say). Names are `#define`d once
  in `resources.h` as UTF-8 string literals.
- U+E000–U+EFFF is used because LVGL's built-in `LV_SYMBOL_*` sit at U+F000 and
  up, so a Montserrat fallback never shadows an icon.
- The SVG is rendered with resvg at `size` px tall (width by viewBox aspect)
  and only the alpha channel is kept, so `currentColor` strokes/fills work and
  the label's text color applies. Tabler and Lucide SVGs render as-is.
- `line_height` is `size` and the baseline is the bottom of the icon, so an
  icon reached through `fallback` from a text font sits on the text baseline
  rather than being centered on the line.

## Text fonts

- Rasterized with FreeType, light hinting (`FT_LOAD_TARGET_LIGHT`).
- `line_height`/`base_line` come from the face's ascender/descender, not from
  the included glyphs, so changing the `glyph` subset never moves layout.
  lv_font_conv does the opposite, so its fonts may be a few px tighter.
- Characters in `glyph`/`glyph_file` that the font lacks are skipped with a
  build warning.
- No kerning. An `lv_font_t` is stored uncompressed; a `pack` is RLE compressed
  per glyph.
- With `CONFIG_LV_FONT_FMT_TXT_LARGE` off, a font is limited to 1 MB of bitmap
  and 255 px glyph boxes; a generated font that exceeds this carries an
  `#error` asking for the option.

## Font packs

A pack is for large glyph sets (CJK) that are only reached through a fallback
chain. Neither LVGL's `lv_font_fmt_txt` nor its compressed variant
(`LV_USE_FONT_COMPRESSED`, which decompresses and `lv_malloc`s line buffers on
*every* draw) keeps a decoded glyph around, so the pack has its own format and
is drawn by `PackedFont` (`libs/ui_framework/inc/packed_font.hpp`). It caches
decoded glyphs as A8 masks in PSRAM and hands them to LVGL through the
static-bitmap path (no per-draw allocation, no copy into a draw buffer):

```cpp
#include "packed_font.hpp"
#include "resources.h"

PackedFont japanese{noto_sans_jp_24};
lv_font_t body = lv_font_montserrat_24;  // copy: the built-in fonts are const
body.fallback = japanese.font();
```

`resgen_font_pack_t` (`libs/ui_framework/inc/resgen_font_pack.h`, included by the
generated `resources.h`) is a sorted `uint16_t` codepoint table, a
`resgen_glyph_t` per glyph, and one bitmap blob. Codepoints are limited to the
BMP.

`resgen_glyph_t::bitmap` is the byte offset into the blob, with bit 31 set when
the glyph is RLE compressed. Glyphs that do not get smaller are stored raw
(packed `bpp` bit rows, no row padding), so the decoder handles both regardless
of what `compress` says.

The RLE is the scheme LVGL uses for its own compressed fonts (see `decompress()`
and `rle_next()` in `lv_font_fmt_txt.c`): every row is XORed with the row above
it, a value costs `bpp` bits, a value equal to the previous one switches to
repeat mode where each further repeat is a single 1 bit, a 0 bit ends the run,
and the 11th repeat is followed by a 6 bit count. The count runs out on a
value — not on a repeat — and the decoder does *not* re-enter repeat mode on
that value; encoders that get this wrong produce streams that are one pixel
short per long run. `resgen.py check <definition>` decodes every glyph back and
compares, and is the way to verify a change to either side of the codec.

The blob carries two zero bytes of slack at the end because the decoder reads a
24 bit window.

The pack also stores `max_ascent`/`max_descent` of the included glyphs, so a
chain can grow the primary font's line box just enough that no glyph of the
pack is clipped, instead of adopting the pack face's own (often much taller)
line metrics.

Decomposed kana (NFD: か followed by the combining U+3099) are composed by
`PackedFont`. LVGL has no notion of combining marks and would draw U+3099/U+309A
as glyphs of their own, so `PackedFont` looks at the `next` codepoint LVGL
passes for kerning and returns the precomposed glyph for the base, and an empty
zero-width glyph for the mark. That `next` is only passed when the top font of
the chain has kerning enabled, which the built-in Montserrat does. A mark after
a base with no precomposed form is dropped.

## Images

Pixels are stored in LVGL's native little-endian layout with a packed stride
(`LV_DRAW_BUF_STRIDE_ALIGN` is 1). Formats without alpha drop the source alpha
instead of compositing it.

## Build integration

- `resgen.py cmake` runs at configure time and lists each entry's input files;
  the JSON and the script are `CMAKE_CONFIGURE_DEPENDS`. Finding the JSON
  through a `file(GLOB ... CONFIGURE_DEPENDS)` lets adding or removing it
  reconfigure without a manual `cmake`.
- Each font/image is its own custom command, so Ninja runs them in parallel and
  an SVG edit only regenerates the fonts that use it.
- Outputs are only written when their content changes. CMake's Ninja custom
  commands are `restat`, so a JSON edit reruns every entry but only recompiles
  the `.c` files whose content actually changed.
- The command line carries the absolute `$RESGEN_PYTHON` store path, so a
  nixpkgs update reruns every entry at the next reconfigure.
- In partition mode each entry also writes `<name>.bin`, and `resgen.py blob`
  joins them into `resources.bin` and `resources.c`.
- The simulator's `idf_component_register` shim adds sources to the
  `simulator` target, which is defined in another directory. Custom command
  outputs are not attached to a target in a different directory, hence the
  `resgen_<target>` custom target plus `GENERATED` on the target's directory in
  `resgen.cmake`.
