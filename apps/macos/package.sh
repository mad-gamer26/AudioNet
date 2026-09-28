#!/bin/sh
# Builds a release of the macOS app (Apple silicon and Intel) into
# dist/AudioNet-macOS-<version>.zip, with its .sha256.
#
#   apps/macos/package.sh [--default-server URL] [--update-url URL --public-key-file FILE]
#                         [--this-mac-only]
#
# --update-url is the release manifest URL the Windows app uses (for
# example https://audionet.example.com/downloads/latest.json); the Mac app
# reads latest-macos.json next to it. Sign the manifest on the machine that
# holds the release key (scripts/release_sign.py manifest --product
# audionet-macos-universal ...), never on a build or web server.
#
# Code signing: the Developer ID Application certificate in the login
# keychain, if there is one (or DEVELOPER_ID / TEAM_ID); otherwise ad-hoc.
# Signing needs the user's session: over SSH run this through
# in-session.sh. A signed build is also notarized and the ticket stapled
# when an App Store Connect API key is configured in ~/.audionet-notary
# (or NOTARY_PROFILE names a notarytool keychain profile), so it opens
# without Gatekeeper warnings. The version is
# the workspace version in Cargo.toml (VERSION overrides it, for tests).
# --this-mac-only builds for this Mac's processor only (faster; tests).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
server="" update_url="" public_key="" universal=--universal archs="arm64 x86_64"
while [ $# -gt 0 ]; do
    case "$1" in
        --default-server) server=$2; shift 2 ;;
        --update-url) update_url=$2; shift 2 ;;
        --public-key-file) public_key=$(tr -d ' \r\n' < "$2"); shift 2 ;;
        --this-mac-only) universal="" archs=$(uname -m); shift ;;
        *) echo "unknown option $1" >&2; exit 2 ;;
    esac
done
if [ -n "$update_url" ] && [ -z "$public_key" ]; then
    echo "--update-url needs --public-key-file" >&2; exit 2
fi
version=${VERSION:-$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' "$root/Cargo.toml")}
# A Developer ID Application certificate in the login keychain is used by
# itself (signing needs the user's session: over SSH, run this through
# in-session.sh).
if [ -z "${DEVELOPER_ID:-}" ]; then
    DEVELOPER_ID=$(security find-identity -v -p codesigning 2>/dev/null \
        | sed -n 's/.*"\(Developer ID Application: .*\)"/\1/p' | head -1)
fi
if [ -n "${DEVELOPER_ID:-}" ] && [ -z "${TEAM_ID:-}" ]; then
    TEAM_ID=$(echo "$DEVELOPER_ID" | sed -n 's/.*(\([A-Z0-9]*\))$/\1/p')
fi
# Notarization with an App Store Connect API key, from ~/.audionet-notary
# (not in the repository; readable only by this user):
#   NOTARY_KEY=/path/to/AuthKey_XXXXXXXXXX.p8
#   NOTARY_KEY_ID=XXXXXXXXXX
#   NOTARY_ISSUER=xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx
if [ -f "$HOME/.audionet-notary" ]; then . "$HOME/.audionet-notary"; fi
identity=${DEVELOPER_ID:--}
team=${TEAM_ID:-}

sh "$here/build-core.sh" $universal
cd "$here"
xcodegen generate -q
rm -rf "$here/build"
xcodebuild -project AudioNet.xcodeproj -scheme AudioNet -configuration Release \
    -derivedDataPath "$here/build" -destination 'generic/platform=macOS' \
    ARCHS="$archs" ONLY_ACTIVE_ARCH=NO \
    MARKETING_VERSION="$version" \
    AUDIONET_DEFAULT_SERVER="$server" \
    AUDIONET_UPDATE_URL="$update_url" \
    AUDIONET_UPDATE_PUBLIC_KEY="$public_key" \
    CODE_SIGN_IDENTITY="$identity" DEVELOPMENT_TEAM="$team" \
    OTHER_CODE_SIGN_FLAGS="--timestamp" \
    CODE_SIGN_INJECT_BASE_ENTITLEMENTS=NO \
    build | grep -E "error:|BUILD" || true
app="$here/build/Build/Products/Release/AudioNet.app"
[ -d "$app" ] || { echo "the build failed" >&2; exit 1; }
codesign --verify --deep --strict "$app"
lipo -archs "$app/Contents/MacOS/AudioNet"

mkdir -p "$root/dist"
zip="$root/dist/AudioNet-macOS-$version.zip"
rm -f "$zip"
ditto -c -k --keepParent "$app" "$zip"
notarize=""
if [ "$identity" != "-" ] && [ -n "${NOTARY_KEY:-}" ]; then
    notarize="--key $NOTARY_KEY --key-id $NOTARY_KEY_ID --issuer $NOTARY_ISSUER"
elif [ -n "${NOTARY_PROFILE:-}" ]; then
    notarize="--keychain-profile $NOTARY_PROFILE"
fi
if [ -n "$notarize" ]; then
    # shellcheck disable=SC2086
    xcrun notarytool submit "$zip" $notarize --wait
    xcrun stapler staple "$app"
    rm -f "$zip"
    ditto -c -k --keepParent "$app" "$zip"
fi
(cd "$root/dist" && shasum -a 256 "$(basename "$zip")" > "$(basename "$zip").sha256")
echo "Built $zip ($( [ "$identity" = "-" ] && echo "ad-hoc signed, not notarized" || echo "signed by $identity" ))"
