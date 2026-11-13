# media_engine

The chip's media accelerators, driven from IDF's LL headers and its 2D-DMA
allocator rather than its drivers. Built for chips with a 2D engine (ESP32-P4, untried on ESP32-S31); the simulator runs the
same operations on the CPU, as `idf_compat`'s PPA does.

## 2D engine

```rust
let ppa = media_engine::ppa()?;
let mut op = Srm::new(picture);
op.rotation = Rotation::R90;
op.scale_x = 0.5;
op.scale_y = 0.5;
let mut slot = Slot::new();
ppa.srm(&op, &mut target, &mut slot)?.await?;   // or .wait()? on a task that is not async
ppa.fill(&Fill { rect, color }, &mut target, &mut slot)?.wait()?;
```

- `srm`, `blend` and `fill` write into a `devkit_gfx::Target` and return a
  `Job` that borrows the target, the source pictures and a `Slot`: the job's
  place in the queue, so queuing allocates nothing. Jobs in flight together
  need a slot each. Scale-rotate-mirror is
  one engine, blend and fill share the other; each runs its jobs in the order
  they came, and the two run side by side.
- Coordinates and the operation's turn and mirrors are in the target's logical
  picture; the target's own turn is added on top. Blend refuses a turned
  target.
- An `Srm` that leaves the target's area is cut off at it, to whole source
  pixels: a scaled picture may stop up to one scaled pixel short of the edge.
  One that misses the area entirely is a job that is done at once.
- Dropping a job blocks until the engine is done with its pixels, and stops it
  after a second without an end. A job must never be leaked while it runs
  (`mem::forget`, a leaked box, an `Rc` cycle): the engine would go on writing
  memory that is no longer the job's.
- What the engine cannot do with a format, a rectangle or a turn it refuses with
  `NOT_SUPPORTED` or `INVALID_ARG`, without working around it.
- Before a job goes, the caches over its source rows are written back and those
  over its output rows written back and dropped. A CPU writer whose pixels share
  a cache line with the output must not write them while the job runs.
- `wait` parks the calling task itself; `block_on` would cost an executor's
  setup, about 30 us, for every job.

## JPEG decoder

```rust
let mut dec = Decoder::new(&DecodeConfig { format: ImageFormat::Rgb565, yuv })?;
let data = [&file[..]];                          // or the pieces it arrived in
let header = Header::parse(&data)?;              // padded_width x padded_height is what it writes
dec.decode(&data, &mut target)?.await?;          // or .wait()?

let mut strips = dec.strips(&data, &mut [&mut a, &mut b])?;
while let Some(strip) = strips.next().await? {   // or next_blocking()?
    // strip.pixels(): padded_rows() rows of stride_bytes(), from row y()
}
```

- The file comes as a list of pieces, read in order without being joined:
  the 2D-DMA reads one descriptor per piece. Only a header that runs past
  the first piece is joined, into a copy of its own.
- Baseline JPEGs, gray or 4:4:4, 4:2:2 and 4:2:0 colour. A colour picture
  comes out as `Rgb888`, `Bgr888` or `Rgb565`, a gray one as `L8`; anything
  else is `NOT_SUPPORTED`. `yuv` picks studio or full range (what JFIF files
  use) and BT.601 or BT.709.
- The decoder writes the picture rounded up to whole MCUs, rows
  `padded_width` long. `decode` needs a target that is exactly that wide in
  stride, at least `padded_height` tall, unturned and in the configured format.
- `strips` decodes into one or two 64-byte aligned buffers the 2D-DMA reaches.
  A strip is as many 16-row bands as fit a buffer, but no more than an even
  share of the picture between the buffers. Holding a `Strip` keeps its buffer
  from the decoder; with two buffers the decoder fills the other one meanwhile,
  with one it waits.
- The timeout (`set_timeout`, 200 ms at first) counts time the decoder goes
  without writing, not time a strip is held.
- Dropping a `Decode` or a `Strips` stops the codec. Like a PPA job, neither
  may be leaked while it runs.
- It takes the codec's interrupt for good on its first frame, so IDF's JPEG
  driver must not be in use in the same firmware afterwards.

## JPEG into a target

```rust
let mut t = Transform::new();
t.rotation = Rotation::R90;
t.at = (x, y);
dec.render(&data, &mut [&mut a, &mut b], &t, &mut target)?.await?;   // or .wait()?
```

- `render` decodes through strips, as `strips` does, and lays each strip into
  the target with the 2D engine as soon as it is written. `Transform` is an
  `Srm` without its source: the picture's part, scale, turn, mirrors and place,
  cut off at the target's area the same way.
- The strips are in the configured decode format; the target's format is the
  output, converted by the engine.
- Every strip boundary inside the picture's part must land on a whole output
  row: with a scale of n/16, a boundary's distance from the part's top must be
  a multiple of 16/gcd(16, n). Otherwise `render` refuses with `INVALID_ARG`.
- The decoder's and the engine's interrupts hand strips to each other; the
  calling task only waits for the end. Strips go into the engine's queue one at
  a time, so other jobs can run between them.
- The caches over what lands in the target are written back and dropped once,
  before the decode starts.

## Interrupt path

The engine start (`picked` in `src/ppa/hw.rs`) runs inside `dma2d_enqueue`
for a job that finds the engine idle, and in the 2D-DMA interrupt for one that
waited behind another; the done callback always runs there. All of it, the LL
wrappers included, runs from flash, as IDF's own PPA driver does, and a 500
frame run of queued strips on Tab5 showed no fault.

If `Illegal instruction` faults at cache-line boundaries turn up around PPA jobs,
suspect this first: jpeg_decode_enhanced's code in the same interrupt path hit
them about once in 500 frames, and putting that code in IRAM made them go.
`picked`, `done`, `finish` and `start_running` are what would go there, with the
LL calls they make.

`render` plans each strip's job in the 2D-DMA interrupt, so nothing on that path
may use floating point: the chip faults with "Coprocessors must not be used in
ISRs". `Placement` holds scales as integer sixteenths for that reason, and
`srm_plan` takes one instead of an `Srm`'s `f32` scales.
