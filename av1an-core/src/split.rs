use std::path::Path;
use std::process::{Command, Stdio};

pub fn segment(input: &Path, temp: &Path, segments: &[usize]) {
  let mut cmd = Command::new("ffmpeg");

  cmd.stdout(Stdio::piped());
  cmd.stderr(Stdio::piped());

  cmd.args(&["-hide_banner", "-y", "-i"]);
  cmd.arg(input);
  cmd.args(&[
    "-map",
    "0:v:0",
    "-an",
    "-c",
    "copy",
    "-avoid_negative_ts",
    "1",
    "-vsync",
    "0",
  ]);

  if !segments.is_empty() {
    let segments_to_string = segments
      .iter()
      .map(|x| x.to_string())
      .collect::<Vec<String>>();
    let segments_joined = segments_to_string.join(",");

    cmd.args(&["-f", "segment", "-segment_frames", &segments_joined]);
    let split_path = Path::new(temp).join("split").join("%05d.mkv");
    let split_str = split_path.to_str().unwrap();
    cmd.arg(split_str);
  } else {
    let split_path = Path::new(temp).join("split").join("0.mkv");
    let split_str = split_path.to_str().unwrap();
    cmd.arg(split_str);
  }
  let out = cmd.output().unwrap();
  assert!(out.status.success());
}

pub fn extra_splits(
  split_locations: Vec<usize>,
  total_frames: usize,
  split_size: usize,
) -> Vec<usize> {
  let mut result_vec: Vec<usize> = split_locations.clone();

  let mut total_length = split_locations;
  total_length.insert(0, 0);
  total_length.push(total_frames);

  let iter = total_length[..total_length.len() - 1]
    .iter()
    .zip(total_length[1..].iter());

  for (x, y) in iter {
    let distance = y - x;
    if distance > split_size {
      let additional_splits = (distance / split_size) + 1;
      for n in 1..additional_splits {
        let new_split = (distance as f64 * (n as f64 / additional_splits as f64)) as usize + x;
        result_vec.push(new_split);
      }
    }
  }

  result_vec.sort_unstable();

  result_vec
}
