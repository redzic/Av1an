use circular_queue::CircularQueue;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{ChildStdin, Command, Stdio};
use std::string::FromUtf8Error;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Instant;

use crate::encoder::{Chunk, PassProgress, TwoPassProgress};

/// API for encoding a single chunk. For a high-level API to encode
/// an entire video, use <TODO>.
pub trait EncodeChunk<I, ProgressData: Copy, O> {
  fn encode_chunk(&mut self, output: O, input: I, chunk: Chunk);
}

#[derive(Debug)]
enum PipeError {
  IO(io::Error),
  NotUtf8(FromUtf8Error),
}

#[derive(Debug)]
enum PipedLine {
  Line(String),
  Eof,
}

/// Reads data from the pipe byte-by-byte and returns the lines.
pub struct PipeStreamReader {
  lines: Receiver<Result<PipedLine, PipeError>>,
}

impl PipeStreamReader {
  // Starts a background task reading bytes from the pipe.
  fn new(mut stream: Box<dyn io::Read + Send>) -> PipeStreamReader {
    PipeStreamReader {
      lines: {
        let (tx, rx) = mpsc::channel();

        thread::spawn(move || {
          let mut buf = Vec::new();
          let mut byte = [0u8];
          loop {
            match stream.read(&mut byte) {
              Ok(0) => {
                let _ = tx.send(Ok(PipedLine::Eof));
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

pub struct OnePassEncoder<'a, ParseFunc, Gen: Fn(usize, &'a [&'a str]) -> &'a mut Command> {
  progress_info: usize,
  frame_times: CircularQueue<f32>,
  parse_func: ParseFunc,
  progress_sender: Sender<PassProgress>,
  first_pass_gen: Gen,
  encoder_args: &'a [&'a str],
}

impl<'a, F, E: Fn(usize, &'a [&'a str]) -> &'a mut Command> OnePassEncoder<'a, F, E> {
  pub fn new(
    moving_average_size: usize,
    parse_func: F,
    first_pass_gen: E,
    progress_sender: Sender<PassProgress>,
    encoder_args: &'a [&'a str],
  ) -> Self {
    Self {
      encoder_args,
      first_pass_gen,
      parse_func,
      progress_info: 0,
      frame_times: CircularQueue::with_capacity(moving_average_size),
      progress_sender,
    }
  }
}

pub struct TwoPassEncoder<
  'a,
  FpParseFunc: Fn(&str) -> Option<usize>,
  SpParseFunc: Fn(&str) -> Option<usize>,
  FpGen: Fn(usize, &[&str], &Path) -> Command,
  SpGen: Fn(usize, &[&str], &Path) -> Command,
> {
  // Basically a fast HashMap<usize, usize> where the index represents the key.
  progress_info: [(Box<[usize]>, usize); 2],
  frame_times: [CircularQueue<f32>; 2],
  timers: [Instant; 2],
  first_pass_parse_func: FpParseFunc,
  second_pass_parse_func: SpParseFunc,
  first_pass_gen: FpGen,
  second_pass_gen: SpGen,
  progress_sender: Sender<TwoPassProgress>,
  encoder_args: &'a [&'a str],
}

impl<
    'a,
    FpParseFunc: Fn(&str) -> Option<usize>,
    SpParseFunc: Fn(&str) -> Option<usize>,
    FpGen: Fn(usize, &[&str], &Path) -> Command,
    SpGen: Fn(usize, &[&str], &Path) -> Command,
  > TwoPassEncoder<'a, FpParseFunc, SpParseFunc, FpGen, SpGen>
{
  pub fn new(
    moving_average_size: usize,
    chunks: usize,
    first_pass_parse_func: FpParseFunc,
    second_pass_parse_func: SpParseFunc,
    first_pass_gen: FpGen,
    second_pass_gen: SpGen,
    progress_sender: Sender<TwoPassProgress>,
    encoder_args: &'a [&'a str],
  ) -> Self {
    Self {
      encoder_args,
      first_pass_gen,
      second_pass_gen,
      first_pass_parse_func,
      second_pass_parse_func,
      progress_info: [
        (vec![0; chunks].into_boxed_slice(), 0),
        (vec![0; chunks].into_boxed_slice(), 0),
      ],
      frame_times: [
        CircularQueue::with_capacity(moving_average_size),
        CircularQueue::with_capacity(moving_average_size),
      ],
      progress_sender,
      timers: [Instant::now(); 2],
    }
  }
}

// needs to also be generic over if the encoder uses stderr/stdout
// TODO fix this, it only works for aomenc or encoders that output
// progress information to stderr
impl<
    'a,
    // W: 'a + Write + Send + Sync,
    // TODO change lifetime, this requires unsafe transmutes
    // this would involve using scoped thread instead
    T,
    Pipe: 'static + Fn(&Path, usize, usize, &mut ChildStdin) -> T + Send + Sync,
    FpFrameParseFunc: Fn(&str) -> Option<usize>,
    SpFrameParseFunc: Fn(&str) -> Option<usize>,
    FpCmdGen: Fn(usize, &[&str], &Path) -> Command,
    SpCmdGen: Fn(usize, &[&str], &Path) -> Command,
  > EncodeChunk<Pipe, PassProgress, (&'a Path, &'a Path)>
  for TwoPassEncoder<'a, FpFrameParseFunc, SpFrameParseFunc, FpCmdGen, SpCmdGen>
{
  fn encode_chunk(&mut self, output: (&'a Path, &'a Path), input: Pipe, chunk: Chunk) {
    let mut first_pass = (self.first_pass_gen)(chunk.index, self.encoder_args, output.0)
      .stdout(Stdio::piped())
      .stderr(Stdio::piped())
      .stdin(Stdio::piped())
      .spawn()
      .unwrap();

    let mut stdin = first_pass.stdin.take().unwrap();

    // TODO use scoped threads so that it doesn't required a static lifetime
    let input = thread::spawn(move || {
      input(Path::new("input.vpy"), chunk.start, chunk.end, &mut stdin);
      input
    });

    let stderr = first_pass.stderr.take().unwrap();
    let out = PipeStreamReader::new(Box::new(stderr));

    for line in out.lines {
      match line {
        Ok(PipedLine::Line(s)) => {
          let s = s.as_str();
          if let Some(frame) = (self.first_pass_parse_func)(s) {
            self
              .progress_sender
              .send(TwoPassProgress::FirstPass(PassProgress {
                chunk_index: chunk.index,
                frames_encoded: frame,
                // TODO
                fps: 0.0,
              }))
              .unwrap();
          }
        }
        _ => break,
      }
    }

    let input = input.join().unwrap();
    first_pass.wait().unwrap();

    while !output.0.exists() {}

    // TODO fix output
    let mut second_pass = (self.second_pass_gen)(chunk.index, self.encoder_args, output.1)
      .stdout(Stdio::piped())
      .stderr(Stdio::piped())
      .stdin(Stdio::piped())
      .spawn()
      .unwrap();

    let mut stdin = second_pass.stdin.take().unwrap();

    // TODO use scoped threads so that it doesn't required a static lifetime
    let vspipe = thread::spawn(move || {
      input(Path::new("input.vpy"), chunk.start, chunk.end, &mut stdin);
    });

    let stderr = second_pass.stderr.take().unwrap();
    let out = PipeStreamReader::new(Box::new(stderr));

    for line in out.lines {
      match line {
        Ok(PipedLine::Line(s)) => {
          let s = s.as_str();
          if let Some(frame) = (self.second_pass_parse_func)(s) {
            self
              .progress_sender
              .send(TwoPassProgress::SecondPass(PassProgress {
                chunk_index: chunk.index,
                frames_encoded: frame,
                // TODO
                fps: 0.0,
              }))
              .unwrap();
          }
        }
        _ => break,
      }
    }

    vspipe.join().unwrap();
    second_pass.wait().unwrap();
  }
}

// impl<'a, W: Write, F, E: Fn(usize, &'a [&'a str]) -> &'a mut Command>
//   EncodeChunk<W, PassProgress, &'a Path> for OnePassEncoder<'a, F, E>
// {
//   fn encode_chunk(
//     &mut self,
//     output: &Path,
//     input: W,
//     chunk: Chunk,
//     progress_channel: Sender<PassProgress>,
//   ) {
//     let mut first_pass = (&self.first_pass_gen)(chunk.index, self.encoder_args)
//       .spawn()
//       .unwrap();

//     let mut stdin = first_pass.stdin.take();

//     thread::spawn(move || {
//       // input
//     });
//   }
// }
