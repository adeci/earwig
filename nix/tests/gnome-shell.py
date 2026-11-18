import ast
import time
from typing import Any

BUS = f"DBUS_SESSION_BUS_ADDRESS=unix:path={RUNTIME}/bus"


def user(cmd: str) -> str:
    """Run `cmd` as alice with her session bus."""
    return machine.succeed(f"runuser -u alice -- env XDG_RUNTIME_DIR={RUNTIME} {BUS} {cmd}")


def shell_js(js: str) -> Any:
    """Evaluate JavaScript in gnome-shell (run with --unsafe-mode); Eval returns the value as
    JSON."""
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


def gnome_session() -> None:
    """Wait for alice's shell, and leave the Activities overview GNOME logs in to, where keys go
    to its search."""
    machine.wait_for_unit("display-manager.service")
    machine.wait_for_file(f"{RUNTIME}/wayland-0")
    retry(shell_started, timeout=td(minutes=3))
    shell_js("(Main.overview.hide(), true)")


def windows() -> list[dict[str, Any]]:
    return shell_js(
        "global.get_window_actors().map(a => a.meta_window).map(w => ({title: w.title,"
        " app: w.get_wm_class(), focus: w.has_focus(), x11: w.get_client_type() === 1,"
        " override_redirect: w.is_override_redirect()}))"
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


def earwig_pid() -> str:
    return machine.succeed("systemctl --user -M alice@ show -P MainPID earwig.service").strip()
