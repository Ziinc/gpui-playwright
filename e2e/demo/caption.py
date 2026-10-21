"""Adds a caption bar under each filmed frame showing the Playwright call
running at that moment. Usage: caption.py WORK_DIR FPS"""

import json
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

work, fps = Path(sys.argv[1]), int(sys.argv[2])
start = float((work / "start").read_text())
steps = [json.loads(line) for line in (work / "steps.jsonl").read_text().splitlines()]
font = ImageFont.truetype("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf", 13)
bar = 40

out = work / "captioned"
out.mkdir()
for frame in sorted((work / "frames").glob("*.png")):
    at = start + (int(frame.stem) - 1) / fps
    code = next((s["code"] for s in reversed(steps) if s["at"] <= at), None)
    image = Image.open(frame).convert("RGB")
    captioned = Image.new("RGB", (image.width, image.height + bar), "#1e1e2e")
    captioned.paste(image, (0, 0))
    if code:
        draw = ImageDraw.Draw(captioned)
        draw.text((12, image.height + bar // 2), code, font=font, fill="#cdd6f4", anchor="lm")
    captioned.save(out / frame.name)
