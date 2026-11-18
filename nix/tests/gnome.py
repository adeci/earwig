TOKEN = "/home/alice/.local/state/earwig/remote-desktop.token"
HISTORY = "/home/alice/.local/share/earwig/history.jsonl"
# earwig's default GlobalShortcuts trigger, CTRL+space, in QEMU's key names.
SHORTCUT = "ctrl-spc"
REMOTE_DESKTOP = "Remote Desktop"
SHORTCUTS = "Add Keyboard Shortcuts"


def activate(key: str, value: str) -> None:
    """Give the window whose `key` ("title" or "app") is `value` the focus, as a click on it
    would."""
    field = {"title": "w.title", "app": "w.get_wm_class()"}[key]
    shell_js(
        "(global.get_window_actors().map(a => a.meta_window)"
        f".find(w => {field} === {json.dumps(value)}).activate(global.get_current_time()), true)"
    )


def answer(dialog: str, keys: list[str], shot: str) -> None:
    """Focus GNOME's `dialog` once it is up, press `keys` in it and wait until it closes."""
    wait_for(f"GNOME's {dialog} dialog", lambda: dialog in titles())
    activate("title", dialog)
    machine.sleep(2)
    machine.screenshot(shot)
    for key in keys:
        machine.send_key(key)
        machine.sleep(1)
    wait_for(f"the {dialog} dialog to close", lambda: dialog not in titles())


def overlays() -> list:
    """The overlay: an override-redirect X11 window (GPUI's WM_CLASS is the app id)."""
    return [
        w for w in windows() if w["override_redirect"] and w["x11"] and w["app"] == "earwig-overlay"
    ]


def dictate_by_shortcut(app: str, shot: str) -> dict:
    """With `app` focused, tap the shortcut (QEMU presses and releases it: a tap, which latches
    recording), play the clip, tap again once the overlay shows live text in its override-redirect
    window; returns the dictation's result, with what the pill said on a failure."""
    wait_for(app, lambda: focused() == app)
    machine.sleep(2)
    events = watch_events()
    machine.send_key(SHORTCUT)
    wait_status(lambda s: s["state"]["state"] == "recording", "recording")
    play_clip()
    heard = wait_live_text(shot)
    assert any(w in heard.lower() for w in ("quick", "brown", "fox")), heard
    assert overlays(), f"no override-redirect overlay window: {windows()}"
    machine.send_key(SHORTCUT)
    result = wait_result(events)
    if result["event"] == "insert-failed":
        # The pill shows the failure for under two seconds.
        result["pill"] = read_overlay()
    print(f"{shot}: {result}")
    return result


start_all()

with subtest("GNOME session"):
    gnome_session()

check_virtual_mic()
user_unit_active("earwig.service")

with subtest("EARWIG: GNOME asks for the shortcut and to type at start, and earwig answers meanwhile"):
    wait_for("both of GNOME's dialogs", lambda: {REMOTE_DESKTOP, SHORTCUTS} <= set(titles()))
    st = status()
    print(f"status: {st}")
    assert st["insert_methods"] == ["portal-paste"], st
    assert st["hotkey"]["source"] == "socket", st
    pid = earwig_pid()

with subtest("EARWIG: refusing both leaves earwig running, and its next start asks again"):
    answer(SHORTCUTS, ["alt-c"], "shortcut-cancel")
    answer(REMOTE_DESKTOP, ["esc"], "remote-desktop-escape")
    machine.sleep(3)
    assert status()["hotkey"]["source"] == "socket", status()
    machine.fail(f"test -e {TOKEN}")
    assert earwig_pid() == pid, "earwig restarted"
    user("systemctl --user restart earwig.service")
    wait_for("both of GNOME's dialogs again", lambda: {REMOTE_DESKTOP, SHORTCUTS} <= set(titles()))
    pid = earwig_pid()

with subtest("EARWIG: adding the shortcut binds it; typing is refused again"):
    answer(SHORTCUTS, ["alt-a"], "shortcut-add")
    wait_status(lambda s: s["hotkey"]["source"] == "portal", "the portal shortcut")
    answer(REMOTE_DESKTOP, ["esc"], "remote-desktop-escape-again")
    assert earwig_pid() == pid, "earwig restarted"

