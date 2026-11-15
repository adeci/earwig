# earwig.app around the cargo build: Info.plist, the icon, an ad-hoc signature over the whole
# bundle, and `bin/earwig` for the command line. The app stands alone, so the release signs this
# very bundle for Macs without Nix (packaging/macos/dmg.sh).
{
  lib,
  stdenvNoCC,
  earwig,
  cctools,
  rcodesign,
  librsvg,
  libicns,
}:
let
  plist = ../../assets/macos/Info.plist;
  # The oldest macOS Info.plist promises, checked against the executable's below.
  minimumSystemVersion = lib.head (
    builtins.match ".*<key>LSMinimumSystemVersion</key>[[:space:]]*<string>([^<]*)</string>.*" (
      builtins.readFile plist
    )
  );
  inherit (lib.versions) major minor patch;
  inherit (earwig) version;
in
stdenvNoCC.mkDerivation {
  pname = "earwig";
  inherit version;
  dontUnpack = true;
  nativeBuildInputs = [
    cctools
    rcodesign
    librsvg
    libicns
  ];
  installPhase = ''
    runHook preInstall
    app=$out/Applications/earwig.app
    exe=$app/Contents/MacOS/earwig
    mkdir -p $app/Contents/MacOS $app/Contents/Resources $out/bin
    install -m 755 ${earwig}/bin/earwig $exe

    # Rust's libc crate links libiconv, nixpkgs' here; every Mac has its own.
    for dylib in $(otool -L $exe | awk '$1 ~ "^/nix/store/.*/libiconv[.]2[.]dylib$" { print $1 }'); do
      install_name_tool -change $dylib /usr/lib/libiconv.2.dylib $exe
    done
    if otool -l $exe | grep -E '^ +(name|path) /nix/store/'; then
      echo 'earwig would load the above from /nix/store, which Macs without Nix lack' >&2
      exit 1
    fi
    minos=$(otool -l $exe | awk '$1 == "minos" { print $2 }')
    if [[ $minos != "${minimumSystemVersion}" ]]; then
      echo "earwig runs on macOS $minos and newer: say so in Info.plist and the README" >&2
      exit 1
    fi

    # CFBundleVersion is digits and periods only: 0.1.0 for 0.1.0-rc.1.
    substitute ${plist} $app/Contents/Info.plist \
      --subst-var-by VERSION ${version} \
      --subst-var-by BUNDLE_VERSION ${major version}.${minor version}.${patch version}
    # The 16 px frame from the icon drawn for that size.
    rsvg-convert -w 16 -h 16 ${../../assets/icon-small.svg} -o icon_16.png
    for size in 32 128 256 512; do
      rsvg-convert -w $size -h $size ${../../assets/io.github.adeci.earwig.svg} -o icon_$size.png
    done
    png2icns $app/Contents/Resources/earwig.icns icon_*.png
    # No certificate: an ad-hoc signature, sealing Info.plist and resources.
    chmod -R u+w $app
    rcodesign sign $app
    ln -s ../Applications/earwig.app/Contents/MacOS/earwig $out/bin/earwig
    runHook postInstall
  '';
  # Fixup would re-sign the executable alone and break the bundle's seal.
  dontFixup = true;
  passthru = earwig.passthru // {
    unwrapped = earwig;
  };
  meta = earwig.meta // {
    platforms = lib.platforms.darwin;
  };
}
