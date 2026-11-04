# Dictation end to end on niri: virtual mic -> earwig -> focused foot.
#
# niri's TTY backend refuses software EGL ("software EGL renderers are
# skipped", src/backend/tty.rs) and QEMU in the build sandbox only offers
# llvmpipe, so niri gets no output there. Its winit backend has no such check:
# niri runs as a client of cage (pixman), and everything earwig talks to
# (layer-shell, input-method-v2, text-input-v3, virtual keyboard,
# data-control, IPC) is niri's own.
{ self }:
let
  common = import ./common.nix { inherit self; };
in
{
  name = "earwig-niri";
  enableOCR = true;

  nodes.machine =
    { pkgs, ... }:
    {
      imports = [ common.machine ];

      programs.niri.enable = true;
      environment.systemPackages = [
        pkgs.cage
        pkgs.jq
      ];

      # Started from niri once it listens, standing in for niri.service
      # (which only exists for `niri --session` on a TTY).
      systemd.user.targets.niri-nested = {
        bindsTo = [ "graphical-session.target" ];
        wants = [ "graphical-session-pre.target" ];
        after = [ "graphical-session-pre.target" ];
      };

      environment.etc."niri/config.kdl".text = ''
        prefer-no-csd
        hotkey-overlay { skip-at-startup; }
        animations { off; }
        spawn-at-startup "sh" "-c" "systemctl --user import-environment WAYLAND_DISPLAY NIRI_SOCKET XDG_CURRENT_DESKTOP && systemctl --user start niri-nested.target"
        // niri 26.04 has no release binds (press/release sections land
        // after it), so the key toggles; the test drives press/release
        // through the CLI.
        binds {
          Mod+Space { spawn "earwig" "toggle"; }
          Mod+Escape { spawn "earwig" "cancel"; }
        }
      '';

      programs.bash.loginShellInit = ''
        if [ "$(tty)" = "/dev/tty1" ]; then
          export XDG_CURRENT_DESKTOP=niri
          WLR_RENDERER=pixman cage -s -- niri > /tmp/niri.log 2>&1
        fi
      '';
    };

  testScript = ''
    ${common.prelude}

    start_all()
    machine.wait_for_unit("multi-user.target")
    user_unit_active("niri-nested.target")
    niri_socket = machine.succeed(f"ls {RUNTIME}/niri.*.sock").strip()


    def niri_msg(args: str) -> str:
        return as_alice(f"env NIRI_SOCKET={niri_socket} niri msg {args}")


    print(niri_msg("version"))

    check_virtual_mic()

    start_target_terminal()
    machine.wait_until_succeeds(
        f"su - alice -c 'env NIRI_SOCKET={niri_socket} niri msg --json focused-window' | jq -e '.app_id == \"earwig-target\"'"
    )
    check_typing_reaches_target()
    machine.screenshot("target_terminal")

    ${common.dictation}
  '';
}
