"""Fetch the clang-tidy of the clang-cl the pinned WebRTC brings, pinned by hash, and put it next to that clang-cl."""
import hashlib, io, pathlib, re, tarfile, urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[2]
WEBRTC = ROOT / 'temp/w/src'
LLVM = WEBRTC / 'third_party/llvm-build/Release+Asserts'
# The bucket WebRTC's tools/clang/scripts/update.py takes its clang from; the
# clang-tidy package there is built from the same revision as the clang-cl
# the WebRTC checkout has (update.py's CLANG_REVISION and CLANG_SUB_REVISION
# name it). update.py itself is not used for the package: asked for one
# package it clears the whole llvm-build folder first, clang-cl included.
BUCKET = 'https://commondatastorage.googleapis.com/chromium-browser-clang/Win'
# The archive's SHA-256, by the version update.py names. A WebRTC update that
# moves the version stops here until the new archive has been looked at and
# its hash added.
PINNED = {'llvmorg-24-init-7747-g62397f8b-31': '44743a0d903f7d0bcc7b289111aad8acceb75ab99ef4f9c146af44e28db4327f'}


def get(url):
    req = urllib.request.Request(url, headers={'User-Agent': 'ksip-build/0.1'})
    return urllib.request.urlopen(req, timeout=120).read()


def version():
    text = (WEBRTC / 'tools/clang/scripts/update.py').read_text(encoding='utf-8')
    revision = re.search(r"^CLANG_REVISION = '([^']+)'", text, re.M)
    sub = re.search(r'^CLANG_SUB_REVISION = (\d+)', text, re.M)
    if not revision or not sub:
        raise SystemExit('tools/clang/scripts/update.py does not name the clang version the way this script reads it')
    return f'{revision.group(1)}-{sub.group(1)}'


def main():
    if not (LLVM / 'bin/clang-cl.exe').is_file():
        raise SystemExit('Run scripts/deps/fetch-webrtc.py first: the clang-cl is not there')
    wanted = version()
    # The stamp update.py leaves with the clang package: the clang-cl that is
    # there must be the one update.py names, or the two would not match.
    present = (LLVM / 'cr_build_revision').read_text(encoding='utf-8').strip().split(',')[0]
    if present != wanted:
        raise SystemExit(f'The clang-cl there is {present}, update.py names {wanted}')
    exe = LLVM / 'bin/clang-tidy.exe'
    stamp = LLVM / 'clang-tidy_revision'
    if exe.is_file() and stamp.is_file() and stamp.read_text(encoding='utf-8').strip() == wanted:
        print('clang-tidy', wanted, 'is there')
        return
    pinned = PINNED.get(wanted)
    if not pinned:
        raise SystemExit(f'No pinned hash for clang-tidy {wanted}: look at {BUCKET}/clang-tidy-{wanted}.tar.xz and add it to PINNED')
    name = f'clang-tidy-{wanted}.tar.xz'
    data = get(f'{BUCKET}/{name}')
    digest = hashlib.sha256(data).hexdigest()
    if digest != pinned:
        raise SystemExit(f'{name}: SHA256 {digest} is not the pinned {pinned}')
    with tarfile.open(fileobj=io.BytesIO(data)) as tar:
        with tar.extractfile('bin/clang-tidy.exe') as member:
            exe.write_bytes(member.read())
    stamp.write_text(wanted + '\n', encoding='utf-8')
    print('clang-tidy', wanted, digest)


if __name__ == '__main__':
    main()
