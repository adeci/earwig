{
  projectRootFile = "flake.nix";
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
