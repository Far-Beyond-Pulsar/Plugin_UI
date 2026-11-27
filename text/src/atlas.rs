//! The glyph atlas: one fixed-size RGBA texture of glyph images, filled on
//! demand.
//!
//! It is divided into plots, each shelf-packed (Skia's `GrDrawOpAtlas`
//! shape): when nothing has room, the least recently used plot is cleared as a
//! whole. Memory never grows. An eviction moves [`GlyphAtlas::epoch`], and
//! whoever holds primitives that point into the atlas rebuilds them. Each plot
//! carries a version, so a renderer uploads only the plots that changed since
//! it last looked.

use std::collections::HashMap;

pub const ATLAS_SIZE: u32 = 2048;
pub const PLOT_SIZE: u32 = 256;
const PLOTS_PER_ROW: u32 = ATLAS_SIZE / PLOT_SIZE;
const PLOTS: usize = (PLOTS_PER_ROW * PLOTS_PER_ROW) as usize;
/// Empty texels between glyphs, so filtering never reaches a neighbour.
const GAP: u32 = 1;

/// A glyph image to place: `texels` is RGBA, `width * height * 4` bytes.
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub texels: Vec<u8>,
    /// The caller's metrics, kept with the entry.
    pub offset: [f32; 2],
}

/// Where a glyph is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Entry {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub offset: [f32; 2],
    plot: u16,
}

#[derive(Default)]
struct Plot {
    /// `(top, height, next free x)`.
    shelves: Vec<(u32, u32, u32)>,
    used_height: u32,
    last_used: u64,
    version: u64,
}

impl Plot {
    fn allocate(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        // The tightest shelf that fits, else a new one.
        let best = self
            .shelves
            .iter_mut()
            .filter(|(_, h, x)| *h >= height && *h <= height + height / 2 + 2 && x + width <= PLOT_SIZE)
            .min_by_key(|(_, h, _)| *h);
        if let Some(shelf) = best {
            let at = (shelf.2, shelf.0);
            shelf.2 += width;
            return Some(at);
        }
        if self.used_height + height > PLOT_SIZE || width > PLOT_SIZE {
            return None;
        }
        let top = self.used_height;
        self.used_height += height;
        self.shelves.push((top, height, width));
        Some((0, top))
    }
}

/// See the module doc. `K` names a glyph image.
pub struct GlyphAtlas<K: std::hash::Hash + Eq + Clone> {
    texels: Vec<u8>,
    plots: Vec<Plot>,
    entries: HashMap<K, Option<Entry>>,
    /// Moves when entries are evicted.
    epoch: u64,
    /// Moves when any texel changes.
    version: u64,
    /// The current use stamp (one per batch of lookups).
    clock: u64,
    /// Glyphs that did not fit even after eviction.
    dropped: u64,
}

