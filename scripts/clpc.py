# /// script
# requires-python = ">=3.10"
# dependencies = ["capstone==5.0.9"]
# ///
"""Extract CLPC energy IDs from Apple IPSWs; see docs/clpc-discovery.md.

scan [latest|VERSION|BUILD|APPLE_URL] prints discovered CPU/GPU/ANE IDs as Rust constants.
history MAJOR compares every indexed stable release in a macOS branch.
compare OLD_JSON NEW_JSON compares two saved catalogs.
Downloads, catalogs and comparisons stay under the repository's out/clpc-static/.
"""

import argparse
import concurrent.futures
import ctypes
import hashlib
import io
import json
import plistlib
import re
import struct
import subprocess
import sys
import time
import zipfile
from pathlib import Path
from urllib.parse import urlparse

from clpc_binary import Image, discover

OUT = Path(__file__).resolve().parent.parent / "out" / "clpc-static"
COMPONENTS = ("CPU", "GPU", "ANE")
APPLE_CATALOG = (
    "https://mesu.apple.com/assets/macos/com_apple_macOSIPSW/com_apple_macOSIPSW.xml"
)
ARCHIVE_API = "https://api.ipsw.me/v4"


def save(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2) + "\n")


def rust_table(catalog):
    if not catalog["complete"]:
        raise ValueError("Cannot export an incomplete catalog")
    indices = {
        tuple(int(d["channels"][c]["id"], 16) >> 32 for c in COMPONENTS)
        for d in catalog["drivers"]
    }
    if len(indices) != 1:
        raise ValueError(
            "Drivers have different table indices; inspect JSON before updating Rust"
        )
    cpu, gpu, ane = indices.pop()
    major = int(catalog["macos"].split(".")[0])
    lines = [
        f"// Generated from macOS {catalog['macos']} ({catalog['build']}) by scripts/clpc.py.",
        f"const INDICES_{major}: ClpcIndices = ClpcIndices {{ cpu: {cpu}, gpu: {gpu}, ane: {ane} }};",
        "",
        "const CLPC_KEYS: &[ClpcKeys] = &[",
    ]
    for driver in catalog["drivers"]:
        lines.extend(["  ClpcKeys {", f'    bundle: "{driver["bundle"]}",'])
        for component in COMPONENTS:
            key = int(driver["channels"][component]["id"], 16) & 0xFFFFFFFF
            lines.append(
                f"    {component.lower()}: 0x{key >> 16:04x}_{key & 0xFFFF:04x},"
            )
        lines.append("  },")
    return "\n".join([*lines, "];", ""])


def fetch(url, *args):
    return subprocess.check_output(
        ["curl", "-fsSL", "--retry", "2", "--max-time", "90", *args, url]
    )


class RemoteZip(io.RawIOBase):
    def __init__(self, url):
        self.url, self.pos = url, 0
        headers = fetch(url, "-I").decode()
        self.size = int(
            next(
                line.split(":", 1)[1]
                for line in headers.splitlines()
                if line.lower().startswith("content-length:")
            )
        )

    def seek(self, offset, whence=0):
        self.pos = offset + (
            0 if whence == 0 else self.pos if whence == 1 else self.size
        )
        return self.pos

    def tell(self):
        return self.pos

    def read(self, size=-1):
        size = self.size - self.pos if size < 0 else min(size, self.size - self.pos)
        if not size:
            return b""
        if not 0 < size < 150_000_000:
            raise ValueError("Unexpected ZIP range size")
        data = fetch(self.url, "--range", f"{self.pos}-{self.pos + size - 1}")
        if len(data) != size:
            raise ValueError("Server did not return the requested byte range")
        self.pos += size
        return data


def download(url, member, path):
    with zipfile.ZipFile(RemoteZip(url)) as archive:
        path.write_bytes(archive.read(member))


def decompress(source):
    target = source.with_name(source.name + ".macho")
    if not target.exists():
        data = source.read_bytes()
        payload = data[data.index(b"bvx2") :]
        lib = ctypes.CDLL("/usr/lib/libcompression.dylib")
        decode = lib.compression_decode_buffer
        decode.argtypes = [
            ctypes.c_void_p,
            ctypes.c_size_t,
            ctypes.c_void_p,
            ctypes.c_size_t,
            ctypes.c_void_p,
            ctypes.c_int,
        ]
        decode.restype = ctypes.c_size_t
        buf = ctypes.create_string_buffer(256 * 1024 * 1024)
        size = decode(buf, len(buf), payload, len(payload), None, 0x801)
        if not 0 < size < len(buf) or buf.raw[:4] != b"\xcf\xfa\xed\xfe":
            raise ValueError("Unsupported or invalid LZFSE kernelcache")
        target.write_bytes(buf.raw[:size])
    return target


