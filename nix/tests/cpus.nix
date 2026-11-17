# The x86-64 package on older CPUs, as QEMU models them: on a Sandy Bridge (AVX without AVX2,
# a ThinkPad X220's) the sway test dictates with transcribe.cpp's sandybridge CPU module; on the
# first x86-64 CPUs (SSE2, no SSE4.2 or AVX) the baseline module still transcribes.
{ self }:
let
  sway = import ./sway.nix { inherit self; };
in
{
  name = "earwig-cpus";
  inherit (sway) enableOCR;

  nodes = {
    machine = {
      imports = [ sway.nodes.machine ];
      virtualisation.qemu.options = [ "-cpu SandyBridge" ];
    };

    baseline = {
      imports = [ self.nixosModules.default ];
      services.earwig = {
        enable = true;
        settings.model.backend = "cpu";
      };
      virtualisation = {
        memorySize = 4096;
        cores = 8;
        qemu.options = [ "-cpu qemu64" ];
      };
      environment.etc."earwig-test/clip.wav".source = ../../tests/fixtures/quick-brown-fox.wav;
      documentation.enable = false;
    };
  };

  testScript = sway.testScript + ''

    import re


    def cpu_flags(node) -> set[str]:
        return set(node.succeed("grep -m1 '^flags' /proc/cpuinfo").split(":", 1)[1].split())


    def cpu_modules(node, pid: str) -> set[str]:
        """The transcribe.cpp CPU modules process `pid` has loaded."""
        maps = node.succeed(f"cat /proc/{pid}/maps")
        return set(re.findall(r"libggml-cpu-[\w.]+\.so", maps))


    with machine.nested("EARWIG: on a Sandy Bridge the daemon runs the sandybridge CPU module"):
        flags = cpu_flags(machine)
        assert "avx" in flags and "avx2" not in flags and "f16c" not in flags, flags
        pid = machine.succeed("systemctl --user -M alice@ show -P MainPID earwig.service").strip()
        print(machine.execute("journalctl _UID=1000 _SYSTEMD_USER_UNIT=earwig.service -o cat --no-pager | grep -E 'transcribe_init_backends|loaded CPU backend'")[1])
        modules = cpu_modules(machine, pid)
        assert modules == {"libggml-cpu-sandybridge.so"}, modules

    with baseline.nested("EARWIG: on the first x86-64 CPUs the baseline CPU module transcribes"):
        flags = cpu_flags(baseline)
        assert "sse4_2" not in flags and "avx" not in flags, flags
        code, text = baseline.execute(
            "XDG_DATA_DIRS=/run/current-system/sw/share earwig transcribe /etc/earwig-test/clip.wav 2>/tmp/transcribe.log",
            timeout=900,
        )
        print(baseline.succeed("cat /tmp/transcribe.log"))
        assert code == 0 and "brown fox" in text.lower(), text
  '';
}
