# Dictation end to end on GNOME 50 (Wayland), the portal-only desktop:
# virtual mic -> earwig -> focused GNOME Text Editor and GNOME Console.
#
# What GNOME exercises that the wlroots tests do not: the RemoteDesktop
# portal + libei inserter (consent dialog once, Ctrl+V / Ctrl+Shift+V by
# AT-SPI role), the GlobalShortcuts portal as the hotkey source (bind dialog,
# Activated/Deactivated), and the overlay as an override-redirect X11 popup
# through XWayland (Mutter has no layer-shell).
{ self }:
let
  common = import ./common.nix { inherit self; };
  # earwig's default GlobalShortcuts trigger, CTRL+space, in QEMU's key names.
  key = "ctrl-spc";
in
{
  name = "earwig-gnome";
  enableOCR = true;

  nodes.machine =
    { pkgs, lib, ... }:
    {
      imports = [ common.machine ];

      virtualisation.memorySize = lib.mkForce 6144;

      # GDM logs alice in, not the tty.
      services.getty.autologinUser = lib.mkForce null;
      services.displayManager.gdm.enable = true;
      services.displayManager.autoLogin = {
        enable = true;
        user = "alice";
      };
      services.desktopManager.gnome.enable = true;
      services.gnome.gnome-initial-setup.enable = false;
      environment.gnome.excludePackages = [ pkgs.gnome-tour ];

      # The test reads focus and window geometry through Shell.Eval, which
      # is internal API.
      systemd.user.services."org.gnome.Shell@".serviceConfig.ExecStart = [
        ""
        "${pkgs.gnome-shell}/bin/gnome-shell --unsafe-mode"
      ];

      programs.dconf.profiles.user.databases = [
        {
          settings = {
            "org/gnome/desktop/session".idle-delay = lib.gvariant.mkUint32 0;
            "org/gnome/desktop/screensaver".lock-enabled = false;
            "org/gnome/desktop/notifications".show-banners = false;
          };
        }
      ];

      # No portal_trigger here: the test binds earwig's default, which must
      # not collide with GNOME's own shortcuts.
      systemd.user.services.earwig.environment.RUST_LOG = "info,earwig_core=debug";
    };

  testScript = ''
    ${common.prelude}

    KEY = "${key}"
    TOKEN = "/home/alice/.local/state/earwig/remote-desktop.token"


    def shell_js(js: str):
        """Evaluate JavaScript in gnome-shell; Eval returns JSON."""
        out = machine.succeed(
            "su - alice -c "
            + shlex.quote(
                f"DBUS_SESSION_BUS_ADDRESS=unix:path={RUNTIME}/bus gdbus call --session "
                f"-d org.gnome.Shell -o /org/gnome/Shell -m org.gnome.Shell.Eval {shlex.quote(js)}"
            )
        ).strip()
        assert out.startswith("(true, '") and out.endswith("')"), out
        return json.loads(out[len("(true, '") : -len("')")].replace("\\'", "'"))


    WINDOWS_JS = (
        "global.get_window_actors().map(a => { const w = a.meta_window; const r = w.get_frame_rect();"
        " return {title: w.title, wm_class: w.get_wm_class(), x11: w.get_client_type() === 1,"
        " override_redirect: w.is_override_redirect(), x: r.x, y: r.y, w: r.width, h: r.height,"
        " focus: w.has_focus()}; })"
    )


    def windows():
        return shell_js(WINDOWS_JS)


    def focused_class():
        return shell_js("global.display.focus_window?.get_wm_class() ?? null")


    def wait_for(predicate, what: str, timeout: int = 60) -> None:
        for _ in range(timeout * 2):
            if predicate():
                return
            machine.sleep(td(milliseconds=500))
        print(json.dumps(windows()))
        print(daemon_log())
        machine.screenshot(f"failed-{what.replace(' ', '_')}")
        raise Exception(f"timed out waiting for {what}")


    def daemon_log() -> str:
        return machine.execute("journalctl _SYSTEMD_USER_UNIT=earwig.service --no-pager -o cat")[1]


    def tap_shortcut() -> None:
        # QEMU presses and releases the chord: Activated then Deactivated,
        # a tap under 300 ms, which latches recording in the default
        # hold-or-toggle mode; the next tap stops it.
        machine.send_key(KEY)


    def dictate_by_shortcut(name: str) -> str:
        """Tap, play the clip into the virtual mic, wait until the overlay
        shows live text (CPU decoding trails the audio), tap again."""
        tap_shortcut()
        wait_status(lambda s: s["state"]["state"] == "recording", "recording")
        play_clip()
        try:
            text = wait_live_text(f"{name}-recording")
        finally:
            print(f"windows while recording ({name}): {json.dumps(windows())}")
        tap_shortcut()
        return text


    start_all()

    with machine.nested("GNOME session"):
        machine.wait_for_unit("display-manager.service")
        machine.wait_for_file(f"{RUNTIME}/wayland-0")
        machine.wait_until_succeeds(
            "su - alice -c "
            + shlex.quote(
                f"DBUS_SESSION_BUS_ADDRESS=unix:path={RUNTIME}/bus gdbus call --session "
                "-d org.gnome.Shell -o /org/gnome/Shell -m org.gnome.Shell.Eval Main.layoutManager._startingUp"
            )
            + " | grep -q 'true,..false'",
            timeout=td(minutes=3),
        )
        machine.send_key("esc")
        print(machine.succeed(
            "su - alice -c "
            + shlex.quote(f"DBUS_SESSION_BUS_ADDRESS=unix:path={RUNTIME}/bus gsettings get "
                          "org.gnome.desktop.wm.keybindings switch-input-source")
        ))

    check_virtual_mic()

    with machine.nested("EARWIG: daemon asks for RemoteDesktop and the shortcut, once"):
        user_unit_active("earwig.service")
        accepted = set()

        def accept_dialogs() -> bool:
            titles = [w["title"] for w in windows()]
            if "Remote Desktop" in titles and "remote" not in accepted:
                machine.sleep(2)
                machine.screenshot("consent-remote-desktop")
                machine.send_key("alt-i")
                machine.sleep(1)
                machine.send_key("alt-s")
                accepted.add("remote")
            elif "Add Keyboard Shortcuts" in titles and "shortcuts" not in accepted:
                machine.sleep(2)
                machine.screenshot("consent-shortcuts")
                machine.send_key("alt-a")
                accepted.add("shortcuts")
            titles = [w["title"] for w in windows()]
            return (
                accepted == {"remote", "shortcuts"}
                and "Remote Desktop" not in titles
                and "Add Keyboard Shortcuts" not in titles
                and machine.execute(f"test -s {TOKEN}")[0] == 0
                # Only the daemon knows when the portal finished binding.
                and "portal shortcut dictate" in daemon_log()
            )

        wait_for(accept_dialogs, "both portal consents", timeout=180)
        st = wait_status(lambda s: s["insert_methods"], "insertion methods")
        print(f"status: {st}")
        assert st["insert_methods"] == ["portal-paste"], st
        assert "portal" in st["hotkey_sources"], st

    with machine.nested("EARWIG: the user service has the session's X11 display"):
        pid = machine.succeed("systemctl --user -M alice@ show -p MainPID --value earwig.service").strip()
        env = machine.succeed(f"tr '\\0' '\\n' < /proc/{pid}/environ")
        print(f"earwig environment: {[l for l in env.splitlines() if 'DISPLAY' in l]}")
        assert "\nDISPLAY=" in "\n" + env, "earwig.service has no DISPLAY"

    with machine.nested("EARWIG: model ready"):
        wait_status(lambda s: s["model"]["state"] == "ready", "the model", timeout=300)

    with machine.nested("EARWIG: dictation into GNOME Text Editor"):
        as_alice("touch /tmp/editor.txt")
        spawn("/run/current-system/sw/bin/gnome-text-editor --standalone /tmp/editor.txt")
        wait_for(lambda: focused_class() == "org.gnome.TextEditor", "the text editor")
        machine.sleep(2)
        events = watch_events()
        overlay_text = dictate_by_shortcut("editor")
        overlay = [w for w in windows() if w["override_redirect"] and w["x11"]]
        assert overlay, "no override-redirect overlay window"
        assert any(w in overlay_text.lower() for w in ("quick", "brown", "fox")), overlay_text
        # CPU decoding in the VM finishes well after the stop tap. Save only
        # once the daemon reports the insertion: a Ctrl+S from QEMU racing the
        # Ctrl+V from libei would mix modifier state.
        assert wait_insertion(events)["method"] == "portal-paste"
        assert focused_class() == "org.gnome.TextEditor", focused_class()
        machine.sleep(1)
        machine.send_key("ctrl-s")
        machine.wait_until_succeeds("grep -qi 'brown fox' /tmp/editor.txt", timeout=td(seconds=30))
        print(f"editor file: {machine.succeed('cat /tmp/editor.txt')!r}")
        machine.screenshot("editor-inserted")

    with machine.nested("EARWIG: dictation into GNOME Console (Ctrl+Shift+V)"):
        spawn(
            "/run/current-system/sw/bin/kgx -- sh -c 'stty -icanon; exec cat >> /tmp/console.txt'"
        )
        wait_for(lambda: focused_class() == "org.gnome.Console", "the console")
        machine.sleep(2)
        events = watch_events()
        dictate_by_shortcut("console")
        assert wait_insertion(events)["method"] == "portal-paste"
        machine.wait_until_succeeds("grep -qi 'brown fox' /tmp/console.txt", timeout=td(minutes=1))
        received = machine.succeed("cat -v /tmp/console.txt")
        print(f"console received: {received!r}")
        # Ctrl+Shift+V by AT-SPI role; plain Ctrl+V would arrive as a literal ^V.
        assert "^V" not in received, received
        machine.screenshot("console-inserted")
        print_latency()
  '';
}
