use av1an_core::chunk;
use av1an_core::{
  chunk::create_video_queue_vs, scenedetect, ChunkMethod, ConcatMethod, Encoder, SplitMethod,
};
use clap::AppSettings::ColoredHelp;
use clap::Clap;
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::process::{Command, Stdio};
use std::sync::mpsc::Receiver;
use std::thread;
use thiserror::Error;

use dialoguer::Confirm;

use dialoguer::theme::ColorfulTheme;
use log::{error, info, warn};
use static_init::dynamic;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

// TODO refactor to having InputType
#[derive(Debug)]
pub enum InputType {
  /// Vapoursynth (.vpy, .py) input source
  Vapoursynth,
  /// Normal video input in a container
  Video,
}

#[derive(Debug)]
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
#[derive(Clap, Debug)]
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
  video_params: Option<String>,

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

#[dynamic]
static BAR: ProgressBar = ProgressBar::hidden();

use std::io;
use std::string::FromUtf8Error;
use std::sync::mpsc::channel;

#[derive(Debug)]
enum PipeError {
  IO(io::Error),
  NotUtf8(FromUtf8Error),
}

#[derive(Debug)]
enum PipedLine {
  Line(String),
  EOF,
}

// Reads data from the pipe byte-by-byte and returns the lines.
// Useful for processing the pipe's output as soon as it becomes available.
struct PipeStreamReader {
  lines: Receiver<Result<PipedLine, PipeError>>,
}

impl PipeStreamReader {
  // Starts a background task reading bytes from the pipe.
  fn new(mut stream: Box<dyn io::Read + Send>) -> PipeStreamReader {
    PipeStreamReader {
      lines: {
        let (tx, rx) = channel();

        thread::spawn(move || {
          let mut buf = Vec::new();
          let mut byte = [0u8];
          loop {
            match stream.read(&mut byte) {
              Ok(0) => {
                let _ = tx.send(Ok(PipedLine::EOF));
                break;
              }
              Ok(_) => {
                if byte[0] == b'\r' {
                  tx.send(match String::from_utf8(buf.clone()) {
                    Ok(line) => Ok(PipedLine::Line(line)),
                    Err(err) => Err(PipeError::NotUtf8(err)),
                  })
                  .unwrap();
                  buf.clear()
                } else {
                  buf.push(byte[0])
                }
              }
              Err(error) => {
                tx.send(Err(PipeError::IO(error))).unwrap();
              }
            }
          }
        });

        rx
      },
    }
  }
}

