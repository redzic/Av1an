pub trait Encoder {
  /// Creates an FFmpeg pipe for the arguments provided
  fn compose_ffmpeg_pipe() {}
}

/// libaom reference AV1 encoder
pub struct Aom {}

impl Encoder for Aom {}

const IRRELEVANT_CHARS: &str = "Pass 2/2 frame  XXX/";

const FIRST_PASS_CHARS: &str = "Pass 1/2 frame";

pub fn extract_second_pass_frame(s: &str) -> Option<usize> {
  if cfg!(any(target_arch = "x86", target_arch = "x86_64")) {
    unsafe { extract_second_pass_frame_sse2(s) }
  } else {
    extract_second_pass_frame_fallback(s)
  }
}

pub fn extract_first_pass_frame(s: &str) -> Option<usize> {
  if cfg!(any(target_arch = "x86", target_arch = "x86_64")) {
    unsafe { extract_first_pass_frame_sse2(s) }
  } else {
    extract_first_pass_frame_fallback(s)
  }
}

fn extract_first_pass_frame_fallback(s: &str) -> Option<usize> {
  s.get(FIRST_PASS_CHARS.len()..IRRELEVANT_CHARS.len() - 1)?
    .split_ascii_whitespace()
    .map(|s| s.parse().ok())
    .next()?
}

/// SSE2 implementation of first pass frame parsing.
#[target_feature(enable = "sse2")]
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
unsafe fn extract_first_pass_frame_sse2(s: &str) -> Option<usize> {
  #[cfg(target_arch = "x86")]
  use std::arch::x86::*;
  #[cfg(target_arch = "x86_64")]
  use std::arch::x86_64::*;

  let _check = s.as_bytes().get(..FIRST_PASS_CHARS.len() + 16)?;

  let s_subslice = s.get_unchecked(FIRST_PASS_CHARS.len()..);

  // Unaligned SSE2 load, which reads 16 bytes.
  let data = _mm_loadu_si128(std::mem::transmute::<*const u8, *const __m128i>(
    s_subslice.as_ptr(),
  ));
  let mask = _mm_set1_epi8(b' ' as i8);
  let cmp = _mm_cmpeq_epi8(data, mask);

  let spaces = _mm_movemask_epi8(cmp).trailing_ones();

  s.get_unchecked(FIRST_PASS_CHARS.len() + spaces as usize..IRRELEVANT_CHARS.len() - 1)
    .parse()
    .ok()
}

fn extract_second_pass_frame_fallback(s: &str) -> Option<usize> {
  let relevant_chars = s.get(IRRELEVANT_CHARS.len()..)?;
  relevant_chars
    .get(..relevant_chars.find(' ')?)?
    .parse()
    .ok()
}

/// SSE2/AVX implementation of second pass frame parsing.
///
/// If AVX is available, it uses the AVX versions. Otherwise,
/// it generates SSE2 instructions.
#[target_feature(enable = "sse2")]
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
unsafe fn extract_second_pass_frame_sse2(s: &str) -> Option<usize> {
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

  let data = _mm_loadu_si128(std::mem::transmute::<*const u8, *const __m128i>(
    s_subslice.as_ptr(),
  ));
  let mask = _mm_set1_epi8(b' ' as i8);
  let cmp = _mm_cmpeq_epi8(data, mask);
  let digits = _mm_movemask_epi8(cmp).trailing_zeros();

  s_subslice.get_unchecked(..digits as usize).parse().ok()
}
