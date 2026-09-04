#!/usr/bin/env python3
"""Build patched copies of the mempool backend's compiled JS for the pinned image.

Usage: patch-backend.py <docker-compose.yml> <out-dir>

Reads the `api` service image from the compose file, copies the three files touched by
datum-template-creator.patch out of that image, applies the same change to the compiled JS,
and writes them under <out-dir>/backend/... for the pre-start hook to bind-mount over
/backend/package/... The image digest is recorded in <out-dir>/IMAGE so the work is skipped
when nothing changed.

Exit status is non-zero (and <out-dir>/backend is removed) whenever an anchor is missing, so
the hook never mounts half-patched files over a backend it does not recognise.
"""
import hashlib
import re
import shutil
import subprocess
import sys
from pathlib import Path

FILES = ["api/blocks.js", "repositories/BlocksRepository.js", "utils/bitcoin-script.js"]

HELPERS = r'''
/**
 * Whether a coinbase scriptSig carries a pooled DATUM template-creator tag.
 *
 * DATUM gateways (datum_gateway/src/datum_coinbaser.c, generate_coinbase_input) write the
 * first push after the BIP34 height as `<primary tag> 0x0F <secondary tag> 0x00`, where the
 * primary tag is set by the pool and the secondary tag by the gateway operator (the template
 * creator). The next push is the gateway's unique id: 3 bytes when mining solo, longer when
 * the pool's prime id is appended. Any pool that speaks DATUM produces this layout, so it is
 * detected structurally instead of by pool name.
 */
function isDATUMCoinbase(coinbaseRaw) {
    if (!coinbaseRaw || coinbaseRaw.length % 2 !== 0 || !/^[0-9a-fA-F]+$/.test(coinbaseRaw)) {
        return false;
    }
    const bytes = Buffer.from(coinbaseRaw, 'hex');
    const heightLength = bytes[0];
    if (heightLength < 1 || heightLength > 8) {
        return false;
    }
    let lengthIndex = 1 + heightLength;
    if (lengthIndex >= bytes.length) {
        return false;
    }
    let tagsLength = bytes[lengthIndex];
    if (tagsLength === 0x4c) {
        lengthIndex += 1;
        if (lengthIndex >= bytes.length) {
            return false;
        }
        tagsLength = bytes[lengthIndex];
    }
    const tagStart = lengthIndex + 1;
    if (tagsLength < 4 || tagStart + tagsLength > bytes.length) {
        return false;
    }
    const tags = bytes.subarray(tagStart, tagStart + tagsLength);
    if (tags[tags.length - 1] !== 0x00) {
        return false;
    }
    const uidIndex = tagStart + tagsLength;
    if (uidIndex >= bytes.length) {
        return false;
    }
    const uidLength = bytes[uidIndex];
    if (uidLength <= 3 || uidLength > 75 || uidIndex + 1 + uidLength > bytes.length) {
        return false;
    }
    let separators = 0;
    for (let i = 0; i < tags.length - 1; i++) {
        const b = tags[i];
        if (b === 0x0f) {
            if (i === 0 || i === tags.length - 2) {
                return false;
            }
            separators++;
        }
        else if (b < 0x20 || b > 0x7e) {
            return false;
        }
    }
    return separators === 1;
}
exports.isDATUMCoinbase = isDATUMCoinbase;
/** Miner (template creator) names for a block, when the pool exposes them in the coinbase. */
function parseTemplateCreator(poolName, coinbaseRaw) {
    if (poolName === 'OCEAN') {
        return parseDATUMTemplateCreator(coinbaseRaw);
    }
    if (poolName === 'DMND' && typeof parseDMNDTemplateCreator === 'function') {
        return parseDMNDTemplateCreator(coinbaseRaw);
    }
    if (isDATUMCoinbase(coinbaseRaw)) {
        const names = parseDATUMTemplateCreator(coinbaseRaw);
        if (names && names.length > 1 && names[1].trim().toLowerCase() === names[0].trim().toLowerCase()) {
            return null;
        }
        return names;
    }
    return null;
}
exports.parseTemplateCreator = parseTemplateCreator;
'''

