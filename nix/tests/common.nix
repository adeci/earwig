# Shared parts of the desktop VM tests: a user `alice` (uid 1000) with
# autologin on tty1, PipeWire with a virtual microphone, earwig through the
# NixOS module with the default model pinned, and Python helpers for the test
# script.
{ self }:
{
  machine =
    { pkgs, lib, ... }:
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
        cores = 8;
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
      environment.etc."earwig-test/clip.wav".source = ../../tests/fixtures/quick-brown-fox.wav;

      # Black on white for OCR, as in nixpkgs' sway test.
      fonts.packages = [ pkgs.inconsolata ];
      environment.etc."xdg/foot/foot.ini".text = lib.generators.toINI { } {
        main.font = "inconsolata:size=14";
        colors-dark = {
          foreground = "000000";
          background = "ffffff";
        };
      };

      documentation.enable = false;
    };

  # Python helpers; the compositor-specific script comes after this.
  prelude = ''
    import json
    import re
    import shlex
    import time
    from datetime import timedelta as td

    RUNTIME = "/run/user/1000"


    def as_alice(cmd: str) -> str:
        """Run `cmd` as alice in her session environment (no compositor env)."""
        return machine.succeed(
            f"su - alice -c {shlex.quote(f'XDG_RUNTIME_DIR={RUNTIME} {cmd}')}"
        )


    def earwig_log() -> str:
        """The daemon's log (the user service's journal)."""
        return machine.succeed("journalctl _UID=1000 _SYSTEMD_USER_UNIT=earwig.service -o cat --no-pager")


    def log_lines() -> int:
        return len(earwig_log().splitlines())


    def wait_log(pattern: str, timeout: float = 60, since: int = 0) -> str:
        """Wait for a daemon log line matching `pattern` after the first
        `since` lines; returns the line."""
        deadline = time.monotonic() + timeout
        while True:
            lines = earwig_log().splitlines()[since:]
            for line in lines:
                if re.search(pattern, line):
                    return line
            if time.monotonic() > deadline:
                print("\n".join(lines[-60:]))
                raise Exception(f"the daemon never logged {pattern!r}")
            time.sleep(0.5)


    def play_clip() -> None:
        """Speak the clip into the virtual microphone, in the background."""
        machine.succeed(
            f"su - alice -c {shlex.quote(f'XDG_RUNTIME_DIR={RUNTIME} pw-play --target test_mic_in /etc/earwig-test/clip.wav')} >&2 &"
        )


    def wait_live_text(shot: str) -> str:
        """After the clip ends, OCR the screen until the overlay shows the
        live transcript (CPU decoding trails the audio); returns the text."""
        machine.wait_until_fails("pgrep -x pw-play", timeout=td(seconds=30))
        text = ""

        def shown(_last_try: bool) -> bool:
            nonlocal text
            text = machine.get_screen_text()
            return any(w in text.lower() for w in ("quick", "brown", "fox"))

        try:
            retry(shown, timeout=td(seconds=90))
        finally:
            print(f"OCR while recording: {text!r}")
            machine.screenshot(shot)
        return text


    def spawn(cmd: str) -> None:
        """Start `cmd` in alice's systemd user manager, which holds the
        WAYLAND_DISPLAY the compositor imported."""
        machine.succeed(
            f"systemd-run --user -M alice@ --collect --quiet -- {cmd}"
        )


    def user_unit_active(unit: str) -> None:
        machine.wait_until_succeeds(f"systemctl --user -M alice@ is-active {unit}", timeout=td(minutes=2))


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
        # systemd resolves the executable without the session's PATH.
        spawn("/run/current-system/sw/bin/foot --app-id earwig-target sh -c 'stty -icanon; exec cat >> /tmp/out'")
        try:
            machine.wait_for_file("/tmp/out", timeout=td(minutes=1))
        except Exception:
            print(machine.execute("journalctl _UID=1000 --no-pager | tail -n 40")[1])
            raise


    def check_typing_reaches_target() -> None:
        """Keys typed into the focused window land in /tmp/out; then empty it."""
        with machine.nested("focused terminal receives keyboard input"):
            machine.send_chars("probe")
            machine.wait_until_succeeds("grep -q probe /tmp/out", timeout=td(seconds=30))
            # --no-create: fs.protected_regular denies root O_CREAT on
            # alice's file in sticky /tmp.
            machine.succeed("truncate --no-create -s 0 /tmp/out")


    def status() -> dict:
        return json.loads(as_alice("earwig status"))


    def wait_status(predicate, what: str, timeout: float = 60) -> dict:
        """Poll `earwig status` until `predicate(status)` holds; returns it."""
        st = {}

        def check(_last_try: bool) -> bool:
            nonlocal st
            code, out = machine.execute(f"su - alice -c {shlex.quote(f'XDG_RUNTIME_DIR={RUNTIME} earwig status')}")
            st = json.loads(out) if code == 0 else {}
            return bool(st) and bool(predicate(st))

        try:
            retry(check, timeout=td(seconds=timeout))
        except Exception:
            raise Exception(f"timed out waiting for {what}; status: {st}")
        return st


    def watch_events() -> str:
        """Stream daemon events into a fresh file in the background; returns
        its path once the subscription is live."""
        path = machine.succeed("mktemp -u /tmp/earwig-events-XXXXXX").strip()
        machine.succeed(
            f"su - alice -c {shlex.quote(f'XDG_RUNTIME_DIR={RUNTIME} earwig events > {path}')} >&2 &"
        )
        machine.wait_until_succeeds(f"test -s {path}", timeout=td(seconds=30))
        return path


    def wait_insertion(events: str, timeout: float = 120) -> dict:
        """Wait for the next dictation result in `events`; it must be `inserted`."""
        result = {}

        def done(_last_try: bool) -> bool:
            nonlocal result
            for line in machine.succeed(f"cat {events}").splitlines():
                event = json.loads(line)
                if event["event"] in ("inserted", "insert-failed"):
                    result = event
                    return True
            return False

        retry(done, timeout=td(seconds=timeout))
        assert result["event"] == "inserted", result
        return result


    def print_latency() -> None:
        """The daemon's per-dictation timings (only logged, so read from the journal)."""
        print(machine.succeed("journalctl _UID=1000 _SYSTEMD_USER_UNIT=earwig.service -o cat --no-pager | grep -E 'final text|done [0-9]+ ms' || true"))


    def dictate() -> str:
        """Hold the hotkey (via the CLI, like a compositor bind) over the clip;
        returns the OCR'd screen while recording."""
        as_alice("earwig press")
        play_clip()
        text = wait_live_text("overlay_recording")
        as_alice("earwig release")
        return text
  '';

  # Everything that needs earwig itself (daemon, pipeline, overlay, an
  # inserter); the steps before it only exercise the harness.
  dictation = ''
    with machine.nested("EARWIG: user service runs in the session"):
        try:
            user_unit_active("earwig.service")
        finally:
            print(machine.execute("journalctl _SYSTEMD_USER_UNIT=earwig.service --no-pager | tail -n 30")[1])
        print(wait_status(lambda s: s["insert_methods"], "an insertion method"))

    events = watch_events()
    overlay_text = dictate()

    with machine.nested("EARWIG: overlay showed live text while recording"):
        assert any(w in overlay_text.lower() for w in ("quick", "brown", "fox")), overlay_text

    with machine.nested("EARWIG: transcript inserted into the focused terminal"):
        print(wait_insertion(events))
        machine.wait_until_succeeds("grep -qi 'brown fox' /tmp/out", timeout=td(minutes=1))
        print(machine.succeed("cat /tmp/out"))
        machine.screenshot("inserted")
  '';
}
