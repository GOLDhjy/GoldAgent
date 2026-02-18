class Goldagent < Formula
  desc "GoldAgent local CLI assistant"
  homepage "https://github.com/GOLDhjy/GoldAgent"
  url "https://github.com/GOLDhjy/GoldAgent/archive/refs/heads/main.tar.gz"
  version "main"
  sha256 :no_check

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args(path: ".")
  end

  test do
    assert_match "GoldAgent", shell_output("#{bin}/goldagent --help")
  end
end
