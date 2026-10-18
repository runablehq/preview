#!/usr/bin/env python3
"""A static stand-in for the npm registry, for the update scenarios in smoke.sh.

    registry.py <root> publish <platform> <version>    adds a fixture version and makes it latest
    registry.py <root> latest <platform> <version>     moves dist-tag latest
    registry.py <root> deprecate <platform> <version>  marks a version deprecated
    registry.py <root> corrupt <platform> <version>    breaks a version's integrity

Serve <root> with `python3 -m http.server`; preview reads <url>/@runablehq%2fpreview-<platform>.
A fixture "binary" is a script that answers --version like preview and prints
`fixture <version>` for anything else, so a test can see which version ran.
"""

import base64
import hashlib
import io
import json
import sys
import tarfile
from pathlib import Path


def main():
    root, action, platform, version = Path(sys.argv[1]), *sys.argv[2:5]
    name = f"@runablehq/preview-{platform}"
    meta_path = root / "@runablehq" / f"preview-{platform}"
    meta_path.parent.mkdir(parents=True, exist_ok=True)
    meta = json.loads(meta_path.read_text()) if meta_path.exists() else {
        "name": name, "dist-tags": {}, "versions": {}}

    if action == "publish":
        script = (f'#!/bin/sh\nif [ "$1" = --version ]; then echo "preview {version}"; exit 0; fi\n'
                  f'echo "fixture {version}"\n').encode()
        tgz = tarball({"package/package.json": json.dumps({"name": name, "version": version}).encode(),
                       "package/bin/preview": script})
        file = root / "tarballs" / f"preview-{platform}-{version}.tgz"
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_bytes(tgz)
        integrity = "sha512-" + base64.b64encode(hashlib.sha512(tgz).digest()).decode()
        url = f"{sys.argv[5].rstrip('/')}/tarballs/{file.name}" if len(sys.argv) > 5 else ""
        meta["versions"][version] = {"name": name, "version": version,
                                     "dist": {"tarball": url, "integrity": integrity}}
        meta["dist-tags"]["latest"] = version
    elif action == "latest":
        meta["dist-tags"]["latest"] = version
    elif action == "deprecate":
        meta["versions"][version]["deprecated"] = "broken, use the previous version"
    elif action == "corrupt":
        meta["versions"][version]["dist"]["integrity"] = "sha512-" + base64.b64encode(b"x" * 64).decode()
    else:
        sys.exit(f"unknown action {action}")
    meta_path.write_text(json.dumps(meta))


def tarball(files):
    out = io.BytesIO()
    with tarfile.open(fileobj=out, mode="w:gz") as tar:
        for path, body in files.items():
            info = tarfile.TarInfo(path)
            info.size = len(body)
            info.mode = 0o755
            tar.addfile(info, io.BytesIO(body))
    return out.getvalue()


if __name__ == "__main__":
    main()
