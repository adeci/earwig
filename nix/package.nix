{
  lib,
  stdenv,
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
  apple-sdk_15,
  llvmPackages,
  jq,
}:
let
  inherit (stdenv.hostPlatform) isLinux;
  # One x86-64 package for every x86-64 CPU: transcribe.cpp as a shared library whose backends
  # are modules beside it, one CPU module per instruction-set generation, the best picked at run
  # time (`dynamic-backends`; inside Nix, ggml would otherwise build for the SSE2 floor).
  # Elsewhere it stays linked in: macOS has a fixed CPU floor and Metal, and on ARM the modules
  # would be a single build of the same portable CPU code.
  dynamicBackends = isLinux && stdenv.hostPlatform.isx86_64;
  root = ./..;
  src = lib.fileset.toSource {
    inherit root;
    fileset = lib.fileset.unions [
      (root + "/Cargo.toml")
      (root + "/Cargo.lock")
      (root + "/crates")
      (root + "/vendor")
      (root + "/assets")
      (root + "/tests/fixtures")
    ];
  };

  # GPUI and the Vulkan loader are dlopen'd; dlopen ignores buildInputs, so
  # these go into RUNPATH after fixup.
  runtimeLibs = lib.optionals isLinux [
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
    # Vendored from the real source: the dummy one is IFD, which breaks evaluating other systems.
    cargoVendorDir = craneLib.vendorCargoDeps { inherit src; };
    strictDeps = true;
    nativeBuildInputs = [
      cmake
    ]
    ++ lib.optionals isLinux [
      pkg-config
      shaderc
    ];
    buildInputs =
      if isLinux then
        [
          spirv-headers
          vulkan-headers
          alsa-lib
          fontconfig
          freetype
        ]
        ++ runtimeLibs
      else
        [ apple-sdk_15 ];
    # The cmake crate builds transcribe.cpp; there's no cmake configure phase.
    dontUseCmakeConfigure = true;
  }
  // lib.optionalAttrs stdenv.hostPlatform.isDarwin {
    # ggml's Metal backend checks the OS version at run time (`@available`), and rustc links
    # without clang's runtime library, which has that check.
    NIX_LDFLAGS = "${llvmPackages.compiler-rt}/lib/darwin/libclang_rt.osx.a";
  };

  cargoArtifacts = craneLib.buildDepsOnly (
    removeAttrs commonArgs [ "src" ]
    // {
      # The patched GPUI crates (vendor/) are path dependencies, which would get stubbed out
      # like the workspace's own crates.
      dummySrc = craneLib.mkDummySrc {
        inherit src;
        extraDummyScript = ''
          rm -rf $out/vendor
          cp -r --no-preserve=mode ${src}/vendor $out/vendor
        '';
      };
    }
  );
in
craneLib.buildPackage (
  commonArgs
  // {
    inherit cargoArtifacts;
    cargoExtraArgs =
      "--locked -p earwig" + lib.optionalString dynamicBackends " --features dynamic-backends";
    # Workspace tests run as the `tests` flake check, sharing cargoArtifacts.
    doCheck = false;

    nativeBuildInputs = commonArgs.nativeBuildInputs ++ lib.optionals isLinux [ addDriverRunpath ];

    postInstall =
      lib.optionalString isLinux ''
        install -Dm644 assets/io.github.adeci.earwig.desktop \
          $out/share/applications/io.github.adeci.earwig.desktop
        install -Dm644 assets/io.github.adeci.earwig.svg \
          $out/share/icons/hicolor/scalable/apps/io.github.adeci.earwig.svg
      ''
      # libtranscribe and its modules, in one directory: it loads them from beside itself. They
      # come from the install prefix of this build's transcribe-cpp-sys, as its manifest lists it.
      + lib.optionalString dynamicBackends ''
        prefix=$(${lib.getExe jq} -rR 'fromjson? | select(.reason == "build-script-executed"
          and (.package_id | contains("#transcribe-cpp-sys@"))) | .out_dir' "$cargoBuildLog")
        manifest=$(echo "$prefix"/lib*/transcribe-link.json)
        mkdir -p $out/lib/earwig
        for dir in $(${lib.getExe jq} -r '.lib_dir, .module_dir' "$manifest" | sort -u); do
          cp -P "$prefix/$dir"/lib*.so* $out/lib/earwig/
        done
      '';

    postFixup = lib.optionalString isLinux ''
      patchelf --add-rpath ${
        lib.makeLibraryPath runtimeLibs + lib.optionalString dynamicBackends ":$out/lib/earwig"
      } $out/bin/earwig
      addDriverRunpath $out/bin/earwig
    '';

    passthru = {
      inherit runtimeLibs;
      tests = {
        clippy = craneLib.cargoClippy (
          commonArgs
          // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--workspace --all-targets -- --deny warnings";
          }
        );
        # Model tests stay #[ignore]d: the sandbox has no network and no model.
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
      description = "Local, instant dictation";
      homepage = "https://github.com/adeci/earwig";
      # OFL: the IBM Plex Sans compiled into the binary.
      license = [
        lib.licenses.mit
        lib.licenses.ofl
      ];
      mainProgram = "earwig";
      platforms = lib.platforms.linux ++ lib.platforms.darwin;
    };
  }
)
