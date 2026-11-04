import json
import shlex
import time


def wait_until(predicate, what, timeout=60):
    deadline = time.monotonic() + timeout
    while not predicate():
        if time.monotonic() > deadline:
            raise Exception(f"timed out waiting for {what}")
        time.sleep(0.5)

TOKEN = "/home/alice/.local/state/earwig/remote-desktop.token"
TARGET = "/tmp/target.txt"


def shell_js(js):
    """Evaluate JavaScript in gnome-shell; Eval returns JSON."""
    out = machine.succeed(f"shell-eval {shlex.quote(js)}").strip()
    assert out.startswith("(true, '") and out.endswith("')"), out
    return json.loads(out[len("(true, '") : -len("')")].replace("\\'", "'"))


WINDOWS_JS = """
global.get_window_actors().map(a => {
  const w = a.meta_window;
  const r = w.get_frame_rect();
  return {title: w.title, wm_class: w.get_wm_class(), type: w.get_window_type(),
          client: w.get_client_type(), x: r.x, y: r.y, w: r.width, h: r.height,
          above: w.is_above(), focus: w.has_focus(), stacking: a.get_parent().get_children().indexOf(a)};
})
"""


def windows():
    return shell_js(WINDOWS_JS)


def focused_title():
    return shell_js("global.display.focus_window ? global.display.focus_window.title : ''")


def wait_focus(title, timeout=60):
    machine.wait_until_succeeds(
        f"shell-eval {shlex.quote('global.display.focus_window?.title ?? null')} | grep -qF {shlex.quote(title)}",
        timeout=timeout,
    )


def activate(title):
    shell_js(
        "global.get_window_actors().map(a => a.meta_window)"
        f".find(w => w.title === {json.dumps(title)}).activate(global.get_current_time()), true"
    )
    wait_focus(title)


def wait_window(title, timeout=60):
    machine.wait_until_succeeds(
        f"shell-eval {shlex.quote(WINDOWS_JS)} | grep -qF {shlex.quote(title)}",
        timeout=timeout,
    )


def launch(unit, *cmd):
    machine.succeed("launch " + shlex.join([unit, *cmd]))


def unit_log(unit):
    return machine.succeed(f"cat /tmp/{unit}.log /tmp/{unit}.err")


def events(unit):
    return [
        json.loads(line)
        for line in machine.succeed(f"cat /tmp/{unit}.log").splitlines()
        if line.startswith("{")
    ]


def wait_inserted(unit, timeout=60):
    machine.wait_until_succeeds(
        f"grep -qE '\"event\":\"(inserted|insert-failed)\"' /tmp/{unit}.log",
        timeout=timeout,
    )
    inserted = [e for e in events(unit) if e["event"] in ("inserted", "insert-failed")][0]
    print(f"{unit}: {inserted}")
    assert inserted["event"] == "inserted", unit_log(unit)
    return inserted


def target_text():
    return machine.succeed(f"cat {TARGET} || true")


def wait_target_contains(text, timeout=30):
    machine.wait_until_succeeds(f"grep -qF {shlex.quote(text)} {TARGET}", timeout=timeout)


def arrival_us(chars_before, chars_added):
    """Unix time (us) when the target first held chars_before + chars_added chars."""
    for line in machine.succeed(f"cat {TARGET}.times").splitlines():
        t, n = map(int, line.split())
        if n >= chars_before + chars_added:
            return t
    raise Exception("text never fully arrived")


start_all()

with subtest("GNOME session"):
    machine.wait_for_unit("display-manager.service")
    machine.wait_for_file("/run/user/1000/wayland-0")
    machine.wait_for_unit("default.target", "alice")
    machine.wait_until_succeeds(
        "shell-eval Main.layoutManager._startingUp | grep -q 'true,..false'"
    )
    machine.send_key("esc")
    print(machine.succeed("runuser -u alice -- env XDG_RUNTIME_DIR=/run/user/1000 "
                          "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus "
                          "systemctl --user show-environment"))

with subtest("Target text view has focus"):
    launch("target", "earwig-target", TARGET)
    wait_focus("earwig-target")

with subtest("First RemoteDesktop session asks once; typing lands in the focused app"):
    launch("type1", "portal_check", "insert", "type", "Hello from earwig, typed!", "8")
    wait_window("Remote Desktop")
    machine.sleep(2)
    machine.screenshot("01-remote-desktop-dialog")
    machine.send_key("alt-i")
    machine.sleep(1)
    machine.screenshot("02-remote-desktop-dialog-allowed")
    machine.send_key("alt-s")
    wait_inserted("type1")
    wait_target_contains("Hello from earwig, typed!")
    machine.succeed(f"test -s {TOKEN}")
    print(unit_log("type1"))

