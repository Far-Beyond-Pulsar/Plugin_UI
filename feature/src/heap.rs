//! A layer's primitives in one GPU buffer, uploaded by what changed.
//!
//! Each widget's batch has a contiguous run with some room to grow, in draw
//! order, so the whole screen layer is one instanced draw. A changed batch
//! that still fits rewrites only its run (a score that gains a digit); one
//! that outgrows it, or a change of the widgets or their order, rewrites from
//! there to the end. Unchanged layers cost one revision compare. This is the
//! shape of UI Toolkit's retained mesh pages and Slate's cached element
//! batches.

use helio_ui_api::{UiLayer, UiPrimitive};

/// Runs are multiples of this many primitives.
const RUN: usize = 16;

/// Where one batch is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    pub key: u64,
    pub revision: u64,
    pub start: u32,
    pub capacity: u32,
    /// Primitives in use.
    pub len: u32,
    pub placement: u32,
}

/// One buffer write: primitives from `start`.
pub struct Write<'a> {
    pub start: u32,
    pub primitives: &'a [UiPrimitive],
}

/// The CPU side of a layer's buffer.
#[derive(Default)]
pub struct Heap {
    runs: Vec<Run>,
    revision: Option<u64>,
    scratch: Vec<UiPrimitive>,
}

impl Heap {
    pub fn runs(&self) -> &[Run] {
        &self.runs
    }

    /// Primitives the buffer must hold, padding included.
    pub fn len(&self) -> u32 {
        self.runs.last().map_or(0, |r| r.start + r.capacity)
    }

    /// Whether `layer` is not what the buffer holds.
    pub fn is_stale(&self, layer: &UiLayer) -> bool {
        self.revision != Some(layer.revision)
    }

    /// Forget what the buffer holds (it was replaced).
    pub fn invalidate(&mut self) {
        self.runs.clear();
        self.revision = None;
    }

    /// The primitives `layer` needs room for, if rewritten from nothing.
    pub fn required(layer: &UiLayer) -> usize {
        layer
            .batches
            .iter()
            .map(|b| capacity_for(b.primitives.len()))
            .sum()
    }

    /// Bring the runs to `layer`, handing `write` each range to upload.
    pub fn sync(&mut self, layer: &UiLayer, write: &mut dyn FnMut(Write<'_>)) {
        if !self.is_stale(layer) {
            return;
        }
        self.revision = Some(layer.revision);
        // The leading batches that keep their place.
        let mut kept = 0;
        for (run, batch) in self.runs.iter_mut().zip(&layer.batches) {
            if run.key != batch.key || batch.primitives.len() > run.capacity as usize {
                break;
            }
            if run.revision != batch.revision {
                // Rewrite the run, clearing what the batch no longer uses.
                let used = batch.primitives.len().max(run.len as usize);
                self.scratch.clear();
                self.scratch.extend_from_slice(&batch.primitives);
                self.scratch.resize(used, UiPrimitive::default());
                if !self.scratch.is_empty() {
                    write(Write {
                        start: run.start,
                        primitives: &self.scratch,
                    });
                }
                run.revision = batch.revision;
                run.len = batch.primitives.len() as u32;
                run.placement = batch.placement;
            }
            kept += 1;
        }
        if kept == layer.batches.len() {
            self.runs.truncate(kept);
            return;
        }
        self.runs.truncate(kept);
        let tail_start = self.len();
        let mut start = tail_start;
        self.scratch.clear();
        for batch in &layer.batches[kept..] {
            let capacity = capacity_for(batch.primitives.len());
            let end = self.scratch.len() + capacity;
            self.scratch.extend_from_slice(&batch.primitives);
            self.scratch.resize(end, UiPrimitive::default());
            self.runs.push(Run {
                key: batch.key,
                revision: batch.revision,
                start,
                capacity: capacity as u32,
                len: batch.primitives.len() as u32,
                placement: batch.placement,
            });
            start += capacity as u32;
        }
        write(Write {
            start: tail_start,
            primitives: &self.scratch,
        });
    }
}

fn capacity_for(len: usize) -> usize {
    (len + RUN / 4).div_ceil(RUN).max(1) * RUN
}

#[cfg(test)]
mod tests {
    use super::*;
    use helio_ui_api::UiBatch;

    fn batch(key: u64, revision: u64, len: usize) -> UiBatch {
        UiBatch {
            key,
            revision,
            primitives: vec![UiPrimitive::default(); len].into(),
            placement: 0,
        }
    }

    fn writes(heap: &mut Heap, layer: &UiLayer) -> Vec<(u32, usize)> {
        let mut out = Vec::new();
        heap.sync(layer, &mut |w| out.push((w.start, w.primitives.len())));
        out
    }

    #[test]
    fn only_what_changed_is_written() {
        let mut heap = Heap::default();
        let mut layer = UiLayer {
            revision: 1,
            batches: vec![batch(1, 1, 5), batch(2, 1, 5), batch(3, 1, 5)],
        };
        assert_eq!(writes(&mut heap, &layer), vec![(0, 48)]);
        assert!(writes(&mut heap, &layer).is_empty(), "an unchanged layer");

        // The middle widget's number gains digits but fits its run.
        layer.revision = 2;
        layer.batches[1] = batch(2, 2, 9);
        assert_eq!(writes(&mut heap, &layer), vec![(16, 9)]);

        // It shrinks: what it no longer uses is cleared.
        layer.revision = 3;
        layer.batches[1] = batch(2, 3, 4);
        assert_eq!(writes(&mut heap, &layer), vec![(16, 9)]);

        // It outgrows the run: it and what follows move.
        layer.revision = 4;
        layer.batches[1] = batch(2, 4, 40);
        assert_eq!(writes(&mut heap, &layer), vec![(16, 64)]);
        assert_eq!(heap.len(), 80);

        // One leaves.
        layer.revision = 5;
        layer.batches.pop();
        assert!(writes(&mut heap, &layer).is_empty());
        assert_eq!(heap.len(), 64);
    }
}
