"""Check the CycloneDX SBOM of release/ksip.exe: made only for the exe the build recorded, valid against the official 1.6 schema, the same content each time it is made, and what it lists matches what cargo, the native source lock, the linked WebRTC library and the toolchain say is built in."""
import hashlib, importlib.util, json, os, re, subprocess, sys, tomllib
from pathlib import Path
from json_schema import Schemas

ROOT = Path(__file__).resolve().parents[2]
EXE = ROOT / 'release/ksip.exe'
OUT = ROOT / 'temp/build/sbom-test'
TARGET = 'x86_64-pc-windows-msvc'
failures = []
spec = importlib.util.spec_from_file_location('sbom', ROOT / 'scripts/build/sbom.py')
sbom = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sbom)


def check(ok, what, detail=''):
    print(('PASS: ' if ok else 'FAIL: ') + what + (f' ({detail})' if detail and not ok else ''), flush=True)
    if not ok:
        failures.append(what)


def generate(name):
    path = OUT / name
    subprocess.run([sys.executable, '-X', 'utf8', str(ROOT / 'scripts/build/sbom.py'), '--exe', str(EXE), '--out', str(path)], check=True, capture_output=True)
    return json.loads(path.read_text(encoding='utf-8'))


def refused(exe, record):
    """What sbom.py says when it will not make the SBOM (empty if it did)."""
    done = subprocess.run([sys.executable, '-X', 'utf8', str(ROOT / 'scripts/build/sbom.py'), '--exe', str(exe), '--record', str(record),
                           '--out', str(OUT / 'refused.cdx.json')], capture_output=True, encoding='utf-8')
    return '' if done.returncode == 0 else (done.stderr.strip().splitlines() or ['?'])[-1]


def cargo_tree(edges):
    """The crates cargo itself reports for Windows over these edges, by name@version, ksip aside."""
    return {f'{n}@{v}' for _, n, v in sbom.cargo_tree(edges) if n != 'ksip'}


def spdx_terms(expression):
    return [t for t in re.split(r'\s+|[()]', expression) if t and t not in ('AND', 'OR', 'WITH')]


