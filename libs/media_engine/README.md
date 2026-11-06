# media_engine

The chip's media accelerators, driven from IDF's LL headers and its 2D-DMA
allocator rather than its drivers. Built on ESP32-P4; the simulator runs the
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
