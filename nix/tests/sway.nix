# Dictation end to end on sway, earwig holding the keyboard: virtual mic -> earwig -> foot through
# the input method, with a replacement the module declares; then with `[insert] method = "paste"`
# into foot and Mousepad, pasted with Shift+Insert from sway's virtual keyboard. Software
# rendering via pixman, as in nixpkgs' sway test.
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
