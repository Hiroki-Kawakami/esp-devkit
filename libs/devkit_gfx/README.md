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
- Damage merges into at most `N` rectangles, and several rectangles covering
  75% promote to the full area.
- `Pixel` implements a pixel format: `Rgb565`, `Bgr888` (B, G, R in memory) and
  `Argb8888` (B, G, R, A, not premultiplied, for pictures laid over others).
- A `Raster` writes pixels itself. It runs on every task that draws, each with
  its own part of the picture.
- `draw_from` draws from an operation on, in the clips open there. `overlap`
  finds what those operations paint inside a rectangle.
