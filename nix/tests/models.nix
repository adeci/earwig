# `earwig models search/download` against a fake Hugging Face Hub (nginx,
# static files, no network). earwig reads the Hub base URL from
# EARWIG_HF_ENDPOINT.
{ self }:
{ pkgs, lib, ... }:
let
  catalog = (lib.importTOML ../../crates/earwig-core/models.toml).model;
  models = self.legacyPackages.${pkgs.stdenv.hostPlatform.system}.models;
  # The smallest curated model, served from its pinned store path.
  curated = lib.head (lib.sort (a: b: a.size < b.size) catalog);
  rev = "0123456789abcdef0123456789abcdef01234567";

  hub = pkgs.runCommand "fake-hf-hub" { nativeBuildInputs = [ pkgs.python3 ]; } ''
    python3 ${./fake_hub.py} $out ${rev}
    mkdir -p $out/${curated.repo}/resolve/${curated.revision}
    ln -s ${models.${curated.id}} $out/${curated.repo}/resolve/${curated.revision}/${curated.file}
  '';
in
{
  name = "earwig-models";

  nodes = {
    hub = {
      networking.firewall.allowedTCPPorts = [ 80 ];
      services.nginx = {
        enable = true;
        virtualHosts.hub = {
          default = true;
          root = hub;
          locations = {
            # Search only answers the transcribe.cpp filter, like the Hub
            # call earwig is supposed to make.
            "= /api/models".extraConfig = ''
              if ($arg_filter != "transcribe.cpp") { return 400; }
              default_type application/json;
              alias ${hub}/api/models.json;
            '';
            "/api/".extraConfig = ''
              default_type application/json;
            '';
          };
        };
      };
      documentation.enable = false;
    };

    machine = {
      imports = [ self.nixosModules.default ];
      services.earwig = {
        enable = true;
        # Nothing preinstalled: the test downloads.
        models = [ ];
      };
      environment.etc."earwig-test/tiny-asr.sha256".source = "${hub}/meta/tiny-asr.sha256";
      environment.systemPackages = [ pkgs.curl ];
      virtualisation.diskSize = 4096;
      documentation.enable = false;
    };
  };

  testScript = ''
    rev = "${rev}"
    good = f"hf:test/tiny-asr-gguf/tiny-asr-Q8_0.gguf@{rev}"
    data = "/root/.local/share/earwig/models"


    def earwig(args: str) -> str:
        return machine.succeed(f"EARWIG_HF_ENDPOINT=http://hub earwig {args}")

    start_all()
    hub.wait_for_unit("nginx.service")
    hub.wait_for_open_port(80)
    machine.wait_for_unit("multi-user.target")

    with machine.nested("fake Hub answers like the real one"):
        machine.succeed("curl -fsS 'http://hub/api/models?filter=transcribe.cpp&search=asr' | grep -q tiny-asr-gguf")
        machine.fail("curl -fsS 'http://hub/api/models?search=asr'")
        machine.succeed(f"curl -fsS http://hub/api/models/test/tiny-asr-gguf/revision/main | grep -q {rev}")
        machine.succeed(f"curl -fsS 'http://hub/api/models/test/tiny-asr-gguf/tree/{rev}?recursive=true' | grep -q tiny-asr-Q8_0.gguf")
        headers = machine.succeed(
            f"curl -fsS -D - -o /tmp/head.bin -r 0-65535 http://hub/test/tiny-asr-gguf/resolve/{rev}/tiny-asr-Q8_0.gguf"
        )
        assert " 206 " in headers.splitlines()[0], headers
        machine.succeed("head -c 4 /tmp/head.bin | grep -q GGUF")

    with machine.nested("EARWIG: search marks compatible files"):
        out = earwig("models search asr")
        print(out)
        rows = {l.split()[1]: l.split() for l in out.splitlines() if l.startswith("test/")}
        assert rows["tiny-asr-Q8_0.gguf"][-1] == "yes", out
        assert rows["llm-Q8_0.gguf"][-2:] == ["llama", "no"], out

    with machine.nested("EARWIG: download a Hub file, verified against its LFS sha256"):
        path = earwig(f"models download {good}").strip().splitlines()[-1]
        assert path == f"{data}/test/tiny-asr-gguf/{rev}/tiny-asr-Q8_0.gguf", path
        expected = machine.succeed("cat /etc/earwig-test/tiny-asr.sha256").split()[0]
        assert machine.succeed(f"sha256sum {path}").split()[0] == expected

    with machine.nested("EARWIG: a file whose hash does not match is rejected and removed"):
        machine.fail(f"EARWIG_HF_ENDPOINT=http://hub earwig models download hf:test/corrupt-gguf/corrupt-Q8_0.gguf@{rev}")
        machine.fail(f"find {data}/test/corrupt-gguf -type f | grep .")

    with machine.nested("EARWIG: download a curated model by id"):
        path = earwig("models download ${curated.id}").strip().splitlines()[-1]
        assert path == f"{data}/${curated.repo}/${curated.revision}/${curated.file}", path
        assert machine.succeed(f"sha256sum {path}").split()[0] == "${curated.sha256}"
        out = earwig("models list")
        print(out)
        assert "${curated.id}" in out and "installed" in out, out
  '';
}
