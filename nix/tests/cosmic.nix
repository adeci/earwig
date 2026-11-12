# Dictation end to end on COSMIC (cosmic-comp), chosen by capability only:
# cosmic-comp offers layer-shell, input-method-v2, virtual-keyboard and
# data-control to unsandboxed clients, while xdg-desktop-portal-cosmic has
# no GlobalShortcuts, so the hotkey is a COSMIC custom shortcut that runs
# `earwig toggle`. cosmic-comp fires key bindings on press only (and
# modifier-only bindings on release only; src/input/mod.rs), so a COSMIC
# shortcut can toggle but not hold-to-talk.
#
# Session setup follows nixpkgs nixos/tests/cosmic.
{ self }:
let
  common = import ./common.nix { inherit self; };
in
{
  name = "earwig-cosmic";
  enableOCR = true;

  nodes.machine =
    { pkgs, lib, ... }:
    {
      imports = [ common.machine ];

      # The fallback without keyboard access: a compositor shortcut.
      services.earwig.keyboardAccess = false;

      services.getty.autologinUser = lib.mkForce null;

      services.displayManager.cosmic-greeter.enable = true;
      services.desktopManager.cosmic.enable = true;
      services.displayManager.autoLogin = {
        enable = true;
        user = "alice";
      };
      # cosmic-initial-setup would take focus on first login.
      systemd.tmpfiles.rules = [
        "d /home/alice/.config 0755 alice users -"
        "f /home/alice/.config/cosmic-initial-setup-done 0644 alice users -"
      ];
      environment.systemPackages = [ pkgs.lswt ];

      systemd.user.services.earwig.environment.RUST_LOG =
        "info,earwig=debug,earwig_core=debug,zbus=warn,tracing=warn";
    };

  testScript = common.prelude + builtins.readFile ./cosmic.py;
}
