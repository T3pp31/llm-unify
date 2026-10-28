#!/usr/bin/env python3
"""Import or verify the exact approved reference; never execute archive contents."""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import stat
import sys
import zipfile

ROOT = Path(__file__).resolve().parents[2]


def digest(data):
    return hashlib.sha256(data).hexdigest()


def baseline():
    return json.loads((ROOT / "config/parity/baseline.json").read_text())


def reference_config(config):
    """Python/UI reference inputs live outside the tracked Rust distribution."""
    return dict(config, **config.get("reference", {}))


def selected(name, config):
    return (
        name not in config["excluded_files"] and not any(name.startswith(p) for p in config["excluded_directories"])
        and (name in config["include_files"] or any(name.startswith(p) for p in config["include_directories"]))
    )


def patches(config):
    path = config.get("patches")
    return json.loads((ROOT / path).read_text())["files"] if path else {}


def patched(name, data, changes):
    patch = changes.get(name)
    if patch is None:
        return data
    if digest(data) != patch["upstream_sha256"]:
        raise ValueError("patch source hash mismatch: " + name)
    for replacement in patch["replacements"]:
        before, after = replacement["before"].encode(), replacement["after"].encode()
        if data.count(before) != 1:
            raise ValueError("patch context is not unique: " + name)
        data = data.replace(before, after)
    if digest(data) != patch["patched_sha256"]:
        raise ValueError("patch result hash mismatch: " + name)
    return data


def import_source(config):
    archive = ROOT / config["archive"]
    if digest(archive.read_bytes()) != config["archive_sha256"]:
        raise ValueError("reference archive SHA-256 mismatch")
    destination = ROOT / config["destination"]
    changes = patches(config)
    files = {}
    with zipfile.ZipFile(archive) as source:
        for info in source.infolist():
            if info.is_dir():
                continue
            if not info.filename.startswith(config["archive_root"]):
                raise ValueError("archive contains an unexpected root")
            name = info.filename[len(config["archive_root"]):]
            if not selected(name, config):
                continue
            path = PurePosixPath(name)
            if path.is_absolute() or ".." in path.parts or "\\" in name or stat.S_ISLNK(info.external_attr >> 16):
                raise ValueError("archive contains an unsafe path")
            data = source.read(info)
            original_digest = digest(data)
            output = patched(name, data, changes)
            target = destination / name
            if target.is_symlink() or any(parent.is_symlink() for parent in target.parents):
                raise ValueError("destination contains a symlink")
            if target.exists() and target.read_bytes() not in (data, output):
                raise ValueError("refusing to overwrite modified vendor file: " + name)
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(output)
            files[name] = original_digest
    if set(changes) - set(files):
        raise ValueError("patch refers to a file outside the selected source")
    manifest = {"schema_version": 1, "version": config["version"], "commit": config["commit"],
                "archive_sha256": config["archive_sha256"], "files": dict(sorted(files.items()))}
    (destination / "SOURCE.json").write_text(json.dumps(manifest, indent=2) + "\n")
    return len(files)


def verify(config):
    destination = ROOT / config["destination"]
    if digest((destination / "SOURCE.json").read_bytes()) != config["source_manifest_sha256"]:
        raise ValueError("source manifest SHA-256 mismatch")
    manifest = json.loads((destination / "SOURCE.json").read_text())
    changes = patches(config)
    if set(changes) - set(manifest["files"]):
        raise ValueError("patch refers to a file outside the source manifest")
    for key in ["version", "commit", "archive_sha256"]:
        if manifest[key] != config[key]:
            raise ValueError("reference identity mismatch: " + key)
    expected = set(manifest["files"])
    actual = set()
    for path in destination.rglob("*"):
        if not path.is_file():
            continue
        name = path.relative_to(destination).as_posix()
        if name == "SOURCE.json":
            continue
        if name in expected or not any(x in path.parts for x in ["target", "node_modules", ".next", "out", "__pycache__", ".venv"]):
            actual.add(name)
    if expected != actual:
        raise ValueError("vendor file set changed: " + str(sorted(expected ^ actual)[:10]))
    for name, sha256 in manifest["files"].items():
        path = destination / name
        patch = changes.get(name)
        if patch is not None:
            if patch["upstream_sha256"] != sha256 or not patch.get("reason"):
                raise ValueError("patch provenance mismatch: " + name)
            sha256 = patch["patched_sha256"]
        parents = [destination.joinpath(*PurePosixPath(name).parts[:i]) for i in range(1, len(PurePosixPath(name).parts))]
        if path.is_symlink() or any(parent.is_symlink() for parent in parents) or digest(path.read_bytes()) != sha256:
            raise ValueError("vendor hash mismatch: " + name)
    return len(expected)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["import", "verify"])
    parser.add_argument("--reference", action="store_true", help="use the ignored Python comparison source, never vendor it")
    args = parser.parse_args()
    try:
        config = reference_config(baseline()) if args.reference else baseline()
        count = import_source(config) if args.action == "import" else verify(config)
    except (OSError, ValueError, KeyError, zipfile.BadZipFile) as error:
        print(str(error), file=sys.stderr)
        return 1
    print(json.dumps({"action": args.action, "files": count, "verified": True}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
