# devkit_ui

UI framework on `bsp_rs`. A UI is a tree of `Component`s; `firmware/ui_demo` is a
working example.

```rust
#[derive(Clone, Copy)]
enum Msg { Open(u16), OpenMenu, Sort }

impl Component for FileBrowser {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, cx: &mut Cx<Self>) {
        match msg {
            Msg::Open(i) => self.selected = Some(i),
            Msg::OpenMenu => {
                if let Some(anchor) = cx.tap_rect() {
                    cx.popup(Menu::new(anchor, &[("Sort", Msg::Sort)], cx.handle()));
                }
            }
            Msg::Sort => self.names.sort(),
        }
        cx.rebuild();
    }

    fn view<'a>(&'a self, ui: &'a Ui<Msg>) -> Element<'a, Msg> {
        ui.column([
            ui.row([
                ui.text(&TITLE, "Download").foreground(INK).grow(1),
                ui.icon(&ICONS, icons::MORE).id(id!("more")).on_tap(Msg::OpenMenu),
            ]).height(80).align(Align::Center),
            ui.list(self.names.len(), 80, Self::row).grow(1),
        ])
    }
}

impl FileBrowser {
    fn row<'a>(&'a self, i: usize, ui: &'a Ui<Msg>) -> Element<'a, Msg> {
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
- `on_scroll` reports a scroller's offset as it moves, below 0 or above the end
  while it is stretched past one. `scroll_border(id)` shows a divider under a
  node while the scroller with that id is past its start, without a message
  or a rebuild.
- A scroller dragged past an end stretches and springs back, and a fling into
  an end overshoots; `.bounce(false)` holds the ends, and content that fits
  never bounces. `.pull(height, refreshing, msg)` sends `msg` when let go
  `height` past the start and stays that far open until a view gives
  `refreshing` false; the indicator is the app's, drawn from `on_scroll`.
- `cx.scroll_to(id, target, animated)` moves a scroller after the view and
  layout, so `ScrollTarget::End` is the end of what the view just added.
  `Index` shows a row of a `list` whole, moving as little as it takes. A finger
  on the scroller wins over it.
- `.scroll_anchor(ScrollEdge::End)` starts a scroller at its end and keeps it
  there as its content or size changes, while it is there; moved away, it stays
  where it is until it comes back to the end.
- `list` builds only the rows in view, through the row function given to it: a
  method of the component whose view holds the list, as a function pointer, so
  it can be called after the view is gone. Scrolling moves the rows without
  running `view`. A rebuild of the owner rebuilds them, unless `revision` is
  given and did not change. A list keeps its scroll position while its screen
  is covered only if it has an id. Rows exist one past each end of what shows;
  `.overscan(rows)` makes it more.
- `.id` names a node. Among its siblings it matches the node of the last
  rebuild with the same id, and a different id makes a new node. `id!("name")`
  hashes a literal at compile time and keeps the name for `ui tree`;
  `Id::named` does the same for a `&'static str` from a table, `Id::of` for a
  string made at run time, and a number is an id as it is. `cx.focus`,
  `cx.animate`, `cx.redraw` and `ui tap` find nodes by it.
- `app.handle()` and `cx.handle()` give a `Handle`, whose `post` reaches that
  component from any task. Its `update` runs even while a screen covers it, and
  the next `view` shows the result; a message for a closed screen is dropped.
- Results from other tasks come back as values the component owns, not as
  messages, which stay small. `cx.inbox(capacity, msg)` makes an `Inbox`:
  `inbox.sender()` sends from any task, `msg` comes once for everything that
  arrived until `inbox.drain()`, and nothing more comes until then. A full or
  dropped inbox gives the value back. `cx.pending(msg)` pairs a `Pending` with
  the `Reply` for one value. Dropping the `Pending` cancels the reply:
  `reply.is_cancelled()` lets the worker stop early, and `send` gives the value
  back. Keeping it in a field ties it to the component, replacing it drops an
  older request.
