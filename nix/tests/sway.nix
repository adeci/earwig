# Dictation end to end on sway (wlroots): virtual mic -> earwig -> focused
# foot. Software rendering via pixman, as in nixpkgs' sway test.
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

      programs.sway.enable = true;
      environment.variables = {
        SWAYSOCK = "/tmp/sway-ipc.sock";
        WLR_RENDERER = "pixman";
      };
      # The stock config plus the binds a user would add; the test drives the
      # same commands through the CLI.
      environment.etc."sway/config.d/earwig.conf".text = ''
        bindsym --no-repeat Mod4+space exec earwig press
        bindsym --release Mod4+space exec earwig release
        bindsym Mod4+Escape exec earwig cancel
      '';
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

    with machine.nested("earwig user service runs in the session"):
        user_unit_active("earwig.service")
        print(as_alice("earwig status"))

    start_target_terminal()
    machine.wait_until_succeeds(
        "su - alice -c 'swaymsg -t get_tree' | jq -e '.. | objects | select(.app_id? == \"earwig-target\" and .focused)'"
    )
    machine.screenshot("target_terminal")

    overlay_text = dictate()

    ${common.assertions}
  '';
}
