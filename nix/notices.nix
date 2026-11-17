{
  lib,
  stdenv,
  craneLib,
  cargo,
  cargo-about,
  diffutils,
}:
let
  root = ./..;
  # What cargo resolves. packaging/ is not here, so a notices edit does not re-vendor.
  cargoSrc = lib.fileset.unions [
    (root + "/Cargo.toml")
    (root + "/Cargo.lock")
    (root + "/crates")
    (root + "/vendor")
  ];
  cargoVendorDir = craneLib.vendorCargoDeps {
    src = lib.fileset.toSource {
      inherit root;
      fileset = cargoSrc;
    };
  };
in
stdenv.mkDerivation {
  name = "earwig-notices";
  src = lib.fileset.toSource {
    inherit root;
    fileset = lib.fileset.unions [
      cargoSrc
      (root + "/packaging/notices")
      (root + "/packaging/THIRD-PARTY-NOTICES.txt")
    ];
  };
  nativeBuildInputs = [
    cargo
    cargo-about
    diffutils
  ];
  dontConfigure = true;
  dontFixup = true;
  buildPhase = ''
    runHook preBuild
    export CARGO_HOME=$TMPDIR/cargo-home
    mkdir -p "$CARGO_HOME"
    cp ${cargoVendorDir}/config.toml "$CARGO_HOME/config.toml"
    packaging/notices/generate.sh "$TMPDIR/fresh.txt"
    diff -u packaging/THIRD-PARTY-NOTICES.txt "$TMPDIR/fresh.txt"
    runHook postBuild
  '';
  installPhase = ''
    runHook preInstall
    touch $out
    runHook postInstall
  '';
}