with subtest("The restore token skips the dialog; paste carries any character"):
    wait_focus("earwig-target")
    launch("paste1", "portal_check", "insert", "paste", " Pasted: ünïcödé ✓ ", "8")
    inserted = wait_inserted("paste1", timeout=30)
    assert inserted["method"] == "portal-paste"
    assert "Remote Desktop" not in [w["title"] for w in windows()]
    wait_target_contains("Pasted: ünïcödé ✓")
    machine.screenshot("03-typed-and-pasted")

with subtest("auto types what the layout has and pastes the rest"):
    launch("auto1", "portal_check", "insert", "auto", "plain ascii; ", "2")
    assert wait_inserted("auto1")["method"] == "portal-type"
    launch("auto2", "portal_check", "insert", "auto", "café crème ", "8")
    assert wait_inserted("auto2")["method"] == "portal-paste"
    wait_target_contains("plain ascii; café crème ")

with subtest("Speed: a long transcript typed vs pasted"):
    sentence = "The quick brown fox jumps over the lazy dog, then types 42 words! "
    long = (sentence * 8)[:500]
    machine.succeed(f"printf %s {shlex.quote(long)} > /tmp/long.txt")
    for method in ["type", "paste", "type", "paste"]:
        unit = f"speed-{method}-{len(target_text())}"
        before = len(target_text())
        launch(unit, "portal_check", "insert", method, "@/tmp/long.txt", "8")
        inserted = wait_inserted(unit)
        wait_until(lambda: len(target_text()) >= before + len(long), "the long text")
        machine.sleep(1)
        latency = arrival_us(before, len(long)) - inserted["start_unix_us"]
        print(
            f"SPEED {method}: {len(long)} chars, insert() returned after "
            f"{inserted['insert_us'] / 1000:.1f} ms, text complete in the app after "
            f"{latency / 1000:.1f} ms"
        )
    assert long in target_text()

with subtest("GNOME Console: typing works, Ctrl+V does not paste"):
    launch("console", "kgx", "--", "sh", "-c", "cat > /tmp/console.txt")
    machine.wait_until_succeeds(
        "shell-eval 'global.display.focus_window?.get_wm_class() ?? null' | grep -q org.gnome.Console",
        timeout=60,
    )
    machine.sleep(2)
    launch("console-type", "portal_check", "insert", "type", "typed into console\n", "2")
    wait_inserted("console-type")
    machine.wait_until_succeeds("grep -qF 'typed into console' /tmp/console.txt", timeout=30)
    launch("console-paste", "portal_check", "insert", "paste", "PASTED-INTO-CONSOLE", "4")
    wait_inserted("console-paste")
    machine.sleep(2)
    launch("console-end", "portal_check", "insert", "type", "end\n\n", "2")
    wait_inserted("console-end")
    machine.wait_until_succeeds("grep -qF 'end' /tmp/console.txt", timeout=30)
    machine.screenshot("04-console")
    console = machine.succeed("cat -v /tmp/console.txt")
    print(f"console received: {console!r}")
    assert "PASTED-INTO-CONSOLE" not in console

with subtest("GlobalShortcuts: the dictate shortcut reports press and release"):
    activate("earwig-target")
    launch("shortcuts", "portal_check", "shortcuts", "CTRL+ALT+e", "2")
    wait_window("Add Keyboard Shortcuts")
    machine.sleep(2)
    machine.screenshot("05-global-shortcuts-dialog")
    machine.send_key("alt-a")
    machine.wait_until_succeeds("grep -q 'portal shortcut dictate' /tmp/shortcuts.err", timeout=30)
    print(unit_log("shortcuts"))
    machine.send_key("ctrl-alt-e")
    machine.wait_until_succeeds("grep -q '\"cmd\":\"release\"' /tmp/shortcuts.log", timeout=30)
    commands = [e["cmd"] for e in events("shortcuts") if "cmd" in e]
    print(f"shortcut commands: {commands}")
    assert commands == ["press", "release"]

with subtest("Overlay experiment: X11 popup via XWayland, Wayland toplevel"):
    for name, env, kind in [
        ("overlay-x11-popup", ["env", "-u", "WAYLAND_DISPLAY"], "popup"),
        ("overlay-wayland-toplevel", [], "toplevel"),
    ]:
        activate("earwig-target")
        before = target_text()
        launch(name, *env, "gnome_overlay", kind, "30")
        machine.wait_until_succeeds(f"grep -q opened /tmp/{name}.log", timeout=60)
        machine.sleep(3)
        machine.screenshot(f"06-{name}")
        print(f"{name}: {unit_log(name)}")
        print(f"{name} windows: {json.dumps(windows())}")
        print(f"{name} focus: {focused_title()!r}")
        machine.send_chars("zq")
        machine.sleep(2)
        kept_focus = target_text() == before + "zq"
        print(f"{name}: keys reached the editor: {kept_focus}")
        machine.screenshot(f"07-{name}-after-typing")
        machine.succeed(
            "runuser -u alice -- env XDG_RUNTIME_DIR=/run/user/1000 "
            "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus "
            f"systemctl --user stop {name}"
        )
