# Homebrew formula for baba-yaga (also installed as `bay`).
#
# Lives in the tap repo shoaibyte/homebrew-tap as Formula/baba-yaga.rb; see
# packaging/README.md. Before each release, set `version` and replace every
# sha256 placeholder with the hash in the matching .sha256 file attached to
# the GitHub release.
class BabaYaga < Formula
  desc "Dependency vulnerability patrol for Switch rooms"
  homepage "https://github.com/shoaibyte/baba-yaga"
  version "0.2.0"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/shoaibyte/baba-yaga/releases/download/v#{version}/baba-yaga-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_SHA256_OF_aarch64-apple-darwin.tar.gz"
    end
    on_intel do
      url "https://github.com/shoaibyte/baba-yaga/releases/download/v#{version}/baba-yaga-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_SHA256_OF_x86_64-apple-darwin.tar.gz"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/shoaibyte/baba-yaga/releases/download/v#{version}/baba-yaga-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "REPLACE_WITH_SHA256_OF_x86_64-unknown-linux-gnu.tar.gz"
    end
  end

  depends_on "osv-scanner"

  def install
    bin.install "baba-yaga", "bay"
    pkgshare.install "baba-yaga.example.toml"
  end

  def caveats
    <<~EOS
      In the directory that should hold .env and baba-yaga.toml, run:
        bay init
        bay doctor
        bay serve
      An example config is in #{opt_pkgshare}.
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/bay --version")
    assert_match "doctor", shell_output("#{bin}/baba-yaga --help")
  end
end
