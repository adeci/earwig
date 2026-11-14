# A pushed v* tag's GitHub release, made with nixbot's token for this repository. The Windows
# CI job runs the same script before it attaches its packages; whichever is first creates it.
{
  lib,
  gh,
  mkEffect,
  primaryRepo,
}:
mkEffect {
  name = "release";
  inputs = [ gh ];
  secretsMap.github.type = "GitToken";
  effectScript = ''
    GH_TOKEN=$(readSecretString github .token)
    export GH_TOKEN
    bash ${../packaging/github-release.sh} ${
      lib.escapeShellArgs [
        "${primaryRepo.owner}/${primaryRepo.name}"
        primaryRepo.tag
      ]
    }
  '';
}
