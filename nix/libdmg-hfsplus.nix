# `hfsplus` and `dmg`: Mozilla's libdmg-hfsplus, which writes HFS+ volumes and UDIF disk images
# on Linux. Firefox and Chromium build their macOS DMGs with it; packaging/macos/dmg.sh does too.
{
  lib,
  stdenv,
  fetchFromGitHub,
  cmake,
  bzip2,
  zlib,
}:
stdenv.mkDerivation {
  pname = "libdmg-hfsplus";
  version = "0-unstable-2026-10-14";

  src = fetchFromGitHub {
    owner = "mozilla";
    repo = "libdmg-hfsplus";
    rev = "e18579b391950589b9ed16ee630c54ce4319bb29";
    hash = "sha256-fQfGPNBNNRzAzhbfjDdk4rOa5tTA/cHsFo19YBKyzyU=";
  };

  nativeBuildInputs = [ cmake ];
  buildInputs = [
    bzip2
    zlib
  ];

  meta = {
    description = "Tools to make HFS+ volumes and DMG disk images";
    homepage = "https://github.com/mozilla/libdmg-hfsplus";
    license = lib.licenses.gpl3Only;
    platforms = lib.platforms.unix;
  };
}
