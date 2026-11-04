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
      starts with `graphical-session.target`. The desktop must provide the
      rest:
      - niri, sway, Hyprland: bind keys to `earwig press` / `earwig release`
        (or `earwig toggle`) and `earwig cancel` in the compositor config;
        text is inserted through the input-method and virtual-keyboard
        protocols the compositor already offers.
      - GNOME, KDE: enable `xdg.portal` with the desktop's backend
        (`xdg-desktop-portal-gnome` / `-kde`); earwig registers its shortcut
        through the GlobalShortcuts portal and types through RemoteDesktop.
      - an audio server (PipeWire) for the microphone'';

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${system}.default;
      defaultText = lib.literalExpression "earwig.packages.\${system}.default";
      description = "The earwig package.";
    };

    model = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = self.legacyPackages.${system}.models.default;
      defaultText = lib.literalExpression "earwig.legacyPackages.\${system}.models.default";
      example = lib.literalExpression "earwig.legacyPackages.\${system}.models.\"parakeet-unified-en-0.6b\"";
      description = ''
        GGUF model file to use, typically a pinned download from
        `earwig.legacyPackages.''${system}.models`. The default is the
        catalog's default model fetched at build time, so dictation works
        offline right after install. This sets `settings.model.model`, which
        locks the model in the settings window; set it to `null` to let
        users pick and download models themselves.
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
    services.earwig.settings.model.model = lib.mkIf (cfg.model != null) "${cfg.model}";

    environment.systemPackages = [ cfg.package ];

    environment.etc."xdg/earwig/config.toml" = lib.mkIf (cfg.settings != { }) {
      source = configFile;
    };

    systemd.user.services.earwig = {
      description = "earwig dictation daemon";
      partOf = [ "graphical-session.target" ];
      after = [ "graphical-session.target" ];
      wantedBy = [ "graphical-session.target" ];
      # Restarting picks up a new model or settings after a rebuild.
      restartTriggers = [ configFile ];
      serviceConfig = {
        ExecStart = "${lib.getExe cfg.package} daemon";
        Restart = "on-failure";
        RestartSec = 2;
      };
    };
  };
}
