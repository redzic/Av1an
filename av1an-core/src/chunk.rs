use std::cmp::Reverse;
use std::iter;
use std::path::Path;

use crate::encoder::Chunk;
use crate::vapoursynth;

// TODO refactor this into general frame splits -> Chunk conversion function
#[must_use]
pub fn create_video_queue_vs(input: &Path, split_locations: &[usize]) -> Vec<Chunk> {
  let last_frame = vapoursynth::get_num_frames(input).unwrap();

  let mut chunk_boundaries: Vec<Chunk> = split_locations
    .iter()
    .copied()
    .chain(iter::once(last_frame))
    .zip(
      split_locations
        .iter()
        .copied()
        .chain(iter::once(last_frame))
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
