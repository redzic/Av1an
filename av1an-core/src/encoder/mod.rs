#[derive(Copy, Clone, Debug)]
pub struct Chunk {
  /// Index of the chunk
  pub index: usize,
  /// Starting frame (inclusive)
  pub start: usize,
  /// Ending frame (inclusive)
  pub end: usize,
}

#[derive(Copy, Clone, Debug)]
pub struct PassProgress {
  pub chunk_index: usize,
  pub frames_encoded: usize,
}

pub mod aom;
pub mod generic;
