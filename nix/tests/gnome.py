import json
import shlex
import time


def wait_until(predicate, what, timeout=60):
    deadline = time.monotonic() + timeout
    while not predicate():
        if time.monotonic() > deadline:
            dump()
            raise Exception(f"timed out waiting for {what}")
        time.sleep(0.5)


def wait_cmd(cmd, timeout=60):
    wait_until(lambda: machine.execute(cmd)[0] == 0, cmd, timeout)


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
    wait_until(lambda: focused_title() == title, f"focus on {title}", timeout)


def activate(title):
    shell_js(
        "global.get_window_actors().map(a => a.meta_window)"
        f".find(w => w.title === {json.dumps(title)}).activate(global.get_current_time()), true"
    )
    wait_focus(title)


def wait_window(title, timeout=60):
    wait_until(
        lambda: title in [w["title"] for w in windows()], f"a window {title}", timeout
    )


def dump():
    print(f"windows: {json.dumps(windows())}")
    print(machine.execute("tail -n 40 /tmp/*.log /tmp/*.err")[1])
    machine.screenshot("failure")


def launch(unit, *cmd):
    machine.succeed("launch " + shlex.join([unit, *cmd]))


def user_systemctl(*args):
    return machine.execute(
        "runuser -u alice -- env XDG_RUNTIME_DIR=/run/user/1000 "
        "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus "
        "systemctl --user " + shlex.join(args)
    )


FIFO = "/tmp/earwig.fifo"


def serve(unit):
    """Start the harness like the daemon: one portal session, many requests."""
    launch(unit, "portal_check", "serve", FIFO)


def wait_ready(unit, timeout=60):
    wait_cmd(f"grep -q '\"event\":\"ready\"' /tmp/{unit}.log", timeout)
    ready = [e for e in events(unit) if e["event"] == "ready"][0]
    print(f"{unit}: {ready}")
    return ready


def results(unit):
    return [e for e in events(unit) if e["event"] in ("inserted", "insert-failed")]


def request(unit, method, text, expect_ok=True):
    done = len(results(unit))
    line = json.dumps({"method": method, "text": text})
    machine.succeed(f"printf '%s\\n' {shlex.quote(line)} > {FIFO}")
    wait_until(lambda: len(results(unit)) > done, f"{unit} to answer {method}")
    result = results(unit)[done]
    print(f"{unit} {method} {len(text)} chars: {result}")
    if expect_ok:
        assert result["event"] == "inserted", unit_log(unit)
    return result


def start_target(unit, path):
    global TARGET
    TARGET = path
    launch(unit, "earwig-target", path)
    wait_focus("earwig-target")


def unit_log(unit):
    return machine.succeed(f"cat /tmp/{unit}.log /tmp/{unit}.err")


def events(unit):
    return [
        json.loads(line)
        for line in machine.succeed(f"cat /tmp/{unit}.log").splitlines()
        if line.startswith("{")
    ]


def target_text():
    return machine.succeed(f"cat {TARGET} || true")


def wait_target_contains(text, timeout=30):
    wait_cmd(f"grep -qF {shlex.quote(text)} {TARGET}", timeout)


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
    machine.succeed(f"mkfifo -m 666 {FIFO}")
    serve("serve1")
    wait_window("Remote Desktop")
    machine.sleep(2)
    machine.screenshot("01-remote-desktop-dialog")
    machine.send_key("alt-i")
    machine.sleep(1)
    machine.screenshot("02-remote-desktop-dialog-allowed")
    machine.send_key("alt-s")
    wait_ready("serve1")
    wait_focus("earwig-target")
    assert request("serve1", "type", "Hello from earwig, typed!")["method"] == "portal-type"
    wait_target_contains("Hello from earwig, typed!")
    machine.succeed(f"test -s {TOKEN}")
    user_systemctl("stop", "serve1")

