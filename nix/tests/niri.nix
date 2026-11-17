# Dictation end to end on niri, driven through the CLI: virtual mic -> earwig -> focused foot.
# niri's TTY backend skips software EGL, all QEMU offers here, so niri runs nested in cage
# (pixman); everything earwig talks to is still niri's own. With a model that doesn't stream: live
# text from re-decoding, the final text from one decode; and after a recording cancelled mid-clip
# the model dictates again.
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

      services.earwig = {
        models = [ self.legacyPackages.${pkgs.stdenv.hostPlatform.system}.models."parakeet-tdt-ctc-110m" ];
        settings.model.model = "parakeet-tdt-ctc-110m";
      };

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

    with machine.nested("EARWIG: a recording cancelled mid-clip leaves the model ready"):
        cancel_mid_clip(lambda: as_alice("earwig press"))
        as_alice("earwig release")
        machine.succeed("truncate --no-create -s 0 /tmp/out")
        events = watch_events()
        as_alice("earwig press")
        play_clip()
        machine.wait_until_fails("pgrep -x pw-play", timeout=td(seconds=30))
        as_alice("earwig release")
        print(wait_insertion(events))
        machine.wait_until_succeeds("grep -qi 'brown fox' /tmp/out", timeout=td(minutes=4))
  '';
}
