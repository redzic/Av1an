// This is a mostly drop-in reimplementation of vspipe.
// The main difference is what the errors look like.

// TODO switch error handling crate, failure is deprecated
use failure::{bail, err_msg, format_err, Error, ResultExt};

// Modified from vspipe example in vapoursynth crate
// https://github.com/YaLTeR/vapoursynth-rs/blob/master/vapoursynth/examples/vspipe.rs
extern crate num_rational;
extern crate vapoursynth;

use std::collections::HashMap;
use std::fmt::Debug;
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use self::num_rational::Ratio;
use self::vapoursynth::prelude::*;
use super::*;

struct OutputParameters<'core> {
  node: Node<'core>,
  alpha_node: Option<Node<'core>>,
  start_frame: usize,
  end_frame: usize,
}

struct OutputState<'core, W: Write + Send> {
  output_target: W,
  error: Option<(usize, Error)>,
  reorder_map: HashMap<usize, (Option<FrameRef<'core>>, Option<FrameRef<'core>>)>,
  last_requested_frame: usize,
  next_output_frame: usize,
  current_timecode: Ratio<i64>,
  callbacks_fired: usize,
  callbacks_fired_alpha: usize,
  last_fps_report_time: Instant,
  last_fps_report_frames: usize,
  fps: Option<f64>,
}

struct SharedData<'core, W: Write + Send> {
  output_done_pair: (Mutex<bool>, Condvar),
  output_parameters: OutputParameters<'core>,
  output_state: Mutex<OutputState<'core, W>>,
}

// Parses the --arg arguments in form of key=value.
fn parse_arg(arg: &str) -> Result<(&str, &str), Error> {
  arg
    .find('=')
    .map(|index| arg.split_at(index))
    .map(|(k, v)| (k, &v[1..]))
    .ok_or_else(|| format_err!("No value specified for argument: {}", arg))
}

// Returns "Variable" or the value of the property passed through a function.
fn map_or_variable<T, F>(x: &Property<T>, f: F) -> String
where
  T: Debug + Clone + Copy + Eq + PartialEq,
  F: FnOnce(&T) -> String,
{
  match *x {
    Property::Variable => "Variable".to_owned(),
    Property::Constant(ref x) => f(x),
  }
}

fn print_info(writer: &mut dyn Write, node: &Node, alpha: Option<&Node>) -> Result<(), Error> {
  let info = node.info();

  writeln!(
    writer,
    "Width: {}",
    map_or_variable(&info.resolution, |x| format!("{}", x.width))
  )?;
  writeln!(
    writer,
    "Height: {}",
    map_or_variable(&info.resolution, |x| format!("{}", x.height))
  )?;

  #[cfg(feature = "gte-vapoursynth-api-32")]
  writeln!(writer, "Frames: {}", info.num_frames)?;

  #[cfg(not(feature = "gte-vapoursynth-api-32"))]
  writeln!(
    writer,
    "Frames: {}",
    match info.num_frames {
      Property::Variable => "Unknown".to_owned(),
      Property::Constant(x) => format!("{}", x),
    }
  )?;

  writeln!(
    writer,
    "FPS: {}",
    map_or_variable(&info.framerate, |x| format!(
      "{}/{} ({:.3} fps)",
      x.numerator,
      x.denominator,
      x.numerator as f64 / x.denominator as f64
    ))
  )?;

  match info.format {
    Property::Variable => writeln!(writer, "Format Name: Variable")?,
    Property::Constant(f) => {
      writeln!(writer, "Format Name: {}", f.name())?;
      writeln!(writer, "Color Family: {}", f.color_family())?;
      writeln!(
        writer,
        "Alpha: {}",
        if alpha.is_some() { "Yes" } else { "No" }
      )?;
      writeln!(writer, "Sample Type: {}", f.sample_type())?;
      writeln!(writer, "Bits: {}", f.bits_per_sample())?;
      writeln!(writer, "SubSampling W: {}", f.sub_sampling_w())?;
      writeln!(writer, "SubSampling H: {}", f.sub_sampling_h())?;
    }
  }

  Ok(())
}

