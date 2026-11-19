# Models from a fake Hugging Face Hub (nginx, static files, no network; earwig reads its base URL
# from EARWIG_HF_ENDPOINT). First a start without a model in sway's session: the settings open on
# Models and nothing is downloaded; the hotkey's overlay says there is no speech model, and while
# the clicked Download runs, how far it is; then the model loads and a dictation goes in. Then
# `earwig models search/download`.
{ self }:
{ pkgs, lib, ... }:
let
  common = import ./common.nix { inherit self; };
  sway = import ./sway.nix { inherit self; };
  catalog = (lib.importTOML ../../crates/earwig-core/models.toml).model;
  models = self.legacyPackages.${pkgs.stdenv.hostPlatform.system}.models;
  # The smallest curated model, for `earwig models download`, and the default, for the first
  # start, served from their pinned store paths.
  curated = lib.head (lib.sort (a: b: a.size < b.size) catalog);
  default = lib.findFirst (m: m.default or false) null catalog;
  rev = "0123456789abcdef0123456789abcdef01234567";

  hub = pkgs.runCommand "fake-hf-hub" { nativeBuildInputs = [ pkgs.python3 ]; } ''
    python3 ${./fake_hub.py} $out ${rev}
    mkdir -p $out/${curated.repo}/resolve/${curated.revision}
    ln -s ${models.${curated.id}} $out/${curated.repo}/resolve/${curated.revision}/${curated.file}
    mkdir -p $out/${default.repo}/resolve/${default.revision}
    ln -s ${models.${default.id}} $out/${default.repo}/resolve/${default.revision}/${default.file}
  '';
in
{
  name = "earwig-models";
  enableOCR = true;

  nodes = {
    hub = {
      networking.firewall.allowedTCPPorts = [ 80 ];
      services.nginx = {
        enable = true;
        virtualHosts.hub = {
          default = true;
          root = hub;
          locations = {
            # Slow enough for the hotkey to be pressed while the default model downloads.
            "/".extraConfig = ''
              limit_rate 20m;
            '';
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
      imports = [ sway.nodes.machine ];
      # Nothing preinstalled: the test downloads.
      services.earwig.models = [ ];
      systemd.user.services.earwig.environment.EARWIG_HF_ENDPOINT = "http://hub";
      environment.etc."earwig-test/tiny-asr.sha256".source = "${hub}/meta/tiny-asr.sha256";
      virtualisation.diskSize = 4096;
    };
  };

  testScript = ''
    ${common.prelude}

    rev = "${rev}"
    good = f"hf:test/tiny-asr-gguf/tiny-asr-Q8_0.gguf@{rev}"
    data = "/root/.local/share/earwig/models"
    HOTKEY = "meta_l-spc"
    HOTKEY_KEYS = HOTKEY


    def earwig(args: str) -> str:
        return machine.succeed(f"EARWIG_HF_ENDPOINT=http://hub earwig {args}")


    def fetched() -> str:
        """The model files the fake Hub was asked for."""
        return hub.succeed("grep -o 'GET /[^ ]*/resolve/[^ ]*' /var/log/nginx/access.log || true")


    def overlay_says(words: str) -> None:
        """Tap the hotkey until the overlay says `words`."""
        read = ""

        def said(_last_try: bool) -> bool:
            nonlocal read
            machine.send_key(HOTKEY)
            read = read_overlay()
            return words in read.lower()

        try:
            retry(said, timeout=td(minutes=1))
        finally:
            print(f"OCR of the overlay: {read!r}")


    start_all()
    hub.wait_for_unit("nginx.service")
    hub.wait_for_open_port(80)
    machine.wait_for_unit("multi-user.target")
    machine.wait_for_file(f"{RUNTIME}/wayland-1")
    machine.wait_for_file("/tmp/sway-ipc.sock")
    user_unit_active("sway-session.target")

    with machine.nested("EARWIG: started without a model, it opens the settings on Models"):
        user_unit_active("earwig.service")
        st = wait_status(
            lambda s: s["model"]["state"] == "missing" and s["hotkey"]["blocking"],
            "the model missing, the hotkey held back",
        )
        assert st["model"]["model"] == "${default.id}", st
        machine.wait_until_succeeds(
            "su - alice -c 'swaymsg -t get_tree' | jq -e '.. | objects | select(.name? == \"earwig settings\")'",
            timeout=60,
        )
        machine.wait_for_text("not downloaded yet")
        machine.screenshot("first_start")

    with machine.nested("EARWIG: the hotkey's overlay says there is no speech model; nothing is recorded or fetched"):
        events = watch_events()
        overlay_says("no speech model")
        machine.screenshot("no_model")
        seen = [json.loads(line) for line in machine.succeed(f"cat {events}").splitlines()]
        refused = [e for e in seen if e["event"] == "model-not-ready"]
        assert refused and refused[0]["model"]["state"] == "missing", seen
        assert not [e for e in seen if e["event"] == "state"], seen
        assert fetched() == "", fetched()
        machine.fail("find /home/alice/.local/share/earwig/models -type f | grep .")

    with machine.nested("EARWIG: Download fetches the model, the hotkey meanwhile says how far, then it loads"):
        # Not the pill's "Download one in Settings": it goes by itself.
        retry(lambda _last_try: "speech model" not in read_overlay().lower(), timeout=td(seconds=30))
        click_text("Download", negate=True)
        machine.wait_until_succeeds("find /home/alice/.local/share/earwig/models -name '*.part' | grep .", timeout=30)
        overlay_says("still downloading")
        machine.screenshot("downloading")
        wait_status(lambda s: s["model"]["state"] == "ready", "the downloaded model", timeout=600)
        print(fetched())
        assert "${default.file}" in fetched(), fetched()

    start_target_terminal()
    check_typing_reaches_target()

    ${common.dictation}

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
