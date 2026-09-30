# The Homebrew cask for delightfile
# (plans/other-platforms/06-build-and-release.md B6.24). The release workflow
# fills in the version, the dmg's sha256 and the oldest macOS the bundle
# starts on (a bare macOS symbol means that release or later, in Homebrew's
# own style), and attaches the result to the release as delightfile.rb. It
# belongs in brianschwabauer/homebrew-tap as Casks/delightfile.rb, which is
# what makes `brew install --cask brianschwabauer/tap/delightfile` work.
#
# The app is signed ad hoc, not notarized, and Homebrew 6 quarantines what a
# cask installs with no flag to skip it (--no-quarantine is gone), so the
# caveats give the one command that lets macOS open it.
cask "delightfile" do
  version "@VERSION@"
  sha256 "@SHA256@"

  url "https://github.com/brianschwabauer/delightfile/releases/download/v#{version}/delightfile-#{version}-aarch64-macos.dmg"
  name "delightfile"
  desc "Keyboard-first file manager"
  homepage "https://github.com/brianschwabauer/delightfile"

  livecheck do
    url :url
    strategy :github_latest
  end

  depends_on arch: :arm64
  depends_on macos: :MACOS_MINIMUM

  app "delightfile.app"

  zap trash: [
    "~/.config/delightfile",
    "~/.local/state/delightfile",
  ]

  caveats <<~EOS
    delightfile is not signed by an Apple developer ID or notarized, so macOS
    will not open it until the quarantine Homebrew set is removed:

      xattr -dr com.apple.quarantine #{appdir}/delightfile.app

    Or open it once, then allow it in System Settings > Privacy & Security.
  EOS
end
