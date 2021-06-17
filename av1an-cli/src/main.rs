use std::ffi::OsStr;
use std::fs;
use std::fs::remove_dir_all;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Instant;

use av1an::chunk::create_video_queue_vs;
use av1an::encoder;
use av1an::encoder::generic::ParallelEncode;
use av1an::encoder::PassProgress;
use av1an::hash_path;
use av1an::vapoursynth;
use av1an::{scenedetect, ChunkMethod, ConcatMethod, Encoder};

use clap::AppSettings;
use clap::{App, Arg};
use dialoguer::Confirm;
use indicatif::{ProgressBar, ProgressStyle};
use slog::{error, info, o};
use slog::{Drain, Logger};
use thiserror::Error;

#[derive(Debug)]
pub enum InputType {
  /// Vapoursynth (.vpy, .py) input source
  Vapoursynth,
  /// Video input in file in a container (ex: .mkv, .mp4)
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

#[derive(Debug)]
pub struct CliOptions {
  pub input: Input,
  pub output: PathBuf,
  pub keep: bool,
  pub temp_dir: PathBuf,
  pub encoder: Encoder,
  pub video_params: Vec<String>,
  pub audio_params: Vec<String>,
  pub workers: usize,
  pub concat_method: ConcatMethod,
  pub chunk_method: ChunkMethod,
}

#[derive(Error, Debug)]
pub enum CliError<'a> {
  #[error("ivf concatenation can only be used with VP9 and AV1")]
  InvalidConcatMethod,

