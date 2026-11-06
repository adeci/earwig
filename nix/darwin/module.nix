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
  # Same layout as the user's store: <owner>/<repo>/<commit>/<file>.
  models = pkgs.linkFarm "earwig-models" (
    map (m: {
      name = "${m.repo}/${m.revision}/${m.file}";
      path = m;
    }) cfg.models
  );
  # earwig's machine-wide settings and models.
  systemDir = "/Library/Application Support/earwig";
in
{
  options.services.earwig = {
    enable = lib.mkEnableOption ''
      earwig, local dictation.

      This installs earwig.app and a launchd user agent that starts it at
      login. On first start earwig asks for Microphone, Accessibility and
      Input Monitoring access, which System Settings grants'';

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
        `${systemDir}/models`), from `earwig.legacyPackages.''${system}.models`.
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
        hotkey.key = "RightAlt";
        audio.keep_open = false;
      };
      description = ''
        System-wide earwig configuration, written to
        `${systemDir}/config.toml`. See `earwig-core/src/config.rs` for the
        keys. Every key set here is locked: the settings window shows it
        read-only and user configuration cannot override it.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];

    system.activationScripts.postActivation.text = ''
      mkdir -p '${systemDir}'
      ln -sfn ${models} '${systemDir}/models'
      ${
        if cfg.settings != { } then
          "ln -sfn ${configFile} '${systemDir}/config.toml'"
        else
          "rm -f '${systemDir}/config.toml'"
      }
    '';

    launchd.user.agents.earwig.serviceConfig = {
      ProgramArguments = [
        "${cfg.package}/Applications/earwig.app/Contents/MacOS/earwig"
        "daemon"
      ];
      RunAtLoad = true;
      # Restart after a crash, not after `earwig quit` or the menu's Quit.
      KeepAlive.SuccessfulExit = false;
      ThrottleInterval = 10;
      ProcessType = "Interactive";
      LimitLoadToSessionType = "Aqua";
    };
  };
}
