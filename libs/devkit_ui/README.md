# devkit_ui

UI framework on `bsp_rs`. A UI is a tree of `Component`s; `firmware/ui_demo` is a
working example.

```rust
#[derive(Clone, Copy)]
enum Msg { Open(u16), ToggleMenu }

impl Component for FileBrowser {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, cx: &mut Cx<Self>) {
        match msg {
            Msg::Open(i) => self.selected = Some(i),
            Msg::ToggleMenu => self.menu = !self.menu,
        }
        cx.rebuild();
    }

    fn view<'a>(&'a self, ui: &'a Ui<Msg>) -> Element<'a, Msg> {
        ui.column([
            ui.row([
                ui.text(&TITLE, "Download").color(INK).grow(1),
                ui.icon(&ICONS, icons::MORE).id("more").on_tap(Msg::ToggleMenu),
            ]).height(80).align(Align::Center),
            ui.list(0, self.names.len(), 80).grow(1),
        ])
    }

    fn list_row<'a>(&'a self, _list: ListId, i: usize, ui: &'a Ui<Msg>) -> Element<'a, Msg> {
        ui.row([ui.text(&BODY, &self.names[i])]).on_tap(Msg::Open(i as u16))
    }
}

let app = App::new(display, board.touch, FileBrowser::new(), &AppConfig::default())?;
app.run();
```

- `update` runs when a message arrives and `view` when it called
  `cx.rebuild()`. `view` only builds descriptions in per-rebuild memory, which is
  matched against the kept nodes; only what differs is laid out and painted.
- Messages are `Copy`, at most 16 bytes, and handlers take values, so a rebuild
  allocates nothing for them. Larger payloads stay in the state.
- `Child` is a component made by a parent's `view`: `ui.child::<C>(props)`, or
  `ui.child_mapped` to turn its `cx.emit` events into the parent's messages. It
  keeps its state and rebuilds only when its props differ or it asks to.
- Widgets are functions returning an `Element`; the primitives are boxes, text,
  images, `scroll` and `list`.
- `list` builds only the rows in view, through `Component::list_row`. Scrolling
  moves the rows without running `view`. A rebuild of the owner rebuilds them,
  unless `revision` is given and did not change.
- `Handle::post` sends a message to the root component from any task.

## Layout and state

- Sizes are `i16` pixels. A row, column or grid places children by `grow`,
  `gap`, `align` and `justify`; `absolute` takes a child out of the flow.
- Layout works as in CSS flex. A parent offers each child what it has and the
  child says what it wants; overflow along the main axis comes out of `shrink`
  children, text by default, free space goes to `grow` children, and those are
  measured again for their final size, which is where wrapped text gets its
  height. `width_fraction` takes a share of the parent, `max_width` and
  `min_width` bound it.
- `State` says what a node is going through: `PRESSED`, `HOVERED` and `FOCUSED`
  come from input, without a rebuild; `.disabled()`, `.selected()` and
  `.checked()` from the view. A `WidgetLook` maps states to a `Look`, and its
  foreground colors text inside that sets none. A disabled node takes no input
  and shields what is below it.

## Touch

- Each finger acts on its own, so separate lists scroll and flings run at the
  same time. A node belongs to the first finger on it; another finger landing
  on it does nothing until it lifts and lands again.
- `.gestures(Gestures::PINCH | Gestures::PAN, Msg::Gesture)` recognizes gestures
  on a node and reports a `Gesture`; what they do to the view is the caller's.
  `PRESS` reports `Down` and `Up` whatever else is recognized. With `DOUBLE_TAP`,
  the node's own `on_tap` comes after the double tap window.
- Both fingers of a `PINCH` or `ROTATE` start on the node. Inside a list one
  finger scrolls; a second stops that and pinches. A `PAN` takes the finger from
  a scrolling parent when it lands. Once a pinch ends, the finger left behind
  does nothing until it lifts.

## Screens and themes

- `Screen` is a component a `Navigator` shows. `ui.navigator::<K, S>(props)`
  places one, marked by a type `K`, anywhere in a view; `App` adds `Root`, and
  `Overlay` above the chrome for modals. `cx.nav()` acts on the nearest one,
  `cx.nav_of::<K>()` on the nearest above, then inside, then anywhere. Moves
  slide inside the navigator's own area; full screens and panes work alike.
