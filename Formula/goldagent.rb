class Goldagent < Formula
  desc "GoldAgent local CLI assistant"
  homepage "https://github.com/GOLDhjy/GoldAgent"
  version "0.1.2"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/GOLDhjy/GoldAgent/releases/download/v0.1.2/goldagent-v0.1.2-macos-aarch64.tar.gz"
      sha256 "3572cfc7553f50fa566328610fe97640b45147b42891d2305c3a2004ec1ef56d"
    else
      url "https://github.com/GOLDhjy/GoldAgent/releases/download/v0.1.2/goldagent-v0.1.2-macos-x86_64.tar.gz"
      sha256 "e436e3258fbb892d9f9513623bbf427503272d67969f646315f722e7ea18144a"
    end
  end

  on_linux do
    url "https://github.com/GOLDhjy/GoldAgent/releases/download/v0.1.2/goldagent-v0.1.2-linux-x86_64.tar.gz"
    sha256 "6fee12c5738d4ec09ac1229dd5d08c13e0ec7748dd5d2524693a4213e3ad5beb"
  end

  def install
    bin.install "goldagent"
  end

  test do
    assert_match "GoldAgent", shell_output("#{bin}/goldagent --help")
  end
end
