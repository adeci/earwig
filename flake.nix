{
  description = "earwig: local, instant dictation for Linux desktops";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
      treefmt-nix,
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      treefmt = forAllSystems (pkgs: treefmt-nix.lib.evalModule pkgs ./nix/treefmt.nix);
    in
    {
      formatter = forAllSystems (pkgs: treefmt.${pkgs.stdenv.hostPlatform.system}.config.build.wrapper);

      devShells = forAllSystems (pkgs: {
        default = pkgs.callPackage ./nix/devshell.nix { };
      });

      checks = forAllSystems (
        pkgs:
        {
          treefmt = treefmt.${pkgs.stdenv.hostPlatform.system}.config.build.check self;
        }
        # GNOME slice: VM test (the Nix slice owns the final wiring).
        // nixpkgs.lib.optionalAttrs (pkgs.stdenv.hostPlatform.system == "x86_64-linux") {
          gnome = import ./nix/tests/gnome.nix {
            inherit pkgs;
            craneLib = crane.mkLib pkgs;
          };
        }
      );
    };
}