fn print_y4m_header<W: Write>(writer: &mut W, node: &Node) -> Result<(), Error> {
  let info = node.info();

  if let Property::Constant(format) = info.format {
    write!(writer, "YUV4MPEG2 C")?;

    match format.color_family() {
      ColorFamily::Gray => {
        write!(writer, "mono")?;
        if format.bits_per_sample() > 8 {
          write!(writer, "{}", format.bits_per_sample())?;
        }
      }
      ColorFamily::YUV => {
        write!(
          writer,
          "{}",
          match (format.sub_sampling_w(), format.sub_sampling_h()) {
            (1, 1) => "420",
            (1, 0) => "422",
            (0, 0) => "444",
            (2, 2) => "410",
            (2, 0) => "411",
            (0, 1) => "440",
            _ => bail!("No y4m identifier exists for the current format"),
          }
        )?;

        if format.bits_per_sample() > 8 && format.sample_type() == SampleType::Integer {
          write!(writer, "p{}", format.bits_per_sample())?;
        } else if format.sample_type() == SampleType::Float {
          write!(
            writer,
            "p{}",
            match format.bits_per_sample() {
              16 => "h",
              32 => "s",
              64 => "d",
              _ => unreachable!(),
            }
          )?;
        }
      }
      _ => bail!("No y4m identifier exists for the current format"),
    }

    if let Property::Constant(resolution) = info.resolution {
      write!(writer, " W{} H{}", resolution.width, resolution.height)?;
    } else {
      unreachable!();
    }

    if let Property::Constant(framerate) = info.framerate {
      write!(
        writer,
        " F{}:{}",
        framerate.numerator, framerate.denominator
      )?;
    } else {
      unreachable!();
    }

    #[cfg(feature = "gte-vapoursynth-api-32")]
    let num_frames = info.num_frames;

    #[cfg(not(feature = "gte-vapoursynth-api-32"))]
    let num_frames = {
      if let Property::Constant(num_frames) = info.num_frames {
        num_frames
      } else {
        unreachable!();
      }
    };

    writeln!(writer, " Ip A0:0 XLENGTH={}", num_frames)?;

    Ok(())
  } else {
    unreachable!();
  }
}

// Checks if the frame is completed, that is, we have the frame and, if needed, its alpha part.
fn is_completed(entry: &(Option<FrameRef>, Option<FrameRef>), have_alpha: bool) -> bool {
  entry.0.is_some() && (!have_alpha || entry.1.is_some())
}

fn print_frame<W: Write>(writer: &mut W, frame: &Frame) -> Result<(), Error> {
  const RGB_REMAP: [usize; 3] = [1, 2, 0];

  let format = frame.format();
  #[allow(clippy::needless_range_loop)]
  for plane in 0..format.plane_count() {
    let plane = if format.color_family() == ColorFamily::RGB {
      RGB_REMAP[plane]
    } else {
      plane
    };

    if let Ok(data) = frame.data(plane) {
      writer.write_all(data)?;
    } else {
      for row in 0..frame.height(plane) {
        writer.write_all(frame.data_row(plane, row))?;
      }
    }
  }

  Ok(())
}

fn write_frames<W: Write>(
  writer: &mut W,
  parameters: &OutputParameters,
  frame: &Frame,
  alpha_frame: Option<&Frame>,
) -> Result<(), Error> {
  writeln!(writer, "FRAME")?;

  print_frame(writer, frame)?;
  if let Some(alpha_frame) = alpha_frame {
    print_frame(writer, alpha_frame)?;
  }

  Ok(())
}

fn update_timecodes<W: Write + Send>(
  frame: &Frame,
  state: &mut OutputState<W>,
) -> Result<(), Error> {
  let props = frame.props();
  let duration_num = props.get_int("_DurationNum")?;
  let duration_den = props.get_int("_DurationDen")?;

  assert!(duration_den != 0);

  state.current_timecode += Ratio::new(duration_num, duration_den);

  Ok(())
}

fn frame_done_callback<'core, W: Write + Send + 'core>(
  frame: Result<FrameRef<'core>, GetFrameError>,
  n: usize,
  _node: &Node<'core>,
  shared_data: &Arc<SharedData<'core, W>>,
  alpha: bool,
) {
  let parameters = &shared_data.output_parameters;
  let mut state = shared_data.output_state.lock().unwrap();

  // Increase the progress counter.
  if !alpha {
    state.callbacks_fired += 1;
    if parameters.alpha_node.is_none() {
      state.callbacks_fired_alpha += 1;
    }
  } else {
    state.callbacks_fired_alpha += 1;
  }

  match frame {
    Err(error) => {
      if state.error.is_none() {
        state.error = Some((
          n,
          err_msg(error.into_inner().to_string_lossy().into_owned()),
        ))
      }
    }
    Ok(frame) => {
      // Store the frame in the reorder map.
      {
        let entry = state.reorder_map.entry(n).or_insert((None, None));
        if alpha {
          entry.1 = Some(frame);
        } else {
          entry.0 = Some(frame);
        }
      }

      // If we got both a frame and its alpha frame, request one more.
      if is_completed(&state.reorder_map[&n], parameters.alpha_node.is_some())
        && state.last_requested_frame < parameters.end_frame
        && state.error.is_none()
      {
        let shared_data_2 = shared_data.clone();
        parameters.node.get_frame_async(
          dbg!(state.last_requested_frame + 1),
          move |frame, n, node| frame_done_callback(frame, n, &node, &shared_data_2, false),
        );

        if let Some(ref alpha_node) = parameters.alpha_node {
          let shared_data_2 = shared_data.clone();
          alpha_node.get_frame_async(
            dbg!(state.last_requested_frame + 1),
            move |frame, n, node| frame_done_callback(frame, n, &node, &shared_data_2, true),
          );
        }

        state.last_requested_frame += 1;
      }

      // Output all completed frames.
      while state
        .reorder_map
        .get(&state.next_output_frame)
        .map(|entry| is_completed(entry, parameters.alpha_node.is_some()))
        .unwrap_or(false)
      {
        let next_output_frame = state.next_output_frame;
        let (frame, alpha_frame) = state.reorder_map.remove(&next_output_frame).unwrap();

        let frame = frame.unwrap();
        if state.error.is_none() {
          if let Err(error) = write_frames(
            &mut state.output_target,
            parameters,
            &frame,
            alpha_frame.as_deref(),
          ) {
            state.error = Some((n, error));
          }
        }

        state.next_output_frame += 1;
      }
    }
  }

  // if state.next_output_frame == parameters.end_frame + 1 {
  // This condition works with error handling:
  let frames_requested = state.last_requested_frame - parameters.start_frame + 1;
  if state.callbacks_fired == frames_requested && state.callbacks_fired_alpha == frames_requested {
    *shared_data.output_done_pair.0.lock().unwrap() = true;
    shared_data.output_done_pair.1.notify_one();
  }
}

