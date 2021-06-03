// TODO switch error handling crate, failure is deprecated
use failure::{Error, ResultExt};

extern crate num_rational;
extern crate vapoursynth;

use anyhow::anyhow;
use std::fs::File;
use std::io::Write;
use std::path::Path;

use self::vapoursynth::prelude::*;
use super::*;

struct OutputParameters<'core> {
  node: Node<'core>,
  start_frame: usize,
  end_frame: usize,
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
            _ => unreachable!(),
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
      _ => panic!("No y4m identifier exists for the current format"),
    }

    if let Property::Constant(resolution) = info.resolution {
      write!(writer, " W{} H{}", resolution.width, resolution.height)?;
    }

    if let Property::Constant(framerate) = info.framerate {
      write!(
        writer,
        " F{}:{}",
        framerate.numerator, framerate.denominator
      )?;
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

fn write_frames<W: Write>(writer: &mut W, frame: &Frame) -> Result<(), Error> {
  writeln!(writer, "FRAME")?;

  print_frame(writer, frame)?;

  Ok(())
}

fn output<W: Write + Send + Sync>(
  mut out: &mut W,
  parameters: OutputParameters,
) -> Result<(), Error> {
  print_y4m_header(&mut out, &parameters.node)?;

  for n in parameters.start_frame..=parameters.end_frame {
    let frame = parameters.node.get_frame(n).unwrap();
    write_frames(&mut out, &frame)?;
  }

  out.flush()?;

  Ok(())
}

// TODO try to eliminate code duplication
pub fn pipe<W: Write + Send + Sync>(
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
  let (node, _) = (
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
      // alpha_node,
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
  let (node, _) = (
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

  Ok(num_frames)
}

/// Creates vs pipe file
pub fn create_vapoursynth_source_script(
  temp: &Path,
  source: &Path,
  chunk_method: ChunkMethod,
) -> anyhow::Result<PathBuf> {
  let source = Path::new(source).canonicalize()?;
  let load_script_path = temp.join("loadscript.vpy");

  let mut load_script = File::create(&load_script_path)?;

  let cache_file = std::env::current_dir()?.join(temp.join(format!(
    "cache.{}",
    match chunk_method {
      ChunkMethod::FFMS2 => "ffindex",
      ChunkMethod::LSMASH => "lwi",
      _ => return Err(anyhow!("Cannot create vapoursynth script for chunk method")),
    }
  )));

  load_script.write_all(
    // TODO should probably check if the syntax for rust strings and escaping utf and stuff like that is the same as in python
    format!(
      "from vapoursynth import core\n\
core.{}({:?}, cachefile={:?}).set_output()",
      match chunk_method {
        ChunkMethod::FFMS2 => "ffms2.Source",
        ChunkMethod::LSMASH => "lsmas.LWLibavSource",
        _ => unreachable!(),
      },
      source,
      cache_file
    )
    .as_bytes(),
  )?;

  Ok(load_script_path)
}

/// Creates vs pipe file
pub fn create_vapoursynth_scenedetect_script(
  temp: &Path,
  source: &Path,
  chunk_method: ChunkMethod,
) -> anyhow::Result<PathBuf> {
  let source = Path::new(source).canonicalize()?;
  let load_script_path = temp.join("loadscript.vpy");

  let mut load_script = File::create(&load_script_path)?;

  let cache_file = std::env::current_dir()?.join(temp.join(format!(
    "cache_scenedetect.{}",
    match chunk_method {
      ChunkMethod::FFMS2 => "ffindex",
      ChunkMethod::LSMASH => "lwi",
      _ => return Err(anyhow!("Cannot create vapoursynth script for chunk method")),
    }
  )));

  load_script.write_all(
    // TODO only scale by a factor of 2, so that it works better for different resolutions and aspect ratios
    format!(
      "import vapoursynth\n\
core = vapoursynth.get_core()
core.resize.Bilinear(core.{}({:?}, cachefile={:?}), width=640, height=360, format=vapoursynth.YUV420P8).set_output()",
      match chunk_method {
        ChunkMethod::FFMS2 => "ffms2.Source",
        ChunkMethod::LSMASH => "lsmas.LWLibavSource",
        _ => unreachable!(),
      },
      source,
      cache_file
    )
    .as_bytes(),
  )?;

  Ok(load_script_path)
}
