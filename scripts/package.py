#!/usr/bin/env python3
"""把 `cargo build --release` 的产物打成 FuyuBot bundle zip（契约 v1）。

一个仓库三个插件，每个插件各自一份 `plugin.json`（在 `crates/<crate>/` 根部）。
本脚本对三个插件一视同仁，用 `--plugin` 选目标：

    python scripts/package.py --plugin fuyubot-plugin-sqlite \
        --platform windows-x86_64 --version 0.1.0

bundle 结构（契约 v1）：

    <plugin-id>/
    ├── plugin.json        # entry 指向 bin/ 下的动态库
    └── bin/<动态库>

同一份 plugin.json 服务所有平台，`--platform` 只改 `entry` 与 `platforms`，
这样商城按平台挑资产时不需要另一份清单。
"""

from __future__ import annotations

import argparse
import json
import shutil
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
# platform: (动态库后缀名, 是否带 lib 前缀)
TARGETS = {
    "windows-x86_64": (".dll", False),
    "windows-aarch64": (".dll", False),
    "linux-x86_64": (".so", True),
    "linux-aarch64": (".so", True),
    "macos-aarch64": (".dylib", True),
    "android-aarch64": (".so", True),
    "android-x86_64": (".so", True),
}
# Android 用 NDK 交叉编译，产物在 target/<triple>/release/。
ANDROID_TRIPLES = {
    "android-aarch64": "aarch64-linux-android",
    "android-x86_64": "x86_64-linux-android",
}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--plugin", required=True, help="crate 目录名，如 fuyubot-plugin-sqlite")
    parser.add_argument("--platform", required=True, choices=sorted(TARGETS))
    parser.add_argument("--version", required=True)
    parser.add_argument("--target-dir", default="target/release")
    args = parser.parse_args()

    crate = ROOT / "crates" / args.plugin
    manifest_path = crate / "plugin.json"
    if not manifest_path.is_file():
        raise SystemExit(f"找不到 {manifest_path}")
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))

    stem = args.plugin.replace("-", "_")
    suffix, prefixed = TARGETS[args.platform]
    lib_name = f"{'lib' if prefixed else ''}{stem}{suffix}"

    triple = ANDROID_TRIPLES.get(args.platform)
    release = ROOT / "target" / triple / "release" if triple else ROOT / args.target_dir
    lib = release / lib_name
    if not lib.is_file():
        raise SystemExit(f"找不到 {lib}；先编译对应平台")

    dist = ROOT / "dist"
    bundle = dist / "bundle"
    if bundle.exists():
        shutil.rmtree(bundle)
    bin_dir = bundle / "bin"
    bin_dir.mkdir(parents=True)

    shutil.copy2(lib, bin_dir / lib_name)

    manifest["version"] = args.version
    manifest["entry"] = f"bin/{lib_name}"
    manifest["platforms"] = [args.platform]
    (bundle / "plugin.json").write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )

    plugin_id = manifest["id"]
    archive = dist / f"{plugin_id}-{args.version}-{args.platform}.zip"
    with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as zf:
        for path in sorted(bundle.rglob("*")):
            if path.is_file():
                zf.write(path, path.relative_to(bundle))
    print(f"[OK] {archive}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