- `.task(&inbox, |reply| ...)` starts work for a node: the closure runs once
  when the node is made, and the `Reply` sends into the inbox. Its node going
  cancels it: a row leaving the list's rows, the screen leaving, or the view
  dropping the task. The closure is never dropped, so it holds only what needs
  no drop.

  ```rust
  ui.column([]).id(entry.id).task(&self.thumbs, |reply| THUMBS.push(entry.path(), reply))
  ```

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
  `.checked()` from the view. A node holding a drag is pressed too. A disabled
  node takes no input and shields what is below it.
- `.foreground()` colors text and icons on the node and inside, where they set
  none. `.background()` and the other colors take a `Color` or a `ColorRole`, which
  follows the palette.
- Text breaks only at `\n` unless `.wrap()`; `.max_lines(n)` lets it grow up to
  `n` lines. `.ellipsis(Elide::End)`, `Start` or `Middle` shortens what goes past
  that, keeping the beginning, the end or both.
- `.radius()` rounds the corners `.corners()` names, all by default.
  `.border(color, width)` runs along the sides `.border_sides()` names, all by
  default.
- Every value of a `Style` can change with a state, with no view running: what
  needs layout gets it, the rest only repaints. A `StateStyle` is a style to share:
  `base` values and what each state changes. `.style(&S)` puts the base on the
  node, so what is set after it wins; a second style adds its base and replaces
  the states. `.when()` changes values for one node, from values known at run
  time, and costs memory there. `Prop` is one value.

  ```rust
  static DANGER: StateStyle = StateStyle {
      font: None,
      base: &[Prop::Background(colors::DESTRUCTIVE), Prop::Foreground(colors::ON_DESTRUCTIVE)],
      states: &[(State::PRESSED, &[Prop::Opacity(200)])],
  };

  ui.row(children).style(&DANGER).radius(8).when(State::SELECTED, &[Prop::Border(self.color.into(), 4)])
  ```

- A heavier state wins, whatever the order: `CHECKED`, `SELECTED`, `FOCUSED`,
  `HOVERED`, `PRESSED`, `DISABLED`, rising. Several states together are heavier than
  the heaviest alone. So it goes the shared base, the node's own values, then by
  weight the shared states and `.when` of each. A value of a shared state thus wins
  over `.background()` while that state holds.
- `.state_source()` lets nodes inside take states with `.inherit_state(states)`,
  from the nearest source in the component. This is how a widget passes its
  state to its pieces; states go no other way.

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
  play inside the navigator's own area; full screens and panes work alike.
- A move plays `cx.nav().transition(t)` if asked, else the navigator's
  `.transition(t)`, else the theme's `motion.transition`. A `pop` without one
  plays what the screen came with backwards. `replace`, and `show` beside a
  split main screen, change at once unless asked; `Overlay` always does.

  ```rust
  ui.navigator::<Pane, Storages>(()).transition(Transition::None)
  cx.nav().transition(Transition::Zoom).push(Detail::new())
  ```

- `Transition::Zoom` draws RGB565 pictures of both screens taken when it
  starts, the old one at half size, so neither changes while it plays. Without
  memory for them the screens change at once, with a warning.
- `.split(on, width)` shows a navigator in two panes: the bottom of the stack on
  the left and the rest on the right. One stack serves both layouts, so a turn
  loses nothing. The main screen opens things with `cx.nav().show(screen)`,
  which replaces all above it.

  ```rust
  ui.navigator::<Pane, Storages>(()).split(width > height, SIDEBAR_WIDTH).grow(1)
  ```

- `.split_detail(|| Browser::new())` fills the right while nothing is open;
  without it the right stays empty. The function takes no state. That screen is
  outside the stack: one pane does not show it, and it is made anew when needed.
  Pushing from it keeps it in the stack to come back to, which it cannot leave
  while split.
- `ui.can_pop()` says whether a back button means anything here: not on the first
  screen on the right, which `cx.nav().root()` closes. `ui.split_role()` says
  whether this screen is the main one, with something open on the right, or a
  detail. `show` changes the right at once unless asked.
