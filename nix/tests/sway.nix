# Dictation end to end on sway, earwig holding the keyboard: virtual mic -> earwig -> focused
# foot, with a replacement the module declares. Software rendering via pixman, as in nixpkgs'
# sway test.
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
      environment.systemPackages = [ pkgs.jq ];
    };

  testScript = ''
    ${common.prelude}

    start_all()
    machine.wait_for_unit("multi-user.target")
    machine.wait_for_file(f"{RUNTIME}/wayland-1")
    machine.wait_for_file("/tmp/sway-ipc.sock")
    user_unit_active("sway-session.target")

    check_virtual_mic()

    start_target_terminal()
    machine.wait_until_succeeds(
        "su - alice -c 'swaymsg -t get_tree' | jq -e '.. | objects | select(.app_id? == \"earwig-target\" and .focused)'"
    )
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

    with machine.nested("EARWIG: neither tap reached foot or sway"):
        out = machine.succeed("cat /tmp/out")
        # A leaked Space would be in the text; sway's Mod4+space would have
        # taken focus from foot, and the text would have gone elsewhere.
        assert out == wait_insertion(events)["text"], out

    with machine.nested("EARWIG: the module's replacement is written, and locked"):
        assert "sleepy cat" in out and "dog" not in out.lower(), out
        listed = as_alice("earwig replacements list")
        assert "* lazy dog  sleepy cat" in listed, listed
        refusal = machine.fail(
            "su - alice -c "
            + shlex.quote(f"XDG_RUNTIME_DIR={RUNTIME} earwig replacements remove 'lazy dog' 2>&1")
        )
        assert "system config" in refusal, refusal
  '';
}
