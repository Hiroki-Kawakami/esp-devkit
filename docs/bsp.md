# bsp_rs

The board support package in Rust (`libs/bsp_rs`). The board is chosen with
`CONFIG_BSP_RS_BOARD_*`, independent of the C `bsp`'s `CONFIG_BSP_BOARD_*`.
`firmware/tab5_rs` builds it for the Tab5 and its simulator.

```rust
let board = bsp_rs::init(&bsp_rs::Config::default())?;
```

- `init` brings the board up once; later calls return `INVALID_STATE`.
- `Board` holds a handle per capability: `&'static T` for handles shared
  between tasks, owned values where the caller holds the state.
- Handle types are per-board aliases. A capability the board lacks is
  `Absent`, an empty enum, so code using it compiles away.

## Display

`board.display` is shared by every task (`&'static Display`); its methods take
`&self` and never lock.

- `frame_buffer(i)` returns a `FrameBuffer`: a raw, `Copy`able view of a
  host framebuffer, sendable to other tasks. Writing through `as_mut_slice` /
  `row_mut` is `unsafe`: tasks drawing to overlapping areas at the same time
  is the caller's to prevent. DMA engines take `as_ptr()`.
- `present(i)` puts framebuffer `i` on the panel at the next frame boundary
  and returns without waiting for it. `wait_frame()` blocks until that boundary
  and `scanning()` is the framebuffer being scanned, so a framebuffer other than
  it and the last presented one is free to draw into.
- `set_brightness` takes 0..=100; the backlight starts off.
- `PixelFormat::Rgb888` is B, G, R in memory.
- `readback(area)` lends what the panel shows, in its pixel format, as `Rows`
  from `next()`. It is `unsafe`: rows may be torn by concurrent drawing, and
  only one `Readback` may exist at a time.

## Touch

`board.touch` is shared the same way. `read` returns the latest sample without
waiting on the dispatch task, `set_event_cb` delivers each sample on that task,
and `wait_interrupt` blocks until the controller signals. `inject` delivers
points as if the controller had reported them, without waking `wait_interrupt`.
