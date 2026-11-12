# earwig.app around the cargo build: Info.plist (menu-bar app, microphone
# usage text, bundle id io.github.adeci.earwig), the icon, an ad-hoc
# signature over the whole bundle, and `bin/earwig` for the command line.
{
  lib,
  stdenvNoCC,
  earwig,
  rcodesign,
  librsvg,
  libicns,
}:
stdenvNoCC.mkDerivation {
  pname = "earwig";
  inherit (earwig) version;
  dontUnpack = true;
  nativeBuildInputs = [
    rcodesign
    librsvg
    libicns
  ];
  installPhase = ''
    runHook preInstall
    app=$out/Applications/earwig.app
    mkdir -p $app/Contents/MacOS $app/Contents/Resources $out/bin
    cp ${earwig}/bin/earwig $app/Contents/MacOS/earwig
    substitute ${../../assets/macos/Info.plist} $app/Contents/Info.plist \
      --subst-var-by VERSION ${earwig.version}
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
