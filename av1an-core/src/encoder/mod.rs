#[derive(Copy, Clone, Debug)]
pub struct PassProgress {
  pub chunk_index: usize,
  pub frames_encoded: usize,
}

pub mod aom;
pub mod generic;