- A covered screen keeps its state and loses its nodes. List positions and
  nested navigators come back with it; state of its child components does not.
- `App::chrome` adds a component above the screens that stays across navigation,
  with the `Insets` it covers. Screens read them with `ui.insets()` and can avoid
  them or draw beneath. `ScreenStyle` tells the chrome what the top root screen
  wants. `Handle`s from `App::chrome` post to that component.
- `Theme` holds colors, metrics, fonts and motion, read with `ui.theme()`.
  `Screen::theme` replaces it for a screen, `cx.set_theme` for everything.
- `devkit_ui::widgets` are functions over the theme. State stays with the caller.
- `cx.animate(id, animation)` moves a node's offset or opacity, without running a
  view or layout.

## Text input

- Text comes through one path: the on-screen keyboard and `App::input` (a
  physical keyboard, a test harness) both reach the focused node as `TextInput`.
  A tap on another node ends the input unless it has `.keep_focus()`;
  `cx.focus("id")` moves the focus from `update`.
- `TextBuffer` stays with the caller, which applies what the input reports.
  `Enter` and `Custom` are left to it. The caret blinks without a view.

  ```rust
  text_field(ui, &self.name, Msg::Name).placeholder("Name").id("name")
  text_field(ui, &self.pin, Msg::Pin).input(InputKind::Number)
  // Msg::Name(input) => { self.name.apply(input); cx.rebuild(); }
  ```

- `AppConfig::keyboard` says when the docked keyboard shows: with
  `KeyboardMode::Auto`, while an input has focus and no physical keyboard is
  reported by `Input::set_keyboard_connected`. It raises `insets().bottom`.
  `widgets::keyboard` places one in a view instead.
- A `Keyboard` is static data. Label and action of a `Key` are separate, a look
  or font can replace the theme's, and widths go by quarters of a column: a key
  `n` wide covers `n` pitches less one gap, so a wide key spans the gaps it
  covers. `Theme::keyboards` holds the defaults, `.input` picks one of them and
  `.keyboard` overrides it.

  ```rust
  static PORT_KEYS: Keyboard = Keyboard {
      columns: 3,
      rows: 1,
      layers: &[Layer {
          rows: &[&[
              Key::text("."),
              Key::text(":"),
              Key::action(KeyLabel::Text("OK"), KeyAction::Custom(1)).role(KeyRole::Primary),
          ]],
      }],
  };
  ```

- `TextBuffer::set_composing` marks text an input method has not committed; the
  field underlines it. Input methods live outside this crate.

## Canvas

- `ui.canvas(id)` is a node painted by `Component::draw`, in its own coordinates
  and clipped to it. `cx.redraw(id)` and `cx.redraw_rect(id, rect)` repaint it
  without a view or layout; `canvas.needs(rect)` tells what this frame repaints,
  across framebuffers, so the rest need not be recorded.

  ```rust
  ui.canvas(TERMINAL).grow(1).id("term").on_size(Msg::Resized).on_input(Msg::Key)

  fn draw<'a>(&'a self, _: CanvasId, canvas: &mut Canvas<'a, '_>) {
      for (row, run) in self.rows.iter().enumerate() {
          let rect = Rect::new(0, row as i16 * LINE, canvas.size().0, LINE);
          if canvas.needs(rect) {
              canvas.fill(rect, BACKGROUND);
              canvas.text(0, rect.y, run, FOREGROUND);
          }
      }
  }
  ```

- `.on_size(Msg)` comes with the first layout and whenever the size changes,
  before the canvas is drawn at that size. `draw` takes `&self`, so whatever
  depends on the size is made there. `canvas.size()` is enough to only draw.
- `canvas.raster(rect, &self.plot)` writes pixels through a `Raster`. It runs on
  every drawing task at once, each with its own part of the picture, in the
  panel's format. Text is not available there.
- Keeping a picture across frames is the app's: draw into its own memory through
  a `Surface`, in the format of `ui.pixel_format()`, and show it with
  `canvas.image` and `Image::from_pixels`.

## External surfaces

