use std::{cmp::Reverse, iter};

#[derive(Copy, Clone, Debug)]
pub struct Chunk {
  /// Index of the chunk
  pub index: usize,
  /// Starting frame (inclusive)
  pub start: usize,
  /// Ending frame (inclusive)
  pub end: usize,
}

#[must_use]
pub fn splits_to_chunks(num_frames: usize, split_locations: &[usize]) -> Vec<Chunk> {
  let mut chunk_boundaries: Vec<Chunk> = split_locations
    .iter()
    .copied()
    .chain(iter::once(num_frames))
    .zip(
      split_locations
        .iter()
        .copied()
        .chain(iter::once(num_frames))
        // it's zero-indexed so the second frame needs to be offset to not get a frame that doesn't exist
        .map(|x| x - 1)
        .skip(1),
    )
    .enumerate()
    .map(|(index, (start, end))| Chunk { index, start, end })
    .collect();

  chunk_boundaries.sort_unstable_by_key(|chunk| Reverse(chunk.end - chunk.start));

  chunk_boundaries
}
