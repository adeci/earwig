# GNOME on Wayland: RemoteDesktop/libei insertion (typing and paste), the
# restore token, GlobalShortcuts press/release, and the overlay experiment.
#
#   nix build .#checks.x86_64-linux.gnome -L
{
  pkgs,
  craneLib,
}:
let
  inherit (pkgs) lib;

  src = lib.fileset.toSource {
    root = ../..;
    fileset = lib.fileset.unions [
      ../../Cargo.toml
      ../../Cargo.lock
      ../../crates
    ];
  };

  # The portal harness (earwig-core example): no GPU or audio stack needed.
  portalCheck =
    let
      args = {
        pname = "earwig-portal-check";
        version = "0";
        inherit src;
        strictDeps = true;
        nativeBuildInputs = [ pkgs.pkg-config ];
        buildInputs = [
          pkgs.alsa-lib
          pkgs.libxkbcommon
        ];
        cargoExtraArgs = "--locked -p earwig-core";
      };
    in
    craneLib.buildPackage (
      args
      // {
        cargoArtifacts = craneLib.buildDepsOnly args;
        cargoExtraArgs = "--locked -p earwig-core --example portal_check";
        doCheck = false;
        installPhaseCommand = ''
          install -Dm755 target/release/examples/portal_check $out/bin/portal_check
        '';
      }
    );

  # The GPUI overlay experiment (earwig example). GPUI dlopens these.
  runtimeLibs = with pkgs; [
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
  gnomeOverlay =
    let
      args = {
        pname = "earwig-gnome-overlay";
        version = "0";
        inherit src;
        strictDeps = true;
        nativeBuildInputs = with pkgs; [
          cmake
          pkg-config
          shaderc
        ];
        buildInputs =
          with pkgs;
          [
            spirv-headers
            vulkan-headers
            alsa-lib
            fontconfig
            freetype
          ]
          ++ runtimeLibs;
        dontUseCmakeConfigure = true;
        cargoExtraArgs = "--locked -p earwig";
      };
    in
    craneLib.buildPackage (
      args
      // {
        cargoArtifacts = craneLib.buildDepsOnly args;
        cargoExtraArgs = "--locked -p earwig --example gnome_overlay";
        doCheck = false;
        nativeBuildInputs = args.nativeBuildInputs ++ [ pkgs.addDriverRunpath ];
        installPhaseCommand = ''
          install -Dm755 target/release/examples/gnome_overlay $out/bin/gnome_overlay
        '';
        postFixup = ''
          patchelf --add-rpath ${lib.makeLibraryPath runtimeLibs} $out/bin/gnome_overlay
          addDriverRunpath $out/bin/gnome_overlay
        '';
      }
    );

  # GNOME only talks to host apps whose app id matches an installed desktop
  # file.
  desktopFile = pkgs.makeDesktopItem {
    name = "io.github.adeci.earwig";
    desktopName = "earwig";
    exec = "portal_check";
  };

  # A GTK 4 text view that records when text arrives: <out> holds the text,
  # <out>.times "<unix-us> <chars>" per change.
  targetScript = pkgs.writers.writePython3 "earwig-target" {
    libraries = [ pkgs.python3Packages.pygobject3 ];
    flakeIgnore = [ "E501" ];
  } ''
    import os
    import sys
    import time

    import gi

    gi.require_version("Gtk", "4.0")
    from gi.repository import Gio, GLib, Gtk  # noqa: E402

    out = sys.argv[1]
    times = []
    dirty = [False]


    def changed(buf):
        text = buf.get_text(buf.get_start_iter(), buf.get_end_iter(), False)
        times.append((time.time_ns() // 1000, len(text)))
        dirty[0] = text


    def flush():
        if dirty[0] is not False:
            with open(out + ".tmp", "w") as f:
                f.write(dirty[0])
            os.replace(out + ".tmp", out)
            with open(out + ".times", "w") as f:
                f.writelines(f"{t} {n}\n" for t, n in times)
            dirty[0] = False
        return True


    def activate(app):
        win = Gtk.ApplicationWindow(application=app, title="earwig-target")
        win.set_default_size(1000, 520)
        view = Gtk.TextView()
        view.set_wrap_mode(Gtk.WrapMode.WORD_CHAR)
        view.get_buffer().connect("changed", changed)
        win.set_child(view)
        win.present()
        view.grab_focus()


    GLib.timeout_add(100, flush)
    app = Gtk.Application(
        application_id="org.earwig.Target", flags=Gio.ApplicationFlags.NON_UNIQUE
    )
    app.connect("activate", activate)
    app.run(None)
  '';
  target = pkgs.stdenvNoCC.mkDerivation {
    name = "earwig-target";
    dontUnpack = true;
    nativeBuildInputs = [
      pkgs.wrapGAppsHook4
      pkgs.gobject-introspection
    ];
    buildInputs = [ pkgs.gtk4 ];
    installPhase = "install -Dm755 ${targetScript} $out/bin/earwig-target";
  };

  # Run a command as alice in her graphical session (systemd user manager).
  launch = pkgs.writeShellScriptBin "launch" ''
    # launch <unit> <command...>: start as a transient user unit, output in
    # /tmp/<unit>.log
    unit=$1
    shift
    exec runuser -u alice -- env \
      DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus \
      XDG_RUNTIME_DIR=/run/user/1000 \
      systemd-run --user --collect --quiet --unit="$unit" \
        --setenv=RUST_LOG=info,earwig_core=debug \
        -p StandardOutput=file:/tmp/"$unit".log \
        -p StandardError=file:/tmp/"$unit".err \
        -- "$@"
  '';
  shellEval = pkgs.writeShellScriptBin "shell-eval" ''
    # shell-eval <js>: evaluate in gnome-shell (unsafe mode), print the result
    exec runuser -u alice -- env \
      DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus \
      gdbus call --session -d org.gnome.Shell -o /org/gnome/Shell \
        -m org.gnome.Shell.Eval "$1"
  '';
in
pkgs.testers.runNixOSTest {
  name = "earwig-gnome";
  enableOCR = true;

  nodes.machine = {
    imports = [ "${pkgs.path}/nixos/tests/common/user-account.nix" ];

    virtualisation = {
      memorySize = 4096;
      cores = 4;
      qemu.options = [
        "-vga none"
        "-device virtio-gpu-pci"
      ];
    };

    services.displayManager.gdm.enable = true;
    services.displayManager.autoLogin = {
      enable = true;
      user = "alice";
    };
    services.desktopManager.gnome.enable = true;
    services.gnome.gnome-initial-setup.enable = false;
    environment.gnome.excludePackages = [ pkgs.gnome-tour ];

    # Eval is internal API; the test needs it to read focus and window
    # geometry.
    systemd.user.services."org.gnome.Shell@".serviceConfig.ExecStart = [
      ""
      "${pkgs.gnome-shell}/bin/gnome-shell --unsafe-mode"
    ];

    programs.dconf.profiles.user.databases = [
      {
        settings = {
          "org/gnome/desktop/session".idle-delay = lib.gvariant.mkUint32 0;
          "org/gnome/desktop/screensaver".lock-enabled = false;
          "org/gnome/desktop/notifications".show-banners = false;
        };
      }
    ];

    environment.systemPackages = [
      portalCheck
      gnomeOverlay
      desktopFile
      target
      launch
      shellEval
    ];
  };

  testScript = builtins.readFile ./gnome.py;
}
