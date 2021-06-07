use std::path::Path;
use std::process::Command;
use std::sync::mpsc::Sender;

use crate::encoder::generic::TwoPassEncoder;
use crate::encoder::TwoPassProgress;

pub struct Aom<'a>(
  pub  TwoPassEncoder<
    'a,
    fn(&str) -> Option<usize>,
    fn(&str) -> Option<usize>,
    fn(&[&str], &Path) -> Command,
    fn(&[&str], (&Path, &Path)) -> Command,
  >,
);

impl<'a> Aom<'a> {
  pub fn new(progress_sender: Sender<TwoPassProgress>, encoder_args: &'a [&'a str]) -> Self {
    Self(TwoPassEncoder::new(
      parse_first_pass_frames,
      parse_second_pass_frames,
      // TODO remove unused parameter
      |encoder_args, output_file| {
        let mut first_pass = Command::new("aomenc");
        first_pass.args(&["--passes=2", "--pass=1"]);
        first_pass.args(encoder_args);
        first_pass.arg(format!(
          "--fpf={}",
          output_file.as_os_str().to_string_lossy()
        ));
        first_pass.args(&["-o", "/dev/null", "-"]);
        first_pass
      },
      |encoder_args, output_file| {
        let mut second_pass = Command::new("aomenc");
        second_pass.args(&["-", "--passes=2", "--pass=2"]);
        second_pass.args(encoder_args);
        second_pass.arg(format!(
          "--fpf={}",
          output_file.0.as_os_str().to_string_lossy()
        ));
        second_pass.arg("-o");
        second_pass.arg(output_file.1);
        second_pass
      },
      progress_sender,
      encoder_args,
    ))
  }
}

const IRRELEVANT_CHARS: &str = "Pass -/- frame  ---/";
const FIRST_PASS_CHARS: &str = "Pass 1/2 frame";

/// Parse the number of frames encoded by aomenc for the second pass.
fn parse_second_pass_frames(s: &str) -> Option<usize> {
  if cfg!(any(target_arch = "x86", target_arch = "x86_64")) {
    unsafe { extract_second_pass_frames_sse2(s) }
  } else {
    extract_second_pass_frames_fallback(s)
  }
}

/// Parse the number of frames encoded by aomenc for the first pass.
fn parse_first_pass_frames(s: &str) -> Option<usize> {
  if cfg!(any(target_arch = "x86", target_arch = "x86_64")) {
    unsafe { extract_first_pass_frames_sse2(s) }
  } else {
    extract_first_pass_frames_fallback(s)
  }
}

fn extract_first_pass_frames_fallback(s: &str) -> Option<usize> {
  s.get(FIRST_PASS_CHARS.len()..IRRELEVANT_CHARS.len() - 1)?
    .split_ascii_whitespace()
    .map(|s| s.parse().ok())
    .next()?
}

/// SSE2/AVX implementation of first pass frame parsing.
#[target_feature(enable = "sse2")]
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
unsafe fn extract_first_pass_frames_sse2(s: &str) -> Option<usize> {
  #[cfg(target_arch = "x86")]
  use std::arch::x86::*;
  #[cfg(target_arch = "x86_64")]
  use std::arch::x86_64::*;

  let _check = s.as_bytes().get(..FIRST_PASS_CHARS.len() + 16)?;

  let s_subslice = s.get_unchecked(FIRST_PASS_CHARS.len()..);

  // Unaligned load which reads 16 bytes.
  let data = _mm_loadu_si128(s_subslice.as_ptr() as *const __m128i);
  let mask = _mm_set1_epi8(b' ' as i8);
  let cmp = _mm_cmpeq_epi8(data, mask);

  let spaces = _mm_movemask_epi8(cmp).trailing_ones();

  s.get_unchecked(FIRST_PASS_CHARS.len() + spaces as usize..IRRELEVANT_CHARS.len() - 1)
    .parse()
    .ok()
}

fn extract_second_pass_frames_fallback(s: &str) -> Option<usize> {
  let relevant_chars = s.get(IRRELEVANT_CHARS.len()..)?;
  relevant_chars
    .get(..relevant_chars.find(' ')?)?
    .parse()
    .ok()
}

/// SSE2/AVX implementation of second pass frame parsing.
#[target_feature(enable = "sse2")]
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
// TODO make all of this just accept byte slices
unsafe fn extract_second_pass_frames_sse2(s: &str) -> Option<usize> {
  #[cfg(target_arch = "x86")]
  use std::arch::x86::*;
  #[cfg(target_arch = "x86_64")]
  use std::arch::x86_64::*;

  // subslice of the string with the irrelevant bits taken out
  let s_subslice = s.get(IRRELEVANT_CHARS.len()..)?;

  // Check if 16 bytes are available upfront before doing
  // unaligned load. In this regard, this version technically
  // is not semantically equivalent if the 16 bytes are not available.
  // However, the output of aomenc should always be such that
  // these bytes are available, so in practice it should be the same.
  //
  // This bounds check actually reduces the intructions
  // generated by 1 instruction on znver1.
  let _check = s_subslice.as_bytes().get(..16)?;

  let data = _mm_loadu_si128(s_subslice.as_ptr() as *const __m128i);
  let mask = _mm_set1_epi8(b' ' as i8);
  let cmp = _mm_cmpeq_epi8(data, mask);
  let digits = _mm_movemask_epi8(cmp).trailing_zeros();

  s_subslice.get_unchecked(..digits as usize).parse().ok()
}
