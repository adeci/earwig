# Dictation on Plasma 6 with the module's defaults: earwig reads the keyboard for its hotkey
# (Super+Space) and pastes through its own keyboard, the clipboard and the primary selection set
# by data-control, so no consent dialog ever shows; in KWrite, Konsole and foot, and with Dvorak
# active among two layouts.
{ self }:
let
  common = import ./common.nix { inherit self; };
in
{
  name = "earwig-kde-defaults";
  enableOCR = true;

  nodes.machine =
    { lib, ... }:
    {
      imports = [
        common.machine
        common.plasma
      ];
      environment.etc."xdg/kxkbrc".text = lib.generators.toINI { } {
        Layout = {
          Use = true;
          LayoutList = "us,us";
          VariantList = ",dvorak";
        };
      };
    };

  testScript = common.prelude + builtins.readFile ./kwin.py + builtins.readFile ./kde-defaults.py;
}
