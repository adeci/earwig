TOKEN = "/home/alice/.local/state/earwig/remote-desktop.token"
HISTORY = "/home/alice/.local/share/earwig/history.jsonl"
REMOTE_CONTROL = "Remote Control Requested"
SHORTCUTS = "Global Shortcuts Requested"
# KWin::Layer (src/effect/globals.h); zwlr_layer_shell_v1 overlay surfaces
# land in OverlayLayer, xdg toplevels in NormalLayer.
KWIN_OVERLAY_LAYER = 9


def answer(title: str, button: str | None, shot: str) -> None:
    """Wait for the portal's dialog titled `title`, press `button` in it (Return accepts) and
    wait until it closes."""
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
    if button:
        click_text(button, pick=-1)
    else:
        machine.send_key("ret")
    deadline = time.monotonic() + 30
    while dialogs(title):
        if time.monotonic() > deadline:
            machine.screenshot(f"{shot}-stuck")
            raise Exception(f"{title!r} dialog did not close")
        time.sleep(1)


def earwig_pid() -> int:
    return int(machine.succeed("pgrep -u alice -x earwig").split()[0])


def dictate_by_key(target_cls: str, shot: str) -> dict:
    """With `target_cls` focused, tap the shortcut (earwig's default Ctrl+Space, bound through
    the portal) to latch, speak the clip and tap again once the overlay shows live text. Checks
    the overlay (a non-focusable layer-shell surface while the target keeps focus, gone once the
    dictation is done); returns the dictation's result, with what the pill said on a failure."""
    wait_active(target_cls)
    machine.sleep(2)
    events = watch_events()
    machine.send_key("ctrl-spc")
    wait_status(lambda s: s["state"]["state"] == "recording" and s["state"]["latched"], "a latched recording", timeout=20)
    play_clip()
    wait_live_text(shot)

    pid = earwig_pid()
    overlay = [w for w in windows() if w["pid"] == pid]
    focus = active()
    print(f"earwig windows while recording: {overlay}; focus: {focus}")
    assert len(overlay) == 1, overlay
    assert overlay[0]["layer"] == KWIN_OVERLAY_LAYER, f"overlay is not a layer-shell overlay: {overlay[0]}"
    assert not overlay[0]["wantsInput"] and not overlay[0]["active"], overlay[0]
    assert focus == target_cls, f"the overlay took focus: {focus}"

    machine.send_key("ctrl-spc")
    result = wait_result(events)
    if result["event"] == "insert-failed":
        # The pill shows the failure for under two seconds.
        result["pill"] = read_overlay()
    print(f"{shot}: {result}")
    deadline = time.monotonic() + 30
    while left := [w for w in windows() if w["pid"] == pid]:
        if time.monotonic() > deadline:
            raise Exception(f"the overlay stayed up after the dictation: {left}")
        time.sleep(1)
    return result


start_all()

with subtest("Plasma session"):
    plasma_session()
    machine.screenshot("00-desktop")

check_virtual_mic()
user_unit_active("earwig.service")

with subtest("EARWIG: KDE asks for the shortcut and to type at start, and earwig answers meanwhile"):
    deadline = time.monotonic() + 120
    while not (dialogs(REMOTE_CONTROL) and dialogs(SHORTCUTS)):
        if time.monotonic() > deadline:
            raise Exception(f"KDE did not ask for both: {windows()}")
        time.sleep(1)
    st = status()
    print(f"status: {st}")
    assert st["insert_methods"] == ["portal-paste"], st
    assert st["hotkey"]["source"] == "socket", st
    pid = earwig_pid()

