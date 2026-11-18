# GNOME with the module as it comes: earwig reads Super+Space from the keyboard and keeps it
# from the shell, its icon sits in the top bar through the extension the module switches on, and
# a dictation is pasted into Text Editor and into foot, a terminal, without anything asking
# first.
{ self }:
let
  common = import ./common.nix { inherit self; };
in
{
  name = "earwig-gnome-defaults";
  enableOCR = true;

  nodes.machine =
    { lib, ... }:
    {
      imports = [
        common.machine
        common.gnome
      ];
      # A second layout: a Super+Space that reached GNOME would switch to it.
      programs.dconf.profiles.user.databases = [
        {
          settings."org/gnome/desktop/input-sources".sources = map lib.gvariant.mkTuple [
            [
              "xkb"
              "us"
            ]
            [
              "xkb"
              "de"
            ]
          ];
        }
      ];
    };

  testScript =
    common.prelude + builtins.readFile ./gnome-shell.py + builtins.readFile ./gnome-defaults.py;
}
