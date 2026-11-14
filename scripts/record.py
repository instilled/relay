#!/usr/bin/env python3
"""Records a tmux pane as frames, then renders them to a GIF and an MP4.

    python3 scripts/record.py capture --sock S --target T --out DIR --stop FILE
        capture-pane -e every 0.1s into DIR until FILE exists; each change
        kept with the time it appeared (DIR/frames.txt)
    python3 scripts/record.py render --dir DIR --cols C --rows R --title T --gif G --mp4 M
        each frame through scripts/ansi2svg.py and rsvg-convert to PNG, then
        ffmpeg: an MP4 and a GIF (palette per clip), each frame held as long
        as it was on screen

Used by scripts/demo-video. Needs tmux, rsvg-convert and ffmpeg.
"""

import argparse
import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))


def capture(a):
    os.makedirs(a.out, exist_ok=True)
    last = None
    n = 0
    start = time.monotonic()
    with open(os.path.join(a.out, "frames.txt"), "w") as idx:
        while not os.path.exists(a.stop):
            r = subprocess.run(
                [a.tmux, "-L", a.sock, "capture-pane", "-e", "-p", "-t", a.target],
                capture_output=True,
            )
            if r.returncode == 0 and r.stdout != last:
                last = r.stdout
                name = f"{n:05d}.ansi"
                with open(os.path.join(a.out, name), "wb") as f:
                    f.write(r.stdout)
                idx.write(f"{time.monotonic() - start:.3f} {name}\n")
                idx.flush()
                n += 1
            time.sleep(0.1)
        idx.write(f"{time.monotonic() - start:.3f} END\n")


def render(a):
    rows = [l.split() for l in open(os.path.join(a.dir, "frames.txt"))]
    concat = []
    for (t, name), (t2, _) in zip(rows, rows[1:]):
        base = os.path.join(a.dir, name[:-5])
        with open(base + ".ansi", "rb") as src, open(base + ".svg", "wb") as svg:
            subprocess.run(
                [sys.executable, os.path.join(HERE, "ansi2svg.py"), "--cols", str(a.cols),
                 "--rows", str(a.rows), "--title", a.title],
                stdin=src, stdout=svg, check=True,
            )
        subprocess.run([a.rsvg, "-w", str(a.width), base + ".svg", "-o", base + ".png"], check=True)
        concat.append(f"file '{base}.png'\nduration {max(float(t2) - float(t), 0.05):.3f}\n")
    # The concat demuxer drops the last duration unless the file repeats.
    concat.append(f"file '{base}.png'\n")
    lst = os.path.join(a.dir, "concat.txt")
    open(lst, "w").write("".join(concat))
    ff = [a.ffmpeg, "-y", "-loglevel", "error"]
    # The MP4 first, at a steady rate; the GIF is made from it in two passes
    # (one palette of 64 colours for the clip, then only what changed per
    # frame): a terminal needs few colours, and a GIF straight from the
    # concat list drops most of its frames.
    subprocess.run(ff + ["-f", "concat", "-safe", "0", "-i", lst,
                         "-vf", "fps=30,format=yuv420p,scale=trunc(iw/2)*2:trunc(ih/2)*2",
                         "-c:v", "libx264", "-crf", "20", "-movflags", "+faststart", a.mp4], check=True)
    if a.gif:
        scale = f"fps=10,scale={a.gif_width}:-1:flags=lanczos"
        pal = os.path.join(a.dir, "palette.png")
        subprocess.run(ff + ["-i", a.mp4, "-vf", f"{scale},palettegen=max_colors=64:stats_mode=full", pal],
                       check=True)
        subprocess.run(ff + ["-i", a.mp4, "-i", pal, "-lavfi",
                             f"{scale}[x];[x][1:v]paletteuse=dither=none:diff_mode=rectangle",
                             "-loop", "0", a.gif], check=True)


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("capture")
    c.add_argument("--sock", required=True)
    c.add_argument("--target", required=True)
    c.add_argument("--out", required=True)
    c.add_argument("--stop", required=True)
    c.add_argument("--tmux", default="tmux")
    r = sub.add_parser("render")
    r.add_argument("--dir", required=True)
    r.add_argument("--cols", type=int, default=110)
    r.add_argument("--rows", type=int, default=30)
    r.add_argument("--title", default="")
    r.add_argument("--width", type=int, default=1400)
    r.add_argument("--gif-width", type=int, default=900)
    r.add_argument("--gif")
    r.add_argument("--mp4", required=True)
    r.add_argument("--rsvg", default="rsvg-convert")
    r.add_argument("--ffmpeg", default="ffmpeg")
    a = ap.parse_args()
    {"capture": capture, "render": render}[a.cmd](a)


if __name__ == "__main__":
    main()
