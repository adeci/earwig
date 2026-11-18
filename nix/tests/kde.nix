# Dictation end to end on Plasma 6 without keyboard access: the hotkey through GlobalShortcuts;
# insertion through earwig's own data-control selections and Shift+Insert over the RemoteDesktop
# portal (KWin has no input-method-v2). KDE asks at start while earwig keeps answering; a
# shortcut refused there works once given a key in KDE's settings, and a dictation while typing
# is refused says what to allow and has KDE ask again. Dictations land once in KWrite, Konsole and
# foot, and after a restart nothing asks again.
{ self }:
let
  common = import ./common.nix { inherit self; };
in
{
  name = "earwig-kde";
  enableOCR = true;

  nodes.machine = {
    imports = [
      common.machine
      common.plasma
    ];
    services.earwig.keyboardAccess = false;
  };

  testScript = common.prelude + builtins.readFile ./kwin.py + builtins.readFile ./kde.py;
}
