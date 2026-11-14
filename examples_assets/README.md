# Widget examples

Complete, valid `.widget` assets. Open one in the widget editor, or copy it
into your project's assets to start from it. This folder is read-only.

A `.widget` file is JSON. JSON has no comments, so each node carries a
`comment` field (kept by the editor on save) and the widget a `description`.

| File | Teaches |
|---|---|
| `MainMenu.widget` | Canvas anchors and offsets, Border panels, Vertical Box slots, Buttons and their `action` names |
| `HealthBar.widget` | Parameters (text, number, color) that drive node properties through `targets` |
| `SettingsPanel.widget` | Scroll Box, Grid (`column_sizes`, `column`/`row` slots), Check Box, Slider, Combo Box, Text Input |
| `DialogFrame.widget` | A Named Slot (a hole) and a Title parameter |
| `ConfirmDialog.widget` | Instancing another widget by its id, overriding a parameter, filling a Named Slot with `for_slot` |
| `HudScreen.widget` | Several instances with different overrides, corner anchoring, a Wrap Box and Size Boxes |

Widgets refer to each other by `id` only, never by file name or path. The
examples use fixed ids so `ConfirmDialog` and `HudScreen` keep resolving
`DialogFrame` and `HealthBar`. A copy of an example keeps its id: give the copy
a new one (the editor offers "Give this widget a new id") when both live in one
project.
