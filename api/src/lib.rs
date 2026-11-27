//! UI's public contract (`pulsar.ui`): what the UI passes draw.
//!
//! A widget is drawn as **primitives**, one instanced quad each
//! ([`UiPrimitive`]), everything analytic in the fragment shader: rounded
//! boxes with a border and a gradient, blurred rounded boxes (shadows and
//! glows), images, glyphs and line strokes. The primitives of a layer share
//! one pipeline and one set of bindings, so clips, textures and fonts never
//! split a batch.
//!
//! A frame's primitives reach the passes as the CPU payload [`UI_FRAME`]: the
//! screen and world layers' [`UiBatch`]es (one per widget, with a revision, so
//! a pass uploads only the widgets that changed), the world widgets'
//! placements and the glyph atlas. A widget's primitives are a draw-ordered
//! run, and the layer's order (what blends over what) is a property of all its
//! widgets together, which per-entity GPU columns cannot express; that is why
//! they are a payload and not SceneDB rows.

use std::sync::Arc;

use bytemuck::{Pod, Zeroable};

/// Nothing: pads a batch to its capacity.
pub const KIND_NONE: u32 = 0;
/// A rounded box: fill (solid or gradient) and border.
pub const KIND_BOX: u32 = 1;
/// A blurred rounded box (soft shadow, glow); `border_softness[1]` is the blur.
pub const KIND_SHADOW: u32 = 2;
/// An image from the scene's texture table, tinted.
pub const KIND_IMAGE: u32 = 3;
/// A glyph's coverage raster from the atlas, sampled one texel per pixel.
pub const KIND_GLYPH: u32 = 4;
/// A glyph's distance field from the atlas, with an optional outline.
pub const KIND_FIELD_GLYPH: u32 = 5;
/// A round-capped line from `uv.xy` to `uv.zw`, `border_softness[0]` wide.
pub const KIND_STROKE: u32 = 6;
pub const KIND_MASK: u32 = 0xff;

pub const FLAG_LINEAR_GRADIENT: u32 = 1 << 8;
pub const FLAG_RADIAL_GRADIENT: u32 = 1 << 9;
pub const FLAG_MIDDLE_STOP: u32 = 1 << 10;

/// An image with no texture yet.
pub const NO_TEXTURE: u32 = u32::MAX;

/// The GPU row of one primitive. Positions are in the layer's layout units:
/// output pixels on screen, widget units for a world widget.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct UiPrimitive {
    /// `x, y, width, height` before the transform.
    pub rect: [f32; 4],
    /// Corner radii: top-left, top-right, bottom-right, bottom-left.
    pub radii: [f32; 4],
    /// Images: `u0, v0, u1, v1` in 0..1. Glyphs: atlas texels. Strokes: the
    /// two end points.
    pub uv: [f32; 4],
    /// The clip box `x0, y0, x1, y1`, after the transform.
    pub clip: [f32; 4],
    pub clip_radii: [f32; 4],
    /// The 2x2 of the primitive's transform, column major.
    pub transform: [f32; 4],
    pub translation: [f32; 2],
    /// Border width (outline width for a field glyph, line width for a
    /// stroke), in layout units, and blur radius (shadows).
    pub border_softness: [f32; 2],
    /// Straight-alpha sRGB, `0xAABBGGRR`: the fill's first stop (or the
    /// colour or tint), the middle stop, the last stop, the border or
    /// outline.
    pub colors: [u32; 4],
    /// Gradient angle in radians, middle stop position, dash length, and the
    /// distance field's layout units per atlas texel.
    pub params: [f32; 4],
    /// Kind and flags; texture slot; world widget placement row; unused.
    pub info: [u32; 4],
}

/// The placement of one world widget, in the frame's camera-relative space.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct UiPlacement {
    /// The widget's centre; `w` is the coordinate-space slot, numerically.
    pub centre: [f32; 4],
    /// The unit vector to the widget's right; `w`: [`PLACE_BILLBOARD`] and
    /// [`PLACE_CONSTANT_SIZE`] bits, numerically.
    pub right: [f32; 4],
    /// The unit vector down the widget; `w`: metres per widget unit.
    pub down: [f32; 4],
    /// The widget's size in widget units (`xy`).
    pub size: [f32; 4],
}

/// The widget turns to face the camera.
pub const PLACE_BILLBOARD: u32 = 1;
/// The widget keeps its size on screen: its metres are its size one metre
/// away.
pub const PLACE_CONSTANT_SIZE: u32 = 2;

