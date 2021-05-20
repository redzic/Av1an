use std::cmp::Reverse;
use std::iter;
use std::path::Path;

use crate::vapoursynth;

pub fn create_vs_chunk(
  temp: &Path,
  vs_script: &Path,
  index: usize,
  frame_start: usize,
  mut frame_end: usize,
) {
  assert!(frame_end > frame_start);

  let frames = frame_end - frame_start;
  frame_end -= 1;
}

pub fn create_video_queue_vs(input: &Path, split_locations: &[usize]) -> Vec<(usize, usize)> {
  let last_frame = vapoursynth::get_num_frames(input).unwrap();

  let mut chunk_boundaries: Vec<(usize, usize)> = split_locations
    .iter()
    .chain(iter::once(&last_frame))
    .zip(
      split_locations
        .iter()
        .chain(iter::once(&last_frame))
        .skip(1),
    )
    // TODO move this so that each element is already not a reference?
    .map(|(x, y)| (*x, *y))
    .collect();

  chunk_boundaries.sort_unstable_by_key(|(start, end)| Reverse(end - start));

  chunk_boundaries
}
