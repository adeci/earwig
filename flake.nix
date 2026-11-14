{
  description = "earwig: local, instant dictation";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # mkEffect for the release effect (herculesCI below).
    nixbot = {
      url = "github:Mic92/nixbot";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.treefmt-nix.follows = "treefmt-nix";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
      treefmt-nix,
      nixbot,
    }:
    let
      inherit (nixpkgs) lib;
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
      forAllSystems = f: lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      treefmt = forAllSystems (pkgs: treefmt-nix.lib.evalModule pkgs ./nix/treefmt.nix);
      # On macOS the package is earwig.app (bin/earwig links into it); the
      # cargo build alone is its `unwrapped`.
      earwigFor =
        pkgs:
        let
          earwig = pkgs.callPackage ./nix/package.nix {
            craneLib = crane.mkLib pkgs;
          };
        in
        if pkgs.stdenv.hostPlatform.isDarwin then
          pkgs.callPackage ./nix/darwin/app.nix { inherit earwig; }
        else
          earwig;
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
      darwinModules.default = import ./nix/darwin/module.nix self;

      formatter = forAllSystems (pkgs: treefmt.${pkgs.stdenv.hostPlatform.system}.config.build.wrapper);

      devShells = forAllSystems (pkgs: {
        default = pkgs.callPackage ./nix/devshell.nix {
          craneLib = crane.mkLib pkgs;
          earwig =
            let
              earwig = self.packages.${pkgs.stdenv.hostPlatform.system}.earwig;
            in
            earwig.unwrapped or earwig;
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
          vm-keyd = vmTest ./nix/tests/keyd.nix;
          vm-kde = vmTest ./nix/tests/kde.nix;
          vm-cosmic = vmTest ./nix/tests/cosmic.nix;
          vm-models = vmTest ./nix/tests/models.nix;
          vm-gnome = vmTest ./nix/tests/gnome.nix;
        }
      );

      # nixbot's effects: a pushed v* tag gets its GitHub release once its commit built green.
      # Branches get none (`runIf false` would still build its inputs on every push), so their
      # builds stay the checks above.
      herculesCI =
        { primaryRepo, ... }:
        {
          onPush.default.outputs.effects =
            lib.optionalAttrs (primaryRepo.tag != null && lib.hasPrefix "v" primaryRepo.tag)
              {
                release =
                  let
                    # Effects run on the nixbot host.
                    pkgs = nixpkgs.legacyPackages.x86_64-linux;
                  in
                  pkgs.callPackage ./nix/release.nix {
                    inherit (nixbot.lib.effects { inherit pkgs; }) mkEffect;
                    inherit primaryRepo;
                  };
              };
        };
    };
}
