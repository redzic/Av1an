use regex::Regex;
use std::{
  fs::File,
  io::prelude::*,
  path::{Path, PathBuf},
  process::{Command, Stdio},
};

use crate::Encoder;

/// Get frame count. Direct counting of frame count using ffmpeg
pub fn ffmpeg_get_frame_count(source: &Path) -> usize {
  let source_path = Path::new(&source);

  let mut cmd = Command::new("ffmpeg");
  cmd.args(&[
    "-hide_banner",
    "-i",
    source_path.to_str().unwrap(),
    "-map",
    "0:v:0",
    "-c",
    "copy",
    "-f",
    "null",
    "-",
  ]);

  cmd.stdout(Stdio::piped());
  cmd.stderr(Stdio::piped());

  let out = cmd.output().unwrap();

  assert!(out.status.success());

  let re = Regex::new(r".*frame=\s*([0-9]+)\s").unwrap();
  let output = String::from_utf8(out.stderr).unwrap();

  let cap = re.captures(&output).unwrap();

  cap[cap.len() - 1].parse::<usize>().unwrap()
}

/// Returns vec of all keyframes
pub fn get_keyframes(source: &Path) -> Vec<usize> {
  let mut cmd = Command::new("ffmpeg");

  cmd.stdout(Stdio::piped());
  cmd.stderr(Stdio::piped());

  cmd.args(&[
    "-hide_banner",
    "-i",
    source.to_str().unwrap(),
    "-vf",
    r"select=eq(pict_type\,PICT_TYPE_I)",
    "-f",
    "null",
    "-loglevel",
    "debug",
    "-",
  ]);

  let out = cmd.output().unwrap();
  assert!(out.status.success());

  let re = Regex::new(r".*n:([0-9]+)\.[0-9]+ pts:.+key:1").unwrap();
  let output = String::from_utf8(out.stderr).unwrap();
  let mut kfs: Vec<usize> = Vec::new();
  for found in re.captures_iter(&output) {
    kfs.push(found.get(1).unwrap().as_str().parse::<usize>().unwrap());
  }

  if kfs.is_empty() {
    return vec![0];
  };

  kfs
}

pub fn write_concat_file(temp_folder: &Path) {
  let concat_file = &temp_folder.join("concat");
  let encode_folder = &temp_folder.join("encode");

  let mut files: Vec<PathBuf> = std::fs::read_dir(encode_folder)
    .unwrap()
    .into_iter()
    .filter_map(|d| d.ok())
    .filter_map(|d| {
      if let Ok(file_type) = d.file_type() {
        if file_type.is_file() {
          Some(d.path())
        } else {
          None
        }
      } else {
        None
      }
    })
    .collect();

  assert!(!files.is_empty());

  files.sort_unstable_by_key(|x| {
    // If the temp directory follows the expected format of 00000.ivf, 00001.ivf, etc.,
    // then these unwraps will not fail
    x.file_stem()
      .unwrap()
      .to_str()
      .unwrap()
      .parse::<u32>()
      .unwrap()
  });

  let mut contents = String::with_capacity(files.len() * 16);

  for file in files {
    contents.push_str(format!("file {}\n", file.canonicalize().unwrap().display()).as_str());
  }

  let mut file = File::create(concat_file).unwrap();
  file.write_all(contents.as_bytes()).unwrap();
}

pub fn has_audio(file: &Path) -> bool {
  let mut cmd = Command::new("ffmpeg");

  cmd.stdout(Stdio::piped());
  cmd.stderr(Stdio::piped());

  let re = Regex::new(r".*Stream.+(Audio)").unwrap();

  cmd.args(&["-hide_banner", "-i"]);
  cmd.arg(file);

  let out = cmd.output().unwrap();

  let output = String::from_utf8(out.stderr).unwrap();

  re.is_match(&output)
}

/// Extracting audio
pub fn encode_audio(input: &Path, temp: &Path, audio_params: &[String]) {
  let have_audio = has_audio(input);

  if have_audio {
    let audio_file = Path::new(temp).join("audio.mkv");
    let mut process_audio = Command::new("ffmpeg");

    process_audio.stdout(Stdio::piped());
    process_audio.stderr(Stdio::piped());

    process_audio.args(&["-y", "-hide_banner", "-loglevel", "error", "-i"]);
    process_audio.arg(input);
    process_audio.args(&["-map_metadata", "-1", "-dn", "-vn", "-sn", "-map", "0:a"]);

    process_audio.args(audio_params);
    process_audio.arg(audio_file);

    let out = process_audio.output().unwrap();
    assert!(out.status.success());
  }
}

/// Concatenates using ffmpeg
pub fn concatenate(temp: &Path, output: &Path, encoder: Encoder) {
  let concat = &temp.join("concat");

  write_concat_file(temp);

  let audio = temp.join("audio.mkv");

  let mut cmd = Command::new("ffmpeg");

  cmd.stdout(Stdio::piped());
  cmd.stderr(Stdio::piped());

  match encoder {
    Encoder::x265 => {
      cmd.args(&[
        "-y",
        "-fflags",
        "+genpts",
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "concat",
        "-safe",
        "0",
        "-i",
      ]);
      if audio.exists() {
        if let Ok(metadata) = audio.metadata() {
          if metadata.len() > 1000 {
            cmd.arg(concat).arg("-i").arg(audio).args(&[
              "-map", "1:a", "-c:a", "copy", "-map", "0:v", "-c:v", "copy", "-sn",
            ]);
          }
        }
      }
      cmd
        .args(&[
          "-movflags",
          "frag_keyframe+empty_moov",
          "-map",
          "0",
          "-f",
          "mp4",
        ])
        .arg(output);
    }
    _ => {
      cmd
        .args(&[
          "-y",
          "-hide_banner",
          "-loglevel",
          "error",
          "-f",
          "concat",
          "-safe",
          "0",
          "-i",
        ])
        .arg(concat)
        .arg("-i")
        .arg(audio)
        .args(&[
          "-map", "1:a", "-c:a", "copy", "-map", "0:v", "-c:v", "copy", "-sn",
        ])
        .arg(output);
    }
  }

  let out = cmd.output().unwrap();
  assert!(out.status.success());
}

pub fn get_frame_types(file: &Path) -> Vec<String> {
  let mut cmd = Command::new("ffmpeg");

  cmd.stdout(Stdio::piped());
  cmd.stderr(Stdio::piped());

  cmd.args(&[
    "ffmpeg",
    "-hide_banner",
    "-i",
    file.to_str().unwrap(),
    "-vf",
    "showinfo",
    "-f",
    "null",
    "-loglevel",
    "debug",
    "-",
  ]);

  let out = cmd.output().unwrap();

  assert!(out.status.success());

  let output = String::from_utf8(out.stderr).unwrap();

  output
    .split('\n')
    .map(|s| s.to_string())
    .collect::<Vec<_>>()
}
