#![feature(core_intrinsics)]

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Instant;

use av1an_core::chunk::create_video_queue_vs;
use av1an_core::encoder::generic::EncodeChunk;
use av1an_core::encoder::{self, Chunk, TwoPassProgress};
use av1an_core::{scenedetect, ChunkMethod, ConcatMethod, Encoder, SplitMethod};

use clap::AppSettings::ColoredHelp;
use clap::Clap;
use dialoguer::theme::ColorfulTheme;
use dialoguer::Confirm;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};

use circular_queue::CircularQueue;
use log::error;
use rayon::prelude::*;
use thiserror::Error;

#[derive(Debug, Serialize, Deserialize)]
pub enum InputType {
  /// Vapoursynth (.vpy, .py) input source
  Vapoursynth,
  /// Video input in file in a container (ex: .mkv, .mp4)
  Video,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Input {
  path: PathBuf,
  r#type: InputType,
}

impl From<&OsStr> for Input {
  fn from(s: &OsStr) -> Self {
    Input {
      path: PathBuf::from(s),
      r#type: if let Some(ext) = Path::new(s).extension() {
        if ext == "vpy" || ext == "py" {
          InputType::Vapoursynth
        } else {
          InputType::Video
        }
      } else {
        InputType::Video
      },
    }
  }
}

/// Cross-platform command-line AV1 / VP9 / HEVC / H264 / VVC encoding framework with per scene quality encoding
#[derive(Clap, Debug, Serialize, Deserialize)]
#[clap(name = "av1an", setting = ColoredHelp, version)]
pub struct Args {
  /// Input file or vapoursynth (.py, .vpy) script
  #[clap(short, long, parse(from_os_str))]
  input: Input,

  /// Temporary directory to use
  #[clap(long, parse(from_os_str))]
  temp_dir: Option<PathBuf>,

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
  #[clap(long)]
  keep: bool,

  /// Path to config file (creates if it does not exist)
  #[clap(short = 'C', long, parse(from_os_str))]
  config: Option<PathBuf>,

  /// Output to webm
  #[clap(long)]
  webm: bool,

  /// Method for creating chunks
  #[clap(short = 'm', long, default_value="ffms2", possible_values=&["select", "ffms2", "lsmash", "hybrid"])]
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
  video_params: Option<String>,

  #[clap(short, long, default_value = "aom", possible_values=&["aom", "rav1e", "libvpx", "svt-av1", "svt-vp9", "x264", "x265"])]
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

