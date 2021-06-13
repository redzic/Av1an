use av_scenechange::{detect_scene_changes, DetectionOptions};
use std::path::Path;
use std::process::{Command, Stdio};

/// Detect scene changes using rav1e scene detector.
///
/// src: Input to vapoursynth script source.
pub fn get_scene_changes(
  src: &Path,
  callback: Option<Box<dyn Fn(usize, usize)>>,
) -> anyhow::Result<Vec<usize>> {
  Ok(
    detect_scene_changes::<_, u8>(
      &mut y4m::Decoder::new(
        Command::new("vspipe")
          .arg("-y")
          .arg(src)
          .arg("-")
          .stdout(Stdio::piped())
          .stderr(Stdio::null())
          .spawn()?
          .stdout
          .unwrap(),
      )?,
      DetectionOptions {
        fast_analysis: true,
        ignore_flashes: true,
        lookahead_distance: 5,
        max_scenecut_distance: Some(200),
        min_scenecut_distance: Some(12),
      },
      callback,
    )
    .scene_changes,
  )
}
