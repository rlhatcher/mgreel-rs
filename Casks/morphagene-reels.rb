# The app from this repo, so the repo works as a Homebrew tap:
#   brew tap rlhatcher/mgreel https://github.com/rlhatcher/mgreel-rs
#   brew trust --cask rlhatcher/mgreel/morphagene-reels
#   brew install --cask rlhatcher/mgreel/morphagene-reels
# The Release workflow sets version and sha256 after each release.
cask "morphagene-reels" do
  version "1.1.0"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"

  url "https://github.com/rlhatcher/mgreel-rs/releases/download/v#{version}/Morphagene-Reels-#{version}-arm64.dmg"
  name "Morphagene Reels"
  desc "Reel and splice manager for the Make Noise Morphagene"
  homepage "https://github.com/rlhatcher/mgreel-rs"

  livecheck do
    url :url
    strategy :github_latest
  end

  depends_on arch: :arm64
  depends_on :macos

  app "Morphagene Reels.app"

  # Releases are ad-hoc signed, not notarized, so macOS would block the first
  # launch. This clears the quarantine flag Homebrew puts on the download.
  postflight_steps do
    run "/usr/bin/xattr", args: ["-dr", "com.apple.quarantine", "{{appdir}}/Morphagene Reels.app"]
  end

  uninstall quit: "com.rlhatcher.mgreel"
end
