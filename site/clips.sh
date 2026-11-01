#!/usr/bin/env bash
# Encodes the website's clips and pictures into site/media/, from what the ignored test
# `record_site_media` renders into target/site-media (see docs/testing.md):
#
#   cargo test -p dbrover-app record_site_media -- --ignored
#   site/clips.sh [SOURCE_DIR]     # default: target/site-media
#
# Each clip <name>-<theme>/ becomes <name>-<theme>.webm (VP9), .mp4 (H.264, for browsers
# without VP9) and -poster.png (its last frame, also shown instead of the clip to visitors who
# ask for reduced motion). Each still <scene>-<theme>.png becomes <scene>-<theme>.webp.
#
# The results are committed, so they replace the files in place under the same names: never add
# a second version, because every byte stays in the repository's history.
#
# Needs ffmpeg (and its ffprobe) with libvpx-vp9 and libx264, and cwebp (brew install ffmpeg
# webp).
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
src="${1:-${root}/target/site-media}"
out="${root}/site/media"

# Clips are shown at most about 1100 px wide; 1440 keeps text sharp on a high-density screen
# within the 2 MB budget per clip. Stills sit in a column about half as wide, and are kept at
# most 1400 px wide.
clip_width=1440
still_width=1400

# Quality, tuned by eye on the hero clip: VP9's CRF 34 and x264's CRF 23 keep code and grid text
# legible. The interface is mostly still between steps, so both compress very well.
vp9_crf=34
x264_crf=23
webp_quality=86

mkdir -p "${out}"

encode_clip() {
    local frames="$1" name="$2"
    local scale="scale=${clip_width}:-2:flags=lanczos"
    ffmpeg -hide_banner -loglevel error -y -f concat -i "${frames}/frames.ffconcat" \
        -vf "${scale},fps=30" -an -c:v libvpx-vp9 -b:v 0 -crf "${vp9_crf}" -row-mt 1 \
        -deadline good -cpu-used 1 -pix_fmt yuv420p "${out}/${name}.webm"
    ffmpeg -hide_banner -loglevel error -y -f concat -i "${frames}/frames.ffconcat" \
        -vf "${scale},fps=30" -an -c:v libx264 -crf "${x264_crf}" -preset veryslow \
        -tune stillimage -pix_fmt yuv420p -movflags +faststart "${out}/${name}.mp4"
    local last
    last="$(find "${frames}" -name 'frame-*.png' | sort | tail -n 1)"
    ffmpeg -hide_banner -loglevel error -y -i "${last}" -vf "${scale}" -pred mixed \
        "${out}/${name}-poster.png"
}

for frames in "${src}"/*/; do
    [ -d "${frames}" ] || continue
    name="$(basename "${frames}")"
    encode_clip "${frames%/}" "${name}"
    echo "encoded ${name}"
done

for still in "${src}"/*.png; do
    [ -e "${still}" ] || continue
    name="$(basename "${still}" .png)"
    # Stills are cropped to what they show, so some are narrower than still_width: shrink the
    # wide ones, never enlarge the narrow ones.
    width="$(ffprobe -v error -show_entries stream=width -of csv=p=0 "${still}")"
    cwebp -quiet -q "${webp_quality}" -resize "$((width < still_width ? width : still_width))" 0 \
        "${still}" -o "${out}/${name}.webp"
    echo "encoded ${name}"
done

ls -l "${out}"
