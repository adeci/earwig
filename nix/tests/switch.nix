# Applying a configuration (`switch-to-configuration test`) to a running sway session: earwig
# starts when first enabled, with the keyboards in reach, after it was quit and after it had
# given up; restarts once for a changed setting, unless kept from restarts; is left alone when
# nothing changed; and never starts for a user whose manager runs without a graphical session.
# No speech model: nothing here dictates.
{ self }:
let
  common = import ./common.nix { inherit self; };
in
{
  name = "earwig-switch";

  nodes.machine =
    { pkgs, ... }:
    let
      earwig = options: {
        configuration = {
          services.earwig = {
            enable = true;
            models = [ ];
          }
          // options;
          # One line per start of the unit.
          systemd.user.services.earwig.serviceConfig.ExecStartPre =
            "${pkgs.runtimeShell} -c 'echo >> /tmp/earwig-starts'";
        };
      };
    in
    {
      imports = [ self.nixosModules.default ];

      users.users.alice = {
        isNormalUser = true;
        uid = 1000;
        password = "foobar";
      };
      services.getty.autologinUser = "alice";
      # A user manager without a graphical session.
      users.users.bob = {
        isNormalUser = true;
        uid = 1001;
        linger = true;
      };

      virtualisation = {
        memorySize = 2048;
        cores = 4;
        # As in common.nix: KMS for sway, software rendering.
        qemu.options = [ "-vga none -device virtio-gpu-pci" ];
      };
      hardware.graphics.enable = true;

      # The session of sway.nix.
      programs.sway.enable = true;
      environment.variables = {
        SWAYSOCK = "/tmp/sway-ipc.sock";
        WLR_RENDERER = "pixman";
      };
      programs.bash.loginShellInit = ''
        if [ "$(tty)" = "/dev/tty1" ]; then
          sway > /tmp/sway.log 2>&1
        fi
      '';
      # NixOS 26.05 and newer start active targets again on a switch, and sway's would pull in
      # graphical-session.target with all it wants. niri holds that target from a service, which
      # no switch starts again: kept out here, sway's target stands for that session too.
      systemd.user.targets.sway-session.unitConfig.X-OnlyManualStart = true;

      specialisation = {
        enabled = earwig { };
        rebound = earwig { settings.hotkey.key = "F9"; };
        # A build that cannot run here.
        broken = earwig { package = pkgs.writeShellScriptBin "earwig" "exit 1"; };
        pinned.configuration = {
          imports = [ (earwig { settings.hotkey.key = "F10"; }).configuration ];
          systemd.user.services.earwig.restartIfChanged = false;
        };
      };

      documentation.enable = false;
    };

  testScript = ''
    ${common.prelude}


    def apply(configuration: str) -> None:
        """Apply a specialisation as `nixos-rebuild test` does; returns once the unit's jobs are done."""
        machine.succeed(f"/run/booted-system/specialisation/{configuration}/bin/switch-to-configuration test >&2")
        machine.wait_until_succeeds(
            'test -z "$(systemctl --user -M alice@ list-jobs --no-legend earwig.service)"',
            timeout=td(minutes=1),
        )


    def unit(prop: str, name: str = "earwig.service", user: str = "alice") -> str:
        return machine.succeed(f"systemctl --user -M {user}@ show -P {prop} {name}").strip()


    def wait_state(name: str, *states: str) -> None:
        def reached(_last_try: bool) -> bool:
            return unit("ActiveState", name) in states

        retry(reached, timeout=td(minutes=1))


    def starts() -> int:
        return int(machine.succeed("wc -l < /tmp/earwig-starts"))


    def hotkey(key: str) -> None:
        wait_status(lambda s: s["hotkey"]["key"] == key, f"the hotkey {key}")


    start_all()
    machine.wait_for_unit("multi-user.target")
    machine.wait_for_file("/tmp/sway-ipc.sock")
    user_unit_active("sway-session.target")
    machine.wait_for_unit("user@1001.service")

    with machine.nested("EARWIG: enabled by the configuration, it starts in the running session and sees the keyboard"):
        apply("enabled")
        user_unit_active("earwig.service")
        watched = wait_status(lambda s: s["hotkey"]["source"] == "keyboard", "the keyboard watched")
        default_key = watched["hotkey"]["key"]
        pid = unit("MainPID")
        assert starts() == 1, starts()

    with machine.nested("EARWIG: the same configuration again leaves it alone"):
        apply("enabled")
        assert (unit("MainPID"), starts()) == (pid, 1), (unit("MainPID"), starts())

    with machine.nested("EARWIG: a changed setting restarts it once"):
        apply("rebound")
        hotkey("F9")
        assert unit("MainPID") != pid and starts() == 2, (pid, unit("MainPID"), starts())

    with machine.nested("EARWIG: quit, it starts again"):
        as_alice("earwig quit")
        wait_state("earwig.service", "inactive")
        apply("rebound")
        user_unit_active("earwig.service")
        hotkey("F9")
        assert starts() == 3, starts()

    with machine.nested("EARWIG: given up on after failing, it starts again once its build runs"):
        before = starts()
        apply("broken")

        def gave_up(_last_try: bool) -> bool:
            return unit("SubState") == "failed" and starts() - before == int(unit("StartLimitBurst"))

        retry(gave_up, timeout=td(minutes=1))
        apply("rebound")
        user_unit_active("earwig.service")
        hotkey("F9")

    with machine.nested("EARWIG: kept from restarts, a change leaves it running, and it starts when stopped"):
        pid = unit("MainPID")
        before = starts()
        apply("pinned")
        hotkey("F9")
        assert (unit("MainPID"), starts()) == (pid, before), (pid, unit("MainPID"), starts())
        as_alice("earwig quit")
        wait_state("earwig.service", "inactive")
        apply("pinned")
        user_unit_active("earwig.service")
        hotkey("F10")

    with machine.nested("EARWIG: never started for a user without a graphical session"):
        activated = unit("ExecMainStartTimestampMonotonic", "nixos-activation.service", "bob")
        apply("enabled")
        hotkey(default_key)
        assert unit("ExecMainStartTimestampMonotonic", "nixos-activation.service", "bob") != activated
        machine.wait_until_succeeds(
            'test -z "$(systemctl --user -M bob@ list-jobs --no-legend earwig.service)"',
            timeout=td(minutes=1),
        )
        assert unit("InactiveExitTimestampMonotonic", user="bob") == "0", unit("ActiveState", user="bob")
  '';
}