  /// Number of threads to use for VMAF calculation
  #[clap(long)]
  vmaf_threads: Option<usize>,

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

/// Unicode characters used to create a "finer" or smoother-looking progress bar.
const FINE_PROGRESS_CHARS: &str = "█▉▊▋▌▍▎▏  ";

pub fn main() -> anyhow::Result<()> {
  let args: Args = Args::parse();

  validate_args(&args)?;

  println!("{}", serde_json::to_string_pretty(&args).unwrap());

  return Ok(());

  // SAFETY: The spawned threads cannot exceed the lifetime of the main thread, because they always
  // terminate before the main function, because they are always `.join()`-ed before this function
  // returns. Therefore, it is safe for the spawned threads to reference this function as if it had
  // a 'static lifetime.
  //
  // TODO: Use scoped threads instead.
  let input = unsafe { std::mem::transmute::<_, &'static Path>(args.input.path.as_path()) };

  let temp_dir = args
    .temp_dir
    .unwrap_or_else(|| av1an_core::hash_path(input));

  let encode_dir = temp_dir.join("encode");
  let splits_dir = temp_dir.join("split");

  let _ = fs::create_dir(&temp_dir);
  let _ = fs::create_dir(&encode_dir);
  let _ = fs::create_dir(&splits_dir);

  let vsinput = av1an_core::vapoursynth::create_vapoursynth_source_script(
    &splits_dir,
    input,
    args.chunk_method,
  )?;
  let vsinput = unsafe { std::mem::transmute::<_, &'static Path>(vsinput.as_path()) };

  pretty_env_logger::init_custom_env("AV1AN_LOG");

  if args.output.exists() {
    if Confirm::with_theme(&ColorfulTheme::default())
      .with_prompt(format!("Output file {:?} exists. Overwrite?", &args.output))
      .interact()?
    {
      println!("Overwriting output");
    } else {
      println!("Output file already exists");
      return Ok(());
    }
  }

  if args.probing_rate < 4 {
    println!("probing rate < 4 is currently experimental");
  }

  let total_frames = av1an_core::vapoursynth::get_num_frames(&vsinput).unwrap() as u64;

  // TODO use formula instead
  let total_frames_width = total_frames.to_string().len();

  let bar = ProgressBar::new(total_frames);
  let (sender, receiver) = mpsc::channel();

  bar.set_style(
    ProgressStyle::default_bar()
      .template(
        format!(
          "{{prefix:.bold}}▕{{bar:60.blue}}▏{{msg}}\t{{pos:>{}}}/{{len}}\teta {{eta}}",
          total_frames_width
        )
        .as_str(),
      )
      .progress_chars(FINE_PROGRESS_CHARS),
  );
  bar.set_prefix("Scene detection");

  let encode_start_time = Instant::now();

  let scene_changes = thread::spawn(move || {
    scenedetect::get_scene_changes(
      &vsinput,
      Some(Box::new(move |frames, _| {
        sender.send(frames as u64).unwrap();
      })),
    )
    .unwrap()
  });

  while let Ok(frames) = receiver.recv() {
    bar.set_position(frames as u64);
    let fps = frames as f64 / encode_start_time.elapsed().as_secs_f64();
    bar.set_message(format!(
      "{:3}%\t{:>6.2} fps",
      100 * frames as u64 / total_frames,
      fps
    ));
  }

  bar.finish();

  let scene_changes = scene_changes.join().unwrap();
  let splits = create_video_queue_vs(vsinput, &scene_changes);
  let splits = unsafe { std::mem::transmute::<_, &'static [Chunk]>(splits.as_slice()) };

  // The CLI handles the configuration of rayon
  rayon::ThreadPoolBuilder::new()
    .num_threads(8)
    .build_global()?;

  let (tx, rx): (Sender<TwoPassProgress>, _) = mpsc::channel();

  let mut aomenc = encoder::aom::Aom::new(
    240,
    splits.len(),
    tx,
    &["--cpu-used=6", "--cq-level=30", "--end-usage=q"],
  );

  let t = thread::spawn(move || {
    aomenc.0.encode_chunk(
      (Path::new("fpf0.log"), Path::new("out.ivf")),
      av1an_core::vapoursynth::pipe,
      Chunk {
        index: 0,
        start: 0,
        end: 50,
      },
    );
  });

  // let mut progress: [(Box<[usize]>, usize); 2] = [
  //   // First pass progress information
  //   (vec![0; splits.len()].into_boxed_slice(), 0),
  //   // Second pass progress information
  //   (vec![0; splits.len()].into_boxed_slice(), 0),
  // ];

  // let progress_bars = MultiProgress::new();

  // let bars = [
  //   progress_bars.add(ProgressBar::new(total_frames)),
  //   progress_bars.add(ProgressBar::new(total_frames)),
  // ];

  // bars[0].set_style(
  //   ProgressStyle::default_bar()
  //     .template(
  //       format!(
  //         "{{prefix:.bold}}▕{{bar:60.green}}▏{{msg}}\t{{pos:>{}}}/{{len}}\teta {{eta}}",
  //         total_frames_width
  //       )
  //       .as_str(),
  //     )
  //     .progress_chars(FINE_PROGRESS_CHARS),
  // );
  // bars[0].set_prefix("     First pass");

  // bars[1].set_style(
  //   ProgressStyle::default_bar()
  //     .template(
  //       format!(
  //         "{{prefix:.bold}}▕{{bar:60.magenta}}▏{{msg}}\t{{pos:>{}}}/{{len}}\teta {{eta}}",
  //         total_frames_width
  //       )
  //       .as_str(),
  //     )
  //     .progress_chars(FINE_PROGRESS_CHARS),
  // );
  // bars[1].set_prefix("    Second pass");

  // for bar in &bars {
  //   bar.set_position(0);
  //   bar.set_message("  0%\t  0.00 fps");
  // }

  // let mut timers: [Instant; 2] = [Instant::now(); 2];

  while let Ok(progress) = rx.recv() {
    dbg!(progress);
  }

  t.join().unwrap();

  // while let Ok(p) = rx.recv() {
  //   let (frames, chunk_index, pass) = match p {
  //     TwoPassProgress::FirstPass(p) => (p.frames_encoded, p.chunk_index, 0),
  //     TwoPassProgress::SecondPass(p) => (p.frames_encoded, p.chunk_index, 1),
  //   };

  //   // We can continuously sum up the total number of frames encoded instead of recalculating
  //   // the sum of the encoded frames per chunk every time this loop body is entered, which
  //   // saves a few CPU cycles.
  //   unsafe {
  //     // SAFETY: `pass` will always be 0 or 1, so it can never be out of bounds since
  //     // `progress` will always have 2 elements.
  //     let (progress, sum) = progress.get_unchecked_mut(pass);

  //     // SAFETY: `chunk_index` will always be in range of `progress`, because the initialization
  //     // of progress and the value of `chunk_index` are both based on the length of the splits.
  //     //
  //     // However, this calculation can cause integer overflow if `frames < progress[chunk_index]`.
  //     // This should not be possible, however.

  //     // how many frames were newly encoded
  //     let diff = frames - progress.get_unchecked(chunk_index);
  //     *sum += diff;

  //     // SAFETY: Indexing by `chunk_index` is safe for the same reasons explained above.
  //     *progress.get_unchecked_mut(chunk_index) = frames;

  //     let new_instant = Instant::now();
  //     frame_times
  //       .get_unchecked_mut(pass)
  //       .push((new_instant - *timers.get_unchecked(pass)).as_secs_f32());
  //     *timers.get_unchecked_mut(pass) = new_instant;

  //     if diff > 0 {
  //       let fps = frame_times.get_unchecked(pass).len() as f32
  //         / frame_times
  //           .get_unchecked(pass)
  //           .iter()
  //           // Allow LLVM to unroll the loop. This is equivalent to using -Ofast because
  //           // it allows the compiler to accumulate the sum in multiple registers depending
  //           // on the CPU architecture, which changes the semantics of the additions for
  //           // floating point numbers, but in this case it doesn't matter since we are only
  //           // using 2 decimal places anyway, and great precision is not required.
  //           .fold(0f32, |sum, val| std::intrinsics::fadd_fast(sum, *val));

  //       bars.get_unchecked(pass).set_position(*sum as u64);
  //       bars.get_unchecked(pass).set_message(format!(
  //         "{:3}%\t{:>6.2} fps",
  //         100 * *sum as u64 / total_frames,
  //         fps
  //       ));
  //     }
  //   }
  // }

  // chunk_queue.join().unwrap();

  // for bar in &bars {
  //   bar.finish();
  // }

  // println!("Concatenating...");

  println!("Took {:.2?}", encode_start_time.elapsed());

  Ok(())
}
