{
  lib,
  craneLib,
  cmake,
  pkg-config,
  shaderc,
  addDriverRunpath,
  writableTmpDirAsHomeHook,
  spirv-headers,
  vulkan-headers,
  vulkan-loader,
  alsa-lib,
  wayland,
  libxkbcommon,
  fontconfig,
  freetype,
  libGL,
  libx11,
  libxcb,
  libxcursor,
  libxi,
  libxrandr,
}:
let
  root = ./..;
  src = lib.fileset.toSource {
    inherit root;
    fileset = lib.fileset.unions [
      (root + "/Cargo.toml")
      (root + "/Cargo.lock")
      (root + "/crates")
      (root + "/assets")
      (root + "/tests/fixtures")
    ];
  };

  # GPUI and the Vulkan loader are dlopen'd; dlopen ignores buildInputs, so
  # these go into RUNPATH after fixup.
  runtimeLibs = [
    vulkan-loader
    wayland
    libxkbcommon
    libGL
    libx11
    libxcb
    libxcursor
    libxi
    libxrandr
  ];

  commonArgs = {
    pname = "earwig";
    inherit ((lib.importTOML ../Cargo.toml).workspace.package) version;
    inherit src;
    strictDeps = true;
    nativeBuildInputs = [
      cmake
      pkg-config
      shaderc
    ];
    buildInputs = [
      spirv-headers
      vulkan-headers
      alsa-lib
      fontconfig
      freetype
    ]
    ++ runtimeLibs;
    # transcribe.cpp is built through the `cmake` crate, not by a cmake
    # configure phase.
    dontUseCmakeConfigure = true;
  };

  cargoArtifacts = craneLib.buildDepsOnly commonArgs;
in
craneLib.buildPackage (
  commonArgs
  // {
    inherit cargoArtifacts;
    cargoExtraArgs = "--locked -p earwig";
    # Workspace tests run as the `tests` flake check, sharing cargoArtifacts.
    doCheck = false;

    nativeBuildInputs = commonArgs.nativeBuildInputs ++ [ addDriverRunpath ];

    postInstall = ''
      install -Dm644 assets/io.github.adeci.earwig.desktop \
        $out/share/applications/io.github.adeci.earwig.desktop
      install -Dm644 assets/io.github.adeci.earwig.svg \
        $out/share/icons/hicolor/scalable/apps/io.github.adeci.earwig.svg
    '';

    postFixup = ''
      patchelf --add-rpath ${lib.makeLibraryPath runtimeLibs} $out/bin/earwig
      addDriverRunpath $out/bin/earwig
    '';

    passthru = {
      inherit cargoArtifacts commonArgs;
      tests = {
        clippy = craneLib.cargoClippy (
          commonArgs
          // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--workspace --all-targets -- --deny warnings";
          }
        );
        # Model-dependent tests are #[ignore]d and stay that way here: the
        # sandbox has no network and no model.
        tests = craneLib.cargoTest (
          commonArgs
          // {
            inherit cargoArtifacts;
            nativeBuildInputs = commonArgs.nativeBuildInputs ++ [ writableTmpDirAsHomeHook ];
            cargoTestExtraArgs = "--workspace";
          }
        );
      };
    };

    meta = {
      description = "Local, instant dictation for Linux desktops";
      homepage = "https://github.com/adeci/earwig";
      # OFL: the IBM Plex Sans compiled into the binary.
      license = [
        lib.licenses.mit
        lib.licenses.ofl
      ];
      mainProgram = "earwig";
      platforms = lib.platforms.linux;
    };
  }
)
