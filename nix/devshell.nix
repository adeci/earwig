{
  lib,
  mkShell,
  rustc,
  cargo,
  clippy,
  rustfmt,
  rust-analyzer,
  cmake,
  pkg-config,
  shaderc,
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
  # GPUI and the Vulkan backend dlopen these at runtime.
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
in
mkShell {
  packages = [
    rustc
    cargo
    clippy
    rustfmt
    rust-analyzer
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
  LD_LIBRARY_PATH = lib.makeLibraryPath runtimeLibs;
  RUST_SRC_PATH = "${rustc.unwrapped}/lib/rustlib/src/rust/library";
}
