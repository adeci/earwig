# The x86-64 package on older CPUs, as QEMU models them: on a Sandy Bridge (AVX without AVX2,
# a ThinkPad X220's) the daemon in sway's session loads transcribe.cpp's sandybridge CPU module;
# on the first x86-64 CPUs (SSE2, no SSE4.2 or AVX) the baseline module. Both transcribe the clip.
{ self }:
let
  common = import ./common.nix { inherit self; };
  sway = import ./sway.nix { inherit self; };
in
{
  name = "earwig-cpus";

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

  testScript = ''
    ${common.prelude}

    import re


    def cpu_flags(node) -> set[str]:
        return set(node.succeed("grep -m1 '^flags' /proc/cpuinfo").split(":", 1)[1].split())


    def transcribes(node) -> None:
        code, text = node.execute(
            "XDG_DATA_DIRS=/run/current-system/sw/share earwig transcribe /etc/earwig-test/clip.wav 2>/tmp/transcribe.log",
            timeout=900,
        )
        print(node.succeed("cat /tmp/transcribe.log"))
        assert code == 0 and "brown fox" in text.lower(), text


    start_all()

    with machine.nested("EARWIG: on a Sandy Bridge the daemon runs the sandybridge CPU module"):
        flags = cpu_flags(machine)
        assert "avx" in flags and "avx2" not in flags and "f16c" not in flags, flags
        user_unit_active("earwig.service")
        wait_status(lambda s: s["model"]["state"] == "ready", "the model", timeout=300)
        pid = machine.succeed("systemctl --user -M alice@ show -P MainPID earwig.service").strip()
        modules = set(re.findall(r"libggml-cpu-[\w.]+\.so", machine.succeed(f"cat /proc/{pid}/maps")))
        assert modules == {"libggml-cpu-sandybridge.so"}, modules
        transcribes(machine)

    with baseline.nested("EARWIG: on the first x86-64 CPUs the baseline CPU module transcribes"):
        flags = cpu_flags(baseline)
        assert "sse4_2" not in flags and "avx" not in flags, flags
        transcribes(baseline)
  '';
}
