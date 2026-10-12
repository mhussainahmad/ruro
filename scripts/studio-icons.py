"""Subsets the Lucide icon font to the glyphs the studio uses and writes icons.rs.

Download lucide-static from npm (https://registry.npmjs.org/lucide-static/latest),
unpack it, add the icon to ICONS below and run, from the repository root:

    python3 scripts/studio-icons.py <lucide-static>/package/font crates/ruro-studio

Needs fonttools (pyftsubset).

"""
import json
import subprocess
import sys

font_dir, crate = sys.argv[1], sys.argv[2]

# Constant name -> Lucide icon name.
ICONS = """
PLAY play
PAUSE pause
SQUARE square
ROTATE_CW rotate-cw
REFRESH_CW refresh-cw
SEARCH search
FILE file
FILE_PLUS file-plus
FILE_CODE file-code
FILE_DIFF file-diff
FOLDER_OPEN folder-open
SAVE save
PLUS plus
MINUS minus
X x
CHEVRON_RIGHT chevron-right
CHEVRON_DOWN chevron-down
CHEVRON_UP chevron-up
EYE eye
EYE_OFF eye-off
CIRCLE_ALERT circle-alert
TRIANGLE_ALERT triangle-alert
CIRCLE_CHECK circle-check
CIRCLE circle
CIRCLE_DOT circle-dot
CIRCLE_STOP circle-stop
DISC disc
CAMERA camera
IMAGE image
ACTIVITY activity
SLIDERS_HORIZONTAL sliders-horizontal
GAUGE gauge
BOT bot
WAVES waves
TERMINAL terminal
LIST list
LIST_TREE list-tree
PANEL_BOTTOM panel-bottom
PANEL_LEFT panel-left
PANEL_RIGHT panel-right
RADIO_TOWER radio-tower
PLUG plug
UNPLUG unplug
ZAP zap
TRASH_2 trash-2
COPY copy
COPY_PLUS copy-plus
CLIPBOARD_PASTE clipboard-paste
SCISSORS scissors
UNDO undo-2
REDO redo-2
BOX box
BOXES boxes
BLOCKS blocks
WORKFLOW workflow
CPU cpu
SETTINGS settings
COMMAND command
LAYERS layers
DROPLET droplet
CONTRAST contrast
CHART_LINE chart-line
CHART_SPLINE chart-spline
SIGMA sigma
BRANCH git-branch
COMPARE git-compare-arrows
MERGE merge
GROUP group
UNGROUP ungroup
NETWORK network
SERVER server
MONITOR monitor
SKULL skull
INFO info
HASH hash
TIMER timer
CLOCK clock
HISTORY history
LAYOUT_GRID layout-grid
LAYOUT_TEMPLATE layout-template
ELLIPSIS ellipsis
KEYBOARD keyboard
BOOK_OPEN book-open
EXTERNAL_LINK external-link
OPEN_IN square-arrow-out-up-right
CROSSHAIR crosshair
SCAN_EYE scan-eye
CABLE cable
ARROW_RIGHT_LEFT arrow-right-left
MOVE move
MOVE_3D move-3d
ROTATE_3D rotate-3d
AXIS_3D axis-3d
ORBIT orbit
MAXIMIZE_2 maximize-2
MINIMIZE_2 minimize-2
LOCATE_FIXED locate-fixed
FOCUS focus
MEMORY_STICK memory-stick
SCALING scaling
SUN sun
MOON moon
SUN_MOON sun-moon
SHIELD_CHECK shield-check
ALIGN_LEFT align-start-vertical
ALIGN_CENTER_X align-center-vertical
ALIGN_TOP align-start-horizontal
ALIGN_CENTER_Y align-center-horizontal
DISTRIBUTE_X align-horizontal-distribute-center
DISTRIBUTE_Y align-vertical-distribute-center
SQUARE_DASHED square-dashed
FAST_FORWARD fast-forward
SKIP_BACK skip-back
FILM film
"""

info = json.load(open(f"{font_dir}/info.json"))
pairs = [line.split() for line in ICONS.strip().splitlines()]
missing = [n for _, n in pairs if n not in info]
if missing:
    sys.exit(f"missing icons: {missing}")

codes = []
lines = [
    "//! Lucide icons (https://lucide.dev, ISC licence) as glyphs of a subset icon font,",
    "//! `assets/icons/lucide.ttf`. This file is written by `scripts/studio-icons.py`: add an",
    "//! icon there and rerun it rather than editing this file.",
    "#![allow(dead_code)]",
    "",
]
for const, name in pairs:
    code = info[name]["encodedCode"].lstrip("\\").upper()
    codes.append(f"U+{code}")
    lines.append(f'pub const {const}: &str = "\\u{{{code}}}"; // {name}')
open(f"{crate}/src/icons.rs", "w").write("\n".join(lines) + "\n")

subprocess.run(
    [
        "pyftsubset",
        f"{font_dir}/lucide.ttf",
        "--unicodes=" + ",".join(codes),
        f"--output-file={crate}/assets/icons/lucide.ttf",
        "--no-hinting",
        "--desubroutinize",
    ],
    check=True,
)
print(f"{len(pairs)} icons")
