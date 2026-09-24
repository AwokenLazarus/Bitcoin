#!/usr/bin/env python3
"""Apply node/umbrel/mempool-patches/datum-template-creator.patch to the hub's installed backend.

Same change, same anchors as patch-backend.py, but against /opt/mempool-backend/package instead of
an image: the hub runs the backend from systemd, so the Umbrel pre-start hook never applied it.
"""
import glob, pathlib, re, shutil, sys, time

BASE = pathlib.Path("/opt/mempool-backend/package")
TS = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
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
        else if (b < 0x20 || b === 0x7f) {
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
CALL_SITE = re.compile(
    r"^(?P<indent>[ \t]*)if \(extras\.pool\.name === 'OCEAN'\) \{\n"
    r"[ \t]*extras\.pool\.minerNames = \(0, bitcoin_script_1\.parseDATUMTemplateCreator\)\(extras\.coinbaseRaw\);\n"
    r"(?:[ \t]*\}\n[ \t]*else if \(extras\.pool\.name === 'DMND'\) \{\n"
    r"[ \t]*extras\.pool\.minerNames = \(0, bitcoin_script_1\.parseDMNDTemplateCreator\)\(extras\.coinbaseRaw\);\n)?"
    r"[ \t]*\}\n",
    re.M,
)

REPLACEMENT = "\\g<indent>extras.pool.minerNames = (0, bitcoin_script_1.parseTemplateCreator)(extras.pool.name, extras.coinbaseRaw);\n"

# start from the untouched files if an earlier ad-hoc edit is in place
for rel in ("api/blocks.js", "repositories/BlocksRepository.js"):
    p = BASE / rel
    baks = sorted(glob.glob(str(p) + ".bak-*-pre-minernames"))
    if baks:
        shutil.copy2(baks[0], p)
        print("restored", rel, "from", pathlib.Path(baks[0]).name)

script = BASE / "utils/bitcoin-script.js"
s = script.read_text()
if "function isDATUMCoinbase" not in s:
    shutil.copy2(script, str(script) + f".bak-{TS}-pre-datum")
    script.write_text(s.rstrip() + "\n" + HELPERS)
    print("added helpers to utils/bitcoin-script.js")
elif "b > 0x7e" in s:
    # applied before UTF-8 tags were accepted (XBT-047): upgrade the one byte test in place
    shutil.copy2(script, str(script) + f".bak-{TS}-pre-utf8")
    script.write_text(s.replace("(b < 0x20 || b > 0x7e)", "(b < 0x20 || b === 0x7f)"))
    print("upgraded utils/bitcoin-script.js: UTF-8 tags accepted")

for rel in ("api/blocks.js", "repositories/BlocksRepository.js"):
    p = BASE / rel
    s = p.read_text()
    new, n = CALL_SITE.subn(REPLACEMENT, s)
    if not n and "bitcoin_script_1.parseTemplateCreator)(extras.pool.name" in s:
        print(f"already patched {rel}")
        continue
    if not n:
        print(f"ANCHOR MISSING in {rel}", file=sys.stderr); sys.exit(1)
    shutil.copy2(p, str(p) + f".bak-{TS}-pre-datum")
    p.write_text(new)
    print(f"patched {rel} ({n} call site)")

