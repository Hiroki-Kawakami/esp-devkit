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
