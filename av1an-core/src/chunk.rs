use std::cmp::Reverse;
use std::iter;
use std::path::Path;

use crate::vapoursynth;

#[must_use]
pub fn create_video_queue_vs(
  input: &Path,
  split_locations: &[usize],
) -> Vec<(usize, (usize, usize))> {
  let last_frame = vapoursynth::get_num_frames(input).unwrap();

  // (index, (start, end))
  let mut chunk_boundaries: Vec<(usize, (usize, usize))> = split_locations
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
    .collect();

  chunk_boundaries.sort_unstable_by_key(|(_, (start, end))| Reverse(end - start));

  chunk_boundaries
}
