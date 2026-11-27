//! CPU time and allocations a frame the runtime widgets cost, measured with a
//! counting allocator on one thread: a static HUD, one changing number, and
//! 100 widgets of which some change. Fails if a static HUD allocates.
//!
//! `cargo test --release -p widget_component --test measure`

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::time::{Duration, Instant};

use common::*;
use glam::DVec3;
use pulsar_scenedb::{Entity, World};
use pulsar_world_registry::pulsar_scene_model::Transform;
use serde_json::json;
use widget_asset::model::schema::ValueClass;
use widget_component::runtime::{ensure_runtime, WidgetRuntime};
use widget_component::WidgetComponent;

struct Counting;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

fn count() {
    let _ = COUNTING.try_with(|on| {
        if on.get() {
            ALLOCATIONS.with(|n| n.set(n.get() + 1));
        }
    });
}

// SAFETY: every call goes to the system allocator unchanged.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        // SAFETY: the caller's contract is the system allocator's.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: as above.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count();
        // SAFETY: as above.
        unsafe { System.realloc(ptr, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const VIEW: [u32; 2] = [1920, 1080];
const FRAMES: u32 = 600;

/// One frame as a renderer's host part runs it.
fn widget_frame(runtime: &WidgetRuntime, world: &World) {
    let mut scene = runtime.lock();
    scene.sync(world);
    std::hint::black_box(scene.frame(VIEW, DVec3::ZERO, &no_images));
}

/// The mean time and allocations of `FRAMES` frames, each after `change`
/// (which is not measured).
fn run(
    name: &str,
    runtime: &WidgetRuntime,
    world: &mut World,
    mut change: impl FnMut(&mut World, u32),
) -> u64 {
    // Warm up: glyphs, shaped strings and buffers.
    for i in 0..FRAMES {
        change(world, i);
        widget_frame(runtime, world);
    }
    let mut spent = Duration::ZERO;
    let mut allocations = 0;
    let before = runtime.lock().stats;
    for i in 0..FRAMES {
        change(world, i);
        // A game's world changes every frame; the widgets must not care.
        ALLOCATIONS.with(|n| n.set(0));
        COUNTING.with(|on| on.set(true));
        let started = Instant::now();
        widget_frame(runtime, world);
        spent += started.elapsed();
        COUNTING.with(|on| on.set(false));
        allocations += ALLOCATIONS.with(Cell::get);
    }
    let after = runtime.lock().stats;
    println!(
        "{name}: {:.2} us and {:.1} allocations a frame; per frame {:.2} reads, {:.2} layouts, {:.2} repaints, {} resolves",
        spent.as_secs_f64() * 1e6 / f64::from(FRAMES),
        allocations as f64 / f64::from(FRAMES),
        (after.reads - before.reads) as f64 / f64::from(FRAMES),
        (after.layouts - before.layouts) as f64 / f64::from(FRAMES),
        (after.paints - before.paints) as f64 / f64::from(FRAMES),
        after.resolves - before.resolves,
    );
    allocations
}

fn score(world: &mut World, entity: Entity, value: u32) {
    WidgetComponent::set_parameter(
        world,
        entity,
        "score",
        ValueClass::Text,
        json!(format!("{}", 1000 + value % 100)),
    )
    .expect("set");
}

/// What the retained runtime replaces: resolving, laying out and listing the
/// drawing commands of every widget every frame (before any primitive, glyph
/// or upload).
fn baseline(widgets: u32) {
    let widget = hud();
    let library = std::collections::HashMap::new();
    let text = widget_component::text_system();
    let mut spent = Duration::ZERO;
    let mut allocations = 0;
    for i in 0..FRAMES + 10 {
        ALLOCATIONS.with(|n| n.set(0));
        COUNTING.with(|on| on.set(true));
        let started = Instant::now();
        for _ in 0..widgets {
            let mut tree = widget_asset::resolve::resolve(&widget, &library);
            widget_asset::layout::layout(
                &mut tree,
                widget_asset::model::Vec2::new(1280.0, 720.0),
                text.as_ref(),
            );
            std::hint::black_box(widget_asset::display::build(&tree, &Default::default()));
        }
        COUNTING.with(|on| on.set(false));
        if i >= 10 {
            spent += started.elapsed();
            allocations += ALLOCATIONS.with(Cell::get);
        }
    }
    println!(
        "baseline, re-resolving {widgets} widget(s) every frame: {:.2} us and {:.1} allocations a frame",
        spent.as_secs_f64() * 1e6 / f64::from(FRAMES),
        allocations as f64 / f64::from(FRAMES),
    );
}

fn main() {
    let path = write("measure.widget", &hud());

    let mut world = World::new();
    let hud_entity = spawn(&mut world, &path, [0.0; 3]);
    let runtime = ensure_runtime(&mut world).expect("widgets");
    // Something else in the world moves every frame, as in a running game.
    let mover = world.spawn();
    world.insert(mover, Transform::default());
    let idle = run("static HUD", &runtime, &mut world, move |world, i| {
        if let Some(mut transform) = world.get_mut::<Transform>(mover) {
            transform.position[0] = f64::from(i);
        }
    });
    assert_eq!(idle, 0, "a static HUD must allocate nothing");
    run("one number changing every frame", &runtime, &mut world, |world, i| {
        score(world, hud_entity, i);
    });
    let primitives = runtime.lock().build_screen(VIEW, &no_images).batches[0]
        .primitives
        .len();
    println!("the HUD is {primitives} primitives, one draw call");

    baseline(1);
    baseline(100);

    let mut world = World::new();
    let many: Vec<Entity> = (0..100)
        .map(|_| spawn(&mut world, &path, [0.0; 3]))
        .collect();
    let runtime = ensure_runtime(&mut world).expect("widgets");
    let idle = run("100 static widgets", &runtime, &mut world, |_, _| {});
    assert_eq!(idle, 0, "static widgets must allocate nothing");
    run("100 widgets, 5 numbers changing", &runtime, &mut world, |world, i| {
        for k in 0..5 {
            score(world, many[k * 17], i + k as u32);
        }
    });
}
