{
  lib,
  craneLib,
  rustc,
  earwig,
  rust-analyzer,
  vulkan-loader,
  wayland,
  libxkbcommon,
  libGL,
  libx11,
  libxcb,
  libxcursor,
  libxi,
  libxrandr,
}:
craneLib.devShell {
  # Build inputs (cmake, glslc, Vulkan/Wayland/ALSA headers) come from the
  # package; craneLib adds cargo, rustc, clippy and rustfmt.
  inputsFrom = [ earwig ];
  packages = [ rust-analyzer ];
  # GPUI and the Vulkan backend dlopen these at runtime; the package gets
  # them through RUNPATH, `cargo run` through this.
  LD_LIBRARY_PATH = lib.makeLibraryPath [
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
  RUST_SRC_PATH = "${rustc.unwrapped}/lib/rustlib/src/rust/library";
}
