import ast
import re
import time
from pathlib import Path
from typing import Any

BUS = f"DBUS_SESSION_BUS_ADDRESS=unix:path={RUNTIME}/bus"
HOTKEY = "meta_l-spc"
EXTENSION = "appindicatorsupport@rgcjonas.gmail.com"
WATCHER = "org.kde.StatusNotifierWatcher"
# The tray icon's blue, and its red while dictating (crates/earwig/src/ui/icon.rs).
IDLE = (0x35, 0x84, 0xE4)
DICTATING = (0xE5, 0x48, 0x4D)


def user(cmd: str) -> str:
    """Run `cmd` as alice with her session bus."""
    return machine.succeed(f"runuser -u alice -- env XDG_RUNTIME_DIR={RUNTIME} {BUS} {cmd}")


def shell_js(js: str) -> Any:
    """Evaluate JavaScript in gnome-shell; Eval returns the value as JSON."""
    out = user(
        f"gdbus call --session -d org.gnome.Shell -o /org/gnome/Shell -m org.gnome.Shell.Eval {shlex.quote(js)}"
    ).strip()
    assert out.startswith("(true, "), out
    return json.loads(ast.literal_eval(out[len("(true, ") : -1]))


def shell_started(_last_try: bool) -> bool:
    code, out = machine.execute(
        f"runuser -u alice -- env {BUS} gdbus call --session -d org.gnome.Shell -o /org/gnome/Shell "
        "-m org.gnome.Shell.Eval Main.layoutManager._startingUp"
    )
    return code == 0 and out.strip() == "(true, 'false')"


def windows() -> list[dict[str, Any]]:
    return shell_js(
        "global.get_window_actors().map(a => a.meta_window)"
        ".map(w => ({title: w.title, app: w.get_wm_class(), focus: w.has_focus()}))"
    )


def titles() -> list[str]:
    return [w["title"] for w in windows()]


def focused() -> str | None:
    return shell_js("global.display.focus_window?.get_wm_class() ?? null")


def wait_for(what: str, predicate, timeout: float = 60):
    """Poll until `predicate()` gives something and return it."""
    deadline = time.monotonic() + timeout
    while not (got := predicate()):
        if time.monotonic() > deadline:
            machine.screenshot(f"failed-{what.replace(' ', '-')}")
            print(earwig_log())
            raise Exception(f"timed out waiting for {what}; windows: {windows()}")
        time.sleep(0.5)
    return got


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


def click(x: float, y: float) -> None:
    """A left click at a point of the screen, through QEMU's tablet."""
    width, height = shell_js("[global.stage.width, global.stage.height]")
    qmp_input(
        {"type": "abs", "data": {"axis": "x", "value": round(x * 0x7FFF / (width - 1))}},
        {"type": "abs", "data": {"axis": "y", "value": round(y * 0x7FFF / (height - 1))}},
    )
    for down in (True, False):
        time.sleep(0.2)
        qmp_input({"type": "btn", "data": {"down": down, "button": "left"}})


def click_actor(actor: str) -> None:
    at = box(actor)
    click(at["x"] + at["w"] / 2, at["y"] + at["h"] / 2)


def click_word(word: str) -> None:
    """Click `word` where OCR finds it on screen, twice as large for the small text of a window."""
    with machine._managed_screenshot() as shot:
        large = Path(shot).with_suffix(".large.png")
        subprocess.run(["magick", shot, "-filter", "Catrom", "-resize", "200%", large], check=True)
        tsv = subprocess.run(
            ["tesseract", large, "-", "--psm", "11", "tsv"], capture_output=True, text=True, check=True
        ).stdout
        large.unlink()
    for row in tsv.splitlines()[1:]:
        cells = row.split("\t")
        if len(cells) == 12 and cells[11].strip() == word:
            left, top, width, height = (int(cell) / 2 for cell in cells[6:10])
            click(left + width / 2, top + height / 2)
            return
    raise Exception(f"no {word!r} on screen")


def try_status() -> dict[str, Any]:
    """`earwig status`, or nothing while the daemon does not answer."""
    code, out = machine.execute(f"runuser -u alice -- env XDG_RUNTIME_DIR={RUNTIME} earwig status")
    return json.loads(out) if code == 0 else {}


def may_type(st: dict[str, Any]) -> bool:
    return bool(st.get("insert_methods")) and all(
        p["access"] == "granted" for p in st.get("permissions", [])
    )


def allow_typing() -> dict[str, Any]:
    """Give earwig what it needs to type, where it asks: its own Allow button, then GNOME's
    Remote Desktop dialog. Returns the status once it may type."""
    done: set[str] = set()

    def answered():
        open_windows = titles()
        if "Remote Desktop" in open_windows:
            if "desktop" not in done:
                # As a click on the dialog would: it then has the keys.
                shell_js(
                    "(global.get_window_actors().map(a => a.meta_window)"
                    ".find(w => w.title === 'Remote Desktop').activate(global.get_current_time()), true)"
                )
                machine.sleep(td(seconds=2))
                machine.screenshot("consent")
                machine.send_key("alt-i")
                machine.sleep(td(seconds=1))
                machine.send_key("alt-s")
                done.add("desktop")
            return None
        st = try_status()
        if (
            "earwig" not in done
            and "earwig settings" in open_windows
            and any(p["access"] != "granted" for p in st.get("permissions", []))
        ):
            machine.sleep(td(seconds=2))
            click_word("Allow")
            done.add("earwig")
        return st if may_type(st) else None

    return wait_for("earwig to be allowed to type", answered, timeout=180)


def close_settings() -> None:
    """Close the settings window as its close button would."""
    shell_js(
        "(global.get_window_actors().map(a => a.meta_window).filter(w => w.title === 'earwig settings')"
        ".forEach(w => w.delete(global.get_current_time())), true)"
    )
    wait_for("the settings window to close", lambda: "earwig settings" not in titles())


def on_bus(name: str) -> bool:
    """Whether a program owns `name` on alice's session bus."""
    return "true" in user(
        f"busctl --user call org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus NameHasOwner s {name}"
    )


def earwig_pid() -> str:
    return machine.succeed("systemctl --user -M alice@ show -P MainPID earwig.service").strip()


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


start_all()

with subtest("GNOME session"):
    machine.wait_for_unit("display-manager.service")
    machine.wait_for_file(f"{RUNTIME}/wayland-0")
    retry(shell_started, timeout=td(minutes=3))
    # GNOME logs in to the Activities overview, where keys go to its search: leave it.
    shell_js("(Main.overview.hide(), true)")

check_virtual_mic()
user_unit_active("earwig.service")

with subtest("EARWIG: reads the keyboard itself and may type"):
    st = allow_typing()
    print(f"status: {st}")
    assert st["hotkey"]["source"] == "keyboard" and st["hotkey"]["blocking"], st
    close_settings()

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

with subtest("EARWIG: once it quits, Super+Space is GNOME's again"):
    as_alice("earwig quit")
    machine.wait_until_fails("systemctl --user -M alice@ is-active earwig.service", timeout=td(seconds=30))
    machine.send_key(HOTKEY)
    wait_for(
        "GNOME to switch the input source",
        lambda: shell_js("Main.panel.statusArea.keyboard._inputSourceManager.currentSource.id") == "de",
    )
