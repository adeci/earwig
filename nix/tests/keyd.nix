# keyd holds the keyboard (a real desktop's keyd.conf): earwig watches keyd's virtual keyboard
# and says it can't keep the hotkey from apps. After Super is held through workspace switches,
# plain Spaces must start nothing; Super+Space still starts a dictation.
{ self }:
let
  common = import ./common.nix { inherit self; };
in
{
  name = "earwig-keyd";

  nodes.machine =
    { ... }:
    {
      imports = [ common.machine ];

      services.keyd.enable = true;
      environment.etc."keyd/default.conf".source = ./keyd.conf;
      # QEMU's PS/2 keyboard alone, a laptop's "AT Translated Set 2
      # keyboard", which keyd holds and earwig would.
      virtualisation.qemu.virtioKeyboard = false;

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
    };

  testScript = ''
    ${common.prelude}

    import time


    start_all()
    machine.wait_for_unit("keyd.service")
    machine.wait_for_file("/tmp/sway-ipc.sock")
    user_unit_active("earwig.service")

    with subtest("EARWIG: keyd holds the keyboard; earwig watches keyd's"):
        st = wait_status(
            lambda s: s["hotkey"]["source"] == "keyboard" and s["hotkey"].get("reason"),
            "the keyboard watched behind keyd",
        )
        print(st["hotkey"])
        assert not st["hotkey"]["blocking"], st
        assert "keyd" in st["hotkey"]["reason"], st

    with subtest("EARWIG: Spaces after Super held through workspace switches start nothing"):
        events = watch_events()
        # Super held on the keyboard keyd holds for longer than a refused grab's 10 s retry,
        # keyd's events waking earwig throughout.
        key("meta_l", True)
        for digit in "21212121":
            time.sleep(1.5)
            key(digit, True)
            time.sleep(0.1)
            key(digit, False)
        time.sleep(0.5)
        key("meta_l", False)
        machine.send_chars("hello there big world\n")
        machine.send_chars("a b c d e f\n")
        time.sleep(3)
        states = [
            json.loads(line)
            for line in machine.succeed(f"cat {events}").splitlines()
        ]
        started = [e for e in states if e["event"] == "state" and e["state"]["state"] != "idle"]
        if started:
            print("\n".join(line for line in earwig_log().splitlines() if "hotkey" in line))
        assert not started, started
        assert status()["state"]["state"] == "idle"

    with subtest("EARWIG: Super+Space through keyd starts one"):
        machine.send_key("meta_l-spc")
        wait_status(lambda s: s["state"]["state"] == "recording", "recording", timeout=30)
        as_alice("earwig cancel")
        wait_status(lambda s: s["state"]["state"] == "idle", "idle", timeout=60)
  '';
}