with subtest("EARWIG: a shortcut cancelled in KDE's dialog works once given a key in its settings"):
    answer(SHORTCUTS, "Cancel", "01-shortcut-cancel")
    machine.sleep(3)
    assert status()["hotkey"]["source"] == "socket", status()
    # Where the settings window points: System Settings → Keyboard → Shortcuts, earwig's page.
    spawn("/run/current-system/sw/bin/systemsettings kcm_keys --args io.github.adeci.earwig")
    wait_active("systemsettings")
    machine.sleep(3)
    machine.screenshot("02-shortcut-settings")
    # Expand Dictate, tick its default shortcut and apply.
    click_text("Dictate:")
    machine.sleep(1)
    click_text("Ctrl+Space")
    machine.sleep(1)
    machine.screenshot("03-shortcut-set")
    click_text("Apply", pick=-1)
    wait_status(lambda s: s["hotkey"]["source"] == "portal", "the portal shortcut", timeout=30)
    kwin("workspace.windowList().filter(w => w.resourceClass === 'systemsettings').forEach(w => w.closeWindow()); return true;")
    assert earwig_pid() == pid, "earwig restarted"

with subtest("EARWIG: Cancel in KDE's Remote Control dialog leaves earwig running"):
    answer(REMOTE_CONTROL, "Cancel", "04-remote-control-cancel")
    machine.fail(f"test -e {TOKEN}")
    assert earwig_pid() == pid, "earwig restarted"

with subtest("EARWIG: model ready"):
    wait_status(lambda s: s["model"]["state"] == "ready", "the model", timeout=300)

with subtest("EARWIG: a dictation while typing is refused says what to allow, and KDE asks again"):
    as_alice("touch /tmp/kwrite.txt")
    spawn("/run/current-system/sw/bin/kwrite /tmp/kwrite.txt")
    failed = dictate_by_key("org.kde.kwrite", "05-refused-recording")
    assert failed["event"] == "insert-failed" and failed["on_clipboard"], failed
    assert "remote control" in failed["pill"].lower(), failed
    machine.succeed(f"grep -qi 'brown fox' {HISTORY}")
    answer(REMOTE_CONTROL, None, "06-remote-control-allow")
    machine.wait_for_file(TOKEN, timeout=td(seconds=30))

with subtest("EARWIG: the next dictation lands once in KWrite, without a restart"):
    activate("org.kde.kwrite")
    inserted = dictate_by_key("org.kde.kwrite", "07-kwrite-recording")
    assert inserted["event"] == "inserted" and inserted["method"] == "portal-paste", inserted
    machine.sleep(1)
    machine.send_key("ctrl-s")
    check_received("/tmp/kwrite.txt", inserted)
    assert earwig_pid() == pid, "earwig restarted"

with subtest("EARWIG: dictation lands once in Konsole"):
    spawn("/run/current-system/sw/bin/konsole -e sh -c 'stty -icanon; exec cat >> /tmp/konsole.txt'")
    inserted = dictate_by_key("org.kde.konsole", "08-konsole-recording")
    assert inserted["event"] == "inserted" and inserted["method"] == "portal-paste", inserted
    check_received("/tmp/konsole.txt", inserted)

with subtest("EARWIG: dictation lands once in foot, a terminal not on the accessibility bus"):
    start_target_terminal()
    inserted = dictate_by_key("earwig-target", "09-foot-recording")
    assert inserted["event"] == "inserted" and inserted["method"] == "portal-paste", inserted
    check_received("/tmp/out", inserted)

with subtest("EARWIG: after a restart nothing asks, and dictation lands"):
    user("systemctl --user restart earwig.service")
    user_unit_active("earwig.service")
    wait_status(
        lambda s: s["hotkey"]["source"] == "portal" and s["model"]["state"] == "ready",
        "the portal shortcut and the model again",
        timeout=300,
    )
    machine.sleep(10)
    no_dialog()
    assert status()["insert_methods"] == ["portal-paste"], status()
    machine.succeed("truncate --no-create -s 0 /tmp/out")
    activate("earwig-target")
    inserted = dictate_by_key("earwig-target", "10-foot-after-restart")
    assert inserted["event"] == "inserted" and inserted["method"] == "portal-paste", inserted
    check_received("/tmp/out", inserted)

print_latency()