def scan(release):
    url = release["url"]
    cache = OUT / release.get("build", hashlib.sha256(url.encode()).hexdigest()[:12])
    cache.mkdir(parents=True, exist_ok=True)
    source, manifest_path = cache / "source.json", cache / "BuildManifest.plist"
    if not source.exists():
        download(url, "BuildManifest.plist", manifest_path)
        save(
            source,
            {
                "ipsw_url": url,
                "manifest_sha256": hashlib.sha256(
                    manifest_path.read_bytes()
                ).hexdigest(),
            },
        )
    provenance = json.loads(source.read_text())
    data = manifest_path.read_bytes()
    if (
        provenance["ipsw_url"] != url
        or provenance["manifest_sha256"] != hashlib.sha256(data).hexdigest()
    ):
        raise ValueError("Cached image provenance does not match")
    manifest = plistlib.loads(data)
    version, build = manifest["ProductVersion"], manifest["ProductBuildVersion"]
    if "build" in release and (version, build) != (
        release["version"],
        release["build"],
    ):
        raise ValueError("Apple manifest does not match the selected release")
    members = {}
    for identity in manifest["BuildIdentities"]:
        member = (
            identity.get("Manifest", {})
            .get("KernelCache", {})
            .get("Info", {})
            .get("Path")
        )
        if member:
            members.setdefault(member, set()).add(
                (
                    str(identity.get("ApChipID", "")),
                    identity.get("Info", {}).get("DeviceClass", ""),
                )
            )
    if not members or len({Path(m).name for m in members}) != len(members):
        raise ValueError("Missing or ambiguous kernelcache paths")
    result = {
        "macos": version,
        "build": build,
        **provenance,
        "scope": "Static report identities; runtime access and units require hardware validation.",
        "scripts": {
            p.name: hashlib.sha256(p.read_bytes()).hexdigest()
            for p in (Path(__file__), Path(__file__).with_name("clpc_binary.py"))
        },
        "images": [],
        "drivers": [],
        "failures": [],
    }
    seen = {}
    for number, member in enumerate(sorted(members), 1):
        print(
            f"{version} ({build}) [{number}/{len(members)}] {member}",
            file=sys.stderr,
            flush=True,
        )
        image_info = {"member": member, "targets": sorted(members[member])}
        result["images"].append(image_info)
        try:
            path = cache / Path(member).name
            if not path.exists():
                download(url, member, path)
            image = Image(decompress(path))
            image_info["sha256"] = hashlib.sha256(image.data).hexdigest()
            nodes = image.energy_nodes()
            for driver in image.drivers():
                key = driver["bundle"]
                if key in seen:
                    if seen[key] != driver:
                        raise ValueError(f"Conflicting driver copies: {key}")
                    continue
                seen[key] = driver
                row = {**driver, "member": member, "reports": driver["channels"]}
                row.update(discover(image, driver, nodes))
                result["drivers"].append(row)
        except (
            ValueError,
            KeyError,
            IndexError,
            OSError,
            struct.error,
            subprocess.CalledProcessError,
        ) as error:
            result["failures"].append({"member": member, "error": str(error)})
            print(f"FAILED: {member}: {error}", file=sys.stderr)
    result["drivers"].sort(key=lambda d: d["bundle"])
    result["complete"] = (
        bool(result["drivers"])
        and not result["failures"]
        and all(d["status"] == "identified" for d in result["drivers"])
    )
    dest = OUT / f"catalog-{version}-{build}.json"
    save(dest, result)
    lines = ["driver\tCPU\tGPU\tANE\tstatus"] + [
        "\t".join(
            [
                d["bundle"],
                *(d["channels"].get(c, {}).get("id", "?") for c in COMPONENTS),
                d["status"],
            ]
        )
        for d in result["drivers"]
    ]
    dest.with_suffix(".tsv").write_text("\n".join(lines) + "\n")
    print("\n".join(lines), file=sys.stderr)
    print(dest, file=sys.stderr)
    return result


