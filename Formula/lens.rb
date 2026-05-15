# Lens — Homebrew tap formula
#
# Purpose: Distribute Lens via `brew install cfelmer-ctrlhwo/lens/lens` once a
#   GitHub Release exists. Today this formula is a SKELETON — the version,
#   URL, and sha256 are placeholders that will not produce a working install
#   until the v0.1.0 release lands.
# Process: When the first release is cut:
#   1. Update `version` to the released semver.
#   2. Update the `url` to the actual release tarball.
#   3. Replace the `sha256` placeholder with the real digest
#      (`shasum -a 256 lens-<version>-x86_64-apple-darwin.tar.gz`).
#   4. Verify `brew install --build-from-source ./Formula/lens.rb` succeeds locally.
#   5. Confirm `brew test lens` passes.
# Connections: GitHub Releases on cfelmer-ctrlhwo/lens publish the tarball this
#   formula downloads. The CI workflow at .github/workflows/ci.yml gates the
#   release. Tap consumers run `brew tap cfelmer-ctrlhwo/lens` to add it.
#
# Notes:
#   - Lens is macOS-only (Tauri 2 app); `depends_on :macos` enforces that.
#   - The placeholder sha256 is a string of 64 zeros so the Ruby syntax is
#     valid; brew will refuse to install until it's replaced with the real one.

class Lens < Formula
  desc "Mac-native AI activity dashboard"
  homepage "https://github.com/cfelmer-ctrlhwo/lens"
  # TODO: replace url + sha256 with real release artifact when v0.1.0 ships.
  # When the x86_64 tarball lands, swap this single url/sha256 pair for an
  # `on_arm do ... end` / `on_intel do ... end` split at the top level.
  url "https://github.com/cfelmer-ctrlhwo/lens/releases/download/v0.1.0/lens-0.1.0-aarch64-apple-darwin.tar.gz"
  version "0.1.0"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  license "MIT"

  depends_on arch: :arm64
  depends_on :macos

  def install
    # The release tarball is expected to contain a single `lens` binary at the
    # root. Adjust this if the eventual release uses a different layout.
    bin.install "lens"
  end

  test do
    # Smoke test: the binary should report its version. Will need updating
    # once Lens exposes a `--version` flag (Tauri apps don't always).
    assert_match version.to_s, shell_output("#{bin}/lens --version")
  end
end
