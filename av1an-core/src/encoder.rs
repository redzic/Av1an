pub trait Encoder {
  /// Creates an FFmpeg pipe for the arguments provided
  fn compose_ffmpeg_pipe() {}
}

/// libaom reference AV1 encoder
pub struct Aom {}

impl Encoder for Aom {}
