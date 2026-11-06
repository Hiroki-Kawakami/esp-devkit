# devkit_gfx

2D rendering, IDF-independent and host-testable. Memory comes from the
allocators passed in; output goes to a `Surface` over any memory.

```rust
static BODY: FontChain = FontChain::new(&[&resources::BODY_24, &resources::JP_24]);

let corner = CornerMask::new_in(12, Caps::INTERNAL);
let mut cache = GlyphCache::new_in(128 * 1024, Caps::SPIRAM);
let mut run = TextRun::new();
run.layout(&BODY, "Download", &TextOptions::default());

let mut list = DisplayList::new();
list.fill(screen, Color::hex(0xffffff));
list.round_rect(button, corner, Color::hex(0x2196f3));
list.text(24, 20, &run, Color::hex(0x1d2126));

let mut prepared = Prepared::new();
prepared.prepare(&list, damage.regions(), &mut cache);
let surface = unsafe { Surface::new(strip_ptr, stride, strip) };
draw::<Rgb565, _, _>(&list, &prepared, &surface);
```

- `prepare` resolves the glyphs of a frame and runs on one task. `draw` only
  reads, so several tasks can draw disjoint surfaces at once. It must stay
  inside the regions given to `prepare`.
- A mask stays valid until the next `prepare`. The glyphs of one frame must fit
  the cache; `Prepared::missing` counts those that did not, which are not drawn.
- `CornerMask` and `ShadowMask` are generated once and never freed; keep them
  with the theme. `Op::Shadow` leaves the middle of its rectangle unpainted, so
  something opaque must cover it.
- `CornerMask::only(Corners)` rounds some corners and leaves the others square,
  for every operation taking the mask. A `ShadowMask` takes the square corners
  from a mask of the same blur without radius. `border` runs along `Sides`; a
  rounded corner between an enabled and a disabled side ends the enabled one
  where the curve starts.
- Damage merges into at most `N` rectangles, and several rectangles covering
  75% promote to the full area.
- `Pixel` implements a pixel format: `Rgb565`, `Bgr888` (B, G, R in memory) and
  `Argb8888` (B, G, R, A, not premultiplied, for pictures laid over others).
- `line` and `polyline` stroke with round joins and ends; each pixel takes the
  coverage of its nearest segment. `polyline` borrows its points, relative to an
  origin, until the list is drawn.
- `ImageFormat` names byte formats in memory order (`Rgb888` is R, G, B;
  `Bgr888` is B, G, R) and word formats by the bits of a native-endian word
  (`Rgb565`, `Argb8888`). `L1`/`L2`/`L4` put the leftmost pixel in the high
  bits. Its value is the one in resource binaries; refer to it by name. `Image`
  and `draw` take `L8`, `A8`, `Rgb565`, `Bgr888` and `Argb8888`.
- A `Target` is where a writer other than `draw` puts pixels: an area of the
  logical picture, turned by a `Rotation` onto the stored one, with the stride
  in pixels. Whoever holds it is the only writer there, and a writer refuses
  what it cannot do instead of working around it.
- A `Raster` writes pixels itself. It runs on every task that draws, each with
  its own part of the picture.
- `Surface::half` draws at half size: each pixel is the logical pixel at odd
  offsets from the area's top left, nothing averaged. A `Raster` there still
  runs at full size, one row piece at a time.
- `draw_from` draws from an operation on, in the clips open there. `overlap`
  finds what those operations paint inside a rectangle.
- `simd` runs pixel loops on the chip's vector unit, built in for the chips
  that have one (`src/simd/<chip>.S` and `.rs`); scalar loops give the same
  pixels elsewhere. On ESP32-P4 translucent `Rgb565` and `Bgr888` fills use it
  too, so the pixels and the stack of a task drawing them stay out of RTC RAM:
  allocate them with `Caps::SIMD` alone or from PSRAM, never `Caps::INTERNAL`
  alone.
