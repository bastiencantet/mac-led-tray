#!/bin/bash
# Release flow:
#   1. Build a signed LED-$VERSION.dmg
#   2. Sign the dmg with the EdDSA private key (stored in Keychain)
#   3. Create a GitHub release v$VERSION and upload the dmg
#   4. Prepend a new <item> entry to appcast.xml
#   5. Commit + push appcast.xml so Sparkle clients pick up the update
#
# Usage:  ./scripts/release.sh <version> <release-notes>
# Example: ./scripts/release.sh 0.1.2 "Fix slider padding"
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

VERSION="${1:-}"
NOTES="${2:-Minor update.}"

if [[ -z "$VERSION" ]]; then
    echo "usage: $0 <version> <release-notes>" >&2
    exit 1
fi

if [[ -n "$(git status --porcelain)" ]]; then
    echo "ERROR: git working tree is dirty — commit or stash first." >&2
    exit 1
fi

REPO_SLUG="bastiencantet/mac-led-tray"
TAG="v${VERSION}"
DMG="dist/LED-${VERSION}.dmg"
DOWNLOAD_URL="https://github.com/${REPO_SLUG}/releases/download/${TAG}/LED-${VERSION}.dmg"

echo "==> Building dmg for version ${VERSION}"
VERSION="${VERSION}" ./scripts/dmg.sh

if [[ ! -f "$DMG" ]]; then
    echo "ERROR: dmg not produced at ${DMG}" >&2
    exit 1
fi

echo "==> Signing ${DMG} with EdDSA key from Keychain"
SIGN_OUTPUT="$(./vendor/bin/sign_update "$DMG")"
# sign_update prints: sparkle:edSignature="..." length="..."
if [[ -z "$SIGN_OUTPUT" ]]; then
    echo "ERROR: sign_update produced no output" >&2
    exit 1
fi
echo "    ${SIGN_OUTPUT}"

echo "==> Creating GitHub release ${TAG} and uploading ${DMG}"
gh release create "$TAG" "$DMG" \
    --repo "$REPO_SLUG" \
    --title "LED ${VERSION}" \
    --notes "$NOTES"

echo "==> Prepending <item> to appcast.xml"
PUB_DATE="$(LC_ALL=C date -u +"%a, %d %b %Y %H:%M:%S +0000")"
# Escape the release notes for XML CDATA: disallow literal "]]>".
SAFE_NOTES="${NOTES//]]>/]]&gt;}"

ITEM="    <item>\\
      <title>Version ${VERSION}</title>\\
      <sparkle:version>${VERSION}</sparkle:version>\\
      <sparkle:shortVersionString>${VERSION}</sparkle:shortVersionString>\\
      <sparkle:minimumSystemVersion>11.0</sparkle:minimumSystemVersion>\\
      <pubDate>${PUB_DATE}</pubDate>\\
      <description><![CDATA[${SAFE_NOTES}]]></description>\\
      <enclosure url=\"${DOWNLOAD_URL}\" ${SIGN_OUTPUT} type=\"application/octet-stream\" />\\
    </item>"

# Insert before </channel>
# Use a temp file to keep it portable.
TMP="$(mktemp)"
awk -v item="$ITEM" '
/<\/channel>/ {
    gsub(/\\\n[ \t]*/, "\n", item)
    print item
}
{ print }
' appcast.xml > "$TMP"
mv "$TMP" appcast.xml

echo "==> Committing appcast update"
git add appcast.xml
git commit -m "chore: appcast entry for v${VERSION}"
git push origin main

echo "==> Done. Release ${TAG} published."
echo "    dmg: ${DOWNLOAD_URL}"
echo "    feed: https://raw.githubusercontent.com/${REPO_SLUG}/main/appcast.xml"
