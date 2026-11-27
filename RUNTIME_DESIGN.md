# Runtime widgets: design

Widgets drawn by a game: screen space (HUD, menus) and world space (a score over a
fountain, a sign). Status: **structure only**. `asset/` exists; everything else here
is the plan. Update this file with an "As built" section as pieces land.

## Crates

| Directory | Package | Owns |
|---|---|---|
| `asset/` | `widget_asset` | **built**: the `.widget` file, library, instance and parameter resolution, layout (through `TextMetrics`), display commands, hit testing. No GPUI |
| `api/` | `helio-ui-api` | the GPU row ABI of one UI primitive and its WGSL, the buffer contracts. No statics |
| `text/` | `widget_text` | the one owner of runtime text: font files, shaping, line breaking, the glyph atlas, and the `TextMetrics` that layout measures with |
| `component/` | `widget_component` | `WidgetComponent`, the retained runtime instance, parameters, primitive generation, script methods and events |
| `feature/` | `helio-pass-ui` | the screen and world UI passes |
| root | `ui_editor_plugin` | registrations. Stops being `editor_only`: the GPUI editor moves behind the `editor` feature, as `sprites` does |

## Performance contract

- A HUD that does not change costs **no CPU work** per frame: no resolve, no layout,
  no upload, no allocation. One draw call for the screen layer, however many widgets
  (textures and the glyph atlas are bound once).
- A changed parameter costs work in proportion to what it changes. A colour or a
  number shown as text must not re-resolve and re-lay-out every widget in the world.
  `widget_asset::resolve` today clones whole trees and reads properties by string:
  use it to build the retained instance, not every frame. Measure before and after.
- Per-frame cost never scans all objects. Rows reach the GPU through SceneDB's
  mirroring (var-len pools suit primitive runs) or a `RowJoin`/`SceneDerivation`,
  and buffer writes go through `FrameUploads` or the pass contexts. No private
  mailbox into the renderer.

## Drawing

One instanced pipeline, one quad per primitive, everything analytic in the fragment
shader, premultiplied alpha:

- rounded rectangle by signed distance, a radius per corner; anti-aliased at any scale
- border (colour, width; per side if it costs nothing for the common case), styles:
  solid and dashed at least
- fills: solid, linear and radial gradients with several stops (a small ramp table)
- soft shadow and glow by the analytic blurred rounded rectangle (no blur passes)
- images with tint, corner radius, stretch modes, nine-slice
- glyphs from the atlas, with outline and drop shadow
- clipping per primitive (including a rounded clip), so clips never split batches
- opacity and a 2D transform (translate, scale, rotation) per primitive

`widget_asset::display::Cmd` is what the editor paints today. Extend it where a style
above has no command or property yet (font family, gradient stops, radial fill, text
outline, letter spacing, nine-slice), keeping old files loading. The GPUI editor
painter must keep working; it may approximate what GPUI cannot draw.

### Screen space

A node in `Slot::Display` after the tone map, at **output resolution**
(`FrameDesc.width/height`, never the internal render size), so a lowered
`render_scale` never blurs the UI. Order it after sprites (10) and before the
performance overlay (20). Layout is in logical units; one scale (design size against
the viewport height, times the window's display scale where the project asks for it)
maps them to physical pixels, and edges and glyphs snap to the pixel grid.

### World space

The same primitives with a 3D transform: either fixed in the world or facing the
camera, size in metres, optionally a constant size on screen. Drawn at output
resolution after the tone map (colours stay as authored), depth-tested against the
scene's depth in the shader with a soft edge (the depth is internal resolution and
jittered: account for both). Positions are camera-relative, and the rules in the
`helio-architecture` skill's "Coordinate spaces" section apply. Glyphs must stay
sharp at any distance and angle, which rules out per-size raster glyphs for this path.

## Text

