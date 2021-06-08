use ringbuffer::{ConstGenericRingBuffer, RingBuffer, RingBufferExt, RingBufferWrite};
use slog::{error, info, o};
use slog::{Drain, Logger};

use std::ffi::OsStr;
use std::fs;
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

use clap::{App, Arg};
use dialoguer::Confirm;
use indicatif::{ProgressBar, ProgressStyle};

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
    av1an [OPTIONS] -e aom -v --cpu-used=5 --cq-level=25 -- -i <INPUT> -o <OUTPUT>")
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
        .default_value("hybrid")
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

  let chunk_method: ChunkMethod = matches.value_of("CHUNK_METHOD").unwrap().parse().unwrap();

  let video_params: Vec<String> = match matches.values_of("VIDEO_PARAMS") {
    Some(v) => v.into_iter().map(|s| s.to_owned()).collect(),
    None => Vec::with_capacity(0),
  };

  // if !matches!(
  //   encoder,
  //   Encoder::aom | Encoder::rav1e | Encoder::svt_av1 | Encoder::svt_vp9 | Encoder::libvpx
  // ) && concat == ConcatMethod::Ivf
  // {
  //   return Err(CliError::InvalidConcatMethod);
  // }

  Ok(CliOptions {
    input,
    output,
    encoder,
    temp_dir,
    concat_method: ConcatMethod::FFmpeg,
    chunk_method,
    workers,
    video_params,
    audio_params: Vec::with_capacity(0),
  })
}

/// Unicode characters used to create a "finer" or smoother-looking progress bar
// const FINE_PROGRESS_CHARS: &str = "█▉▊▋▌▍▎▏  ";

#[inline(always)]
pub fn _main() -> anyhow::Result<()> {
  let args: CliOptions = parse_cli()?;

  let log_file = File::create("log.log").unwrap();

  let plain = slog_term::PlainSyncDecorator::new(log_file);
  let log = Logger::root(slog_term::FullFormat::new(plain).build().fuse(), o!());

  let input = args.input.path.as_path();

  let encode_dir = args.temp_dir.join("encode");
  let splits_dir = args.temp_dir.join("split");

  // TODO warn if temp folder is not empty
  let _ = fs::create_dir(&args.temp_dir);
  let _ = fs::create_dir(&encode_dir);
  let _ = fs::create_dir(&splits_dir);

  let vsinput =
    vapoursynth::create_vapoursynth_source_script(&splits_dir, input, args.chunk_method)?;
  let vsinput = vsinput.as_path();

  let downscaled =
    vapoursynth::create_vapoursynth_scenedetect_script(&splits_dir, input, args.chunk_method)?;

  if args.output.exists() {
    if !Confirm::new()
      .wait_for_newline(true)
      .with_prompt(format!(
        "Output file '{}' exists. Overwrite?",
        &args.output.to_string_lossy()
      ))
      .interact()?
    {
      println!("Exiting...");
      return Ok(());
    }
  }

  let total_frames = vapoursynth::get_num_frames(&vsinput).unwrap() as u64;

  // TODO use formula instead?
  let total_frames_width = total_frames.to_string().len();

  let bar = ProgressBar::new(total_frames);
  let (sender, receiver) = mpsc::channel();

  bar.set_style(
    ProgressStyle::default_bar().template(
      format!(
        "{{prefix:.bold}}▕{{bar:60}}▏{{msg}}\t{{pos:>{}}}/{{len}}\teta {{eta}}",
        total_frames_width
      )
      .as_str(),
    ), // .progress_chars(FINE_PROGRESS_CHARS),
  );
  bar.set_prefix("Scene detection");

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
  let splits = splits.as_slice();

  info!(log, "scenes({}) = {:#?}", splits.len(), splits);

  let (tx, rx): (Sender<PassProgress>, _) = mpsc::channel();

  let mut aomenc = encoder::aom::Aom::new(
    tx,
    &[
      "--cpu-used=6",
      "--cq-level=30",
      "--end-usage=q",
      "--threads=8",
      "-b",
      "10",
    ],
  );

  rayon::ThreadPoolBuilder::new()
    .num_threads(args.workers)
    .build_global()?;

  // TODO scoped threads
  let splits_dir = splits_dir.as_path();
  let encode_dir = encode_dir.as_path();

  crossbeam::thread::scope(|s| {
    // let log2 = log.clone();
    let mut progress: (Box<[usize]>, usize) = (vec![0; splits.len()].into_boxed_slice(), 0);

    let bar = ProgressBar::new(total_frames);

    bar.set_style(
      ProgressStyle::default_bar().template(
        format!(
          "{{prefix:.bold}}▕{{bar:60}}▏{{msg}}\t{{pos:>{}}}/{{len}}\teta {{eta}}",
          total_frames_width
        )
        .as_str(),
      ), // .progress_chars(FINE_PROGRESS_CHARS),
    );
    bar.set_prefix("         Encode");
    bar.set_message("  0%\t  0.00 fps");
    bar.set_position(0);

    let mut frame_times = ConstGenericRingBuffer::<f32, 256>::new();

    let mut timer = Instant::now();

    let encode = s.spawn(move |_| {
      aomenc.0.encode(
        vsinput,
        vapoursynth::pipe,
        (splits_dir, encode_dir),
        splits,
        log,
      );
    });

    while let Ok(p) = rx.recv() {
      let PassProgress {
        frames_encoded: frames,
        chunk_index,
      } = p;

      // let mut _start;
      // let mut _end;

      // We can continuously sum up the total number of frames encoded instead of recalculating
      // the sum of the encoded frames per chunk every time
      unsafe {
        let (progress, sum) = &mut progress;

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

        let new_instant = Instant::now();

        // _start = Instant::now();
        frame_times.push((new_instant - timer).as_secs_f32());

        timer = new_instant;

        if new_frames > 0 {
          bar.set_position(*sum as u64);
          let fps = frame_times.len() as f32 / frame_times.iter().sum::<f32>();

          bar.set_message(format!(
            "{:3}%\t{:>6.2} fps",
            100 * *sum as u64 / total_frames,
            fps
          ));
        }
        // _end = Instant::now();
      }

      // info!(log2, "loop iteration took {:?}", _end - _start);
    }

    encode.join().unwrap();
    bar.finish();
    println!("Took {:.2?}", encode_start_time.elapsed());
  })
  .unwrap();

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
