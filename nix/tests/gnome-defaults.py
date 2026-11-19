import re
from pathlib import Path

HOTKEY = "meta_l-spc"
HOTKEY_KEYS = HOTKEY
EXTENSION = "appindicatorsupport@rgcjonas.gmail.com"
WATCHER = "org.kde.StatusNotifierWatcher"
# The tray icon's blue, and its red while dictating (crates/earwig/src/ui/icon.rs).
IDLE = (0x35, 0x84, 0xE4)
DICTATING = (0xE5, 0x48, 0x4D)
# Where earwig keeps the spot the overlay was left at, and how far the test drags it.
SPOT = "/home/alice/.local/state/earwig/overlay.json"
DRAG = (-200, -250)


def box(actor: str) -> dict[str, float]:
    """Where a shell actor is on screen."""
    return shell_js(
        f"(() => {{ const a = {actor}; const [x, y] = a.get_transformed_position();"
        " const [w, h] = a.get_transformed_size(); return {x, y, w, h}; })()"
    )


def qmp_input(*events: Any) -> None:
    assert machine.qmp_client is not None
    args: Any = {"events": list(events)}
    machine.qmp_client.send("input-send-event", args)


def point_at(x: float, y: float) -> None:
    """Move the pointer to a point of the screen, through QEMU's tablet."""
    width, height = shell_js("[global.stage.width, global.stage.height]")
    qmp_input(
        {"type": "abs", "data": {"axis": "x", "value": round(x * 0x7FFF / (width - 1))}},
        {"type": "abs", "data": {"axis": "y", "value": round(y * 0x7FFF / (height - 1))}},
    )


def press(down: bool) -> None:
    time.sleep(0.2)
    qmp_input({"type": "btn", "data": {"down": down, "button": "left"}})


def click(x: float, y: float) -> None:
    """A left click at a point of the screen."""
    point_at(x, y)
    for down in (True, False):
        press(down)


def drag(x: float, y: float, by: tuple[float, float]) -> None:
    """Hold the left button from a point of the screen while the pointer moves `by`."""
    point_at(x, y)
    press(True)
    for step in range(1, 11):
        point_at(x + by[0] * step / 10, y + by[1] * step / 10)
    press(False)


def click_actor(actor: str) -> None:
    at = box(actor)
    click(at["x"] + at["w"] / 2, at["y"] + at["h"] / 2)


def close_settings() -> None:
    """Close the settings window as its close button would."""
    shell_js(
        "(global.get_window_actors().map(a => a.meta_window).filter(w => w.title === 'earwig settings')"
        ".forEach(w => w.delete(global.get_current_time())), true)"
    )
    wait_for("the settings window to close", lambda: "earwig settings" not in titles())


def asked() -> set[str]:
    """Dialogs and earwig settings windows that opened since the last call."""
    return {"Remote Desktop", "Add Keyboard Shortcuts", "earwig settings"} & set(
        shell_js("global.__opened.splice(0)")
    )


def on_bus(name: str) -> bool:
    """Whether a program owns `name` on alice's session bus."""
    return "true" in user(
        f"busctl --user call org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus NameHasOwner s {name}"
    )


def pixels(actor: str, colour: tuple[int, int, int]) -> int:
    """How many pixels of a shell actor's place on screen are `colour`."""
    at = box(actor)
    with machine._managed_screenshot() as shot:
        ppm = Path(shot).read_bytes()
    header = re.match(rb"P6\s+(\d+)\s+\d+\s+255\s", ppm)
    assert header, "the screenshot is not a binary PPM"
    width, raster = int(header[1]), ppm[header.end() :]
    return sum(
        all(abs(raster[(y * width + x) * 3 + i] - colour[i]) <= 8 for i in range(3))
        for y in range(round(at["y"]), round(at["y"] + at["h"]))
        for x in range(round(at["x"]), round(at["x"] + at["w"]))
    )


def tray_icon() -> str | None:
    """The top bar's actor for earwig's icon, once the tray lists earwig and draws it."""
    pid = earwig_pid()
    code, out = machine.execute(
        f"runuser -u alice -- env {BUS} busctl --user --json=short get-property "
        f"{WATCHER} /StatusNotifierWatcher {WATCHER} RegisteredStatusNotifierItems"
    )
    if code != 0 or not any(f"-{pid}-" in item for item in json.loads(out)["data"]):
        return None
    for key in shell_js("Object.keys(Main.panel.statusArea)"):
        actor = f"Main.panel.statusArea[{json.dumps(key)}]"
        if f"StatusNotifierItem-{pid}-" in key and pixels(actor, IDLE) >= 30:
            return actor
    return None


