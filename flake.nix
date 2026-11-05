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
      inherit (nixpkgs) lib;
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      treefmt = forAllSystems (pkgs: treefmt-nix.lib.evalModule pkgs ./nix/treefmt.nix);
      earwigFor =
        pkgs:
        pkgs.callPackage ./nix/package.nix {
          craneLib = crane.mkLib pkgs;
        };
    in
    {
      packages = forAllSystems (
        pkgs:
        let
          earwig = earwigFor pkgs;
        in
        {
          inherit earwig;
          default = earwig;
        }
      );

      # Pinned downloads of every curated model: `models.<id>` and
      # `models.default`.
      legacyPackages = forAllSystems (pkgs: {
        models = import ./nix/models.nix { inherit (pkgs) lib fetchurl; };
      });

      overlays.default = final: _prev: {
        earwig = earwigFor final;
      };

      nixosModules.default = import ./nix/module.nix self;

      formatter = forAllSystems (pkgs: treefmt.${pkgs.stdenv.hostPlatform.system}.config.build.wrapper);

      devShells = forAllSystems (pkgs: {
        default = pkgs.callPackage ./nix/devshell.nix {
          craneLib = crane.mkLib pkgs;
          earwig = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
        };
      });

      checks = forAllSystems (
        pkgs:
        let
          inherit (pkgs.stdenv.hostPlatform) system;
          earwig = self.packages.${system}.default;
          vmTest =
            file:
            pkgs.testers.runNixOSTest (
              import file {
                inherit self;
              }
            );
        in
        {
          package = earwig;
          inherit (earwig.passthru.tests) clippy tests;
          treefmt = treefmt.${system}.config.build.check self;
        }
        // lib.optionalAttrs (system == "x86_64-linux") {
          vm-niri = vmTest ./nix/tests/niri.nix;
          vm-sway = vmTest ./nix/tests/sway.nix;
          vm-kde = vmTest ./nix/tests/kde.nix;
          vm-cosmic = vmTest ./nix/tests/cosmic.nix;
          vm-models = vmTest ./nix/tests/models.nix;
          vm-gnome = vmTest ./nix/tests/gnome.nix;
        }
      );
    };
}
