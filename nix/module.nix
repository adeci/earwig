self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.earwig;
  format = pkgs.formats.toml { };
  configFile = format.generate "earwig-config.toml" cfg.settings;
  inherit (pkgs.stdenv.hostPlatform) system;
in
{
  options.services.earwig = {
    enable = lib.mkEnableOption ''
      earwig, local dictation for the graphical session.

      This installs the `earwig` command and a systemd user service that
      starts with `graphical-session.target`. earwig watches its hotkey itself
      (see `keyboardAccess`); the desktop must provide the rest:
      - text is inserted through the input-method and virtual-keyboard
        protocols of niri, sway or Hyprland, or on GNOME and KDE through the
        RemoteDesktop portal (enable `xdg.portal` with the desktop's backend,
        `xdg-desktop-portal-gnome` / `-kde`)
      - an audio server (PipeWire) for the microphone'';

    keyboardAccess = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Let the user at the seat read keyboards and make virtual ones (a udev
        `uaccess` rule), so earwig sees its hotkey on any desktop and keeps it
        from other apps. Any program of that user can then read what is
        typed. Without it, GNOME and KDE deliver the hotkey through the
        GlobalShortcuts portal; elsewhere bind keys to `earwig press` /
        `earwig release` (or `earwig toggle`) and `earwig cancel` in the
        compositor.
      '';
    };

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${system}.default;
      defaultText = lib.literalExpression "earwig.packages.\${system}.default";
      description = "The earwig package.";
    };

    models = lib.mkOption {
      type = lib.types.listOf lib.types.package;
      default = [ self.legacyPackages.${system}.models.default ];
      defaultText = lib.literalExpression "[ earwig.legacyPackages.\${system}.models.default ]";
      example = lib.literalExpression ''
        let models = earwig.legacyPackages.''${system}.models; in [
          models.default
          models."nemotron-3.5-asr-streaming-0.6b"
        ]
      '';
      description = ''
        Models fetched at build time and installed system-wide (under
        `share/earwig/models`), from `earwig.legacyPackages.''${system}.models`.
        earwig finds them like downloaded models, so dictation works offline
        right after install, and users can still switch models or download
        others in the settings window. To force one model, set
        `settings.model.model` (which locks the choice).
      '';
    };

    settings = lib.mkOption {
      inherit (format) type;
      default = { };
      example = {
        hotkey.mode = "hold";
        audio.keep_open = false;
      };
      description = ''
        System-wide earwig configuration, written to
        `/etc/xdg/earwig/config.toml`. See `earwig-core/src/config.rs` for
        the keys. Every key set here is locked: the settings window shows it
        read-only and user configuration cannot override it.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [
      cfg.package
      # Same layout as the user's store: <owner>/<repo>/<commit>/<file>.
      (pkgs.linkFarm "earwig-models" (
        map (m: {
          name = "share/earwig/models/${m.repo}/${m.revision}/${m.file}";
          path = m;
        }) cfg.models
      ))
    ];
    # earwig looks for pre-installed models in $XDG_DATA_DIRS/earwig/models;
    # /run/current-system/sw/share only carries the subtrees listed here.
    environment.pathsToLink = [ "/share/earwig" ];

    environment.etc."xdg/earwig/config.toml" = lib.mkIf (cfg.settings != { }) {
      source = configFile;
    };

    # The rule must sort before 73-seat-late.rules, which applies uaccess;
    # services.udev.extraRules lands in 99-local.rules, too late.
    services.udev.packages = lib.mkIf cfg.keyboardAccess [
      (pkgs.writeTextDir "lib/udev/rules.d/70-earwig.rules" (
        builtins.readFile ../assets/udev/70-earwig.rules
      ))
    ];
    boot.kernelModules = lib.mkIf cfg.keyboardAccess [ "uinput" ];

    # The portal inserter spots terminals by their AT-SPI role, which Qt only exports when asked;
    # without this Konsole gets Ctrl+V (a literal ^V) instead of Ctrl+Shift+V.
    environment.sessionVariables.QT_LINUX_ACCESSIBILITY_ALWAYS_ON = lib.mkDefault "1";

    systemd.user.services.earwig = {
      description = "earwig dictation daemon";
      partOf = [ "graphical-session.target" ];
      # The daemon probes the portal once at startup, and at login the portal is still being
      # D-Bus activated, so start after it. Harmless where there's no portal.
      after = [
        "graphical-session.target"
        "xdg-desktop-portal.service"
      ];
      wants = [ "xdg-desktop-portal.service" ];
      wantedBy = [ "graphical-session.target" ];
      # Restarting picks up a new model or settings after a rebuild.
      restartTriggers = [ configFile ];
      # Give up on a daemon that keeps failing (no model, broken config) instead of looping.
      startLimitBurst = 5;
      startLimitIntervalSec = 60;
      serviceConfig = {
        ExecStart = "${lib.getExe cfg.package} daemon";
        Restart = "on-failure";
        RestartSec = 2;
      };
    };
  };
}