def gnome_untouched(tap: str) -> None:
    """Neither half of Super+Space reached GNOME: Super alone opens the overview, and the chord
    switches to the next input source."""
    machine.sleep(td(seconds=1))
    st = shell_js(
        "({overview: Main.overview.visible,"
        " source: Main.panel.statusArea.keyboard._inputSourceManager.currentSource.id})"
    )
    assert st == {"overview": False, "source": "us"}, f"{tap} reached GNOME: {st}"


def overlay_frame() -> dict[str, int] | None:
    """Where the overlay's window is on screen, while there is one."""
    frames = shell_js(
        "global.get_window_actors().map(a => a.meta_window).filter(w => w.get_wm_class() === 'earwig-overlay')"
        ".map(w => w.get_frame_rect()).map(r => ({x: r.x, y: r.y, w: r.width, h: r.height}))"
    )
    return frames[0] if frames else None


def near(frame: dict[str, int], x: int, y: int) -> bool:
    """Whether a window is at `x`, `y`, give or take the tablet's rounding."""
    return abs(frame["x"] - x) <= 2 and abs(frame["y"] - y) <= 2


def show_overlay() -> dict[str, int]:
    """Start a dictation from the command line; returns where its overlay is once it is drawn."""
    as_alice("earwig toggle")
    wait_for("the overlay", overlay_frame)
    machine.sleep(td(seconds=2))
    frame: dict[str, int] = wait_for("the overlay", overlay_frame)
    print(f"overlay: {frame}")
    return frame


def cancel_overlay() -> None:
    as_alice("earwig cancel")
    wait_for("the overlay to close", lambda: overlay_frame() is None)


def drag_overlay(frame: dict[str, int]) -> dict[str, int]:
    """Drag the overlay's pill by `DRAG`; returns where its window went."""
    x, y = frame["x"] + DRAG[0], frame["y"] + DRAG[1]
    # The pill hugs the bottom of its window 40 px in, one line tall while it listens.
    drag(frame["x"] + frame["w"] / 2, frame["y"] + frame["h"] - 80, DRAG)

    def followed() -> dict[str, int] | None:
        moved = overlay_frame()
        return moved if moved and near(moved, x, y) else None

    return wait_for("the overlay to follow the drag", followed, timeout=10)


def restart_earwig() -> None:
    user("systemctl --user restart earwig.service")
    wait_status(lambda s: s["model"]["state"] == "ready", "the model", timeout=300)


def remember_position(on: bool) -> None:
    """Restart earwig with `[overlay] remember_position` set in alice's own config."""
    as_alice("mkdir -p .config/earwig")
    machine.succeed(
        f"printf '[overlay]\\nremember_position = {str(on).lower()}\\n' > /home/alice/.config/earwig/config.toml"
    )
    restart_earwig()


start_all()

with subtest("GNOME session"):
    gnome_session()
    # Note every title a window takes from now on, to tell whether anything ever asks.
    shell_js(
        "(global.__opened = global.get_window_actors().map(a => a.meta_window.title),"
        " global.display.connect('window-created', (_d, w) => { global.__opened.push(w.title);"
        " w.connect('notify::title', () => global.__opened.push(w.title)); }), true)"
    )

check_virtual_mic()
user_unit_active("earwig.service")

with subtest("EARWIG: reads the keyboard itself and pastes without asking"):
    st = wait_status(lambda s: s["insert_methods"], "earwig to be ready to type", timeout=180)
    print(f"status: {st}")
    assert st["insert_methods"] == ["uinput-paste"], st
    assert st["hotkey"]["source"] == "keyboard" and st["hotkey"]["blocking"], st
    assert not (seen := asked()), seen

with subtest("EARWIG: its icon is in the top bar"):
    icon = wait_for("earwig's icon in the top bar", tray_icon)
    machine.screenshot("top-bar")

with subtest("EARWIG: the icon's menu opens the settings"):
    click_actor(icon)
    wait_for("the icon's menu", lambda: shell_js(f"{icon}.menu.isOpen"))
    machine.sleep(td(seconds=1))
    machine.screenshot("tray-menu")
    click_actor(f"{icon}.menu._getMenuItems().find(i => i.label?.text === 'Settings')")
    wait_for("the settings window in front", lambda: focused() == "io.github.adeci.earwig")
    machine.sleep(td(seconds=2))
    machine.screenshot("settings-from-tray")
    close_settings()
    # The one window the test asked for; it shows the record sees windows.
    assert (seen := asked()) == {"earwig settings"}, seen

with subtest("EARWIG: model ready"):
    wait_status(lambda s: s["model"]["state"] == "ready", "the model", timeout=300)

