class Goldagent < Formula
  desc "GoldAgent local CLI assistant"
  homepage "https://github.com/GOLDhjy/GoldAgent"
  version "0.1.1"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/GOLDhjy/GoldAgent/releases/download/v0.1.1/goldagent-v0.1.1-macos-aarch64.tar.gz"
      sha256 "364b93d64b914d5b2aaa41c2616e3ce1fcf486c213b2d5fb0270581ce5cd7750"
    else
      url "https://github.com/GOLDhjy/GoldAgent/releases/download/v0.1.1/goldagent-v0.1.1-macos-x86_64.tar.gz"
      sha256 "b7aa942df312558460adabcf854bde601eb1265b14dd8cb20141bff601c04afa"
    end
  end

  on_linux do
    url "https://github.com/GOLDhjy/GoldAgent/releases/download/v0.1.1/goldagent-v0.1.1-linux-x86_64.tar.gz"
    sha256 "9955f6c6976a84e8cdde7e7731aa8ea18986ce19a9ec610918e574660e4d327a"
  end

  def install
    bin.install "goldagent"
  end

  test do
    assert_match "GoldAgent", shell_output("#{bin}/goldagent --help")
  end
end
