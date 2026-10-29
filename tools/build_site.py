"""Build the GitHub Pages site using only the Python standard library.

Run from any directory: python3 tools/build_site.py
Site URLs and metadata live in site/site.json. Output: _site/.
"""

import html
import json
from pathlib import Path
import shutil

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "site"
OUTPUT = ROOT / "_site"


def main():
    config = json.loads((SOURCE / "site.json").read_text())
    page = (SOURCE / "index.html").read_text()
    for key in ("repository_url", "site_url", "description"):
        page = page.replace("{{" + key + "}}", html.escape(config[key], quote=True))
    if "{{" in page:
        raise ValueError("Unresolved site template value")
    OUTPUT.mkdir(exist_ok=True)
    (OUTPUT / "index.html").write_text(page)
    for name in ("styles.css", "script.js"):
        shutil.copyfile(SOURCE / name, OUTPUT / name)
    (OUTPUT / ".nojekyll").touch()
    print(f"Built site: {OUTPUT}")


if __name__ == "__main__":
    main()
