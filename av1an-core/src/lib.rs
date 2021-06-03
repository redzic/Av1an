#![feature(trait_alias)]

#[macro_use]
extern crate log;

use std::path::Path;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use strum_macros::EnumString;

pub mod chunk;
pub mod concat;
pub mod encoder;
pub mod ffmpeg;
pub mod scenedetect;
pub mod split;
pub mod target_quality;
pub mod vapoursynth;

#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, Serialize, Deserialize)]
pub enum Encoder {
  aom,
  rav1e,
  libvpx,
  #[strum(serialize = "svt-av1")]
  svt_av1,
  #[strum(serialize = "svt-vp9")]
  svt_vp9,
  x264,
  x265,
}

#[derive(Debug, PartialEq, Eq, EnumString, Serialize, Deserialize)]
pub enum ConcatMethod {
  /// MKVToolNix
  #[strum(serialize = "mkvmerge")]
  MKVMerge,
  /// FFmpeg
  #[strum(serialize = "ffmpeg")]
  FFmpeg,
  /// Concatenate to ivf
  #[strum(serialize = "ivf")]
  Ivf,
}

#[derive(Debug, EnumString, Serialize, Deserialize)]
pub enum SplitMethod {
  PySceneDetect,
  AOMKeyframes,
  FFmpeg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, Serialize, Deserialize)]
pub enum ChunkMethod {
  #[strum(serialize = "select")]
  Select,
  #[strum(serialize = "ffms2")]
  FFMS2,
  #[strum(serialize = "lsmash")]
  LSMASH,
  #[strum(serialize = "hybrid")]
  Hybrid,
}

pub fn hash_path(path: &Path) -> PathBuf {
  let mut s = DefaultHasher::new();
  path.hash(&mut s);
  PathBuf::from(&format!(".{}", s.finish())[..8])
}

/// Check for FFmpeg
pub fn get_ffmpeg_info() -> String {
  let mut cmd = Command::new("ffmpeg");
  cmd.stderr(Stdio::piped());
  String::from_utf8(cmd.output().unwrap().stderr).unwrap()
}

pub fn adapt_probing_rate(rate: usize) -> usize {
  match rate {
    1..=4 => rate,
    _ => 4,
  }
}

/// Determine the optimal number of workers for an encoder
pub fn determine_workers(encoder: Encoder) -> u64 {
  let cpus = num_cpus::get() as u64;
  // get_total_memory returns bytes, convert to gb
  let ram_gb = psutil::memory::virtual_memory().unwrap().total() / 10u64.pow(9);

  std::cmp::max(
    match encoder {
      Encoder::aom | Encoder::rav1e | Encoder::libvpx => std::cmp::min(
        (cpus as f64 / 3.0).round() as u64,
        (ram_gb as f64 / 1.5).round() as u64,
      ),
      Encoder::svt_av1 | Encoder::svt_vp9 | Encoder::x264 | Encoder::x265 => {
        std::cmp::min(cpus, ram_gb) / 8
      }
    },
    1,
  )
}