Pick the stack on evidence and say why. `cosmic-text`, `swash`, `rustybuzz` and
`ttf-parser` are already in `Cargo.lock`. The expected shape: a real shaper and line
breaker, and one distance-field atlas (SDF or MSDF, generated from glyph outlines,
filled on demand, bounded, with eviction) used by both screen and world text. Small
screen text must look as good as a rasterised font; if a distance field cannot, add a
hinted raster path for it. Layout and drawing must use the same shaping, or text
clips and drifts. Ship at least one proportional family with regular and bold and one
monospace, each with its licence file (IBM Plex Sans and JetBrains Mono are OFL and
already in the repository; check their licence files travel with them). Fonts must
reach a packaged game. UTF-8 beyond ASCII must work. The debug overlay's 8x8 font is
a diagnostics tool: leave it.

## The component, parameters and handles

A widget on screen or in the world is an **entity with a `WidgetComponent`**: the
component reference is the handle, which gives liveness, reflected methods, level
authoring, replication of the one existing path and Blueprint component events for
free. No second kind of handle.

`WidgetComponent`: the `.widget` asset (declare its path type with
`pulsar_world_registry::asset_path_type!`; it must be collected by the packer, as
must the images and widgets it refers to), space (screen or world), z order, enabled (off by default),
screen placement or world sizing and billboard mode, and the parameter values that
differ from the asset's defaults (saved with the level).

Parameters are the asset's own (`widget_asset::model::widget::Parameter`: bool,
number, integer, text, color, vec2, margins, choice, image). Script methods, by name:
`set_number`, `set_integer`, `set_text`, `set_bool`, `set_color`, `set_image`,
`set_vec2`, `set_choice`, the matching getters. A widget starts **disabled**, so none covers
the editor's viewport; the Blueprint of the class holding it turns it on (the Set Enabled node). A wrong name or type
is reported once with the widget and parameter named, never silently ignored.
Setting the same value again does no work.

Typed Blueprint nodes per widget asset ("Set Score" with a pin of the parameter's
type, on a handle typed by the asset) are **not part of this build**; they are a
Blueprint compiler feature on top of these methods. Keep what they need available:
an asset's parameter list, readable by path without spawning it.

## Interaction

Hit testing (screen: against the laid-out tree; world: ray against the widget plane),
hover, pressed and disabled looks per node, and a component event
`on_action(action, node)` when a button with an `action` is clicked. The input must
come from the engine's input events in physical window pixels, in the packaged game
and in play-in-editor. Consuming a click so gameplay does not also see it needs an
input-system seam: design it, and build it if it is contained. Showing and freeing
the cursor, level travel and quit are host features **outside this build**.

## Presence

Every shipped pipeline must draw widgets without being edited (a slotted node does).
A custom pipeline without the UI node must produce a finding that says so, rather
than silently drawing nothing, while a widget exists. With the plugin disabled the
whole kind is absent.

## Done means

- A level with a HUD (text, score number, rounded translucent panel, gradient bar,
  border, shadow, image) and a world-space widget draws correctly in the editor
  viewport and in a packaged game, checked on real frames.
- A script changes a score and the widget shows it.
- Measured: CPU time and allocations per frame for a static HUD (must be zero UI
  work), for one changing number, and for 100 widgets with some changing; draw call
  count; GPU time of both passes.
- Tests: row ABI and WGSL layout, text shaping and atlas behaviour, parameter
  changes and their dirtiness, hit testing, plugin disabled.

## Button events (as built)

Each button of the widget a `WidgetComponent` names has its own events: `widget::<widget id>.<button node ids>.<phase>`
for `clicked`, `pressed`, `released`, `hovered`, `unhovered` (`component/src/events.rs`). Instanced widgets' buttons
are included; ids make renaming a button, its action or the file harmless.

- Editor: the root plugin registers an `AssetPathSlot` for `WidgetComponent.widget_asset` and an
  `AssetEventSource` for the `widget` kind (`src/event_source.rs`), so the Blueprint palette offers "On Play
  Clicked" and so on under the component, for whichever widget it names.
- Runtime: a game session declares the events of every project widget before scripts link
  (`GameSessionRegistration` `ui.button_events`); the interaction system raises them on the component's and its
  object's channels. They carry no values.
- `on_action` is still raised for a clicked button that names an action. Its `WidgetAction` value cannot be read
  from a Blueprint.

Tests: `component/tests/widgets.rs` (phases, the button's own event), `event_source::tests`. Checked on screen in
Coin Town's packaged main menu.