def main():
    if not EXE.is_file():
        raise SystemExit(f'Build the app first: {EXE}')
    # Only for the exe the build recorded, and only while what it was built from is unchanged.
    record_path = ROOT / 'temp/build/build-record.json'
    built_from = json.loads(record_path.read_text(encoding='utf-8'))
    OUT.mkdir(parents=True, exist_ok=True)
    other = OUT / 'other.exe'
    other.write_bytes(EXE.read_bytes() + bytes(1))
    said = refused(other, record_path)
    check('not the exe' in said, 'an exe the build did not record (another build, an older version) gets no SBOM', said)
    stale = OUT / 'stale-record.json'
    stale.write_text(json.dumps({**built_from, 'inputs': {**built_from['inputs'], 'src-tauri/Cargo.lock': '0' * 64}}), encoding='utf-8')
    said = refused(EXE, stale)
    check('changed since' in said and 'Cargo.lock' in said, 'nor one whose inputs changed after it was built', said)
    wrong = OUT / 'wrong-version.json'
    wrong.write_text(json.dumps({**built_from, 'version': '0.0.0'}), encoding='utf-8')
    said = refused(EXE, wrong)
    check('says' in said, 'nor one whose version is not what the exe and Cargo.toml say', said)
    # WebRTC's parts come from the record, not from the WebRTC tree as it is
    # when the SBOM is made: pointed at a tree that is not there, the SBOM is
    # made all the same, with the parts the build recorded.
    elsewhere = subprocess.run([sys.executable, '-X', 'utf8', str(ROOT / 'scripts/build/sbom.py'), '--exe', str(EXE), '--out', str(OUT / 'elsewhere.cdx.json')],
                               capture_output=True, encoding='utf-8', env={**os.environ, 'KSIP_WEBRTC_SOURCE': str(OUT / 'no-webrtc-here')})
    moved = json.loads((OUT / 'elsewhere.cdx.json').read_text(encoding='utf-8')) if elsewhere.returncode == 0 else {'components': []}
    got = sorted((c['name'], c.get('version')) for c in moved['components'] if any(p['name'] == 'ksip:bundled_in' for p in c.get('properties', [])))
    recorded = sorted((p['name'], p['version']) for p in built_from['webrtc']['parts'])
    check(elsewhere.returncode == 0 and got == recorded, "WebRTC's parts are the ones the build recorded, whatever tree is there when the SBOM is made", elsewhere.stderr.strip()[-200:] or got)
    bom, again = generate('first.cdx.json'), generate('second.cdx.json')
    fixed = lambda b: {**b, 'serialNumber': None, 'metadata': {**b['metadata'], 'timestamp': None}}
    check(fixed(bom) == fixed(again), 'made twice from the same exe, the SBOMs differ only in the time and the serial number')
    check(bom['serialNumber'] != again['serialNumber'], 'each SBOM made is a document of its own (a serial number of its own)')

    # The document, against the official schema (CycloneDX 1.6, pinned in deps/cyclonedx).
    schemas = Schemas(ROOT / 'deps/cyclonedx')
    errors = schemas.errors(bom, schemas.docs['bom-1.6.schema.json'])
    check(not errors, 'valid against the official CycloneDX 1.6 JSON schema', errors[:5])
    broken = {**bom, 'components': [{**bom['components'][0], 'scope': 'runtime'}]}
    check(schemas.errors(broken, schemas.docs['bom-1.6.schema.json']), 'and the schema check does catch a component that breaks it')
    known = sbom.spdx_ids()
    unknown = sorted({t for c in bom['components'] for l in c['licenses'] if 'expression' in l for t in spdx_terms(l['expression'])
                      if t not in known and not t.startswith('LicenseRef-')})
    check(not unknown, 'every license expression is made of SPDX ids (or LicenseRef- for a text of its own)', unknown)

    top = bom['metadata']['component']
    version = tomllib.loads((ROOT / 'src-tauri/Cargo.toml').read_text(encoding='utf-8'))['package']['version']
    exe_hash = hashlib.sha256(EXE.read_bytes()).hexdigest()
    check(top['name'] == 'ksip' and top['version'] == version and top['hashes'] == [{'alg': 'SHA-256', 'content': exe_hash}],
          'the SBOM names ksip at its version, and the exe by its SHA-256', top.get('hashes'))
    components = bom['components']
    refs = [c['bom-ref'] for c in components] + [top['bom-ref']]
    check(len(refs) == len(set(refs)), 'every component has a reference of its own')
    known_refs = set(refs)
    dangling = [(d['ref'], x) for d in bom['dependencies'] for x in [d['ref'], *d['dependsOn']] if x not in known_refs]
    check(not dangling, 'every dependency names components the SBOM lists', dangling[:3])
    reached, todo = set(), [top['bom-ref']]
    graph = {d['ref']: d['dependsOn'] for d in bom['dependencies']}
    while todo:
        for x in graph.get(todo.pop(), []):
            if x not in reached:
                reached.add(x)
                todo.append(x)
    orphans = [c['name'] for c in components if c['scope'] == 'required' and c['bom-ref'] not in reached]
    check(not orphans, 'every component in the exe is reached from ksip through the dependencies', orphans[:5])

    # The crates: what cargo reports for Windows, split the way cargo splits it.
    crates = {f"{c['name']}@{c['version']}": c for c in components if c['purl'].startswith('pkg:cargo/')}
    linked = {k for k, c in crates.items() if c['scope'] == 'required'}
    in_exe = cargo_tree('normal,no-proc-macro')
    check(linked == in_exe, "the crates in the exe are cargo's normal dependencies for Windows, proc macros aside",
          f'only in SBOM {sorted(linked - in_exe)[:5]}, only in cargo {sorted(in_exe - linked)[:5]}')
    built = cargo_tree('normal,build')
    check(set(crates) == built, 'with the build-only ones, the crates are all cargo builds for Windows, and no test-only crate',
          f'only in SBOM {sorted(set(crates) - built)[:5]}, only in cargo {sorted(built - set(crates))[:5]}')
    check(not any(n.split('@')[0] in ('gtk', 'webkit2gtk', 'objc2', 'cocoa', 'zbus') for n in crates), 'no crate of another platform (GTK, WebKitGTK, macOS, D-Bus)')
    lock = {(p['name'], p['version']): p.get('checksum') for p in tomllib.loads((ROOT / 'src-tauri/Cargo.lock').read_text(encoding='utf-8'))['package']}
    wrong = [k for k, c in crates.items() if [{'alg': 'SHA-256', 'content': lock[tuple(k.split('@'))]}] != c.get('hashes')]
    check(not wrong, "every crate carries Cargo.lock's checksum", wrong[:3])

    # The native sources, as the lock pins them.
    native = json.loads((ROOT / 'deps/native-sources.lock.json').read_text(encoding='utf-8'))
    by_name = {c['name']: c for c in components if not c['purl'].startswith('pkg:cargo/')}
    for name, record in native.items():
        c = by_name.get(name)
        pinned = record.get('commit') if record.get('source_type') == 'git' else record['version']
        ok = c is not None and c['version'] == pinned and c.get('supplier') and (not record.get('sha256') or c.get('hashes') == [{'alg': 'SHA-256', 'content': record['sha256']}])
        check(ok, f'{name} is listed at {pinned[:12]}, with its supplier and the hash the lock pins')
    check(by_name['depot-tools']['scope'] == 'excluded', 'depot_tools is listed as a build tool, not as part of the exe')
    check('pedigree' in by_name['baresip'], 'baresip says it is patched before it is built')

    # What WebRTC bundles: listed by its license generator, and built into the library KSIP links.
    headings = [line[2:].strip() for line in (ROOT / 'temp/build/webrtc-notices/LICENSE.md').read_text(encoding='utf-8').splitlines() if line.startswith('# ')][1:]
    members = sbom.archive_members(sbom.NATIVE_LIB / 'ksip_webrtc_audio.lib')
    bundled = {c['name']: {p['name']: p['value'] for p in c['properties']} for c in components if any(p['value'] == 'google-webrtc' for p in c.get('properties', []) if p['name'] == 'ksip:bundled_in')}
    not_linked = [n.strip() for n in next(p['value'] for p in top['properties'] if p['name'] == 'ksip:webrtc_listed_not_linked').split(',') if n.strip() not in ('', 'none')]
    check(sorted([*bundled, *not_linked, 'compiler-rt']) == sorted(headings), "what WebRTC bundles is its license generator's list, less what is not linked, and compiler-rt listed on its own", [sorted(bundled), not_linked])
    empty = [n for n, p in bundled.items() if not any(m.startswith(f"obj/{p['ksip:webrtc_folder']}/") for m in members)]
    check(not empty, 'every part listed as bundled has objects in the WebRTC library KSIP links', empty)
    present = [n for n in not_linked if any(m.startswith(f'obj/third_party/{n}/') for m in members)]
    check(not present, 'what is left out as not linked has no objects in it', present)

    # The runtimes the toolchain puts into the exe.
    rust = dict(line.split(': ', 1) for line in subprocess.check_output(['rustc', '-vV'], encoding='utf-8').splitlines()[1:] if ': ' in line)
    check(by_name.get('rust-std', {}).get('version') == rust['release'] == built_from['toolchain']['rustc'], "Rust's standard library is listed at the compiler's version, as the build recorded it", rust['release'])
    check(by_name.get('msvc-runtime', {}).get('version') == built_from['toolchain']['msvc'] and by_name.get('ucrt', {}).get('version') == built_from['toolchain']['ucrt'],
          'the MSVC runtime and the UCRT are listed at the versions the build linked with')
    props = {p['name']: p['value'] for p in top['properties']}
    check(props.get('ksip:commit') == built_from['commit'] and props.get('ksip:uncommitted_changes') == str(built_from['uncommitted_changes']),
          'the commit is the one the exe was built from, with whether it had changes not committed')
    for name in ('msvc-runtime', 'ucrt', 'compiler-rt'):
        c = by_name.get(name, {})
        check(c.get('scope') == 'required' and c.get('version') and c.get('supplier'), f'{name} is listed as in the exe, with its version and supplier')

    scopes = [c['scope'] for c in components]
    print(f"components: {len(components)} ({scopes.count('required')} in the exe, {scopes.count('excluded')} build only); crates {len(crates)}, native {len(native)}, bundled in WebRTC {len(bundled)}, not linked {not_linked}")
    print(f"{'FAIL' if failures else 'PASS'}: {len(failures)} failure(s)")
    sys.exit(1 if failures else 0)


if __name__ == '__main__':
    main()
