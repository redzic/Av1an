#[macro_use]
extern crate log;
extern crate av_format;
extern crate av_ivf;
extern crate failure;

use std::path::Path;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::str::FromStr;
use sysinfo::SystemExt;
use thiserror::Error;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

pub mod chunk;
pub mod concat;
pub mod encoder;
pub mod ffmpeg;
pub mod scenedetect;
pub mod split;
pub mod target_quality;
pub mod vapoursynth;

#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoder {
  aom,
  rav1e,
  libvpx,
  svt_av1,
  svt_vp9,
  x264,
  x265,
}

#[derive(Debug, Error)]
pub enum EncoderParseError {
  #[error("Invalid or unknown encoder")]
  InvalidEncoder,
}

impl FromStr for Encoder {
  type Err = EncoderParseError;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    // set to match usage in python code
    match s {
      "aom" => Ok(Self::aom),
      "rav1e" => Ok(Self::rav1e),
      "vpx" => Ok(Self::libvpx),
      "svt-av1" => Ok(Self::svt_av1),
      "svt-vp9" => Ok(Self::svt_vp9),
      "x264" => Ok(Self::x264),
      "x265" => Ok(Self::x265),
      _ => Err(EncoderParseError::InvalidEncoder),
    }
  }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ConcatMethod {
  /// MKVToolNix
  MKVMerge,
  /// FFmpeg
  FFmpeg,
  /// Concatenate to ivf
  Ivf,
}

#[derive(Debug, Error)]
pub enum ConcatMethodParseError {
  #[error("Invalid concatenation method")]
  InvalidMethod,
}

impl FromStr for ConcatMethod {
  type Err = ConcatMethodParseError;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "ffmpeg" => Ok(Self::FFmpeg),
      "mkvmerge" => Ok(Self::MKVMerge),
      "ivf" => Ok(Self::Ivf),
      _ => Err(ConcatMethodParseError::InvalidMethod),
    }
  }
}

#[derive(Debug)]
pub enum SplitMethod {
  PySceneDetect,
  AOMKeyframes,
  FFmpeg,
}

#[derive(Debug, Error)]
pub enum SplitMethodParseError {
  #[error("Invalid split method")]
  InvalidMethod,
}

impl FromStr for SplitMethod {
  type Err = SplitMethodParseError;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "ffmpeg" => Ok(Self::FFmpeg),
      "pyscene" => Ok(Self::PySceneDetect),
      "aom_keyframes" => Ok(Self::AOMKeyframes),
      _ => Err(SplitMethodParseError::InvalidMethod),
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkMethod {
  Select,
  FFMS2,
  LSMASH,
  Hybrid,
}

#[derive(Debug, Error)]
pub enum ChunkMethodParseError {
  #[error("Invalid chunk method")]
  InvalidMethod,
}

impl FromStr for ChunkMethod {
  type Err = ChunkMethodParseError;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    // set to match usage in python code
    match s {
      "vs_ffms2" => Ok(Self::FFMS2),
      "vs_lsmash" => Ok(Self::LSMASH),
      "hybrid" => Ok(Self::Hybrid),
      "select" => Ok(Self::Select),
      _ => Err(ChunkMethodParseError::InvalidMethod),
    }
  }
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
  // TODO look for lighter weight solution? sys-info maybe?
  let mut system = sysinfo::System::new();
  system.refresh_memory();

  let cpu = num_cpus::get() as u64;
  // get_total_memory returns kb, convert to gb
  let ram_gb = system.get_total_memory() / 10u64.pow(6);

  std::cmp::max(
    match encoder {
      Encoder::aom | Encoder::rav1e | Encoder::libvpx => std::cmp::min(
        (cpu as f64 / 3.0).round() as u64,
        (ram_gb as f64 / 1.5).round() as u64,
      ),
      Encoder::svt_av1 | Encoder::svt_vp9 | Encoder::x264 | Encoder::x265 => {
        std::cmp::min(cpu, ram_gb) / 8
      }
    },
    1,
  )
}