impl<K: std::hash::Hash + Eq + Clone> Default for GlyphAtlas<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: std::hash::Hash + Eq + Clone> GlyphAtlas<K> {
    pub fn new() -> Self {
        Self {
            texels: vec![0; (ATLAS_SIZE * ATLAS_SIZE * 4) as usize],
            plots: (0..PLOTS).map(|_| Plot::default()).collect(),
            entries: HashMap::new(),
            epoch: 0,
            version: 0,
            clock: 1,
            dropped: 0,
        }
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Start a batch of lookups: plots used in it are the last to be evicted.
    pub fn tick(&mut self) {
        self.clock += 1;
    }

    /// The entry of `key`, made by `make` the first time. `None` for a glyph
    /// with no image, or one that does not fit.
    pub fn get_or_insert(
        &mut self,
        key: &K,
        make: impl FnOnce() -> Option<Bitmap>,
    ) -> Option<Entry> {
        if let Some(found) = self.entries.get(key) {
            if let Some(entry) = found {
                self.plots[entry.plot as usize].last_used = self.clock;
            }
            return *found;
        }
        let Some(bitmap) = make() else {
            self.entries.insert(key.clone(), None);
            return None;
        };
        let entry = self.place(&bitmap);
        match entry {
            Some(_) => {
                self.entries.insert(key.clone(), entry);
            }
            None => self.dropped += 1,
        }
        entry
    }

    fn place(&mut self, bitmap: &Bitmap) -> Option<Entry> {
        let (w, h) = (bitmap.width + GAP, bitmap.height + GAP);
        if w > PLOT_SIZE || h > PLOT_SIZE || bitmap.width == 0 || bitmap.height == 0 {
            return None;
        }
        let found = self
            .plots
            .iter_mut()
            .enumerate()
            .find_map(|(i, plot)| plot.allocate(w, h).map(|at| (i, at)));
        let (index, (x, y)) = match found {
            Some(found) => found,
            None => {
                let index = self.evict()?;
                let at = self.plots[index].allocate(w, h)?;
                (index, at)
            }
        };
        let (px, py) = Self::plot_origin(index);
        let (x, y) = (px + x, py + y);
        let stride = (ATLAS_SIZE * 4) as usize;
        let row = (bitmap.width * 4) as usize;
        for line in 0..bitmap.height as usize {
            let to = (y as usize + line) * stride + x as usize * 4;
            self.texels[to..to + row].copy_from_slice(&bitmap.texels[line * row..(line + 1) * row]);
        }
        self.version += 1;
        let plot = &mut self.plots[index];
        plot.version = self.version;
        plot.last_used = self.clock;
        Some(Entry {
            x,
            y,
            width: bitmap.width,
            height: bitmap.height,
            offset: bitmap.offset,
            plot: index as u16,
        })
    }

    /// Clear the least recently used plot that the current batch does not use.
    fn evict(&mut self) -> Option<usize> {
        let (index, _) = self
            .plots
            .iter()
            .enumerate()
            .filter(|(_, p)| p.last_used < self.clock)
            .min_by_key(|(_, p)| p.last_used)?;
        self.entries
            .retain(|_, e| e.is_none_or(|e| e.plot as usize != index));
        let (px, py) = Self::plot_origin(index);
        let stride = (ATLAS_SIZE * 4) as usize;
        for line in 0..PLOT_SIZE {
            let at = (py + line) as usize * stride + px as usize * 4;
            self.texels[at..at + (PLOT_SIZE * 4) as usize].fill(0);
        }
        self.version += 1;
        self.epoch += 1;
        self.plots[index] = Plot {
            version: self.version,
            ..Default::default()
        };
        Some(index)
    }

    fn plot_origin(index: usize) -> (u32, u32) {
        let index = index as u32;
        (
            (index % PLOTS_PER_ROW) * PLOT_SIZE,
            (index / PLOTS_PER_ROW) * PLOT_SIZE,
        )
    }

    /// Hand `upload` every plot changed after version `since` as `(x, y, size,
    /// first byte offset into the whole atlas, the atlas's texels)`, and
    /// return the version this brings the reader to.
    pub fn changes_since(&self, since: u64, upload: &mut dyn FnMut(u32, u32, u32, &[u8])) -> u64 {
        if since != self.version {
            for (index, plot) in self.plots.iter().enumerate() {
                if plot.version > since {
                    let (x, y) = Self::plot_origin(index);
                    let start = ((y * ATLAS_SIZE + x) * 4) as usize;
                    upload(x, y, PLOT_SIZE, &self.texels[start..]);
                }
            }
        }
        self.version
    }

    /// The texels (tests).
    pub fn texels(&self) -> &[u8] {
        &self.texels
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(side: u32, value: u8) -> Bitmap {
        Bitmap {
            width: side,
            height: side,
            texels: vec![value; (side * side * 4) as usize],
            offset: [0.0, 0.0],
        }
    }

    #[test]
    fn entries_are_made_once_and_never_overlap() {
        let mut atlas = GlyphAtlas::<u32>::new();
        let mut placed = Vec::new();
        for key in 0..500u32 {
            let side = 10 + key % 30;
            placed.push(atlas.get_or_insert(&key, || Some(square(side, 7))).unwrap());
        }
        for (i, a) in placed.iter().enumerate() {
            assert!(a.x + a.width <= ATLAS_SIZE && a.y + a.height <= ATLAS_SIZE);
            for b in &placed[i + 1..] {
                let apart = a.x + a.width <= b.x
                    || b.x + b.width <= a.x
                    || a.y + a.height <= b.y
                    || b.y + b.height <= a.y;
                assert!(apart, "{a:?} overlaps {b:?}");
            }
        }
        let again = atlas.get_or_insert(&3, || panic!("made twice")).unwrap();
        assert_eq!(again, placed[3]);
    }

    #[test]
    fn a_full_atlas_evicts_the_least_used_plot_and_says_so() {
        let mut atlas = GlyphAtlas::<u32>::new();
        // 120 texel squares: four to a plot, 256 to the atlas.
        for key in 0..256u32 {
            atlas.tick();
            atlas.get_or_insert(&key, || Some(square(120, 1))).unwrap();
        }
        assert_eq!(atlas.epoch(), 0);
        atlas.tick();
        atlas.get_or_insert(&1000, || Some(square(120, 2))).unwrap();
        assert_eq!(atlas.epoch(), 1, "one plot was cleared");
        assert!(atlas.len() <= 256 - 4 + 1, "its entries went with it");
        let mut made = false;
        atlas.get_or_insert(&0, || {
            made = true;
            Some(square(120, 1))
        });
        assert!(made, "the oldest glyph was in the cleared plot");
        assert_eq!(atlas.texels().len(), (ATLAS_SIZE * ATLAS_SIZE * 4) as usize);
    }

    #[test]
    fn a_batch_that_needs_everything_drops_the_overflow_without_evicting_itself() {
        let mut atlas = GlyphAtlas::<u32>::new();
        for key in 0..256u32 {
            atlas.get_or_insert(&key, || Some(square(120, 1))).unwrap();
        }
        assert!(atlas.get_or_insert(&999, || Some(square(120, 1))).is_none());
        assert_eq!((atlas.epoch(), atlas.dropped()), (0, 1));
    }

    #[test]
    fn a_reader_gets_only_what_changed() {
        let mut atlas = GlyphAtlas::<u32>::new();
        atlas.get_or_insert(&1, || Some(square(8, 9)));
        let mut plots = 0;
        let seen = atlas.changes_since(0, &mut |_, _, _, _| plots += 1);
        assert_eq!(plots, 1);
        plots = 0;
        atlas.changes_since(seen, &mut |_, _, _, _| plots += 1);
        assert_eq!(plots, 0);
    }
}