/// One widget's primitives, in draw order.
#[derive(Clone, Debug)]
pub struct UiBatch {
    /// Names the widget instance across frames.
    pub key: u64,
    /// Moves whenever `primitives` differ.
    pub revision: u64,
    pub primitives: Arc<[UiPrimitive]>,
    /// World widgets: the row of [`UiPlacements`] the primitives name in
    /// `info[2]`. Zero on screen.
    pub placement: u32,
}

/// A layer's widgets, back to front.
#[derive(Debug, Default)]
pub struct UiLayer {
    /// Moves whenever any batch or their order changes. Unique across the
    /// layers of one publisher.
    pub revision: u64,
    pub batches: Vec<UiBatch>,
}

/// The world widgets' placements, indexed by [`UiBatch::placement`]. A row
/// keeps its index while its widget lives, so a moved widget rewrites no
/// primitive.
#[derive(Debug, Default)]
pub struct UiPlacements {
    pub revision: u64,
    pub rows: Vec<UiPlacement>,
}

/// The glyph atlas the passes sample, uploaded by what changed.
pub trait AtlasSource: Send + Sync {
    /// The atlas's side in texels (`Rgba8Unorm`).
    fn size(&self) -> u32;
    /// Hand `upload` each square region changed after version `since` as
    /// `(x, y, side, the atlas's texels from the region's first byte)`, rows
    /// `size()` texels apart; returns the version the reader now has.
    fn changes_since(&self, since: u64, upload: &mut dyn FnMut(u32, u32, u32, &[u8])) -> u64;
}

/// What the UI passes draw this frame.
#[derive(Clone)]
pub struct UiFrame {
    pub screen: Arc<UiLayer>,
    pub world: Arc<UiLayer>,
    pub placements: Arc<UiPlacements>,
    pub atlas: Arc<dyn AtlasSource>,
}

/// The payload the UI passes read; published by the component's render-host
/// part.
pub const UI_FRAME: helio_core::PayloadContract<UiFrame> =
    helio_core::PayloadContract::new("pulsar.ui.frame");

/// The pass type names, so a host can tell whether a pipeline draws widgets.
pub const SCREEN_PASS_NAME: &str = "UiScreen";
pub const WORLD_PASS_NAME: &str = "UiWorld";

/// The render graph nodes of the two passes.
pub const SCREEN_NODE: &str = "helio.pass.ui_screen";
pub const WORLD_NODE: &str = "helio.pass.ui_world";

/// The WGSL of the rows, which the passes' shaders include.
pub const UI_WGSL: &str = include_str!("ui_rows.wgsl");

/// `color` (straight-alpha sRGB, 0 to 1) as a row stores it.
pub fn pack_color(color: [f32; 4]) -> u32 {
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
    byte(color[0]) | byte(color[1]) << 8 | byte(color[2]) << 16 | byte(color[3]) << 24
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::offset_of;

    fn wgsl_offsets(name: &str) -> (Vec<(String, u32)>, u32) {
        let module = naga::front::wgsl::parse_str(UI_WGSL).expect("the rows' WGSL parses");
        let (_, ty) = module
            .types
            .iter()
            .find(|(_, t)| t.name.as_deref() == Some(name))
            .expect("the struct");
        let naga::TypeInner::Struct { members, span } = &ty.inner else {
            panic!("not a struct");
        };
        let members = members
            .iter()
            .map(|m| (m.name.clone().expect("named"), m.offset))
            .collect();
        (members, *span)
    }

    macro_rules! assert_layout {
        ($ty:ty, $wgsl:literal, $($field:ident),+) => {{
            let (members, span) = wgsl_offsets($wgsl);
            let rust = vec![$((stringify!($field).to_string(), offset_of!($ty, $field) as u32)),+];
            assert_eq!(members, rust);
            assert_eq!(span as usize, std::mem::size_of::<$ty>());
        }};
    }

    #[test]
    fn the_rows_match_their_wgsl() {
        assert_layout!(
            UiPrimitive,
            "UiPrimitive",
            rect,
            radii,
            uv,
            clip,
            clip_radii,
            transform,
            translation,
            border_softness,
            colors,
            params,
            info
        );
        assert_layout!(UiPlacement, "UiPlacement", centre, right, down, size);
        assert_eq!(std::mem::size_of::<UiPrimitive>(), 160);
    }

    #[test]
    fn colors_pack_little_endian() {
        assert_eq!(pack_color([1.0, 0.0, 0.0, 1.0]), 0xff00_00ff);
        assert_eq!(pack_color([0.0, 1.0, 2.0, -1.0]), 0x00ff_ff00);
    }
}
