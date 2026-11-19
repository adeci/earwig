# Shared by the desktop VM tests: alice (uid 1000) logged in on tty1, PipeWire with a virtual
# mic, earwig from the NixOS module, and Python helpers for the test scripts.
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
        settings = {
          # The VM has no GPU worth using; lavapipe would only be slower.
          model.backend = "cpu";
          # Dark text on light for OCR, like foot below.
          appearance.theme = "light";
        };
      };

      security.rtkit.enable = true;
      services.pipewire = {
        enable = true;
        pulse.enable = true;
        wireplumber.enable = true;
        # A virtual mic: what plays into the test_mic_in sink comes out of the test_mic source,
        # the only (so default) capture device, on PipeWire's dummy driver (no sound card).
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
        # PipeWire 1.6.9's X11 bell crashes it when Xwayland exits (fixed in 1.6.10), as it does
        # 10 s after earwig's first window, whose GPU probe asks X in a VM (Mesa's device-select).
        extraConfig.pipewire."90-no-x11-bell"."context.properties"."module.x11.bell" = false;
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

  # GNOME on Wayland, alice logged in by GDM. The tests read the shell's windows and state
  # through Shell.Eval, which is internal API; a drop-in applies to GNOME 49's unit and to 50's
  # template alike.
  gnome =
    { pkgs, lib, ... }:
    {
      virtualisation.memorySize = lib.mkForce 6144;
      services.getty.autologinUser = lib.mkForce null;
      services.displayManager.gdm.enable = true;
      services.displayManager.autoLogin = {
        enable = true;
        user = "alice";
      };
      services.desktopManager.gnome.enable = true;
      services.gnome.gnome-initial-setup.enable = false;
      environment.gnome.excludePackages = [ pkgs.gnome-tour ];
      systemd.user.services."org.gnome.Shell@" = {
        overrideStrategy = "asDropin";
        # The shell keeps the session's PATH, where it finds the programs of desktop entries.
        enableDefaultPath = false;
        serviceConfig.ExecStart = [
          ""
          "${pkgs.gnome-shell}/bin/gnome-shell --unsafe-mode"
        ];
      };
      programs.dconf.profiles.user.databases = [
        {
          settings = {
            "org/gnome/desktop/session".idle-delay = lib.gvariant.mkUint32 0;
            "org/gnome/desktop/screensaver".lock-enabled = false;
            "org/gnome/desktop/notifications".show-banners = false;
          };
        }
      ];
    };

  # Plasma 6, alice logged in by its login manager. Setup as in nixpkgs' plasma6 test.
  plasma =
    { pkgs, lib, ... }:
    {
      services.getty.autologinUser = lib.mkForce null;
      services.xserver.enable = true;
      services.displayManager.plasma-login-manager.enable = true;
      services.desktopManager.plasma6.enable = true;
      services.displayManager.autoLogin = {
        enable = true;
        user = "alice";
      };
      environment.plasma6.excludePackages = with pkgs.kdePackages; [
        elisa
        # The welcome wizard would take focus on first login.
        plasma-welcome
      ];
      environment.systemPackages = [ pkgs.kdePackages.kate ];
      environment.etc."xdg/kscreenlockerrc".text = lib.generators.toINI { } {
        Daemon = {
          Autolock = false;
          LockOnResume = false;
        };
      };
      systemd.user.services.earwig.environment.RUST_LOG =
        "info,earwig=debug,earwig_core=debug,zbus=warn,tracing=warn";
    };

  # Python helpers; the compositor-specific script comes after this.
  prelude = ''
    import json
    import shlex
    import subprocess
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


    def play_clip() -> None:
        """Speak the clip into the virtual microphone, in the background; returns once it plays."""
        machine.succeed(
            f"su - alice -c {shlex.quote(f'XDG_RUNTIME_DIR={RUNTIME} pw-play --target test_mic_in /etc/earwig-test/clip.wav')} >&2 &"
        )
        machine.wait_until_succeeds("pgrep -x pw-play", timeout=td(seconds=30))


    def read_overlay() -> str:
        """OCR of the bottom 35% of the screen, where the overlay sits, scaled up: on the whole
        screen tesseract misses the pill's small text. One plain pass, to spare the host's CPU."""
        from pathlib import Path
        from test_driver.machine.ocr import perform_ocr_on_screenshot

        band = Path(machine.out_dir) / "ocr-band.png"
        # The driver's own raw screenshot (PPM), which its ImageMagick reads.
        with machine._managed_screenshot() as full:
            cropped = subprocess.run(
                ["magick", full, "-gravity", "south", "-crop", "100%x35%+0+0", "+repage",
                 "-filter", "Catrom", "-resize", "200%", band],
                capture_output=True,
                text=True,
            )
        if cropped.returncode != 0:
            raise Exception(f"cropping the screenshot: {cropped.stderr}")
        try:
            return perform_ocr_on_screenshot(band)
        finally:
            for f in Path(machine.out_dir).glob("ocr-*.png"):
                f.unlink()


    def wait_live_text(shot: str) -> str:
        """After the clip ends, OCR the screen until the overlay shows the
        live transcript (CPU decoding trails the audio); returns the text."""
        machine.wait_until_fails("pgrep -x pw-play", timeout=td(seconds=30))
        text = ""

        def shown(_last_try: bool) -> bool:
            nonlocal text
            text = read_overlay()
            # Any distinctive word of the clip; OCR over a busy background
            # often mangles one or two of them.
            return any(w in text.lower() for w in ("quick", "brown", "fox", "jumps", "lazy"))

        # CPU-only decoding in a busy VM host can trail the audio by a minute.
        try:
            retry(shown, timeout=td(minutes=4))
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


    def key(name: str, down: bool) -> None:
        """Put a key of QEMU's keyboard down or up. (sendkey only taps keys,
        and queues the next ones behind a key it holds.)"""
        from typing import Any

        assert machine.qmp_client is not None
        args: Any = {
            "events": [{"type": "key", "data": {"down": down, "key": {"type": "qcode", "data": name}}}]
        }
        machine.qmp_client.send("input-send-event", args)


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


    def wait_result(events: str, timeout: float = 240) -> dict:
        """Wait for the next dictation's result in `events`: `inserted` or `insert-failed`."""
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
        return result


    def wait_insertion(events: str, timeout: float = 240) -> dict:
        """Wait for the next dictation's result in `events`; it must be `inserted`."""
        result = wait_result(events, timeout)
        assert result["event"] == "inserted", result
        return result


    def check_received(path: str, inserted: dict) -> None:
        """Wait until the target has written the inserted text to `path`, once and alone: in
        `cat -v` a terminal shows Ctrl+V as ^V, a Shift+Insert it took for a key as ^[[2;2~."""
        received = ""

        def once(_last_try: bool) -> bool:
            nonlocal received
            received = machine.succeed(f"cat -v {path}")
            return received.strip("\n") == inserted["text"]

        try:
            retry(once, timeout=td(seconds=30))
        finally:
            print(f"{path}: {received!r}")


    def switch_insert_method(method: str) -> None:
        """Restart earwig with `[insert] method` set in alice's own config; returns once that is
        its only method and the model is ready."""
        as_alice("mkdir -p .config/earwig")
        machine.succeed(f"printf '[insert]\\nmethod = \"{method}\"\\n' > /home/alice/.config/earwig/config.toml")
        machine.succeed("systemctl --user -M alice@ restart earwig.service")
        wait_status(lambda s: s["insert_methods"] == [method] and s["model"]["state"] == "ready", method, timeout=300)


    def dictate_into_target(method: str) -> None:
        """Dictate into the focused foot from `start_target_terminal`, by `method`."""
        machine.succeed("truncate --no-create -s 0 /tmp/out")
        events = watch_events()
        dictate()
        inserted = wait_insertion(events)
        assert inserted["method"] == method, inserted
        check_received("/tmp/out", inserted)


    def dictate_into_editor(wait_focused) -> None:
        """Paste a dictation into Mousepad, a GTK 3 editor, once `wait_focused(app_id)` sees it
        focused, and save it."""
        as_alice("touch /tmp/editor.txt")
        spawn("/run/current-system/sw/bin/mousepad /tmp/editor.txt")
        wait_focused("org.xfce.mousepad")
        events = watch_events()
        dictate()
        inserted = wait_insertion(events)
        assert inserted["method"] == "paste", inserted
        # The editor reads the clipboard after the keys arrive.
        machine.sleep(1)
        machine.send_key("ctrl-s")
        check_received("/tmp/editor.txt", inserted)

    def cancel_mid_clip(start, cancel=lambda: as_alice("earwig cancel")) -> str:
        """Start a recording with `start()` while the clip plays and `cancel()` it once its live
        transcript has words, the engine mid-recording; then wait for idle and the clip's end.
        Returns the file of `watch_events`."""
        events = watch_events()
        start()
        play_clip()

        def heard(_last_try: bool) -> bool:
            return any(
                e["event"] == "transcript" and (e["committed"] or e["tentative"])
                for e in map(json.loads, machine.succeed(f"cat {events}").splitlines())
            )

        retry(heard, timeout=td(minutes=2))
        cancel()
        wait_status(lambda s: s["state"]["state"] == "idle", "idle after the cancel")
        machine.wait_until_fails("pgrep -x pw-play", timeout=td(seconds=30))
        return events


    def print_latency() -> None:
        """The daemon's per-dictation timings (only logged, so read from the journal)."""
        print(machine.succeed("journalctl _UID=1000 _SYSTEMD_USER_UNIT=earwig.service -o cat --no-pager | grep -E 'final text|done [0-9]+ ms' || true"))


    # The keys QEMU taps for the hotkey where earwig reads the keyboard
    # itself; None drives it through the CLI, as a compositor bind would.
    HOTKEY_KEYS: str | None = None


    def dictate() -> str:
        """Hold the hotkey over the clip (or tap `HOTKEY_KEYS` before and
        after it); returns the OCR'd screen while recording."""
        if HOTKEY_KEYS:
            machine.send_key(HOTKEY_KEYS)
            wait_status(lambda s: s["state"]["state"] == "recording", "recording", timeout=30)
        else:
            as_alice("earwig press")
        play_clip()
        text = wait_live_text("overlay_recording")
        if HOTKEY_KEYS:
            machine.send_key(HOTKEY_KEYS)
        else:
            as_alice("earwig release")
        return text


    def click_text(
        text: str, pick: int = 0, timeout: float = 60, negate: bool = False, left_of: int | None = None
    ) -> tuple[int, int]:
        """Left-click the middle of `text` (its words in a row, case-sensitive) where OCR finds it
        on screen, the `pick`-th match from the top, through QEMU's tablet; waits up to `timeout`
        seconds for it to show and returns the point clicked. `negate` reads light text on a dark
        ground, such as a filled button's; `left_of` clicks that many pixels left of the text
        instead of its middle."""
        import re
        import time
        from pathlib import Path
        from typing import Any

        words = text.split()
        matches: list[tuple[int, int]] = []
        screen = (0, 0)
        lines: dict[tuple[str, ...], list[tuple[str, int, int, int, int]]] = {}

        def located(_last_try: bool) -> bool:
            nonlocal matches, screen, lines
            large = Path(machine.out_dir) / "click-text.png"
            with machine._managed_screenshot() as shot:
                with open(shot, "rb") as raw:
                    header = re.match(rb"P6\s+(\d+)\s+(\d+)\s", raw.read(64))
                assert header, "the screenshot is not a binary PPM"
                screen = (int(header[1]), int(header[2]))
                # Twice as large: tesseract misses a window's small text.
                inverted = ["-negate"] if negate else []
                subprocess.run(["magick", shot, *inverted, "-filter", "Catrom", "-resize", "200%", large], check=True)
            lines = {}
            try:
                # Sparse text finds most words, the automatic layout some more.
                for layout in ("11", "3"):
                    tsv = subprocess.run(
                        ["tesseract", large, "-", "--psm", layout, "tsv"], capture_output=True, text=True, check=True
                    ).stdout
                    # Words by line, each with its box on screen.
                    for row in tsv.splitlines()[1:]:
                        cells = row.split("\t")
                        if len(cells) == 12 and cells[11].strip():
                            left, top, width, height = (int(cell) // 2 for cell in cells[6:10])
                            line = lines.setdefault(tuple([layout, *cells[1:5]]), [])
                            line.append((cells[11].strip(), left, top, width, height))
            finally:
                large.unlink()
            matches = []
            for line in lines.values():
                for i in range(len(line) - len(words) + 1):
                    run = line[i : i + len(words)]
                    if [word[0] for word in run] == words:
                        top = min(word[2] for word in run)
                        bottom = max(word[2] + word[4] for word in run)
                        right = run[-1][1] + run[-1][3]
                        x = run[0][1] - left_of if left_of is not None else (run[0][1] + right) // 2
                        y = (top + bottom) // 2
                        # Both layouts read most text: one match per place.
                        if all(abs(x - mx) > 8 or abs(y - my) > 8 for mx, my in matches):
                            matches.append((x, y))
            matches.sort(key=lambda point: (point[1], point[0]))
            return -len(matches) <= pick < len(matches)

        try:
            retry(located, timeout=td(seconds=timeout))
        except Exception:
            machine.screenshot("click-text-missing")
            read = " / ".join(" ".join(word[0] for word in line) for line in lines.values())
            raise Exception(f"no {text!r} (match {pick}) on screen; OCR read: {read}")
        x, y = matches[pick]
        width, height = screen

        def send(*events: Any) -> None:
            assert machine.qmp_client is not None
            args: Any = {"events": list(events)}
            machine.qmp_client.send("input-send-event", args)

        send(
            {"type": "abs", "data": {"axis": "x", "value": round(x * 0x7FFF / (width - 1))}},
            {"type": "abs", "data": {"axis": "y", "value": round(y * 0x7FFF / (height - 1))}},
        )
        for down in (True, False):
            time.sleep(0.2)
            send({"type": "btn", "data": {"down": down, "button": "left"}})
        return x, y
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

    with machine.nested("EARWIG: transcript inserted once into the focused terminal"):
        inserted = wait_insertion(events)
        print(inserted)
        check_received("/tmp/out", inserted)
        machine.screenshot("inserted")
  '';
}
