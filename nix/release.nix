# A pushed v* tag's GitHub release, made with the release token nixbot holds, and the Mac
# download on it: earwig.app, built on the Mac, signed with the Developer ID in a DMG that Apple
# notarizes (packaging/macos/dmg.sh).
{
  lib,
  mkEffect,
  primaryRepo,
  app,
  tools,
}:
let
  inherit (app) version;
in
mkEffect {
  name = "release";
  inputs = tools;
  secretsMap = {
    # `token`: a GitHub token that may publish this repository's releases.
    github = "github_release";
    # `key` and `certificate`: the Developer ID Application identity (PEM); `notary_key`: an
    # App Store Connect API key (rcodesign encode-app-store-connect-api-key).
    apple = "apple_developer_id";
  };
  effectScript = ''
    repo=${lib.escapeShellArg "${primaryRepo.owner}/${primaryRepo.name}"}
    tag=${lib.escapeShellArg primaryRepo.tag}
    # Every release asset is named for the workspace version.
    if [[ $tag != "v${version}" ]]; then
      echo "$tag is not v${version}, the version in Cargo.toml" >&2
      exit 1
    fi
    GH_TOKEN=$(readSecretString github .token)
    export GH_TOKEN
    bash ${../packaging/github-release.sh} "$repo" "$tag"

    # /tmp is a tmpfs in nixbot's sandbox: the keys never reach its disk.
    keys=$(mktemp -d -p /tmp)
    for field in key certificate notary_key; do
      readSecretString apple ".$field" >"$keys/$field"
    done
    dmg=earwig-${version}-macos-arm64.dmg
    bash ${../packaging/macos}/dmg.sh --key "$keys/key" --certificate "$keys/certificate" \
      --notary-key "$keys/notary_key" ${app}/Applications/earwig.app "$dmg"
    gh release upload "$tag" "$dmg" --repo "$repo" --clobber
  '';
}
