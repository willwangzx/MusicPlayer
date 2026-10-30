"""Preserve local source patches without reading credential or runtime folders."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import zipfile

root, target = map(Path, sys.argv[1:3])
files = subprocess.check_output(["git", "ls-files", "--modified", "--others", "--exclude-standard", "-z"], cwd=root).decode("utf-8").split("\0")
files = sorted(set(name for name in files if name))
if any(Path(name).is_absolute() or ".." in Path(name).parts or ".config" in Path(name).parts for name in files):
    raise SystemExit("Unsafe backup input")
target.mkdir(parents=True, exist_ok=False)
manifest = {"head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root).decode().strip(), "files": []}
with zipfile.ZipFile(target / "source-patches.zip", "x", zipfile.ZIP_DEFLATED) as archive:
    for name in files:
        data = (root / name).read_bytes()
        archive.writestr(name, data)
        manifest["files"].append({"path": name, "sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data)})
(target / "manifest.json").write_text(json.dumps(manifest, indent=2), encoding="utf-8")
with zipfile.ZipFile(target / "source-patches.zip") as archive:
    assert all(hashlib.sha256(archive.read(item["path"])).hexdigest() == item["sha256"] for item in manifest["files"])
print(json.dumps({"backed_up": len(files), "verified": True, "destination": str(target)}))