with subtest("The restore token skips the dialog; paste carries any character"):
    serve("serve2")
    ready = wait_ready("serve2", timeout=20)
    assert "Remote Desktop" not in [w["title"] for w in windows()]
    assert ready["methods"] == ["portal-paste", "portal-type"], ready
    result = request("serve2", "auto", " Auto: ünïcödé ✓ ")
    assert result["method"] == "portal-paste"
    wait_target_contains("Auto: ünïcödé ✓")
    request("serve2", "copy", "only on the clipboard")
    machine.screenshot("03-typed-and-pasted")

with subtest("Speed: a long transcript typed vs pasted"):
    user_systemctl("stop", "target")
    start_target("target-speed", "/tmp/speed.txt")
    sentence = "The quick brown fox jumps over the lazy dog, then types 42 words! "
    long = (sentence * 8)[:500]
    for method in ["type", "paste", "type", "paste"]:
        # A new paragraph each time: GTK sends the paragraph as surrounding
        # text with every key.
        request("serve2", "type", "\n\n")
        machine.sleep(1)
        before = len(target_text())
        result = request("serve2", method, long)
        wait_until(lambda: len(target_text()) >= before + len(long), "the long text")
        machine.sleep(1)
        latency = arrival_us(before, len(long)) - result["start_unix_us"]
        print(
            f"SPEED {method}: {len(long)} chars, insert() returned after "
            f"{result['insert_us'] / 1000:.1f} ms, text complete in the app after "
            f"{latency / 1000:.1f} ms"
        )
    assert long in target_text()

with subtest("Stress: typing into a long paragraph"):
    # Grow one paragraph by typing and see whether the GTK client keeps up
    # with the key bursts.
    for i in range(4):
        before = len(target_text())
        request("serve2", "type", long)
        machine.sleep(5)
        alive = user_systemctl("is-active", "target-speed")[0] == 0
        print(f"STRESS burst {i}: paragraph {before} -> {len(target_text())} chars, client alive: {alive}")
        if not alive:
            print(machine.execute("tail -n 3 /tmp/target-speed.err")[1])
            break
    user_systemctl("stop", "target-speed")

with subtest("GNOME Console: the terminal is recognized and pasted with Ctrl+Shift+V"):
    launch("console", "kgx", "--", "sh", "-c", "cat > /tmp/console.txt")
    wait_cmd(
        "shell-eval 'global.display.focus_window?.get_wm_class() ?? null' | grep -q org.gnome.Console"
    )
    machine.sleep(2)
    request("serve2", "auto", "pasted into the console")
    request("serve2", "type", "\ntyped into the console\n")
    wait_cmd("grep -qF 'typed into the console' /tmp/console.txt", 30)
    machine.screenshot("04-console")
    console = machine.succeed("cat -v /tmp/console.txt")
    print(f"console received: {console!r}")
    assert "pasted into the console\n" in console
    machine.succeed("grep -q 'role Ok(Terminal)' /tmp/serve2.err")
    user_systemctl("stop", "console")

with subtest("Back in a GTK app, paste is Ctrl+V again"):
    start_target("target", "/tmp/target.txt")
    machine.sleep(1)
    request("serve2", "auto", "after the console")
    wait_target_contains("after the console")

with subtest("GlobalShortcuts: the dictate shortcut reports press and release"):
    activate("earwig-target")
    launch("shortcuts", "portal_check", "shortcuts", "CTRL+ALT+e", "2")
    wait_window("Add Keyboard Shortcuts")
    machine.sleep(2)
    machine.screenshot("05-global-shortcuts-dialog")
    machine.send_key("alt-a")
    wait_cmd("grep -q 'portal shortcut dictate' /tmp/shortcuts.err", 30)
    print(unit_log("shortcuts"))
    machine.send_key("ctrl-alt-e")
    wait_cmd("grep -q '\"cmd\":\"release\"' /tmp/shortcuts.log", 30)
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
        wait_cmd(f"grep -q opened /tmp/{name}.log")
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
