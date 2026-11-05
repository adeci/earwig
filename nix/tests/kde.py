BUS = "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus"
TOKEN = "/home/alice/.local/state/earwig/remote-desktop.token"
PORTAL_KDE = "org.freedesktop.impl.portal.desktop.kde"
# KWin::Layer (src/effect/globals.h); zwlr_layer_shell_v1 overlay surfaces
# land in OverlayLayer, xdg toplevels in NormalLayer.
KWIN_OVERLAY_LAYER = 9


def user(cmd: str) -> str:
    """Run `cmd` as alice with her session bus."""
    return machine.succeed(f"runuser -u alice -- env XDG_RUNTIME_DIR={RUNTIME} {BUS} {cmd}")


_scripts = [0]


def kwin(js: str):
    """Evaluate `js` (a function body that returns a value) in KWin's
    scripting engine; the result comes back as JSON through KWin's log."""
    _scripts[0] += 1
    n = _scripts[0]
    name = f"earwigtest{n}"
    marker = f"EARWIG-KWIN-{n}:"
    path = f"/tmp/{name}.js"
    body = f"console.warn({json.dumps(marker)} + JSON.stringify((function() {{ {js} }})()));\n"
    machine.succeed(f"cat > {path} <<'EOF'\n{body}EOF\nchmod 644 {path}")
    sid = int(user(f"busctl --user call org.kde.KWin /Scripting org.kde.kwin.Scripting loadScript ss {path} {name}").split()[1])
    user(f"busctl --user call org.kde.KWin /Scripting/Script{sid} org.kde.kwin.Script run")
    line = machine.wait_until_succeeds(
        f"journalctl _UID=1000 -o cat --no-pager | grep -F {shlex.quote(marker)}", timeout=td(seconds=30)
    )
    user(f"busctl --user call org.kde.KWin /Scripting org.kde.kwin.Scripting unloadScript s {name}")
    return json.loads(line.strip().splitlines()[-1].split(marker, 1)[1])


WINDOWS_JS = """
return workspace.windowList().map(w => ({
  caption: w.caption, cls: w.resourceClass, pid: w.pid, layer: w.layer,
  active: w.active, wantsInput: w.wantsInput, skipTaskbar: w.skipTaskbar,
  x: w.x, y: w.y, w: w.width, h: w.height,
}));
"""


def windows():
    return kwin(WINDOWS_JS)


def active():
    return kwin("const w = workspace.activeWindow; return w ? {caption: w.caption, cls: w.resourceClass} : null;")


def wait_active(cls: str, timeout: float = 120) -> None:
    deadline = time.monotonic() + timeout
    while True:
        a = active()
        if a and a["cls"] == cls:
            return
        if time.monotonic() > deadline:
            raise Exception(f"{cls} never got focus (active: {a}); windows: {windows()}")
        time.sleep(1)


def dialogs(title: str):
    return [w for w in windows() if w["cls"] == PORTAL_KDE and title in w["caption"]]


def accept_dialog(title: str, shot: str) -> None:
    """Wait for the portal dialog titled `title`, focus it, screenshot it and
    press Return (PortalDialog accepts on Return); it must close."""
    deadline = time.monotonic() + 120
    while not dialogs(title):
        if time.monotonic() > deadline:
            machine.screenshot(f"{shot}-missing")
            raise Exception(f"no {title!r} dialog; windows: {windows()}")
        time.sleep(1)
    kwin(
        f"const w = workspace.windowList().find(w => w.caption.includes({json.dumps(title)}));"
        " workspace.activeWindow = w; return true;"
    )
    machine.sleep(2)
    machine.screenshot(shot)
    machine.send_key("ret")
    deadline = time.monotonic() + 30
    while dialogs(title):
        if time.monotonic() > deadline:
            machine.screenshot(f"{shot}-stuck")
            raise Exception(f"{title!r} dialog did not close")
        time.sleep(1)


def tap() -> None:
    """Tap the dictation shortcut (earwig's default Ctrl+Space, bound through
    the portal): KWin sees a real press and release."""
    machine.send_key("ctrl-spc")