with subtest("EARWIG: the user service has the session's X11 display"):
    env = machine.succeed(f"tr '\\0' '\\n' < /proc/{pid}/environ")
    assert "\nDISPLAY=" in "\n" + env, "earwig.service has no DISPLAY"

with subtest("EARWIG: model ready"):
    wait_status(lambda s: s["model"]["state"] == "ready", "the model", timeout=300)

with subtest("EARWIG: a dictation while typing is refused says what to allow, and GNOME asks again"):
    as_alice("touch /tmp/editor.txt")
    spawn("/run/current-system/sw/bin/gnome-text-editor --standalone /tmp/editor.txt")
    failed = dictate_by_shortcut("org.gnome.TextEditor", "refused-recording")
    machine.screenshot("refused-overlay")
    assert failed["event"] == "insert-failed" and failed["on_clipboard"], failed
    assert "remote control" in failed["pill"].lower(), failed
    machine.succeed(f"grep -qi 'brown fox' {HISTORY}")
    wait_for("GNOME's dialog again", lambda: REMOTE_DESKTOP in titles())

with subtest("EARWIG: allowing remote interaction lets the next dictation in, without a restart"):
    answer(REMOTE_DESKTOP, ["alt-i", "alt-s"], "remote-desktop-share")
    machine.wait_for_file(TOKEN, timeout=td(seconds=30))
    activate("app", "org.gnome.TextEditor")
    inserted = dictate_by_shortcut("org.gnome.TextEditor", "editor-recording")
    assert inserted["event"] == "inserted" and inserted["method"] == "portal-paste", inserted
    assert focused() == "org.gnome.TextEditor", focused()
    machine.sleep(1)
    machine.send_key("ctrl-s")
    check_received("/tmp/editor.txt", inserted)
    machine.screenshot("editor-inserted")
    # It switched off as the recording stopped; once the text has landed its window is gone too.
    wait_for("the overlay to close", lambda: not overlays(), timeout=30)
    assert earwig_pid() == pid, "earwig restarted"

with subtest("EARWIG: dictation lands once in GNOME Console"):
    spawn("/run/current-system/sw/bin/kgx -- sh -c 'stty -icanon; exec cat >> /tmp/console.txt'")
    inserted = dictate_by_shortcut("org.gnome.Console", "console-recording")
    assert inserted["event"] == "inserted" and inserted["method"] == "portal-paste", inserted
    check_received("/tmp/console.txt", inserted)
    machine.screenshot("console-inserted")

with subtest("EARWIG: dictation lands once in foot, a terminal not on the accessibility bus"):
    start_target_terminal()
    inserted = dictate_by_shortcut("earwig-target", "foot-recording")
    assert inserted["event"] == "inserted" and inserted["method"] == "portal-paste", inserted
    check_received("/tmp/out", inserted)
    machine.screenshot("foot-inserted")

with subtest("EARWIG: after a restart nothing asks, and dictation lands"):
    user("systemctl --user restart earwig.service")
    user_unit_active("earwig.service")
    wait_status(
        lambda s: s["hotkey"]["source"] == "portal" and s["model"]["state"] == "ready",
        "the portal shortcut and the model again",
        timeout=300,
    )
    machine.sleep(10)
    assert not ({REMOTE_DESKTOP, SHORTCUTS} & set(titles())), titles()
    pid = earwig_pid()
    machine.succeed("truncate --no-create -s 0 /tmp/out")
    activate("app", "earwig-target")
    inserted = dictate_by_shortcut("earwig-target", "foot-after-restart")
    assert inserted["event"] == "inserted" and inserted["method"] == "portal-paste", inserted
    check_received("/tmp/out", inserted)

with subtest("EARWIG: keyboard access given while earwig runs takes insertion off the portal"):
    # What the udev rule's uaccess tag does once installed.
    machine.succeed("modprobe uinput && setfacl -m u:alice:rw /dev/uinput && setfacl -m u:alice:r /dev/input/event*")
    wait_status(lambda s: s["insert_methods"] == ["uinput-paste"], "insertion without the portal", timeout=30)
    machine.succeed("truncate --no-create -s 0 /tmp/out")
    inserted = dictate_by_shortcut("earwig-target", "uinput-recording")
    assert inserted["event"] == "inserted" and inserted["method"] == "uinput-paste", inserted
    check_received("/tmp/out", inserted)
    assert earwig_pid() == pid, "earwig restarted"
    print_latency()
