# GNOME with the module as it comes: earwig reads Super+Space from the keyboard and keeps it
# from the shell, its icon sits in the top bar through the extension the module switches on, and
# a dictation lands in Text Editor.
{ self }:
let
  common = import ./common.nix { inherit self; };
in
{
  name = "earwig-gnome-defaults";
  enableOCR = true;

  nodes.machine =
    { pkgs, lib, ... }:
    {
      imports = [ common.machine ];

      virtualisation.memorySize = lib.mkForce 6144;

      # GDM logs alice in, not the tty.
      services.getty.autologinUser = lib.mkForce null;
      services.displayManager.gdm.enable = true;
      services.displayManager.autoLogin = {
        enable = true;
        user = "alice";
      };
      services.desktopManager.gnome.enable = true;
      services.gnome.gnome-initial-setup.enable = false;
      environment.gnome.excludePackages = [ pkgs.gnome-tour ];

      # The test reads the shell's windows, panel and input source through Shell.Eval, which is
      # internal API. A drop-in applies to GNOME 49's unit and to 50's template alike.
      systemd.user.services."org.gnome.Shell@" = {
        overrideStrategy = "asDropin";
        # The shell keeps the session's PATH, where it finds the programs of desktop entries.
        enableDefaultPath = false;
        serviceConfig.ExecStart = [
          ""
          "${pkgs.gnome-shell}/bin/gnome-shell --unsafe-mode"
        ];
      };

      programs.dconf.profiles.user.databases = [
        {
          settings = {
            "org/gnome/desktop/session".idle-delay = lib.gvariant.mkUint32 0;
            "org/gnome/desktop/screensaver".lock-enabled = false;
            "org/gnome/desktop/notifications".show-banners = false;
            # A second layout: a Super+Space that reached GNOME would switch to it.
            "org/gnome/desktop/input-sources".sources = map lib.gvariant.mkTuple [
              [
                "xkb"
                "us"
              ]
              [
                "xkb"
                "de"
              ]
            ];
          };
        }
      ];
    };

  testScript = common.prelude + builtins.readFile ./gnome-defaults.py;
}
