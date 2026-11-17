# Everything Windows CI downloads to build, package and test earwig: an exact version from its
# vendor's official URL and the SHA-256 that tools.ps1 checks before anything runs it. A bump
# changes a tool's lines here and nothing else. (sign.ps1 pins the Artifact Signing client it
# fetches on a tag; models.toml the speech model.)
@{
    # rustup, the runner image's, installs this toolchain and checks every component against the
    # SHA-256 its channel manifest lists: this file, checked against Sha256.
    Rust = @{
        Version = '1.100.0'
        Url = 'https://static.rust-lang.org/dist/channel-rust-1.100.0.toml'
        Sha256 = '3051e859570f64a8c5acb9d09e87425db52c7d97287c67e8f6d57d7d34ef9258'
    }
    # glslc, the headers and vulkan-1.lib for transcribe.cpp's Vulkan backend. The installer also
    # runs the SDK's Vulkan runtime installer, which puts the loader in System32 for the tests;
    # Loader is that vulkan-1.dll's SHA-256, the loader the packages ship.
    VulkanSdk = @{
        Version = '1.4.363.1'
        Url = 'https://sdk.lunarg.com/sdk/download/1.4.363.1/windows/vulkansdk-windows-X64-1.4.363.1.exe'
        Sha256 = 'c02932bd9a968b22d404536108adaacac908c081cd6b0113f5bfefc523dd0a6a'
        Loader = 'f4740173f8706ee968cbf5611246877b55ce9ec1027bccd6aa3a632154378a5b'
    }
    # transcribe-cpp-sys's build script runs cmake to build transcribe.cpp; the runner image's
    # comes from Chocolatey. cmake.org's download page links Kitware's GitHub releases.
    CMake = @{
        Version = '4.4.5'
        Url = 'https://github.com/Kitware/CMake/releases/download/v4.4.5/cmake-4.4.5-windows-x86_64.zip'
        Sha256 = '326c826486d46a23cf9d5a1c9ee3a567d427bdd02c4a34071937a2a675cff004'
    }
    # jrsoftware.org's download page links the installer from its GitHub releases.
    InnoSetup = @{
        Version = '6.7.4'
        Url = 'https://github.com/jrsoftware/issrc/releases/download/is-6_7_4/innosetup-6.7.4.exe'
        Sha256 = 'a549929319f9120716677c685535ca4671e4b937b5fdc87e2585f32d8eb30e8c'
    }
    # Intel SDE, for the smoke test's emulated CPUs.
    Sde = @{
        Version = '10.13.1'
        Url = 'https://downloadmirror.intel.com/924984/sde-external-10.13.1-2026-07-28-win.tar.xz'
        Sha256 = '74e626ede09b0baa5011fc9e51b58627ea92c3fc0bae5fd7db34b490f335f651'
    }
}
