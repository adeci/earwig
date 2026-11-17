# earwig-keys' Linux backend on a real kernel: its test plugs in a uinput keyboard after the
# listener started and drives the hotkey (a key with modifiers, a modifier alone, a chord), the
# recorder, and an unplug with the hotkey held. Root may write /dev/uinput.
{ self }:
{
  name = "earwig-uinput";

  nodes.machine =
    { pkgs, ... }:
    {
      hardware.uinput.enable = true;
      environment.systemPackages = [
        self.packages.${pkgs.stdenv.hostPlatform.system}.default.uinputTest
      ];
      documentation.enable = false;
    };

  testScript = ''
    machine.wait_for_unit("multi-user.target")
    out = machine.succeed("earwig-keys-uinput-test --include-ignored 2>&1")
    print(out)
    assert "1 passed" in out, out
  '';
}
