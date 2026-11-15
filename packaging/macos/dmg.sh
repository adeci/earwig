#!/usr/bin/env bash
# earwig.app as a download for any Mac, Nix or not: the app signed with a Developer ID (hardened
# runtime, Apple's timestamp, earwig.entitlements) in a DMG next to a link to Applications, the
# DMG signed, notarized by Apple and stapled. Runs on Linux: rcodesign signs and notarizes,
# mkfs.hfsplus (hfsprogs) and libdmg-hfsplus make the DMG, as Firefox's are made. nixbot's release
# effect (nix/release.nix) runs it for a tag; `nix develop .#release` has its tools.
#
# Usage: dmg.sh --key KEY.pem --certificate CERT.pem --notary-key API-KEY.json APP OUT.dmg
#   --key, --certificate  the Developer ID Application private key and certificate
#   --notary-key          an App Store Connect API key (rcodesign encode-app-store-connect-api-key)
#   --no-notarize         instead of --notary-key, to try a self-signed identity
#                         (rcodesign generate-self-signed-certificate); Gatekeeper refuses the DMG
set -euo pipefail

usage() {
  echo "usage: $0 --key KEY.pem --certificate CERT.pem (--notary-key API-KEY.json | --no-notarize) APP OUT.dmg" >&2
  exit 2
}

key='' certificate='' notary_key='' no_notarize=''
while [[ $# -gt 0 ]]; do
  case $1 in
  --key | --certificate | --notary-key)
    [[ $# -ge 2 ]] || usage
    case $1 in
    --key) key=$2 ;;
    --certificate) certificate=$2 ;;
    --notary-key) notary_key=$2 ;;
    esac
    shift 2
    ;;
  --no-notarize)
    no_notarize=1
    shift
    ;;
  -*) usage ;;
  *) break ;;
  esac
done
# Notarization is never skipped by accident: --no-notarize instead of --notary-key, not with it.
if [[ $# -ne 2 || -z $key || -z $certificate ]] ||
  [[ -n $notary_key && -n $no_notarize ]] || [[ -z $notary_key && -z $no_notarize ]]; then
  usage
fi
app=$1
dmg=$2

entitlements=$(dirname "${BASH_SOURCE[0]}")/earwig.entitlements
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
identity=(--pem-file "$key" --pem-file "$certificate")

stage=$work/stage
mkdir "$stage"
cp -R "$app" "$stage/earwig.app"
chmod -R u+w "$stage/earwig.app"
rcodesign sign "${identity[@]}" --code-signature-flags runtime \
  --entitlements-xml-file "$entitlements" "$stage/earwig.app"
ln -s /Applications "$stage/Applications"

# An HFS+ volume with room to spare (the empty space compresses away), in a zlib DMG.
hfs=$work/earwig.hfs
truncate -s "$(($(du -sm "$stage" | cut -f1) * 5 / 4 + 16))M" "$hfs"
mkfs.hfsplus -v earwig "$hfs"
hfsplus --symlinks=clone_link --special-modes=no "$hfs" addall "$stage" >/dev/null
# Named as it ships: rcodesign takes the DMG's identifier from the file name.
out=$work/$(basename "$dmg")
dmg --compression zlib build "$hfs" "$out" >/dev/null
rcodesign sign "${identity[@]}" "$out"

if [[ -n $notary_key ]]; then
  # Apple's verdict usually takes minutes. On a rejection rcodesign prints Apple's log and fails;
  # two hours of waiting stay under nixbot's three-hour limit for an effect.
  rcodesign notary-submit --api-key-file "$notary_key" --staple --max-wait-seconds 7200 "$out"
else
  echo "$dmg is not notarized: Gatekeeper refuses to open it" >&2
fi
mv "$out" "$dmg"