- A covered screen keeps its state and loses its nodes. List positions and
  nested navigators come back with it; state of its child components does not.
  It is dropped once it leaves its stack.
- `on_load` runs once, before anything else, with the first `cx` a screen
  gets. `will_appear` runs before the view it is about to show is built,
  `did_appear` once its transition ends, `will_disappear` as it starts to go
  and `did_disappear` once its nodes are gone; on the `Overlay` the `.appear`
  and `.disappear` of what it holds count as the transition. They come in that
  order and in pairs, a move cut short included, and reach screens in nested
  navigators with the screen around them. A move asked from a hook waits until
  the one under way is done.
- `cx.while_shown(guard)` keeps `guard` from now until the screen starts to
  disappear, for what only runs while it shows: a listener, a timer.
- `App::chrome` adds a component along the top, above the screens, that stays
  across navigation. How tall it comes out is the top inset of the screens, 0
  while it shows nothing; planes build from the top down, so screens see the
  height of this update. Screens read it with `ui.insets()` and can avoid it or
  draw beneath. `ui.insets()` is what the planes above cover: the chrome and the
  keyboard under the chrome, only the keyboard on the chrome and the `Overlay`.
  `ScreenStyle` tells the chrome what the top root screen
  wants. `Handle`s from `App::chrome` post to that component.
- `Overlay` holds one screen over a scrim, `widgets.overlay.scrim`. The scrim
  fades with the first node in the screen that has a presence, and keeps
  touches from what is below; `Screen::scrim_tap` says what a tap on it sends.
  A push replaces what shows at once, and a pop empties the overlay once the
  screen's `.disappear` ends. A scrim with alpha 0 draws and damages nothing,
  for a panel that should not repaint.
- `cx.popup(component)` shows a component above everything, the keyboard
  included, and works from any screen, a modal too. What is under it stays;
  `cx.close_popup()` closes it, and so do a tap outside, which goes no
  further, and a turn of the screen. It fills the
  screen and places itself, by `cx.tap_rect()`: where the node tapped for the
  message is, on any node. `Menu` and `DropdownMenu` are two.
- A `Theme` is what resgen generates from a definition: colors, fonts, icons,
  each widget's values, keyboards and motion, so a theme replaces any of them
  without the rest of it reaching the firmware.

  ```rust
  let config = AppConfig::new(&resources::TOUCH);

  static STILL_TOUCH: Theme = Theme { motion: Motion { transition: Transition::None, ..Motion::DEFAULT }, ..resources::TOUCH };
  ```

- `theme.widgets` has one struct per widget: a `StateStyle` for each of its pieces,
  and numbers where code calculates with them. `unit` has the base values of a
  theme, `padding`, `gap` and `radius`, for views of their own. Touch sizes go by
  `widgets.button`, and `widgets.list.row_height` is a default for `ui.list`.
  `Input::set_color_mode` swaps the palette and repaints; a theme without
  dark colors stays light. `ui.colors()` gives the palette in use and
  `ui.color_mode()` the mode. `SavedColorMode` keeps it across restarts.
- `Palette` names the colors every theme has. What a theme adds is a
  `ColorRole::Custom` in `resources::colors`, and `ui.color()` gives its value.
- `cx.set_theme` changes the theme, `Screen::theme` replaces it for a screen.

- `devkit_ui::widgets` are functions over the theme. State stays with the caller.
- `.part` changes pieces of one widget where it is used, on any element holding
  them, and does nothing when the widget has none:

  ```rust
  slider(ui, value, on_drag).part(Part::Thumb, |thumb| thumb.background(RED))
  ```

  | widget | parts |
  |---|---|
  | `slider` | `Track`, `Fill`, `Thumb` |
  | `progress_bar` | `Track`, `Fill` |
  | `switch` | `Track`, `Thumb` |
  | `checkbox` | `Mark` |
  | `list_item` | `Leading`, `Title`, `Meta`, `Trailing` |
  | `top_bar` | `Back`, `Title` |
  | `segmented` | `Item`, `Label` |
  | `menu` | `Panel` |
  | `modal` | `Panel`, `Title`, `Message`, `Actions` |
  | `divider`, `vertical_divider` | `Line` |

  It reaches every match below the element, and not into child components.

