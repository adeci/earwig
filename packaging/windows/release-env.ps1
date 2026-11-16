# How a Windows release package is built, for package.ps1 and the update test's older build:
#   . packaging/windows/release-env.ps1
# then `cargo build --release`.

# transcribe.cpp tunes its CPU code to the machine that builds it (GGML_NATIVE),
# so a package built on a runner with AVX-512 crashed on every CPU without it.
# Pin ggml's x86 tiers up to AVX2 instead (Haswell, Zen and newer): the
# SSE2 floor decodes slower than real time.
$env:TRANSCRIBE_CMAKE_ARGS = "$env:TRANSCRIBE_CMAKE_ARGS -DGGML_SSE42=ON -DGGML_AVX=ON -DGGML_AVX2=ON -DGGML_BMI2=ON -DGGML_FMA=ON -DGGML_F16C=ON -DTRANSCRIBE_X86_CONSERVATIVE=ON".Trim()
# The C runtime linked in (transcribe.cpp's C++ too), so earwig needs no Visual
# C++ Redistributable, which Windows doesn't come with.
$env:RUSTFLAGS = "$env:RUSTFLAGS -C target-feature=+crt-static".Trim()
$env:TRANSCRIBE_CMAKE_ARGS += ' -DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded'

# A release package looks for updates (crates/earwig-core/src/update); other
# builds never do. Only test builds ask a stand-in for GitHub instead.
if ($env:EARWIG_UPDATE_TEST_FEED) {
    throw 'EARWIG_UPDATE_TEST_FEED is set: a release package must not ask a test feed'
}
$env:EARWIG_RELEASE_PACKAGE = '1'
