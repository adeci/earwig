{
  lib,
  stdenv,
  craneLib,
  rustc,
  earwig,
  rust-analyzer,
}:
craneLib.devShell (
  {
    # Build inputs (cmake, glslc, Vulkan/Wayland/ALSA headers, the macOS SDK)
    # come from the package; craneLib adds cargo, rustc, clippy and rustfmt.
    inputsFrom = [ earwig ];
    packages = [ rust-analyzer ];
    RUST_SRC_PATH = "${rustc.unwrapped}/lib/rustlib/src/rust/library";
  }
  // lib.optionalAttrs stdenv.hostPlatform.isLinux {
    # GPUI and the Vulkan backend dlopen these at runtime; the package gets
    # them through RUNPATH, `cargo run` through this.
    LD_LIBRARY_PATH = lib.makeLibraryPath earwig.runtimeLibs;
  }
  // lib.optionalAttrs stdenv.hostPlatform.isDarwin {
    # clang's runtime library, as the package links it (see package.nix).
    inherit (earwig) NIX_LDFLAGS;
  }
)