fn output<W: Write + Send + Sync>(
  mut out: &mut W,
  parameters: OutputParameters,
) -> Result<(), Error> {
  assert!(
    parameters.alpha_node.is_none(),
    "Can't apply y4m headers to a clip with alpha"
  );

  print_y4m_header(&mut out, &parameters.node)?;

  for n in parameters.start_frame..=parameters.end_frame {
    let frame = parameters.node.get_frame(n).unwrap();
    write_frames(&mut out, &parameters, &frame, None)?;
  }

  out.flush()?;

  Ok(())
}

// TODO try to eliminate code duplication
pub fn run<W: Write + Send + Sync>(
  input: &Path,
  start_frame: usize,
  end_frame: usize,
  out: &mut W,
) -> Result<(), Error> {
  // Create a new VSScript environment.
  let mut environment = Environment::new().context("Couldn't create the VSScript environment")?;

  // Evaluate the script.
  environment.eval_file(input, EvalFlags::SetWorkingDir)?;

  // Get the output node.
  let output_index = 0;

  #[cfg(feature = "gte-vsscript-api-31")]
  let (node, alpha_node) = environment.get_output(output_index).context(format!(
    "Couldn't get the output node at index {}",
    output_index
  ))?;
  #[cfg(not(feature = "gte-vsscript-api-31"))]
  let (node, alpha_node) = (
    environment.get_output(output_index).context(format!(
      "Couldn't get the output node at index {}",
      output_index
    ))?,
    None::<Node>,
  );

  let num_frames = {
    let info = node.info();

    if let Property::Variable = info.format {
      panic!("Cannot output clips with varying format");
    }
    if let Property::Variable = info.resolution {
      panic!("Cannot output clips with varying dimensions");
    }
    if let Property::Variable = info.framerate {
      panic!("Cannot output clips with varying framerate");
    }

    #[cfg(feature = "gte-vapoursynth-api-32")]
    let num_frames = info.num_frames;

    #[cfg(not(feature = "gte-vapoursynth-api-32"))]
    let num_frames = {
      match info.num_frames {
        Property::Variable => {
          // TODO: make it possible?
          panic!("Cannot output clips with unknown length");
        }
        Property::Constant(x) => x,
      }
    };

    num_frames
  };

  // Check if the input start and end frames make sense.
  assert!(!(end_frame < start_frame || end_frame >= num_frames));

  output(
    out,
    OutputParameters {
      node,
      alpha_node,
      start_frame: start_frame as usize,
      end_frame: end_frame as usize,
    },
  )?;

  Ok(())
}

pub fn get_num_frames(path: &Path) -> Result<usize, Error> {
  // Create a new VSScript environment.
  let mut environment = Environment::new().context("Couldn't create the VSScript environment")?;

  // Evaluate the script.
  environment.eval_file(path, EvalFlags::SetWorkingDir)?;

  // Get the output node.
  let output_index = 0;

  #[cfg(feature = "gte-vsscript-api-31")]
  let (node, alpha_node) = environment.get_output(output_index).context(format!(
    "Couldn't get the output node at index {}",
    output_index
  ))?;
  #[cfg(not(feature = "gte-vsscript-api-31"))]
  let (node, alpha_node) = (
    environment.get_output(output_index).context(format!(
      "Couldn't get the output node at index {}",
      output_index
    ))?,
    None::<Node>,
  );

  let num_frames = {
    let info = node.info();

    if let Property::Variable = info.format {
      bail!("Cannot output clips with varying format");
    }
    if let Property::Variable = info.resolution {
      bail!("Cannot output clips with varying dimensions");
    }
    if let Property::Variable = info.framerate {
      bail!("Cannot output clips with varying framerate");
    }

    #[cfg(feature = "gte-vapoursynth-api-32")]
    let num_frames = info.num_frames;

    #[cfg(not(feature = "gte-vapoursynth-api-32"))]
    let num_frames = {
      match info.num_frames {
        Property::Variable => {
          // TODO: make it possible?
          bail!("Cannot output clips with unknown length");
        }
        Property::Constant(x) => x,
      }
    };

    num_frames
  };

  Ok(num_frames)
}
