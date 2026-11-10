{
  projectRootFile = "flake.nix";
  # Upstream code, kept as published so its patch stays readable.
  settings.global.excludes = [ "vendor/**" ];
  programs = {
    rustfmt = {
      enable = true;
      edition = "2024";
    };
    nixfmt.enable = true;
    deadnix.enable = true;
    statix.enable = true;
    taplo.enable = true;
  };
}