def difference(before, after):
    def ids(driver):
        return {c: driver["channels"].get(c, {}).get("id") for c in COMPONENTS}

    def hashes(driver):
        return {k: v["sha256"] for k, v in driver["segments"].items()}

    old, new = ids(before), ids(after)
    return {
        "bundle": after["bundle"],
        "uuid_changed": before["uuid"] != after["uuid"],
        "bytes_changed": hashes(before) != hashes(after),
        "executable_changed": before["segments"]["__TEXT_EXEC"]["sha256"]
        != after["segments"]["__TEXT_EXEC"]["sha256"],
        "table_changed": [(c["id"], c["name"]) for c in before["reports"]]
        != [(c["id"], c["name"]) for c in after["reports"]],
        "old_ids": old,
        "new_ids": new,
        "energy_ids_changed": any(old[c] != new[c] for c in COMPONENTS)
        if all(old.values())
        and all(new.values())
        and before["status"] == after["status"] == "identified"
        else None,
    }


def compare(old, new):
    before, after = ({d["bundle"]: d for d in cat["drivers"]} for cat in (old, new))
    return {
        "from": old["build"],
        "to": new["build"],
        "complete": old["complete"] and new["complete"],
        "failures": {
            label: [
                *catalog["failures"],
                *[
                    {"bundle": d["bundle"], "error": "Unresolved producer"}
                    for d in catalog["drivers"]
                    if d["status"] != "identified"
                ],
            ]
            for label, catalog in (("old", old), ("new", new))
        },
        "added": sorted(after.keys() - before.keys()),
        "removed": sorted(before.keys() - after.keys()),
        "drivers": [
            difference(before[k], after[k])
            for k in sorted(before.keys() & after.keys())
        ],
    }


def history(major):
    releases = [
        r
        for r in archive_releases(OUT / "index")
        if r["version"].split(".")[0] == str(major)
    ]
    if not releases:
        raise ValueError(f"No stable UniversalMac images for macOS {major}")
    result = {"major": major, "releases": [], "transitions": [], "failures": []}
    previous = {}
    for release in releases:
        try:
            catalog = scan(release)
            transitions = []
            for driver in catalog["drivers"]:
                key = driver["bundle"]
                if key in previous:
                    old_build, old_driver = previous[key]
                    transitions.append(
                        {
                            "from": old_build,
                            "to": catalog["build"],
                            **difference(old_driver, driver),
                        }
                    )
                previous[key] = (catalog["build"], driver)
            result["transitions"].extend(transitions)
            row = {
                "version": catalog["macos"],
                "build": catalog["build"],
                "drivers": len(catalog["drivers"]),
                "compared": len(transitions),
                "complete": catalog["complete"],
                "unknown_energy": sum(
                    t["energy_ids_changed"] is None for t in transitions
                ),
                **{
                    key: sum(t[key] is True for t in transitions)
                    for key in (
                        "uuid_changed",
                        "bytes_changed",
                        "executable_changed",
                        "table_changed",
                        "energy_ids_changed",
                    )
                },
            }
            result["releases"].append(row)
            if not catalog["complete"]:
                result["failures"].append(
                    {
                        "build": catalog["build"],
                        "errors": catalog["failures"],
                        "unknown": [
                            d["bundle"]
                            for d in catalog["drivers"]
                            if d["status"] != "identified"
                        ],
                    }
                )
        except (ValueError, OSError, subprocess.CalledProcessError) as error:
            result["failures"].append({"build": release["build"], "error": str(error)})
    save(OUT / f"history-{major}.json", result)
    if result["releases"]:
        fields = list(result["releases"][0])
        lines = ["\t".join(fields)] + [
            "\t".join(str(r[k]) for k in fields) for r in result["releases"]
        ]
        (OUT / f"history-{major}.tsv").write_text("\n".join(lines) + "\n")
        print("\n".join(lines))
    return 2 if result["failures"] else 0


def apple_url(url):
    parsed = urlparse(url)
    if (
        parsed.scheme != "https"
        or not parsed.hostname
        or not parsed.hostname.endswith((".apple.com", ".cdn-apple.com"))
    ):
        raise ValueError(f"Expected an HTTPS Apple download URL: {url}")
    return url


def version_key(version):
    if not re.fullmatch(r"\d+(?:\.\d+)*", version):
        raise ValueError(f"Not a stable release version: {version}")
    return tuple(int(n) for n in version.split("."))


