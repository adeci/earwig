HOTKEY_KEYS = "meta_l-spc"


def set_layout(index: int) -> None:
    user(f"busctl --user call org.kde.keyboard /Layouts org.kde.KeyboardLayouts setLayout u {index}")
    machine.wait_until_succeeds(
        f"runuser -u alice -- env {BUS} busctl --user call org.kde.keyboard /Layouts "
        f"org.kde.KeyboardLayouts getLayout | grep -qx 'u {index}'",
        timeout=td(seconds=10),
    )


def dictate_into(cls: str, shot: str) -> None:
    """Dictate with the hotkey while `cls` has focus; it must be pasted by earwig's own keyboard."""
    events = watch_events()
    dictate()
    inserted = wait_insertion(events)
    machine.screenshot(shot)
    assert inserted["method"] == "uinput-paste", inserted
    wait_active(cls)


start_all()

with subtest("Plasma session"):
    plasma_session()

with subtest("EARWIG: ready to paste, without a dialog"):
    user_unit_active("earwig.service")
    st = wait_status(lambda s: s["insert_methods"] and s["hotkey"]["keyboard"] == "watching", "ready", timeout=120)
    print(f"status: {st}")
    assert st["insert_methods"] == ["uinput-paste"], st
    wait_status(lambda s: s["model"]["state"] == "ready", "the model", timeout=300)
    no_dialog()

with subtest("EARWIG: dictation lands in a focused KWrite document"):
    as_alice("touch /tmp/kwrite.txt")
    spawn("/run/current-system/sw/bin/kwrite /tmp/kwrite.txt")
    wait_active("org.kde.kwrite")
    machine.sleep(2)
    dictate_into("org.kde.kwrite", "01-kwrite-inserted")
    machine.send_key("ctrl-s")
    machine.wait_until_succeeds("grep -qi 'brown fox' /tmp/kwrite.txt", timeout=td(seconds=30))
    print(machine.succeed("cat /tmp/kwrite.txt"))

with subtest("EARWIG: dictation lands in Konsole"):
    spawn("/run/current-system/sw/bin/konsole -e sh -c 'stty -icanon; exec cat >> /tmp/konsole.txt'")
    wait_active("org.kde.konsole")
    machine.sleep(2)
    dictate_into("org.kde.konsole", "02-konsole-inserted")
    machine.wait_until_succeeds("grep -qi 'brown fox' /tmp/konsole.txt", timeout=td(seconds=30))
    received = machine.succeed("cat -v /tmp/konsole.txt")
    print(received)
    # Ctrl+V would arrive as a literal ^V, a Shift+Insert a terminal ignored as ^[[2;2~.
    assert "^V" not in received and "^[" not in received, received

with subtest("EARWIG: dictation lands in foot, a terminal not on the accessibility bus"):
    start_target_terminal()
    wait_active("earwig-target")
    machine.sleep(2)
    dictate_into("earwig-target", "03-foot-inserted")
    machine.wait_until_succeeds("grep -qi 'brown fox' /tmp/out", timeout=td(seconds=30))
    received = machine.succeed("cat -v /tmp/out")
    print(received)
    assert "^V" not in received and "^[" not in received, received

with subtest("EARWIG: with Dvorak active, dictation lands in KWrite"):
    activate("org.kde.kwrite")
    set_layout(1)
    dictate_into("org.kde.kwrite", "04-kwrite-dvorak-inserted")
    # QEMU sends key positions: save on the US layout.
    set_layout(0)
    machine.send_key("ctrl-s")
    machine.wait_until_succeeds("test $(grep -oi 'brown fox' /tmp/kwrite.txt | wc -l) -ge 2", timeout=td(seconds=30))
    print(machine.succeed("cat /tmp/kwrite.txt"))
    no_dialog()

print_latency()