# `if (extras.pool.name === 'OCEAN') { ... } [else if (... 'DMND') { ... }]` -> one call.
CALL_SITE = re.compile(
    r"^(?P<indent>[ \t]*)if \(extras\.pool\.name === 'OCEAN'\) \{\n"
    r"[ \t]*extras\.pool\.minerNames = \(0, bitcoin_script_1\.parseDATUMTemplateCreator\)\(extras\.coinbaseRaw\);\n"
    r"(?:[ \t]*\}\n[ \t]*else if \(extras\.pool\.name === 'DMND'\) \{\n"
    r"[ \t]*extras\.pool\.minerNames = \(0, bitcoin_script_1\.parseDMNDTemplateCreator\)\(extras\.coinbaseRaw\);\n)?"
    r"[ \t]*\}\n",
    re.M,
)
REPLACEMENT = "\\g<indent>extras.pool.minerNames = (0, bitcoin_script_1.parseTemplateCreator)(extras.pool.name, extras.coinbaseRaw);\n"


def fail(msg):
    print(f"mempool-patches: {msg}", file=sys.stderr)
    sys.exit(1)


def api_image(compose_text):
    m = re.search(r"^  api:\n(.*?)(?=^  \S)", compose_text, flags=re.M | re.S)
    if not m:
        fail("no api service in compose file")
    im = re.search(r"^\s+image:\s*(?:>-\s*\n\s*)?(\S+)", m.group(1), flags=re.M)
    if not im:
        fail("no image for api service")
    return im.group(1)


def patch_call_sites(text, name):
    new, n = CALL_SITE.subn(REPLACEMENT, text)
    if n != 1:
        fail(f"{name}: expected 1 OCEAN call site, found {n}")
    return new


def patch_helpers(text):
    anchor = "exports.parseDATUMTemplateCreator = parseDATUMTemplateCreator;\n"
    if text.count(anchor) != 1:
        fail("bitcoin-script.js: parseDATUMTemplateCreator export anchor not found")
    if "function parseTemplateCreator" in text:
        fail("bitcoin-script.js: already defines parseTemplateCreator (image ships the fix; drop this patch)")
    return text.replace(anchor, anchor + HELPERS, 1)


def main():
    if len(sys.argv) != 3:
        fail(__doc__.strip().splitlines()[2])
    compose, out = Path(sys.argv[1]), Path(sys.argv[2])
    image = api_image(compose.read_text())
    # rebuild when the pinned image changes or when this script itself changes
    stamp = f"{image} {hashlib.sha256(Path(__file__).read_bytes()).hexdigest()[:16]}"
    marker = out / "IMAGE"
    dest = out / "backend"
    if marker.exists() and marker.read_text().strip() == stamp and all((dest / f).is_file() for f in FILES):
        print(f"mempool-patches: backend files current for {image}")
        return

    if dest.exists():
        shutil.rmtree(dest)
    marker.unlink(missing_ok=True)
    cid = None
    try:
        cid = subprocess.run(["docker", "create", image], check=True, capture_output=True, text=True).stdout.strip()
        for f in FILES:
            target = dest / f
            target.parent.mkdir(parents=True, exist_ok=True)
            subprocess.run(["docker", "cp", f"{cid}:/backend/package/{f}", str(target)], check=True, capture_output=True)
    except subprocess.CalledProcessError as e:
        shutil.rmtree(dest, ignore_errors=True)
        fail(f"docker failed: {e.stderr.strip() if e.stderr else e}")
    finally:
        if cid:
            subprocess.run(["docker", "rm", "-f", cid], capture_output=True)

    try:
        for f in FILES[:2]:
            p = dest / f
            p.write_text(patch_call_sites(p.read_text(), f))
        p = dest / FILES[2]
        p.write_text(patch_helpers(p.read_text()))
    except SystemExit:
        shutil.rmtree(dest, ignore_errors=True)
        raise
    for f in FILES:
        (dest / f).chmod(0o644)
    marker.write_text(stamp + "\n")
    print(f"mempool-patches: patched {', '.join(FILES)} from {image}")


if __name__ == "__main__":
    main()
