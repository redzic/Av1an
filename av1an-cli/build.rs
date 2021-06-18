use vergen::{vergen, Config, SemverKind, ShaKind};

fn main() {
  let mut config = Config::default();
  // Change the SHA output to the short variant
  *config.git_mut().sha_kind_mut() = ShaKind::Short;
  // Change the SEMVER output to the lightweight variant
  *config.git_mut().semver_kind_mut() = SemverKind::Lightweight;
  // Add a `-dirty` flag to the SEMVER output
  *config.git_mut().semver_dirty_mut() = Some("-dirty");

  // Generate the instructions
  vergen(config).unwrap();
}
