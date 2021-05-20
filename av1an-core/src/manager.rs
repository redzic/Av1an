use std::path::{Path, PathBuf};

pub struct WorkerPool {
  queue: Vec<PathBuf>,
}

pub fn process_inputs<P: AsRef<Path>>(inputs: &[P]) {}

impl WorkerPool {
  pub fn new() -> Self {
    todo!()
  }
}
