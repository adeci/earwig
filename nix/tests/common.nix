# Shared parts of the desktop VM tests: a user `alice` (uid 1000) with
# autologin on tty1, PipeWire with a virtual microphone, earwig through the
# NixOS module with the default model pinned, and Python helpers for the test
# script.
{ self }:
let
  # The microphone plays this clip. Prefer the recorded fixture; until it
  # exists, synthesize the same sentence.
  fixture = ../../tests/fixtures/quick-brown-fox.wav;
  sentence = "The quick brown fox jumps over the lazy dog.";
in
{
  inherit sentence;

  machine =
    { pkgs, lib, ... }:
    let
      clip =
        if builtins.pathExists fixture then
          fixture
        else
          pkgs.runCommand "quick-brown-fox.wav" { nativeBuildInputs = [ pkgs.espeak-ng ]; } ''
            espeak-ng -s 140 -w $out ${lib.escapeShellArg sentence}
          '';
    in
    {
      imports = [ self.nixosModules.default ];

      users.users.alice = {
        isNormalUser = true;
        uid = 1000;
        password = "foobar";
        extraGroups = [ "wheel" ];
      };
      services.getty.autologinUser = "alice";

      virtualisation = {
        memorySize = 4096;
        cores = 4;
        # virtio-gpu (no virgl) gives KMS for the compositor; Mesa renders
        # in software (pixman, llvmpipe, lavapipe for GPUI's Vulkan).
        qemu.options = [ "-vga none -device virtio-gpu-pci" ];
      };
      hardware.graphics.enable = true;

      services.earwig = {
        enable = true;
        # The VM has no GPU worth using; lavapipe would only be slower.
        settings.model.backend = "cpu";
      };

      security.rtkit.enable = true;
      services.pipewire = {
        enable = true;
        pulse.enable = true;
        wireplumber.enable = true;
        # A virtual microphone: whatever is played into the `test_mic_in`
        # sink comes out of the `test_mic` source, the only and therefore
        # default capture device. The graph runs on PipeWire's dummy driver
        # since the VM has no sound card.
        extraConfig.pipewire."90-test-mic" = {
          "context.modules" = [
            {
              name = "libpipewire-module-loopback";
              args = {
                "node.description" = "Test microphone";
                "capture.props" = {
                  "node.name" = "test_mic_in";
                  "media.class" = "Audio/Sink";
                  "audio.position" = [ "MONO" ];
                };
                "playback.props" = {
                  "node.name" = "test_mic";
                  "media.class" = "Audio/Source";
                  "audio.position" = [ "MONO" ];
                };
              };
            }
          ];
        };
      };

      environment.systemPackages = [
        pkgs.foot
        pkgs.pipewire
        pkgs.sox
        pkgs.wayland-utils
      ];
      environment.etc."earwig-test/clip.wav".source = clip;

      # Black on white for OCR, as in nixpkgs' sway test.
      fonts.packages = [ pkgs.inconsolata ];
      environment.etc."xdg/foot/foot.ini".text = lib.generators.toINI { } {
        main.font = "inconsolata:size=14";
        colors = {
          foreground = "000000";
          background = "ffffff";
        };
      };

      documentation.enable = false;
    };

  # Python helpers; the compositor-specific script comes after this.
  prelude = ''
    import shlex

    RUNTIME = "/run/user/1000"


    def as_alice(cmd: str) -> str:
        """Run `cmd` as alice in her session environment (no compositor env)."""
        return machine.succeed(
            f"su - alice -c {shlex.quote(f'XDG_RUNTIME_DIR={RUNTIME} {cmd}')}"
        )


    def spawn(cmd: str) -> None:
        """Start `cmd` in alice's systemd user manager, which holds the
        WAYLAND_DISPLAY the compositor imported."""
        machine.succeed(
            f"systemd-run --user -M alice@ --collect --quiet -- {cmd}"
        )


    def user_unit_active(unit: str) -> None:
        machine.wait_until_succeeds(f"systemctl --user -M alice@ is-active {unit}", timeout=120)


    def check_virtual_mic() -> None:
        """Record the virtual mic while the clip plays into it; it must not be silent."""
        with machine.nested("virtual microphone produces audio"):
            as_alice(
                "sh -c 'pw-record --target test_mic --rate 16000 --channels 1 /tmp/mic.wav & "
                "rec=$!; sleep 0.5; pw-play --target test_mic_in /etc/earwig-test/clip.wav; "
                "sleep 0.5; kill -INT $rec; wait $rec || true'"
            )
            stats = machine.succeed("sox /tmp/mic.wav -n stat 2>&1")
            print(stats)
            rms = float(
                next(l for l in stats.splitlines() if l.startswith("RMS     amplitude")).split()[-1]
            )
            assert rms > 0.01, f"virtual mic is silent (RMS {rms})"


    def start_target_terminal() -> None:
        """A focused foot whose input lands in /tmp/out byte by byte."""
        machine.succeed("rm -f /tmp/out")
        spawn("foot --app-id earwig-target sh -c 'stty -icanon; exec cat > /tmp/out'")
        machine.wait_for_file("/tmp/out")


    def dictate() -> str:
        """Hold the hotkey (via the CLI, like a compositor bind) over the clip;
        returns the OCR'd screen while recording."""
        as_alice("earwig press")
        machine.succeed(
            f"su - alice -c {shlex.quote(f'XDG_RUNTIME_DIR={RUNTIME} pw-play --target test_mic_in /etc/earwig-test/clip.wav')} >&2 &"
        )
        machine.sleep(2)
        machine.screenshot("overlay_recording")
        overlay_text = machine.get_screen_text()
        print(f"OCR while recording: {overlay_text!r}")
        machine.wait_until_fails("pgrep -x pw-play", timeout=30)
        machine.sleep(1)
        as_alice("earwig release")
        return overlay_text
  '';

  # Everything that needs earwig itself (daemon, pipeline, overlay, an
  # inserter); the steps before it only exercise the harness.
  dictation = ''
    with machine.nested("EARWIG: user service runs in the session"):
        try:
            user_unit_active("earwig.service")
        finally:
            print(machine.execute("journalctl _SYSTEMD_USER_UNIT=earwig.service --no-pager | tail -n 30")[1])
        print(as_alice("earwig status"))

    overlay_text = dictate()

    with machine.nested("EARWIG: overlay showed live text while recording"):
        assert any(w in overlay_text.lower() for w in ("quick", "brown", "fox")), overlay_text

    with machine.nested("EARWIG: transcript inserted into the focused terminal"):
        machine.wait_until_succeeds("grep -qi 'brown fox' /tmp/out", timeout=60)
        print(machine.succeed("cat /tmp/out"))
        machine.screenshot("inserted")
  '';
}
