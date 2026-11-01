#!/usr/bin/env bash
# Assembles the website into one folder: the pages from site/ with their pictures and
# clips (site/media, made by site/clips.sh), the brand assets, fonts and database logos they use
# (copied, so assets/ stays their only home), and the user manual built by mdBook under
# manual/. The Pages workflow publishes what this produces.
#
#   site/build.sh [OUTPUT_DIR]     # default: target/site
#
# Needs mdbook 0.5.4 and mdbook-linkcheck2 0.13.0 (see docs/testing.md). To preview:
#   site/build.sh && python3 -m http.server -d target/site
#
# DBROVER_ANALYTICS_TOKEN, when set, adds Cloudflare Web Analytics' beacon to every page. Only the
# Pages workflow's deployments set it, so pull requests and local previews aren't counted.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:-${root}/target/site}"

rm -rf "${out}"
mkdir -p "${out}/assets/brand" "${out}/assets/fonts" "${out}/assets/databases"

# The page names the version and links to its files, written in at build time rather than asked
# of GitHub when the page loads. The Pages workflow passes the latest published release as
# DBROVER_SITE_VERSION, because a version bump reaches main about half an hour before its release's
# files exist. A local build falls back to the workspace's version.
version="${DBROVER_SITE_VERSION:-$(sed -n 's/^version = "\(.*\)"$/\1/p' "${root}/Cargo.toml" | head -n 1)}"
if [ -z "${version}" ]; then
    echo "no DBROVER_SITE_VERSION and no workspace version in Cargo.toml" >&2
    exit 1
fi
# The release's files start with the app's name, which was dbdesk up to 0.12.0. Until 0.13.0 is
# published, the newest release is one of those.
case "${version}" in
    0.[0-9].* | 0.1[0-2].*) files=dbdesk ;;
    *) files=dbrover ;;
esac
for page in index.html; do
    sed -e "s/{{version}}/${version}/g" -e "s/{{files}}/${files}/g" \
        "${root}/site/${page}" > "${out}/${page}"
done
cp "${root}/site/style.css" "${out}/"
cp -R "${root}/site/media" "${out}/media"
cp "${root}/assets/brand/logo.svg" "${root}/assets/brand/social-preview.png" \
    "${out}/assets/brand/"
cp "${root}/assets/fonts/inter/Inter-Regular.ttf" "${root}/assets/fonts/inter/Inter-SemiBold.ttf" \
    "${root}/assets/fonts/jetbrains-mono/JetBrainsMono-Regular.ttf" "${out}/assets/fonts/"
cp "${root}/assets/fonts/inter/LICENSE.txt" "${out}/assets/fonts/Inter-LICENSE.txt"
cp "${root}/assets/fonts/jetbrains-mono/OFL.txt" "${out}/assets/fonts/JetBrainsMono-OFL.txt"
cp "${root}/assets/icons/databases/"*.svg "${out}/assets/databases/"

# The linkcheck2 backend fails the build on a broken link inside the manual.
mdbook build "${root}/book"
cp -R "${root}/book/book/html" "${out}/manual"

# Web Analytics rather than its automatic setup, because dbrover.org's DNS record is DNS-only:
# GitHub serves the pages, so Cloudflare never sees a request to add its beacon to. The beacon
# sets no cookies and keeps no visitor identifier, so the site needs no consent banner.
if [ -n "${DBROVER_ANALYTICS_TOKEN:-}" ]; then
    beacon="<script type=\"module\" src=\"https://static.cloudflareinsights.com/beacon.min.js\" data-cf-beacon='{\"token\": \"${DBROVER_ANALYTICS_TOKEN}\"}'></script>"
    find "${out}" -name '*.html' -print0 |
        BEACON="${beacon}" xargs -0 perl -pi -e 's{</head>}{$ENV{BEACON}</head>}'
fi

echo "site written to ${out}"