def current_release():
    catalog = plistlib.loads(fetch(APPLE_CATALOG))
    rows = {}

    def visit(value):
        if isinstance(value, dict):
            url = value.get("FirmwareURL", "")
            if "UniversalMac_" in url and url.endswith(".ipsw"):
                version = value.get("ProductVersion", "")
                if re.fullmatch(r"\d+(?:\.\d+)*", version):
                    rows[url] = {
                        "version": version,
                        "build": value["BuildVersion"],
                        "url": apple_url(url),
                        "index": APPLE_CATALOG,
                    }
            for child in value.values():
                visit(child)
        elif isinstance(value, list):
            for child in value:
                visit(child)

    visit(catalog)
    if not rows:
        raise ValueError("Apple's current catalog has no stable UniversalMac image")
    latest = max(version_key(r["version"]) for r in rows.values())
    choices = [r for r in rows.values() if version_key(r["version"]) == latest]
    if len(choices) != 1:
        raise ValueError(
            f"Multiple current builds; select an exact build or URL: {choices}"
        )
    return choices[0]


def archive_releases(cache):
    cache = Path(cache)
    cache.mkdir(parents=True, exist_ok=True)
    devices_path = cache / "devices.json"
    if not devices_path.exists() or time.time() - devices_path.stat().st_mtime > 86400:
        devices_path.write_bytes(fetch(f"{ARCHIVE_API}/devices?type=ipsw"))
    devices = [
        d
        for d in json.loads(devices_path.read_text())
        if d["identifier"].startswith(("Mac", "iMac", "VirtualMac"))
    ]

    def device_rows(device):
        identifier = device["identifier"]
        path = cache / (identifier + ".json")
        # Short-lived index cache; no signing filter, since offline analysis also
        # needs unsigned releases. Apple still hosts their restore images.
        if not path.exists() or time.time() - path.stat().st_mtime > 86400:
            path.write_bytes(fetch(f"{ARCHIVE_API}/device/{identifier}?type=ipsw"))
        return identifier, json.loads(path.read_text())["firmwares"]

    rows = {}
    with concurrent.futures.ThreadPoolExecutor(max_workers=6) as pool:
        for identifier, firmwares in pool.map(device_rows, devices):
            for firmware in firmwares:
                url, version = firmware["url"], firmware["version"]
                if "UniversalMac_" not in url or not re.fullmatch(
                    r"\d+(?:\.\d+)*", version
                ):
                    continue
                key = (version, firmware["buildid"], url)
                row = rows.setdefault(
                    key,
                    {
                        "version": version,
                        "build": firmware["buildid"],
                        "url": apple_url(url),
                        "releasedate": firmware.get("releasedate"),
                        "devices": [],
                        "index": ARCHIVE_API,
                    },
                )
                row["devices"].append(identifier)
    return sorted(rows.values(), key=lambda r: (version_key(r["version"]), r["build"]))


def resolve(selector, cache):
    if selector.startswith("https://"):
        return {"url": apple_url(selector)}
    if selector == "latest":
        return current_release()
    matches = [
        r for r in archive_releases(cache) if selector in (r["version"], r["build"])
    ]
    if len(matches) != 1:
        builds = ", ".join(r["build"] for r in matches) or "none"
        raise ValueError(
            f"Selector {selector!r} matches {len(matches)} images ({builds}); use an exact build or Apple URL"
        )
    return matches[0]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("scan").add_argument("release", nargs="?", default="latest")
    sub.add_parser("history").add_argument("major", type=int)
    diff = sub.add_parser("compare")
    diff.add_argument("old", type=Path)
    diff.add_argument("new", type=Path)
    args = parser.parse_args()
    if args.command == "history":
        return history(args.major)
    if args.command == "compare":
        result = compare(
            json.loads(args.old.read_text()), json.loads(args.new.read_text())
        )
        dest = OUT / f"compare-{result['from']}-{result['to']}.json"
        save(dest, result)
        print(json.dumps(result, indent=2))
        print(dest)
        return (
            2
            if not result["complete"]
            or any(result["failures"].values())
            or any(d["energy_ids_changed"] is None for d in result["drivers"])
            else 0
        )
    release = resolve(args.release, OUT / "index")
    print(f"Apple IPSW: {release['url']}", file=sys.stderr, flush=True)
    catalog = scan(release)
    if not catalog["complete"]:
        return 2
    try:
        output = rust_table(catalog)
    except ValueError as error:
        print(error, file=sys.stderr)
        return 2
    print(output, end="")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        sys.exit(str(error))
