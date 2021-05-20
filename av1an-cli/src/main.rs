// (Full example with detailed comments in examples/01d_quick_example.rs)
//
// This example demonstrates clap's full 'custom derive' style of creating arguments which is the
// simplest method of use, but sacrifices some flexibility.
use av1an_core::{
  chunk::create_video_queue_vs, scenedetect, vapoursynth, ChunkMethod, ConcatMethod, Encoder,
  SplitMethod,
};
use clap::AppSettings::ColoredHelp;
use clap::Clap;
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::ffi::OsStr;
use thiserror::Error;

use dialoguer::Confirm;

use dialoguer::theme::ColorfulTheme;
use log::{error, info, warn};
use static_init::dynamic;
use std::path::{Path, PathBuf};

// TODO refactor to having InputType
#[derive(Debug)]
pub enum Input {
  /// Vapoursynth (.vpy, .py) input source
  Vapoursynth(PathBuf),
  /// Normal video input in a container
  Video(PathBuf),
}

impl Input {
  pub fn is_vapoursynth(&self) -> bool {
    matches!(&self, Input::Vapoursynth(_))
  }

  pub fn as_path(&self) -> &Path {
    match &self {
      Self::Video(p) => p.as_path(),
      Self::Vapoursynth(p) => p.as_path(),
    }
  }
}

impl From<&OsStr> for Input {
  fn from(s: &OsStr) -> Self {
    let path = PathBuf::from(s);

    if let Some(ext) = path.extension() {
      if ext == "py" || ext == "vpy" {
        Self::Vapoursynth(path)
      } else {
        Self::Video(path)
      }
    } else {
      Self::Video(path)
    }
  }
}

/// Cross-platform command-line AV1 / VP9 / HEVC / H264 / VVC encoding framework with per scene quality encoding
#[derive(Clap, Debug)]
#[clap(name = "av1an", setting = ColoredHelp, version)]
pub struct Args {
  /// Input file or vapoursynth (.py, .vpy) script
  #[clap(short, long, parse(from_os_str))]
  input: Input,

  /// Temporary folder to use
  #[clap(long, parse(from_os_str))]
  temp: Option<PathBuf>,

  /// Specify output file
  #[clap(short, long, parse(from_os_str))]
  output: PathBuf,

  /// Concatenation method to use for splits
  #[clap(short, long, possible_values = &["ffmpeg", "mkvmerge", "ivf"], default_value = "ffmpeg")]
  concat: ConcatMethod,

  /// Disable printing progress to terminal
  #[clap(short, long)]
  quiet: bool,

  /// Enable logging
  #[clap(short = 'L', long)]
  log: Option<String>,

  /// Resume previous session
  #[clap(short, long)]
  resume: bool,

  /// Keep temporary folder after encode
  #[clap(short, long)]
  keep: bool,

  /// Path to config file (creates if it does not exist)
  #[clap(short = 'C', long, parse(from_os_str))]
  config: Option<PathBuf>,

  /// Output to webm
  #[clap(long)]
  webm: bool,

  /// Method for creating chunks
  #[clap(short = 'm', long, default_value = "hybrid")]
  chunk_method: ChunkMethod,

  /// File location for scenes
  #[clap(short, long, parse(from_os_str))]
  scenes: Option<PathBuf>,

  /// Specify splitting method
  #[clap(long)]
  split_method: Option<SplitMethod>,

  /// Number of frames after which make split
  #[clap(short = 'x', long, default_value = "240")]
  extra_split: usize,

  /// PySceneDetect Threshold
  #[clap(long, default_value = "35.0")]
  threshold: f64,

  /// Minimum number of frames in a split
  #[clap(long, default_value = "60")]
  min_scene_len: usize,

  /// Reuse the first pass from aom_keyframes split on the chunks
  #[clap(long)]
  reuse_first_pass: bool,

  /// Specify encoding passes
  #[clap(short, long)]
  passes: Option<u8>,

  /// Parameters passed to the encoder
  #[clap(short, long)]
  // video_params: Option<String>,
  video_params: Option<Vec<String>>,