- `divider` is a line across the width it is given and `vertical_divider` one down
  the height, filling it even where the parent centers its children.
  `.padding_x` on a `divider` keeps its ends clear.
- `table` lays out a scrolling form: `table_section` is a rounded panel of rows and
  `table_header` the title above one. `table_row` holds any children, and
  `table_label`, `table_value` and `table_chevron` are the usual ones. Unlike
  `list`, every row exists.
- `modal` is a dialog for a screen on the `Overlay`, centered in what
  `ui.insets()` leave, so it moves above the keyboard. What it holds stacks as
  it is: `modal_title`, `modal_message`, `modal_actions` with buttons side by
  side at equal widths, or any other element. It is `widgets.modal.width` wide,
  less where the screen keeps `margin` free on each side, and
  `.part(Part::Panel, |panel| panel.width(w))` sets another width.

  ```rust
  modal(ui, [
      modal_title(ui, "Delete this file?"),
      modal_message(ui, "This cannot be undone."),
      modal_actions(ui, [button(ui, "Cancel", b.secondary, Msg::Close), button(ui, "Delete", b.destructive, Msg::Delete)]),
  ])
  ```

- `progress_bar` and `progress_indicator` show `Some(0.0..=1.0)`. With `None`
  they move until they know: a segment crosses the bar and comes back, an arc
  goes round, each fading toward its tail. Only they repaint while moving, and
  `period_ms: 0` in the theme keeps them still. An indicator takes its size
  from `.width` and `.height`, and its ring width from `.stroke`.
- `cx.animate(id, animation)` moves a node's offset or opacity, without running a
  view or layout.
- A style with `animate` eases to its new values by itself, from what shows at
  that moment and without running a view or layout: `Animated::COLOR`, `OPACITY`
  and `POSITION`, the last being where layout puts the node in its parent. The
  value of the state being entered counts, so `pressed` can go at once and
  fade back. Size does not ease; a node coming or going plays `.appear` and
  `.disappear`.
- `.appear(presence)` plays once the node is placed, from how `Presence` says it
  looks while absent. `.disappear` plays when the view drops it or its screen
  leaves the `Overlay`, and the node stays out of layout and touches until it
  ends. Without `.disappear` the node goes as it came, backwards.

  ```rust
  .appear(Presence::fade().scale(0.9)).disappear(Presence::fade())
  .appear(Presence::from_edge(Edge::Bottom))
  .appear(Presence::fade().offset(0, 24).ms(150))
  ```

  `from_edge` starts just past that edge of the screen wherever the node is.
  `Presence::none()` comes or goes at once, and 200 ms is the default length.
  Siblings without an `id` match by place, so one that comes and goes among
  them needs an `id`. `scale` draws an RGB565 picture of the node at half its
  size, taken when it starts; without memory for it the node moves unscaled,
  with a warning.

## Rotation

The UI turns when told to, with `Input::set_rotation`. Everything is built and
painted again at the new size; `ui.rotation()` and `ui.screen_size()` tell the
views.

```rust
let input = app.input();
input.set_rotation(Rotation::R90);
```

`AutoRotation` does the usual thing on top of that, as a plugin: it follows the
IMU, and keeps the turn and a lock in the store.

```rust
let rotation = AutoRotation::new(board.imu);
config.plugins.push(Box::new(rotation.clone()));

// In a settings screen.
rotation.set_locked(true);
```

- It takes the IMU's pose callback. `imu.read` and `imu.orientation` stay
  available; an app that wants the poses itself leaves this type out and calls
  `Input::set_rotation` from its own callback.
- `ScreenStyle::rotations` limits the screen in front. `R0` to `R270` are the
  panel's, so which is upright differs by board; `PORTRAIT` and `LANDSCAPE` mean
  the shape on any board, and `CURRENT` keeps the turn the screen found. The turn
  asked for comes back when a screen allows it.