- `ui.external(&surface)` leaves a rectangle to a task of its own, for video and
  other direct framebuffer writers. The UI never draws inside it. Its top and
  bottom keep to the 16 row grid, so it can come out shorter than the node. One
  can show at a time.

  ```rust
  let video = ExternalSurface::new()?;
  video.set_policy(Policy::Clip);
  video.set_on_refresh(move |area| decoder.redraw(area));

  // On the writing task. `begin` does not wait.
  if let Some(frame) = video.begin() {
      unsafe { blitter.start(&frame.fb, frame.rotation, frame.area, pixels) }?;
      // Wait for the copies, then:
      frame.end();
  }
  ```

  | `Policy` | Written to | UI over it | Presented by |
  |---|---|---|---|
  | `Clip`, nothing over it | the framebuffer on the panel | none | nobody |
  | `Clip`, something over it | the back framebuffer | yes | `end` |
  | `Stamp` | the back framebuffer | yes | `end` |
  | `Exclusive` | the back framebuffer, whole panel | none, the UI stops | `end` |

- A frame always covers the whole `area`, unless the writer tracks by
  `frame.index` what each framebuffer already holds.
- What lies over a surface is drawn into ARGB memory when it changes, and `end`
  blends it with the 2D engine, so video does not wait for the UI.
- `set_on_refresh` asks for `area` again on the UI task when what the surface
  wrote is gone: the rectangle moved or grew, or UI opened, changed or closed
  over it, or `Exclusive` began. Nothing waits for the answer. Playing is no
  reason to drop a request, as the next frame may be long in coming; a frame
  written first answers it. A writer that cannot draw its last frame again must
  not have UI over it.
- When the rectangle shrinks, the UI holds back until the frame being written
  has ended, for 100 ms at most.

## Memory

Memory goes where `Config` says: `data_caps` for nodes, text, display lists and
masks, `strip_caps` for the strips and `lane_stack_caps` for the lane stacks;
`AppConfig` has the glyph cache. The render mode can change while running, apart from
what the UI shows, for a UI that gives way to something faster:

```rust
static SRAM: StaticLease<SharedSram> = StaticLease::new(SharedSram([0; BYTES]));

// In update: strips move to PSRAM, and the internal RAM comes back.
let strips = Strips::allocate(128 * 1024, Caps::SPIRAM)?;
cx.set_render_mode(RenderMode { strips, lanes: 1 }, Msg::VideoReady);
// Msg::VideoReady:
decoder.start(cx.take_strips().and_then(Strips::into_inner::<Lease<SharedSram>>));
```

- `cx.set_render_mode` takes effect before the next frame; the message then
  brings the memory given up, with nothing drawing in it any more. From another
  task `Handle::set_strips` does the same and blocks; it cannot be called from
  the UI task. Before `run`, use `App::set_strips`.
- Memory is split into two buffers per lane, and each must hold 16 rows of the
  logical width.

## Presenter

`Presenter` puts a `DisplayList` on the panel; `App` runs it, and it can be used
alone:

```rust
let mut presenter = Presenter::new(display, &Config::default())?;
loop {
    presenter.acquire()?;
    let dirty = apply_input();
    presenter.invalidate(dirty);
    build(&mut list);
    presenter.render(&list, &mut cache)?;
}
```

- Damaged rows are drawn into internal RAM strips, by one task per core, and
  copied into a framebuffer with `Blitter`, rotated as `Config::rotation` says.
- `PresentMode::Swap` flips at a frame boundary without tearing. With three
  framebuffers a new frame starts drawing while the last one waits for the
  boundary; with two, it waits too. `PresentMode::InPlace` overwrites the
  framebuffer on the panel. It answers up to a frame sooner, and tears.
- Damage is tracked per framebuffer: a frame repaints what changed since that
  framebuffer was last drawn, and nothing is copied between framebuffers.
- Damage rows are aligned to a 16 row grid.
- Call `acquire` before reading input: it may wait for a framebuffer. `render`
  takes it by itself otherwise.
- A stalled copy logs `blit stalled` every second and keeps waiting.

## Test harnesses

- `Probe` serves test harnesses: `tree`, `tap` (through the touch path, refused
  when the node is covered), `invoke` (the handler directly), `idle` and `stats`.
