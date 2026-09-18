"""Pixel verification against real Kitty. Run under xvfb-run; no host packages are changed."""
import json
import os
import sys
from pathlib import Path
import subprocess
import tempfile
import time
from PIL import Image

SYNC = "--sync" in sys.argv
RASTER = "--raster" in sys.argv
ROOT = Path(__file__).resolve().parent.parent
with tempfile.TemporaryDirectory(prefix='paru-tui-pixels-') as temp:
    root = Path(temp)
    frames = root / 'frames.json'
    subprocess.run([
        'cargo', 'test', '--offline', '--features', 'mock,tui', '--lib',
        'repeated_input_keeps_text_and_stripe_in_the_same_pixel_frame' if SYNC else 'list_text_scrolls_in_pixels_and_reuses_cached_rows' if RASTER else 'rectangle_moves_in_pixels_without_reupload_or_color_changes',
    ], cwd=ROOT, env=dict(os.environ, **{("PARU_TUI_TEST_SYNC_FRAMES" if SYNC else "PARU_TUI_TEST_RASTER_FRAMES" if RASTER else "PARU_TUI_TEST_SELECTION_FRAMES"): str(frames)}), check=True)
    (root / 'raster').write_text(str(RASTER or SYNC))
    (root / 'sync').write_text(str(SYNC))
    (root / 'repo').write_text(str(ROOT))
    worker = root / 'worker.py'
    worker.write_text('''import json, sys, time, os, subprocess, fcntl, termios, struct
from pathlib import Path
root = Path(sys.argv[1])
if (root / 'sync').read_text() == 'True':
    rows, cols, width, height = struct.unpack('HHHH', fcntl.ioctl(0, termios.TIOCGWINSZ, bytes(8)))
    cell = f'{width // cols},{height // rows}'
    (root / 'cell').write_text(cell)
    subprocess.run(['cargo', 'test', '--offline', '--features', 'mock,tui', '--lib', 'repeated_input_keeps_text_and_stripe_in_the_same_pixel_frame'], cwd=(root / 'repo').read_text(), env=dict(os.environ, PARU_TUI_TEST_CELL=cell, PARU_TUI_TEST_SYNC_FRAMES=str(root / 'frames.json')), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
print("\\x1b[2J\\x1b[?25l\\x1b[6;5HPackage alpha\\x1b[7;5HPackage beta", end="", flush=True)
if (root / "raster").read_text() == "True": print("\\x1b[2J", end="", flush=True)
for index, frame in enumerate(json.loads((root / 'frames.json').read_text())):
    print(frame, end="", flush=True)
    (root / f'ready-{index}').touch()
    while not (root / f'next-{index}').exists(): time.sleep(.02)
''')
    kitty = subprocess.Popen([
        'kitty', '--config', 'NONE', '-o', 'linux_display_server=x11',
        '-o', 'font_size=14', '-o', 'background=#1e1e1e',
        'python3', str(worker), str(root),
    ], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        boxes = []
        for index in range(6):
            deadline = time.monotonic() + 15
            while not (root / f'ready-{index}').exists():
                assert kitty.poll() is None, kitty.stderr.read().decode()
                assert time.monotonic() < deadline, f'Frame {index} timeout'
                time.sleep(.02)
            capture = root / f'frame-{index}.png'
            # A worker can write before the first window has been mapped/composited.
            deadline = time.monotonic() + 5
            while True:
                time.sleep(.15)
                subprocess.run(['import', '-window', 'root', str(capture)], check=True)
                picture = Image.open(capture).convert('RGB')
                mask = Image.new('L', picture.size)
                mask.putdata([255 if pixel == ((212, 212, 212) if RASTER else (52, 79, 74)) else 0 for pixel in picture.get_flattened_data()])
                box = mask.getbbox()
                if box is not None or index > 0 or time.monotonic() >= deadline:
                    break
            if index < 5:
                if box is None:
                    picture.save('/tmp/paru-tui-pixel-debug.png')
                    print(sorted(picture.getcolors(1280 * 800), reverse=True)[:12])
                assert box is not None, f'No selection rectangle in frame {index}'
                if not RASTER:
                    expected = tuple(map(int, (root / 'cell').read_text().split(','))) if SYNC else (10, 20)
                    assert (box[2] - box[0], box[3] - box[1]) == (40 * expected[0], expected[1]), box
                if SYNC:
                    accent_mask = Image.new('L', picture.size)
                    accent_mask.putdata([255 if pixel == (77, 201, 176) else 0 for pixel in picture.get_flattened_data()])
                    accent_box = accent_mask.getbbox()
                    assert accent_box is not None
                    assert box[1] <= accent_box[1] < accent_box[3] <= box[3], (box, accent_box)
                    assert accent_box[2] > box[0] + 30, 'Only the arrow is accented; package text must also follow the stripe'
                boxes.append(box)
            else:
                assert box is None, 'Selection remains after dismissal'
            (root / f'next-{index}').touch()
        if SYNC:
            assert len({box[1] for box in boxes}) > 2, boxes
        else:
            for index in range(1, 4):
                assert boxes[index][1] - boxes[index - 1][1] == (-5 if RASTER else 5), boxes
            assert boxes[4][1] > boxes[3][1], boxes
        kitty.wait(timeout=5)
        print('PASS: real Kitty keeps accented text and arrow inside the stripe during repeated input and scrolling' if SYNC else 'PASS: real Kitty renders text moving by 5 pixels, then clears it' if RASTER else 'PASS: real Kitty renders a constant 400×20 rectangle moving by 5 pixels, then clears it')
    finally:
        if kitty.poll() is None:
            kitty.terminate()
            kitty.wait(timeout=5)