def dictate_by_key(target_cls: str, shot: str) -> str:
    """Tap to latch, speak the clip, tap again to stop once the overlay shows
    live text. Checks the overlay (a non-focusable layer-shell surface while
    `target_cls` keeps focus) and the insertion; returns the method used."""
    events = watch_events()
    tap()
    wait_status(lambda s: s["state"]["state"] == "recording" and s["state"]["latched"], "a latched recording", timeout=20)
    play_clip()
    wait_live_text(shot)

    pid = int(machine.succeed("pgrep -u alice -x earwig").split()[0])
    overlay = [w for w in windows() if w["pid"] == pid]
    focus = active()
    print(f"earwig windows while recording: {overlay}; focus: {focus}")
    assert len(overlay) == 1, overlay
    assert overlay[0]["layer"] == KWIN_OVERLAY_LAYER, f"overlay is not a layer-shell overlay: {overlay[0]}"
    assert not overlay[0]["wantsInput"] and not overlay[0]["active"], overlay[0]
    assert focus and focus["cls"] == target_cls, f"the overlay took focus: {focus}"

    tap()
    return wait_insertion(events)["method"]


start_all()

with subtest("Plasma session"):
    machine.wait_for_unit("multi-user.target")
    machine.wait_until_succeeds("pgrep -u alice plasmashell", timeout=td(minutes=3))
    machine.wait_until_succeeds(f"ls {RUNTIME}/wayland-*", timeout=td(minutes=1))
    user_unit_active("graphical-session.target")
    machine.sleep(5)
    machine.screenshot("00-desktop")

check_virtual_mic()

# The daemon starts serving its socket only once the RemoteDesktop dialog is
# answered, so `earwig status` would block until then.
user_unit_active("earwig.service")

with subtest("EARWIG: RemoteDesktop asks once; accepted, a restore token is saved"):
    accept_dialog("Remote Control Requested", "01-remote-desktop-dialog")
    machine.wait_for_file(TOKEN, timeout=td(seconds=30))

with subtest("EARWIG: GlobalShortcuts asks to bind the default shortcut; accepted"):
    accept_dialog("Global Shortcuts Requested", "02-global-shortcuts-dialog")
    # Only the daemon knows when the portal finished binding.
    print(wait_log(r"portal shortcut dictate: .*Space", timeout=30))

with subtest("EARWIG: status shows the portal paths"):
    st = status()
    print(f"status: {st}")
    assert st["insert_methods"] == ["portal-paste"], st
    assert "portal" in st["hotkey_sources"], st
    assert not dialogs("Remote Control"), "a second RemoteDesktop dialog"

with subtest("EARWIG: dictation lands in a focused KWrite document"):
    as_alice("touch /tmp/kwrite.txt")
    spawn("/run/current-system/sw/bin/kwrite /tmp/kwrite.txt")
    wait_active("org.kde.kwrite")
    machine.sleep(2)
    method = dictate_by_key("org.kde.kwrite", "03-kwrite-recording")
    assert method == "portal-paste", method
    machine.sleep(1)
    machine.screenshot("04-kwrite-inserted")
    machine.send_key("ctrl-s")
    machine.wait_until_succeeds("grep -qi 'brown fox' /tmp/kwrite.txt", timeout=td(seconds=30))
    print(machine.succeed("cat /tmp/kwrite.txt"))

with subtest("EARWIG: dictation lands in Konsole through the terminal chord"):
    spawn("/run/current-system/sw/bin/konsole -e sh -c 'stty -icanon; exec cat >> /tmp/konsole.txt'")
    wait_active("org.kde.konsole")
    machine.sleep(2)
    method = dictate_by_key("org.kde.konsole", "05-konsole-recording")
    assert method == "portal-paste", method
    machine.wait_until_succeeds("grep -qi 'brown fox' /tmp/konsole.txt", timeout=td(seconds=30))
    machine.screenshot("06-konsole-inserted")
    # Konsole was recognized as a terminal over AT-SPI: Ctrl+Shift+V, not a literal ^V.
    received = machine.succeed("cat -v /tmp/konsole.txt")
    print(received)
    assert "^V" not in received, received

with subtest("EARWIG: after a restart the restore token skips the dialog"):
    since = log_lines()
    user("systemctl --user restart earwig.service")
    user_unit_active("earwig.service")
    st = wait_status(lambda s: s["insert_methods"], "insertion methods")
    assert st["insert_methods"] == ["portal-paste"], st
    print(wait_log(r"portal shortcut dictate: .*Space", timeout=60, since=since))
    machine.sleep(10)
    assert not dialogs("Requested"), f"a consent dialog after restart: {windows()}"
    machine.screenshot("07-after-restart")
    wait_active("org.kde.konsole")
    method = dictate_by_key("org.kde.konsole", "08-konsole-after-restart")
    assert method == "portal-paste", method
    machine.wait_until_succeeds("test $(grep -oi 'brown fox' /tmp/konsole.txt | wc -l) -ge 2", timeout=td(seconds=30))
    machine.screenshot("09-konsole-after-restart")

print_latency()
