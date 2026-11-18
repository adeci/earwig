import time

BUS = f"DBUS_SESSION_BUS_ADDRESS=unix:path={RUNTIME}/bus"
PORTAL_KDE = "org.freedesktop.impl.portal.desktop.kde"


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


def windows():
    return kwin(
        "return workspace.windowList().map(w => ({caption: w.caption, cls: w.resourceClass,"
        " pid: w.pid, layer: w.layer, active: w.active, wantsInput: w.wantsInput}));"
    )


def active() -> str | None:
    """The focused window's class."""
    return kwin("const w = workspace.activeWindow; return w ? w.resourceClass : null;")


def wait_active(cls: str, timeout: float = 120) -> None:
    deadline = time.monotonic() + timeout
    while (now := active()) != cls:
        if time.monotonic() > deadline:
            raise Exception(f"{cls} never got focus (active: {now}); windows: {windows()}")
        time.sleep(1)


def activate(cls: str) -> None:
    kwin(
        f"const w = workspace.windowList().find(w => w.resourceClass === {json.dumps(cls)});"
        " workspace.activeWindow = w; return true;"
    )
    wait_active(cls)


def dialogs(title: str = "") -> list:
    """The portal's dialogs, those whose caption has `title` in it."""
    return [w for w in windows() if w["cls"] == PORTAL_KDE and title in w["caption"]]


def no_dialog() -> None:
    assert not (up := dialogs()), f"a portal dialog: {up}"


def plasma_session() -> None:
    machine.wait_for_unit("multi-user.target")
    machine.wait_until_succeeds("pgrep -u alice plasmashell", timeout=td(minutes=3))
    machine.wait_until_succeeds(f"ls {RUNTIME}/wayland-*", timeout=td(minutes=1))
    user_unit_active("graphical-session.target")
    machine.sleep(5)