pub fn main() -> anyhow::Result<()> {
  let args: Args = Args::parse();

  validate_args(&args)?;

  // This is safe because the spawned threads cannot actually surpass the lifetime of the main function,
  // so this reference is aways valid
  let input = unsafe { std::mem::transmute::<_, &'static Path>(args.input.path.as_path()) };

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

  let total_frames = av1an_core::vapoursynth::get_num_frames(input).unwrap() as u64;

  BAR.set_length(total_frames);
  BAR.set_style(
    ProgressStyle::default_bar()
      .template("[{eta_precise}] {prefix:.bold}▕{bar:60.blue}▏{msg} ({pos}/{len}) {per_sec}")
      .progress_chars("█▉▊▋▌▍▎▏  "),
  );
  BAR.set_draw_target(ProgressDrawTarget::stdout());
  BAR.set_prefix("Scene detection");

  let scene_changes = scenedetect::get_scene_changes(
    &args.input.path,
    Some(Box::new(|frames, _| {
      BAR.set_position(frames as u64);
      BAR.set_message(format!("{:3}%", 100 * frames as u64 / BAR.length()));
    })),
  )?;

  BAR.finish();

  let splits = create_video_queue_vs(&args.input.path, &scene_changes);
  let (tx, rx) = mpsc::channel();

  // TODO fix threading
  thread::spawn(move || {
    for (chunk_num, (start, end)) in splits {
      let mut first_pass = Command::new("aomenc")
        .args(&[
          "-",
          "--threads=8",
          "-b",
          "10",
          "--cpu-used=6",
          "--end-usage=q",
          "--tile-columns=2",
          "--tile-rows=1",
          "--passes=2",
          "--pass=1",
          format!("--fpf={}_fpf", chunk_num).as_str(),
          "-o",
          "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();

      let mut stdin = first_pass.stdin.take().unwrap();
      let vspipe = thread::spawn(move || {
        av1an_core::vapoursynth::run(input, start, end, &mut stdin).unwrap();
      });

      let stderr = first_pass.stderr.take().unwrap();
      let out = PipeStreamReader::new(Box::new(stderr));

      for line in out.lines {
        match line {
          Ok(PipedLine::Line(ref s)) => {
            if let Some(p) = s
              .split_terminator('/')
              .nth(1)
              .and_then(|x| x.get(x.find("frame").unwrap() + "frame".len()..))
              .and_then(|x| x.split_ascii_whitespace().next())
              .and_then(|x| x.parse::<usize>().ok())
            {
              tx.send((p, chunk_num, 1u8)).unwrap();
            }
          }
          _ => break,
        }
      }

      vspipe.join().unwrap();
      first_pass.wait().unwrap();

      let mut second_pass = Command::new("aomenc")
        .args(&[
          "-",
          "--threads=8",
          "-b",
          "10",
          "--cpu-used=6",
          "--end-usage=q",
          "--tile-columns=2",
          "--tile-rows=1",
          "--passes=2",
          "--pass=2",
          format!("--fpf={}_fpf", chunk_num).as_str(),
          "-o",
          format!("{}.ivf", chunk_num).as_str(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();

      let mut stdin = second_pass.stdin.take().unwrap();

      let vspipe = thread::spawn(move || {
        av1an_core::vapoursynth::run(input, start, end, &mut stdin).unwrap();
      });

      let stderr = second_pass.stderr.take().unwrap();
      let out = PipeStreamReader::new(Box::new(stderr));

      for line in out.lines {
        match line {
          Ok(PipedLine::Line(ref s)) => {
            // dbg!(s);
            if let Some(p) = s
              .split_terminator('/')
              .nth(1)
              .and_then(|x| x.get(x.find("frame").unwrap() + "frame".len()..))
              .and_then(|x| x.split_ascii_whitespace().next())
              .and_then(|x| x.parse::<usize>().ok())
            {
              tx.send((p, chunk_num, 2u8)).unwrap();
            }
          }
          _ => break,
        }
      }

      vspipe.join().unwrap();
      second_pass.wait().unwrap();
    }
  });

  let mut first_pass_progress: HashMap<usize, usize> = HashMap::new();
  let mut second_pass_progress: HashMap<usize, usize> = HashMap::new();

  let m = MultiProgress::new();

  let fp = m.add(ProgressBar::new(total_frames as u64));
  fp.set_style(
    ProgressStyle::default_bar()
      .template("[{eta_precise}] {prefix:.bold}▕{bar:60.red}▏{msg} ({pos}/{len}) {per_sec}")
      .progress_chars("█▉▊▋▌▍▎▏  "),
  );
  fp.set_prefix("First pass     ");

  let sp = m.add(ProgressBar::new(total_frames as u64));
  sp.set_style(
    ProgressStyle::default_bar()
      .template("[{eta_precise}] {prefix:.bold}▕{bar:60.magenta}▏{msg} ({pos}/{len}) {per_sec}")
      .progress_chars("█▉▊▋▌▍▎▏  "),
  );
  sp.set_prefix("Second pass    ");

  while let Ok((frames, chunk_num, pass)) = rx.recv() {
    if pass == 1 {
      first_pass_progress.insert(chunk_num, frames);
      let position = first_pass_progress
        .iter()
        .map(|(_, frames)| *frames)
        .sum::<usize>() as u64;
      fp.set_position(position);
      fp.set_message(format!("{:3}%", 100 * position / total_frames));
    } else {
      second_pass_progress.insert(chunk_num, frames);
      let position = second_pass_progress
        .iter()
        .map(|(_, frames)| *frames)
        .sum::<usize>() as u64;
      sp.set_position(position);
      sp.set_message(format!("{:3}%", 100 * position / total_frames));
    }
  }

  fp.finish();
  sp.finish();

  Ok(())
}