  #[clap(short, long, arg_enum, default_value = "aom")]
  encoder: Encoder,

  /// Number of workers
  #[clap(short, long, default_value = "0")]
  workers: usize,

  /// Do not check encodings
  #[clap(short, long)]
  no_check: bool,

  /// Force encoding if input args seen as invalid
  #[clap(long)]
  force: bool,

  /// FFmpeg commands
  #[clap(short = 'F', long)]
  ffmpeg: Option<String>,

  /// FFmpeg audio parameters
  #[clap(short, long, default_value = "-c:a copy")]
  audio_params: String,

  /// FFmpeg pixel format
  #[clap(long, default_value = "yuv420p10le")]
  pix_fmt: String,

  /// Calculate VMAF after encode
  #[clap(long)]
  vmaf: bool,

  /// Path to VMAF models
  #[clap(long, parse(from_os_str))]
  vmaf_path: Option<PathBuf>,

  /// Resolution used in VMAF calculation
  #[clap(long, default_value = "1920x1080")]
  vmaf_res: String,

  /// Threads for VMAF calculation
  #[clap(long)]
  n_threads: Option<usize>,

  /// Value to target
  #[clap(short, long)]
  target_quality: Option<f64>,

  /// Method selection for target quality
  #[clap(long, possible_values = &["per_frame", "per_shot"])]
  target_quality_method: Option<String>,

  /// Number of probes to make for target_quality
  #[clap(long, default_value = "4")]
  probes: usize,

  /// Min q for target_quality
  #[clap(long)]
  min_q: Option<u8>,

  /// Max q for target_quality
  #[clap(long)]
  max_q: Option<u8>,

  /// Make plots of probes in temp folder
  #[clap(long)]
  vmaf_plots: bool,

  /// Framerate for probes, 0 - original
  #[clap(long, default_value = "4")]
  probing_rate: usize,

  /// Filter applied to source at vmaf calcualation, use if you crop source
  #[clap(long)]
  vmaf_filter: Option<String>,
}

#[derive(Error, Debug)]
pub enum CliError {
  #[error("ivf concatenation can only be used with VP9 and AV1")]
  InvalidConcatMethod,
}

pub fn validate_args(args: &Args) -> Result<(), CliError> {
  if !matches!(
    args.encoder,
    Encoder::aom | Encoder::rav1e | Encoder::svt_av1 | Encoder::svt_vp9 | Encoder::libvpx
  ) && args.concat == ConcatMethod::Ivf
  {
    return Err(CliError::InvalidConcatMethod);
  }

  Ok(())
}

#[dynamic]
static BAR: ProgressBar = ProgressBar::hidden();

pub fn main() -> anyhow::Result<()> {
  let args: Args = Args::parse();

  validate_args(&args)?;

  pretty_env_logger::init_custom_env("AV1AN_LOG");

  if args.output.exists() {
    if Confirm::with_theme(&ColorfulTheme::default())
      .with_prompt(format!("Output file {:?} exists. Overwrite?", &args.output))
      .interact()?
    {
      info!("Overwriting output");
    } else {
      error!("Output file already exists");
      return Ok(());
    }
  }

  if args.probing_rate < 4 {
    warn!("probing rate < 4 is currently experimental");
  }

  let total_frames = av1an_core::vapoursynth::get_num_frames(args.input.as_path()).unwrap() as u64;
  BAR.set_length(total_frames);
  BAR.set_draw_target(ProgressDrawTarget::stderr());
  BAR.set_style(
    ProgressStyle::default_bar()
      .template("[{eta_precise}] {prefix:.bold}▕{bar:60.green}▏{msg} ({pos}/{len}) {per_sec}")
      .progress_chars("█▉▊▋▌▍▎▏  "),
  );
  let scene_changes = scenedetect::get_scene_changes(
    &args.input.as_path(),
    Some(Box::new(|frames, _| {
      BAR.set_position(frames as u64);
      BAR.set_message(format!("{:3}%", 100 * frames as u64 / BAR.length()));
    })),
  )?;

  BAR.finish();

  dbg!(create_video_queue_vs(&args.input.as_path(), &scene_changes));

  Ok(())
}
