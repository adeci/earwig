# Dictation end to end on sway, earwig holding the keyboard: virtual mic -> earwig -> foot through
# the input method, with a replacement the module declares; then with `[insert] method = "paste"`
# into foot and Mousepad, pasted with Shift+Insert from sway's virtual keyboard. First a
# replacement typed into the settings window, its Heard field clicked at the edge; then the discard
# shortcut, Super+Shift+Space: it reaches sway while nothing is dictated, and throws a dictation
# away when Shift joins the held hotkey or after a tap. Software rendering via pixman, as in
# nixpkgs' sway test.
{ self }:
let
  common = import ./common.nix { inherit self; };
in
{
  name = "earwig-sway";
  enableOCR = true;

  nodes.machine =
    { pkgs, ... }:
    {
      imports = [ common.machine ];

      # The clip ends "... over the lazy dog."
      services.earwig.settings.replacements."lazy dog" = "sleepy cat";

      programs.sway.enable = true;
      environment.variables = {
        SWAYSOCK = "/tmp/sway-ipc.sock";
        WLR_RENDERER = "pixman";
      };
      programs.bash.loginShellInit = ''
        if [ "$(tty)" = "/dev/tty1" ]; then
          sway > /tmp/sway.log 2>&1
        fi
      '';
      environment.systemPackages = [
        pkgs.jq
        pkgs.mousepad
      ];
    };

  testScript = ''
    ${common.prelude}

    start_all()
    machine.wait_for_unit("multi-user.target")
    machine.wait_for_file(f"{RUNTIME}/wayland-1")
    machine.wait_for_file("/tmp/sway-ipc.sock")
    user_unit_active("sway-session.target")


    def wait_focused(app_id: str) -> None:
        machine.wait_until_succeeds(
            f"su - alice -c 'swaymsg -t get_tree' | jq -e '.. | objects | select(.app_id? == \"{app_id}\" and .focused)'"
        )


    check_virtual_mic()

    with machine.nested("EARWIG: a click on the edge of a settings field types into it"):
        wait_status(lambda _: True, "the daemon")
        spawn("/run/current-system/sw/bin/earwig settings")
        wait_focused("io.github.adeci.earwig")
        click_text("Replacements")
        # In the field's padding, left of its placeholder.
        click_text("Heard,", left_of=6)
        machine.send_chars("teh")
        click_text("Written,")
        machine.send_chars("the\n")
        retry(
            lambda _last_try: ["teh", "the"] in [row.split() for row in as_alice("earwig replacements list").splitlines()],
            timeout=td(seconds=30),
        )
        machine.succeed("su - alice -c " + shlex.quote("""swaymsg '[app_id="io.github.adeci.earwig"] kill'"""))

    start_target_terminal()
    wait_focused("earwig-target")
    check_typing_reaches_target()
    machine.screenshot("target_terminal")

    with machine.nested("EARWIG: earwig watches the keyboard and keeps the hotkey from sway"):
        st = wait_status(
            lambda s: s["hotkey"]["source"] == "keyboard" and s["hotkey"]["blocking"],
            "the keyboard watched, the hotkey held back",
        )
        print(st["hotkey"])
        # Typing now goes through earwig's clones of the keyboards.
        check_typing_reaches_target()


    HOTKEY_KEYS = "meta_l-spc"
    DISCARD_KEYS = "meta_l-shift-spc"


    def target_floats() -> bool:
        """sway's default config floats or tiles the focused window on Super+Shift+Space."""
        kind = machine.succeed(
            "su - alice -c 'swaymsg -t get_tree' | jq -r '.. | objects | select(.app_id? == \"earwig-target\") | .type'"
        )
        return kind.strip() == "floating_con"


    def history() -> str:
        return machine.succeed("cat /home/alice/.local/share/earwig/history.jsonl 2>/dev/null || true")


    with machine.nested("EARWIG: the discard shortcut reaches sway while nothing is dictated"):
        assert status()["hotkey"]["discard"] == "Shift+Super+Space", status()["hotkey"]
        wait_status(lambda s: s["model"]["state"] == "ready", "the model", timeout=300)
        for floats in (True, False):
            machine.send_key(DISCARD_KEYS)
            retry(lambda _last_try: target_floats() == floats, timeout=td(seconds=30))

    kept = history()


    def hold_hotkey() -> None:
        key("meta_l", True)
        key("spc", True)


    with machine.nested("EARWIG: Shift added to the held hotkey throws the dictation away"):
        events = cancel_mid_clip(hold_hotkey, lambda: key("shift", True))
        for name in ("spc", "shift", "meta_l"):
            key(name, False)
        machine.sleep(1)
        assert status()["state"]["state"] == "idle", "letting go started a dictation"

    with machine.nested("EARWIG: after a tap, the discard shortcut throws the dictation away, kept from sway"):
        machine.send_key(HOTKEY_KEYS)
        wait_status(lambda s: s["state"]["state"] == "recording", "recording", timeout=30)
        machine.send_key(DISCARD_KEYS)
        wait_status(lambda s: s["state"]["state"] == "idle", "idle after the discard")
        assert not target_floats()

    with machine.nested("EARWIG: neither dictation was inserted or kept in History"):
        seen = [json.loads(line) for line in machine.succeed(f"cat {events}").splitlines()]
        states = [e["state"]["state"] for e in seen if e["event"] == "state"]
        assert "finalizing" not in states, states
        assert not [e for e in seen if e["event"] in ("inserted", "insert-failed")], seen
        assert machine.succeed("cat /tmp/out") == "", "foot got keys or text"
        assert history() == kept


    ${common.dictation}

    # The text came alone: neither tap reached foot (a Space) or sway (Mod4+space would have
    # moved the focus).
    assert inserted["method"] == "input-method", inserted

    with machine.nested("EARWIG: the module's replacement is written, and locked"):
        out = inserted["text"]
        assert "sleepy cat" in out and "dog" not in out.lower(), out
        listed = as_alice("earwig replacements list")
        assert "* lazy dog  sleepy cat" in listed, listed
        refusal = machine.fail(
            "su - alice -c "
            + shlex.quote(f"XDG_RUNTIME_DIR={RUNTIME} earwig replacements remove 'lazy dog' 2>&1")
        )
        assert "system config" in refusal, refusal

    with machine.nested("EARWIG: with insert method paste, pasted once into foot, whose app id is on no list"):
        switch_insert_method("paste")
        wait_status(lambda s: s["hotkey"]["blocking"], "the hotkey held back again")
        dictate_into_target("paste")

    with machine.nested("EARWIG: pasted once into Mousepad"):
        dictate_into_editor(wait_focused)
  '';
}
