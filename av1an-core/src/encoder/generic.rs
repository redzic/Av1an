use rayon::prelude::*;
use std::io::{self, Read};
use std::path::Path;
use std::process::{ChildStdin, Command, Stdio};
use std::string::FromUtf8Error;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use slog::{info, Logger};

use crate::encoder::{Chunk, PassProgress};

/// API for encoding a single chunk. For a high-level API to encode
/// an entire video, use <TODO>.
pub trait EncodeChunk<Pipe, Input, ProgressData: Copy, Output, Log> {
  fn encode_chunk(
    &mut self,
    output: Output,
    pipe: Pipe,
    input: Input,
    chunk: Chunk,
    progress: Sender<ProgressData>,
    logger: &mut Log,
  );
}

pub trait ParallelEncode<Pipe, Input, ProgressData: Copy, Output, Log> {
  fn encode(&mut self, output: Output, pipe: Pipe, input: Input, chunks: &[Chunk], logger: Log);
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

#[derive(Clone)]
pub struct TwoPassEncoder<
  'a,
  ParseFunc: Fn(&str) -> Option<usize>,
  FpGen: Fn(&[&str], &Path) -> Command,
  SpGen: Fn(&[&str], (&Path, &Path)) -> Command,
> {
  parse_func: ParseFunc,
  first_pass_gen: FpGen,
  second_pass_gen: SpGen,
  progress_sender: Sender<PassProgress>,
  encoder_args: &'a [&'a str],
}

// TODO make param types name consistent
impl<
    'a,
    ParseFunc: Fn(&str) -> Option<usize>,
    FpGen: Fn(&[&str], &Path) -> Command,
    SpGen: Fn(&[&str], (&Path, &Path)) -> Command,
  > TwoPassEncoder<'a, ParseFunc, FpGen, SpGen>
{
  pub fn new(
    parse_func: ParseFunc,
    first_pass_gen: FpGen,
    second_pass_gen: SpGen,
    progress_sender: Sender<PassProgress>,
    encoder_args: &'a [&'a str],
  ) -> Self {
    Self {
      parse_func,
      first_pass_gen,
      second_pass_gen,
      progress_sender,
      encoder_args,
    }
  }
}

// needs to also be generic over if the encoder uses stderr/stdout
// TODO fix this, it only works for aomenc or encoders that output
// progress information to stderr
impl<
    'a,
    // W: 'a + Write + Send + Sync,
    // this would involve using scoped thread instead
    T,
    Pipe: Fn(&Path, usize, usize, &mut ChildStdin) -> T + Send + Sync,
    ParseFunc: Fn(&str) -> Option<usize> + Clone,
    FpCmdGen: Fn(&[&str], &Path) -> Command,
    SpCmdGen: Fn(&[&str], (&Path, &Path)) -> Command,
  > EncodeChunk<Pipe, &Path, PassProgress, (&Path, &Path), Logger>
  for TwoPassEncoder<'a, ParseFunc, FpCmdGen, SpCmdGen>
{
  fn encode_chunk(
    &mut self,
    output: (&Path, &Path),
    pipe: Pipe,
    input: &Path,
    chunk: Chunk,
    progress: Sender<PassProgress>,
    logger: &mut Logger,
  ) {
    info!(logger, "Chunk {} spawned", chunk.index);

    crossbeam::thread::scope(|s| {
      let mut first_pass = (self.first_pass_gen)(self.encoder_args, output.0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();

      let mut stdin = first_pass.stdin.take().unwrap();

      let pipe = s.spawn(move |_| {
        pipe(input, chunk.start, chunk.end, &mut stdin);
        pipe
      });

      let pipe = pipe.join().unwrap();
      let fp = first_pass.wait_with_output().unwrap();

      info!(
        logger,
        "first pass (chunk {}) supposedly finished with output: {:?}", chunk.index, fp
      );

      // wait for first pass file to exist, causes issues otherwise
      for _try in 1usize.. {
        if !output.0.exists() {
          info!(
            logger,
            "waiting for {:?} to exist... (try {})", output.0, _try
          );
          thread::sleep(Duration::from_millis(250));
        } else {
          info!(
            logger,
            "first pass file {:?} exists! took {} tries", output.0, _try
          );
          break;
        }
      }

      // TODO fix output
      let mut second_pass = (self.second_pass_gen)(self.encoder_args, output)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();

      let mut stdin = second_pass.stdin.take().unwrap();

      let pipe = s.spawn(move |_| {
        pipe(input, chunk.start, chunk.end, &mut stdin);
      });

      let stderr = second_pass.stderr.take().unwrap();
      let out = PipeStreamReader::new(Box::new(stderr));

      for line in out.lines {
        match line {
          Ok(PipedLine::Line(s)) => {
            let s = s.as_str();
            // info!(logger, "pass 2 (chunk {}): {}", chunk.index, s);
            if let Some(frame) = (self.parse_func)(s) {
              progress
                .send(PassProgress {
                  chunk_index: chunk.index,
                  frames_encoded: frame,
                })
                .unwrap();
            }
          }
          _ => break,
        }
      }

      pipe.join().unwrap();
      second_pass.wait().unwrap();

      info!(logger, "Chunk {} finished", chunk.index);
    })
    .unwrap();
  }
}

impl<
    'a,
    // this would involve using scoped threads instead
    T,
    Pipe: Fn(&Path, usize, usize, &mut ChildStdin) -> T + Send + Sync + Copy + Clone,
    ParseFunc: Fn(&str) -> Option<usize> + Clone + Send,
    FpCmdGen: Fn(&[&str], &Path) -> Command + Clone + Send,
    SpCmdGen: Fn(&[&str], (&Path, &Path)) -> Command + Clone + Send,
  > ParallelEncode<Pipe, (&'a Path, &'a Path), PassProgress, &Path, Logger>
  for TwoPassEncoder<'a, ParseFunc, FpCmdGen, SpCmdGen>
{
  fn encode(
    &mut self,
    input: &Path,
    pipe: Pipe,
    output: (&'a Path, &'a Path),
    chunks: &[Chunk],
    logger: Logger,
  ) {
    chunks
      .par_iter()
      .for_each_with((self.clone(), logger.clone()), |(enc, logger), chunk| {
        let first_pass_file = format!("fpf{}.log", chunk.index);
        let second_pass_file = format!("{}.ivf", chunk.index);
        enc.encode_chunk(
          (
            &output.0.join(first_pass_file.as_str()),
            &output.1.join(second_pass_file.as_str()),
          ),
          pipe,
          input,
          *chunk,
          enc.progress_sender.clone(),
          logger,
        );
      });
  }
}