with subtest("EARWIG: Super+Space dictates into Text Editor and stays out of GNOME"):
    as_alice("touch /tmp/editor.txt")
    spawn("/run/current-system/sw/bin/gnome-text-editor --standalone /tmp/editor.txt")
    wait_for("the text editor", lambda: focused() == "org.gnome.TextEditor")
    machine.sleep(td(seconds=2))
    events = watch_events()
    machine.send_key(HOTKEY)
    wait_status(lambda s: s["state"]["state"] == "recording", "recording", timeout=30)
    gnome_untouched("the tap that starts")
    wait_for("the icon to turn red", lambda: pixels(icon, DICTATING) >= 30)
    play_clip()
    heard = wait_live_text("recording")
    assert any(w in heard.lower() for w in ("quick", "brown", "fox")), heard
    machine.send_key(HOTKEY)
    inserted = wait_insertion(events)
    assert inserted["method"] == "uinput-paste", inserted
    gnome_untouched("the tap that stops")
    assert focused() == "org.gnome.TextEditor", focused()
    machine.send_key("ctrl-s")
    machine.wait_until_succeeds("grep -qi 'brown fox' /tmp/editor.txt", timeout=td(seconds=30))
    saved = machine.succeed("cat /tmp/editor.txt")
    print(f"editor file: {saved!r}")
    # A Space that got through would be in the text.
    assert saved.strip("\n") == inserted["text"], saved
    machine.screenshot("inserted")
    print_latency()
    assert not (seen := asked()), seen

with subtest("EARWIG: dictation lands in foot, a terminal not on the accessibility bus"):
    start_target_terminal()
    wait_for("foot", lambda: focused() == "earwig-target")
    machine.sleep(td(seconds=2))
    events = watch_events()
    dictate()
    assert wait_insertion(events)["method"] == "uinput-paste"
    machine.wait_until_succeeds("grep -qi 'brown fox' /tmp/out", timeout=td(seconds=30))
    received = machine.succeed("cat -v /tmp/out")
    print(f"foot received: {received!r}")
    # Ctrl+V would arrive as a literal ^V, a Shift+Insert foot ignored as ^[[2;2~.
    assert "^V" not in received and "^[" not in received, received
    machine.screenshot("foot-inserted")
    assert not (seen := asked()), seen

with subtest("EARWIG: the icon appears when the tray starts after earwig"):
    user(f"gnome-extensions disable {EXTENSION}")
    wait_for("the tray to stop", lambda: not on_bus(WATCHER))
    user("systemctl --user restart earwig.service")
    wait_for(
        "earwig to offer its icon with no tray to take it",
        lambda: on_bus(f"org.kde.StatusNotifierItem-{earwig_pid()}-1"),
    )
    assert not on_bus(WATCHER)
    user(f"gnome-extensions enable {EXTENSION}")
    icon = wait_for("earwig's icon in the tray that started later", tray_icon)
    machine.screenshot("top-bar-tray-started-later")
    assert not (seen := asked()), seen

with subtest("EARWIG: a dragged overlay opens at its edge again next time"):
    wait_status(lambda s: s["model"]["state"] == "ready", "the model", timeout=300)
    home = show_overlay()
    drag_overlay(home)
    machine.screenshot("overlay-dragged")
    cancel_overlay()
    assert near(again := show_overlay(), home["x"], home["y"]), again
    cancel_overlay()
    machine.fail(f"test -e {SPOT}")

with subtest("EARWIG: remembering its position, the overlay opens where it was dragged, after a restart too"):
    remember_position(True)
    assert near(first := show_overlay(), home["x"], home["y"]), first
    dragged = drag_overlay(first)
    cancel_overlay()
    machine.succeed(f"test -s {SPOT}")
    assert near(again := show_overlay(), dragged["x"], dragged["y"]), again
    machine.screenshot("overlay-remembered")
    cancel_overlay()
    restart_earwig()
    assert near(again := show_overlay(), dragged["x"], dragged["y"]), again
    cancel_overlay()

with subtest("EARWIG: switched off, it forgets where it was left and opens at its edge"):
    remember_position(False)
    assert near(again := show_overlay(), home["x"], home["y"]), again
    cancel_overlay()
    machine.fail(f"test -e {SPOT}")
    assert not (seen := asked()), seen

with subtest("EARWIG: once it quits, Super+Space is GNOME's again"):
    as_alice("earwig quit")
    machine.wait_until_fails("systemctl --user -M alice@ is-active earwig.service", timeout=td(seconds=30))
    machine.send_key(HOTKEY)
    wait_for(
        "GNOME to switch the input source",
        lambda: shell_js("Main.panel.statusArea.keyboard._inputSourceManager.currentSource.id") == "de",
    )
