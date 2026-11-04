import json

SHORTCUTS = "/home/alice/.config/cosmic/com.system76.CosmicSettings.Shortcuts/v1/custom"


def journal_tail(n: int = 60) -> str:
    return machine.execute(f"journalctl _UID=1000 -o cat --no-pager | tail -n {n}")[1]


def wait_toplevel(app_id: str) -> None:
    """Wait for a window with `app_id` (ext-foreign-toplevel-list, lswt)."""
    machine.wait_until_succeeds(
        f"runuser -u alice -- env XDG_RUNTIME_DIR={RUNTIME} WAYLAND_DISPLAY=$(basename $(ls {RUNTIME}/wayland-? | head -n1))"
        f" lswt --custom a | grep -qx {app_id}",
        timeout=td(minutes=2),
    )


def toggle_by_key() -> None:
    """The COSMIC custom shortcut Super+Space runs `earwig toggle`."""
    machine.send_key("meta_l-spc")


def dictate_toggle(shot: str, toggle) -> tuple[str, int]:
    """Toggle on, speak the clip, toggle off once the overlay shows live
    text; returns that text and the log position before it."""
    since = log_lines()
    toggle()
    wait_log(r"recording \d+ started", timeout=30, since=since)
    play_clip()
    text = wait_live_text(shot)
    toggle()
    return text, since


start_all()

with subtest("COSMIC session"):
    machine.wait_for_unit("multi-user.target")
    try:
        machine.wait_until_succeeds(f"ls {RUNTIME}/wayland-*", timeout=td(minutes=1))
        user_unit_active("graphical-session.target")
        # nixpkgs' COSMIC test: the status notifier watcher comes up last.
        machine.wait_until_succeeds(
            f"runuser -u alice -- env XDG_RUNTIME_DIR={RUNTIME} busctl --user status com.system76.CosmicStatusNotifierWatcher",
            timeout=td(minutes=6),
        )
    finally:
        machine.screenshot("00-desktop")
        print(machine.execute("journalctl -o cat --no-pager | grep -i 'cosmic-comp' | tail -n 60")[1])
    print(machine.succeed("systemctl --user -M alice@ show-environment"))

check_virtual_mic()

with subtest("earwig probes COSMIC's capabilities"):
    try:
        user_unit_active("earwig.service")
    finally:
        print(earwig_log())
    caps = wait_log(r"desktop: DesktopCaps", timeout=60)
    print(caps)
    for cap in ("layer_shell", "input_method", "virtual_keyboard", "data_control"):
        assert f"{cap}: true" in caps, f"{cap} missing: {caps}"
    print(wait_log(r"insertion methods", timeout=30))
    status = json.loads(as_alice("earwig status"))
    print(f"status: {status}")
    assert status["insert_methods"][:2] == ["input-method", "paste"], status

with subtest("A COSMIC custom shortcut runs `earwig toggle`"):
    # What Settings → Keyboard → Custom Shortcuts writes; cosmic-comp reloads
    # it on change. Written in one rename so it never sees a partial file.
    machine.succeed(f"install -d -o alice -g users $(dirname {SHORTCUTS})")
    machine.succeed(
        f"cat > /tmp/custom <<'EOF'\n"
        '{\n    (modifiers: [Super], key: "space"): Spawn("earwig toggle"),\n}\n'
        f"EOF\nchown alice:users /tmp/custom && mv /tmp/custom {SHORTCUTS}"
    )
    machine.sleep(3)

with subtest("COSMIC Text Editor has focus"):
    as_alice("sh -c 'printf \"\" > /tmp/edit.txt'")
    # cosmic-edit always forks into the background; keep the transient unit
    # (and so its cgroup) alive after the parent exits.
    spawn("/run/current-system/sw/bin/sh -c 'cosmic-edit /tmp/edit.txt; exec sleep infinity'")
    try:
        wait_toplevel("com.system76.CosmicEdit")
        machine.sleep(3)
        machine.screenshot("01-cosmic-edit")
        machine.send_chars("probe ")
        machine.send_key("ctrl-s")
        machine.wait_until_succeeds("grep -q probe /tmp/edit.txt", timeout=td(seconds=30))
    finally:
        machine.screenshot("02-cosmic-edit-probe")
        print(journal_tail())

with subtest("Dictate into the editor with the shortcut"):
    overlay_text, since = dictate_toggle("03-cosmic-edit-recording", toggle_by_key)
    print(wait_log(r"final text \d+ ms after release", timeout=120, since=since))
    inserted = wait_log(r"inserted \d+ characters", timeout=60, since=since)
    print(inserted)
    assert inserted.endswith("via InputMethod"), inserted
    print(wait_log(r"done \d+ ms after release", timeout=30, since=since))
    machine.sleep(1)
    machine.send_key("ctrl-s")
    machine.wait_until_succeeds("grep -qi 'brown fox' /tmp/edit.txt", timeout=td(seconds=30))
    machine.screenshot("04-cosmic-edit-inserted")
    print(machine.succeed("cat /tmp/edit.txt"))
    assert any(w in overlay_text.lower() for w in ("quick", "brown", "fox")), overlay_text

with subtest("Dictate into a terminal with `earwig toggle`"):
    # The editor still shows the sentence, which would satisfy the OCR.
    machine.succeed("pkill -f 'cosmic-edit /tmp/edit.txt'")
    start_target_terminal()
    check_typing_reaches_target()
    _, since = dictate_toggle("05-terminal-recording", lambda: as_alice("earwig toggle"))
    print(wait_log(r"final text \d+ ms after release", timeout=120, since=since))
    inserted = wait_log(r"inserted \d+ characters", timeout=60, since=since)
    print(inserted)
    assert inserted.endswith("via InputMethod"), inserted
    machine.wait_until_succeeds("grep -qi 'brown fox' /tmp/out", timeout=td(seconds=30))
    machine.screenshot("06-terminal-inserted")
    print(machine.succeed("cat /tmp/out"))

print(earwig_log())