  #[error("Failed to parse parameter `{arg}`: {msg}")]
  ParseError { arg: &'a str, msg: String },
}

pub fn parse_cli<'a>() -> std::result::Result<CliOptions, CliError<'a>> {
  let ver_short = format!(
    "git {} ({})",
    env!("VERGEN_GIT_BRANCH"),
    env!("VERGEN_CARGO_PROFILE")
  );
  let ver_long = format!(
    "experimental {} branch ({})

   Target:  {} ({})
Toolchain:  rustc {} (LLVM version {})
 Features:  {}",
    env!("VERGEN_GIT_BRANCH"),
    env!("VERGEN_GIT_SHA_SHORT"),
    env!("VERGEN_CARGO_TARGET_TRIPLE"),
    env!("VERGEN_SYSINFO_OS_VERSION"),
    env!("VERGEN_RUSTC_SEMVER"),
    env!("VERGEN_RUSTC_LLVM_VERSION"),
    env!("VERGEN_CARGO_FEATURES"),
  );

  let app = App::new("av1an")
    .version(ver_short.as_str())
    .long_version(ver_long.as_str())
    .about("Cross-platform command-line AV1 / VP9 / HEVC / H264 encoding framework with per-scene quality encoding.")
    .usage("\
    av1an [OPTIONS] -i <INPUT> -o <OUTPUT>
    av1an [OPTIONS] -v [VIDEO_PARAMS ...] -- -i <INPUT> -o <OUTPUT>")
    .setting(AppSettings::ColoredHelp)
    .setting(AppSettings::DeriveDisplayOrder)
    .arg(
      Arg::with_name("INPUT")
        .help("Input file or vapoursynth (.py, .vpy) script")
        .required(true)
        .short("i")
        .takes_value(true)
    )
    .arg(
      Arg::with_name("OUTPUT")
        .help("Output file")
        .required(true)
        .short("o")
        .takes_value(true)
    )
    .arg(
      Arg::with_name("KEEP")
        .help("Do not delete temporary files after encoding")
        .long("keep")
    )
    .arg(
      Arg::with_name("ENCODER")
        .help("Encoder to use")
        .takes_value(true)
        .short("e")
        .long("enc")
        .possible_values(&["aom", "rav1e", "libvpx", "svt-av1", "svt-vp9", "x264", "x265"])
    )
    .arg(
      Arg::with_name("TEMP_DIR")
        .help("Temporary directory to use")
        .long("temp-dir")
        .takes_value(true)
    )
    .arg(
      Arg::with_name("CHUNK_METHOD")
        .help("Method for piping chunks into encoder instances")
        .short("c")
        .long("chunk")
        .takes_value(true)
        .possible_values(&["hybrid", "select", "ffms2", "l-smash"])
    )
    .arg(
      Arg::with_name("WORKERS")
        .help("Set the threadpool size for encoder instances")
        .short("w")
        .long("workers")
        .takes_value(true)
    )
    .arg(
      Arg::with_name("VIDEO_PARAMS")
        .help("Set the video parameters, which are specified directly as arguments to av1an, and are terminated with `--`")
        .short("v")
        .long("video-params")
        .takes_value(true)
        .multiple(true)
        .allow_hyphen_values(true)
        .value_terminator("--")
    )
    .arg(
      Arg::with_name("AUDIO_PARAMS")
        .help("Set the audio parameters for FFmpeg, which are specified directly as arguments to av1an, and are terminated with `--`")
        .short("a")
        .long("audio-params")
        .takes_value(true)
        .multiple(true)
        .allow_hyphen_values(true)
        .value_terminator("--")
    );

  let matches = app.get_matches();
  let input: Input = matches.value_of_os("INPUT").unwrap().into();
  let output: PathBuf = matches.value_of_os("OUTPUT").unwrap().into();

  // TODO should probably warn about video-params being specified if encoder is not
  // specified
  let encoder: Encoder = match matches.value_of("ENCODER") {
    Some(s) => s.parse().unwrap(),
    None => Encoder::aom,
  };

  let temp_dir: PathBuf = matches
    .value_of_os("TEMP_DIR")
    .map(PathBuf::from)
    .unwrap_or_else(|| hash_path(input.path.as_path()));

  let workers: usize = match matches.value_of("WORKERS") {
    Some(s) => s.parse().map_err(|e| CliError::ParseError {
      arg: "--workers",
      msg: format!("{}", e),
    })?,
    None => av1an::determine_workers(encoder),
  };

  let chunk_method = matches
    .value_of("CHUNK_METHOD")
    .and_then(|s| s.parse().ok())
    .unwrap_or(ChunkMethod::Hybrid);

  let video_params: Vec<String> = match matches.values_of("VIDEO_PARAMS") {
    Some(v) => v.into_iter().map(|s| s.to_owned()).collect(),
    None => vec![
      "--threads=8".into(),
      "-b".into(),
      "10".into(),
      "--cpu-used=6".into(),
      "--end-usage=q".into(),
      "--cq-level=30".into(),
      "--tile-columns=2".into(),
      "--tile-rows=1".into(),
    ],
  };

  let audio_params: Vec<String> = match matches.values_of("AUDIO_PARAMS") {
    Some(v) => v.into_iter().map(|s| s.to_owned()).collect(),
    None => vec!["-c:a".into(), "copy".into()],
  };

  Ok(CliOptions {
    input,
    output,
    encoder,
    keep: matches.is_present("KEEP"),
    temp_dir,
    concat_method: ConcatMethod::FFmpeg,
    chunk_method,
    workers,
    video_params,
    audio_params,
  })
}

const INDICATIF_PROGRESS_TEMPLATE: &str = "{spinner:.green} [{elapsed_precise}] [{bar:60.cyan/blue}] {percent:>3.bold}% {pos}/{len} ({fps:.bold}, eta {eta})";

#[inline(always)]
#[allow(clippy::collapsible_if)]
pub fn _main() -> anyhow::Result<()> {
  let args: CliOptions = parse_cli()?;

  println!(
    "workers: {}\ntemp dir: '{}'\nencoder args: {:?}\naudio params: {:?}",
    args.workers,
    &args.temp_dir.display(),
    &args.video_params,
    &args.audio_params
  );

  let log_file = File::create("log.log").unwrap();

  let plain = slog_term::PlainSyncDecorator::new(log_file);
  let log = Logger::root(slog_term::FullFormat::new(plain).build().fuse(), o!());

  let input = args.input.path.as_path();

  let encode_dir = args.temp_dir.join("encode");
  let splits_dir = args.temp_dir.join("split");

  if args.output.exists() {
    if !Confirm::new()
      .with_prompt(format!(
        "Output file '{}' exists. Overwrite?",
        &args.output.display()
      ))
      .interact()?
    {
      println!("Exiting...");
      return Ok(());
    }
  }

  // TODO warn if temp folder is not empty
  let _ = fs::create_dir(&args.temp_dir);
  let _ = fs::create_dir(&encode_dir);
  let _ = fs::create_dir(&splits_dir);

  let vsinput =
    vapoursynth::create_vapoursynth_source_script(&splits_dir, input, args.chunk_method)?;
  let vsinput = vsinput.as_path();

  let downscaled =
    vapoursynth::create_vapoursynth_scenedetect_script(&splits_dir, input, args.chunk_method)?;

  let total_frames = vapoursynth::get_num_frames(vsinput).unwrap() as u64;

  println!("Scene detection");

  let bar = ProgressBar::new(total_frames);
  let (sender, receiver) = mpsc::channel();

  bar.set_style(
    ProgressStyle::default_bar()
      .template(INDICATIF_PROGRESS_TEMPLATE)
      .progress_chars("#>-"),
  );

  let encode_start_time = Instant::now();

  let scene_changes = thread::spawn(move || {
    scenedetect::get_scene_changes(
      &downscaled,
      Some(Box::new(move |frames, _| {
        sender.send(frames as u64).unwrap();
      })),
    )
    .unwrap()
  });

  while let Ok(frames) = receiver.recv() {
    bar.set_position(frames as u64);
  }

  bar.finish();

  let scene_changes = scene_changes.join().unwrap();
  let splits = create_video_queue_vs(vsinput, &scene_changes);
  let splits = splits.as_slice();
  info!(log, "scenes({}) = {:#?}", splits.len(), splits);

  let (tx, rx): (Sender<PassProgress>, _) = mpsc::channel();

  let mut aomenc = encoder::aom::Aom::new(tx, args.video_params);

  let splits_dir = splits_dir.as_path();
  let encode_dir = encode_dir.as_path();

  let workers = args.workers;

  crossbeam_utils::thread::scope(|s| {
    let mut progress: (Box<[usize]>, usize) = (vec![0; splits.len()].into_boxed_slice(), 0);

    let encode = s.spawn(move |_| {
      aomenc.0.encode(
        vsinput,
        vapoursynth::pipe,
        (splits_dir, encode_dir),
        splits,
        log,
        workers,
      );
    });

    println!("Encode");

    let bar = ProgressBar::new(total_frames);

    bar.set_style(
      ProgressStyle::default_bar()
        .template(INDICATIF_PROGRESS_TEMPLATE)
        .progress_chars("#>-"),
    );

    while let Ok(p) = rx.recv() {
      let PassProgress {
        frames_encoded: frames,
        chunk_index,
      } = p;

      let (progress, sum) = &mut progress;

      // We can continuously sum up the total number of frames encoded instead of recalculating
      // the sum of the encoded frames per chunk every time
      unsafe {
        // SAFETY: `chunk_index` will always be in range of `progress`, because the initialization
        // of progress and the value of `chunk_index` are both based on the length of the splits.
        //
        // However, integer overflow can happen if `frames < progress[chunk_index]`, but this should
        // not happen, since the current frame should always be greater than or equal to the last
        // frame sent.
        let new_frames = frames - progress.get_unchecked(chunk_index);
        *sum += new_frames;

        // SAFETY: Indexing by `chunk_index` is safe for the same reasons explained above.
        *progress.get_unchecked_mut(chunk_index) = frames;

        bar.set_position(*sum as u64);
      }
    }

    encode.join().unwrap();
    bar.finish();
  })
  .unwrap();

  let elapsed = encode_start_time.elapsed();

  println!("Concatenating...");

  av1an::ffmpeg::concatenate_ffmpeg(&args.temp_dir, &args.output, args.encoder);

  if !args.keep {
    remove_dir_all(&args.temp_dir)?;
  }

  println!("Encode took {:.1?}", elapsed);

  Ok(())
}

pub fn main() {
  match _main() {
    Ok(()) => {}
    Err(e) => {
      println!("av1an [error]: {}", e);
      std::process::exit(1);
    }
  }
}
