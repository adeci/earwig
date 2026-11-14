# Building on Linux

Without Nix, earwig builds from source. First the packages it needs:

**Ubuntu** 24.04 or newer, **Debian** 13

```sh
sudo apt install curl build-essential cmake pkg-config glslc libvulkan-dev \
  spirv-headers libasound2-dev libfontconfig-dev libxkbcommon-x11-dev
```

**Fedora**

```sh
sudo dnf install gcc-c++ cmake pkg-config glslc vulkan-headers vulkan-loader-devel \
  spirv-headers-devel alsa-lib-devel fontconfig-devel libxkbcommon-x11-devel
```

**Arch**

```sh
sudo pacman -S --needed base-devel cmake shaderc vulkan-headers vulkan-icd-loader \
  spirv-headers alsa-lib fontconfig libxkbcommon-x11
```

Then Rust 1.95 or newer from [rustup](https://rustup.rs), and earwig into
`~/.cargo/bin`:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
. "$HOME/.cargo/env"
cargo install --locked --git https://github.com/adeci/earwig earwig
```

## Start it with your session

As a systemd user service, on GNOME, KDE and other desktops that start
`graphical-session.target`:

```sh
mkdir -p ~/.config/systemd/user
cat > ~/.config/systemd/user/earwig.service <<'EOF'
[Unit]
Description=earwig dictation daemon
PartOf=graphical-session.target
After=graphical-session.target xdg-desktop-portal.service
Wants=xdg-desktop-portal.service

[Service]
ExecStart=%h/.cargo/bin/earwig daemon
Restart=on-failure

[Install]
WantedBy=graphical-session.target
EOF
systemctl --user enable --now earwig
```

Where nothing starts that target, start `~/.cargo/bin/earwig daemon` from the
compositor's config instead.

## First start

Open `earwig settings`, download a model under Models, and press **Allow**
under Keyboard access, which installs a udev rule (it asks for your
password). That rule by hand:

```sh
sudo tee /etc/udev/rules.d/70-earwig.rules <<'EOF'
SUBSYSTEM=="input", KERNEL=="event*", ENV{ID_INPUT_KEYBOARD}=="1", TAG+="uaccess"
KERNEL=="uinput", SUBSYSTEM=="misc", TAG+="uaccess", OPTIONS+="static_node=uinput"
EOF
sudo udevadm control --reload
sudo udevadm trigger --action=change --subsystem-match=input --subsystem-match=misc
```

[desktops.md](desktops.md) has what keyboard access allows, and what works
without it.

## With Nix instead

`nix profile add github:adeci/earwig` builds the same earwig without the
packages above, but outside NixOS its windows only open through
[nixGL](https://github.com/nix-community/nixGL):
`nix run --impure github:nix-community/nixGL -- earwig settings`.
