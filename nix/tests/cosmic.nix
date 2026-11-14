# Dictation end to end on COSMIC without keyboard access: insertion through input-method-v2,
# the hotkey a COSMIC custom shortcut running `earwig toggle` (no GlobalShortcuts portal, and
# cosmic-comp binds fire on press only). Setup as in nixpkgs' cosmic test.
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