- A turn waits for a transition to end. Fingers on the screen act on nothing until
  they lift, flings stop, and an `ExternalSurface` is asked to write again, with
  `frame.rotation` and `frame.area` new from the next frame.
- A strip buffer must hold 16 rows of the longer side; otherwise the turn is
  refused and logged.

## Plugins

`AppConfig::plugins` adds behaviour the app does not otherwise see. `init` runs
before anything is drawn and sets how the UI starts, `start` once the app exists,
and `on_event` on the UI task when the UI turns or changes color mode.

```rust
config.plugins.push(Box::new(SavedColorMode::new()));
```

- A type with more to offer stays with the app as a clone of a shared handle, as
  `AutoRotation` does.
- `Store` keeps small settings in the NVS namespace of `AppConfig::nvs_namespace`,
  `devkit_ui` by default, opened on first use. Keys in use are `rotation`,
  `locked` and `color_mode`.
- `set_u8` returns at once, and the values go to flash 5 seconds after the last
  change, from the FreeRTOS timer task. Flash stops both cores, and a change
  still waiting is lost if the power goes. The save takes about 1 KB of that
  task's stack.
- `on_event` blocks the UI, so what takes long goes elsewhere.

## Text input

- Text comes through one path: the on-screen keyboard and `App::input` (a
  physical keyboard, a test harness) both reach the focused node as `TextInput`.
  A tap on another node ends the input unless it has `.keep_focus()`;
  `cx.focus(id!("name"))` moves the focus from `update`.
- `TextBuffer` stays with the caller, which applies what the input reports.
  `Enter` and `Custom` are left to it. The caret blinks without a view.

  ```rust
  text_field(ui, &self.name, Msg::Name).placeholder("Name").id(id!("name"))
  text_field(ui, &self.pin, Msg::Pin).input(InputKind::Number)
  // Msg::Name(input) => { self.name.apply(input); cx.rebuild(); }
  ```

- `AppConfig::keyboard` says when the docked keyboard shows: with
  `KeyboardMode::Auto`, while an input has focus and no physical keyboard is
  reported by `Input::set_keyboard_connected`. It raises `insets().bottom` of
  what is under it, and slides in from the bottom and out again while
  `insets()` changes at once.
  `widgets::keyboard` places one in a view instead, coming and going at once.
- A `Keyboard` is static data. Label and action of a `Key` are separate, a style
  or font can replace the theme's, and widths go by quarters of a column: a key
  `n` wide covers `n` pitches less one gap, so a wide key spans the gaps it
  covers. `height` is the whole panel's and stays through layer changes, with a
  layer's rows sharing it evenly, and `columns` belongs to each layer.
  `Theme::keyboards` holds the default layouts, defined in the theme as
  [docs/resgen.md](../../docs/resgen.md) describes, and `theme.widgets.keyboard`
  the gap and styles; `.input` picks a layout and `.keyboard` overrides it.

  ```rust
  static PORT_KEYS: Keyboard = Keyboard {
      height: 96,
      layers: &[Layer {
          columns: 3,
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

- `ui.canvas(draw)` is a node painted by `draw`, a method of the component
  whose view holds it, in its own coordinates and clipped to it.
  `cx.redraw(id)` and `cx.redraw_rect(id, rect)` repaint it without a view or
  layout; `canvas.needs(rect)` tells what this frame repaints, across
  framebuffers, so the rest need not be recorded.

  ```rust
  ui.canvas(Self::draw_terminal).grow(1).id(id!("term")).on_size(Msg::Resized).on_input(Msg::Key)

  fn draw_terminal<'a>(&'a self, canvas: &mut Canvas<'a, '_>) {
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
- `canvas.line(from, to, width, color)` and `canvas.polyline(&self.points,
  width, color)` stroke with round joins and ends, antialiased, at `f32`
  positions where pixel `(0, 0)` has its center at `(0.5, 0.5)`. A polyline paints
  each pixel once, so a translucent one does not darken at its joins; its points
  stay borrowed until the frame is drawn, like a `TextRun`.
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
