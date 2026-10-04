"""Write the CycloneDX 1.6 SBOM of the ksip.exe app.ps1 last built (Rust crates, native sources, WebRTC's linked parts, the toolchain's runtime libraries), checked against its build record; no network access."""
import argparse, datetime, hashlib, importlib.util, json, os, re, subprocess, tomllib, uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
TARGET = 'x86_64-pc-windows-msvc'
SPEC = '1.6'
WEBRTC = Path(os.environ.get('KSIP_WEBRTC_SOURCE') or ROOT / 'temp/w/src')
NATIVE_LIB = ROOT / 'temp/build/native/lib'
# The native sources: who supplies them, and their license as read from
# their own files (temp/vendor/<name>/LICENSE or COPYING): an SPDX id or
# expression where the text is a standard one, otherwise a name that points
# at the text KSIP ships in its third-party notices.
NATIVE = {
    're': ('Baresip Foundation', {'id': 'BSD-3-Clause'}),
    'baresip': ('Baresip Foundation', {'id': 'BSD-3-Clause'}),
    'opus': ('Xiph.Org Foundation', {'id': 'BSD-3-Clause'}),
    'libg722': ('Sippy Software, Inc.', {'name': 'Carnegie Mellon ADPCM notice (unrestricted use; see the third-party notices)'}),
    'libressl': ('The OpenBSD Project', {'expression': 'OpenSSL AND ISC'}),
    'google-webrtc': ('Google LLC', {'id': 'BSD-3-Clause'}),
    'depot-tools': ('Google LLC', {'id': 'BSD-3-Clause'}),
}
# Sources patched before they are built, and by what.
PATCHED = {'baresip': 'scripts/build/patch-baresip.py'}
# Chromium's names for licenses (README.chromium) that are not SPDX ids.
CHROMIUM_LICENSES = {'Apache-with-LLVM-Exception': 'Apache-2.0 WITH LLVM-exception'}


def license_entry(known):
    if 'expression' in known:
        return [{'expression': known['expression']}]
    return [{'license': dict(known)}]


def crate_license(package):
    """A crate's declared license as an SPDX expression (the old `A/B` form
    read as `A OR B`), or a pointer to its license file."""
    text = package.get('license')
    if text:
        return [{'expression': re.sub(r'\s*/\s*', ' OR ', text.strip())}]
    if package.get('license_file'):
        return [{'license': {'name': f"see {package['license_file']} in the crate"}}]
    return [{'license': {'name': 'not declared by the crate'}}]


def run(*args, cwd=ROOT):
    return subprocess.check_output(list(args), cwd=cwd, encoding='utf-8').strip()


def cargo_tree(edges):
    """What cargo builds for Windows over these edges, as the real feature
    resolution decides it (cargo metadata's graph is coarser and names
    crates a feature of a test or another platform would pull in): each line
    as (depth, name, version)."""
    out = run('cargo', 'tree', '--locked', '--manifest-path', str(ROOT / 'src-tauri/Cargo.toml'), '--target', TARGET,
              '-e', edges, '--prefix', 'depth', '--format', '{p}')
    return [(int(m[1]), m[2], m[3]) for m in (re.match(r'(\d+)([\w-]+) v([\w.+-]+)', line) for line in out.splitlines()) if m]


def crate_scopes():
    """Each crate built for Windows, as `required` (linked into the exe:
    ksip's normal dependencies, proc macros aside) or `excluded` (only run
    while building: build scripts and proc macros, and what they use), and
    who depends on whom among them. Crates only the tests use are left out."""
    built = cargo_tree('normal,build')
    linked = {(n, v) for _, n, v in cargo_tree('normal,no-proc-macro')}
    root = (built[0][1], built[0][2])
    edges, parents = {}, []
    for depth, name, version in built:
        del parents[depth:]
        if parents:
            edges.setdefault(parents[-1], set()).add((name, version))
        parents.append((name, version))
    crates = {(n, v) for _, n, v in built} - {root}
    return root, edges, {c: 'required' if c in linked else 'excluded' for c in crates}


def sha256(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for block in iter(lambda: f.read(1 << 20), b''):
            h.update(block)
    return h.hexdigest()


def archive_members(path):
    """The object files in a static library (a COFF archive), by the paths
    the librarian recorded, without the MSVC tools."""
    data = Path(path).read_bytes()
    assert data[:8] == b'!<arch>\n', f'{path} is not an archive'
    names, longnames, at = [], b'', 8
    while at + 60 <= len(data):
        header = data[at:at + 60]
        name, size = header[:16].decode('ascii').strip(), int(header[48:58])
        body = data[at + 60:at + 60 + size]
        if name == '//':
            longnames = body
        elif name not in ('/', '/<ECSYMBOLS>/'):
            if name.startswith('/') and name[1:].isdigit():
                start = int(name[1:])
                name = longnames[start:longnames.index(b'\0', start)].decode('utf-8', 'replace')
            names.append(name.rstrip('/').replace('\\', '/'))
        at += 60 + size + (size & 1)
    return names


def readme_chromium(folder):
    fields = {}
    readme = WEBRTC / folder / 'README.chromium'
    if readme.exists():
        for line in readme.read_text(encoding='utf-8', errors='replace').splitlines():
            key, sep, value = line.partition(':')
            if sep and key in ('Name', 'URL', 'Version', 'Revision', 'License') and key not in fields:
                fields[key] = value.strip()
    return fields


def spdx_ids():
    return set(json.loads((ROOT / 'deps/cyclonedx/spdx.schema.json').read_text(encoding='utf-8'))['enum'])


def chromium_license(text, name, known):
    """README.chromium's license list as an SPDX expression: Chromium's own
    names mapped, anything that is not an SPDX id kept as a LicenseRef."""
    terms = []
    for part in [p.strip() for p in text.split(',') if p.strip()]:
        part = CHROMIUM_LICENSES.get(part, part)
        if part.startswith('LicenseRef-') or all(t in known for t in part.split(' WITH ')):
            terms.append(part)
        else:
            terms.append('LicenseRef-webrtc-' + re.sub(r'[^A-Za-z0-9.-]', '-', part))
    if not terms:
        return [{'license': {'name': f'see the {name} section of the WebRTC notices in the third-party notices'}}]
    return [{'expression': ' AND '.join(terms)}]


def webrtc_parts(commit):
    """Read at the build, by build-record.py, which keeps the result for the
    SBOM. What WebRTC's license generator lists for KSIP's target, kept only
    where the library KSIP links holds objects built from it (the generator
    also lists what the target merely could use), with the revision each is
    at: its README.chromium, or WebRTC's DEPS where that says DEPS, or the
    WebRTC commit for a copy that lives in WebRTC's own tree."""
    notices = ROOT / 'temp/build/webrtc-notices/LICENSE.md'
    listed = [line[2:].strip() for line in notices.read_text(encoding='utf-8').splitlines() if line.startswith('# ')]
    assert listed and listed[0] == 'webrtc', f'{notices}: WebRTC first, then what it bundles'
    generator = (WEBRTC / 'tools_webrtc/libs/generate_licenses.py').read_text(encoding='utf-8')
    folders = {m[1]: m[2] for m in re.finditer(r"'([^']+)':\s*\[\s*'([^']+)'", generator)}
    members = archive_members(NATIVE_LIB / 'ksip_webrtc_audio.lib')
    deps_file = (WEBRTC / 'DEPS').read_text(encoding='utf-8')
    known = spdx_ids()
    parts, unlinked = [], []
    for name in listed[1:]:
        if name == 'compiler-rt':
            continue  # linked on its own, from the toolchain: see toolchain()
        folder = os.path.dirname(folders[name])
        folder = folder[:-4] if folder.endswith('/src') else folder
        if not any(m.startswith(f'obj/{folder}/') for m in members):
            unlinked.append(name)
            continue
        info = readme_chromium(folder)
        version = info.get('Version', 'N/A')
        revision = info.get('Revision', '')
        if revision == 'DEPS':
            pinned = re.search(r"'src/" + re.escape(folder) + r"':\s*\n?\s*[^@\n]*@'?\s*\+?\s*'?([0-9a-f]{40})", deps_file)
            revision = pinned[1] if pinned else ''
        in_tree = not revision and version == 'N/A'
        parts.append({'name': name, 'folder': folder, 'title': info.get('Name', ''), 'url': info.get('URL', ''),
                      'version': version if version != 'N/A' else (revision or commit),
                      'revision': revision or commit, 'in_tree': in_tree,
                      'licenses': chromium_license(info.get('License', ''), name, known)})
    return parts, unlinked


def build_record(exe, path):
    """What the exe was built from, as app.ps1 recorded it (build-record.py),
    once it is sure to be this exe's and nothing it names has changed since:
    the SBOM describes the build, and a hash alone would put today's parts
    on any exe. The exe's own version resource must say the same version."""
    record = json.loads(Path(path).read_text(encoding='utf-8'))
    digest = sha256(exe)
    if record.get('exe_sha256') != digest:
        raise SystemExit(f'{exe}: not the exe {path} records (that is {record.get("exe_sha256", "?")[:12]}, this {digest[:12]}); '
                         'an SBOM is made for the exe scripts/build/app.ps1 last built')
    changed = [name for name, value in record['inputs'].items() if not (ROOT / name).exists() or sha256(ROOT / name) != value]
    if changed:
        raise SystemExit(f'changed since {exe} was built: {", ".join(changed)}; build it again before making its SBOM')
    manifest = tomllib.loads((ROOT / 'src-tauri/Cargo.toml').read_text(encoding='utf-8'))['package']['version']
    built = exe_version(exe)
    if not record['version'] == manifest == built:
        raise SystemExit(f'{exe} says {built}, its record {record["version"]}, Cargo.toml {manifest}')
    return record


def exe_version(path):
    spec = importlib.util.spec_from_file_location('build_record', ROOT / 'scripts/build/build-record.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.exe_version(path)


def toolchain(record):
    """The runtime libraries the toolchain put into the exe, as the build
    recorded them: Rust's standard library, the MSVC and Universal C
    runtimes (static: +crt-static and /MT), and the clang runtime WebRTC's
    toolchain supplies."""
    tools = record['toolchain']
    rust = {'release': tools['rustc'], 'commit-hash': tools['rustc_commit']}
    msvc, ucrt, clang = tools['msvc'], tools['ucrt'], tools['clang']
    # The libraries themselves, by hash, beside the version: a serviced SDK
    # or toolset keeps its version folder and changes these.
    crt = tools['crt_libs']
    return [
        {'name': 'rust-std', 'version': rust['release'], 'supplier': 'The Rust Project', 'licenses': [{'expression': 'MIT OR Apache-2.0'}],
         'purl': f"pkg:generic/rust-std@{rust['release']}?vcs_url=git%2Bhttps://github.com/rust-lang/rust%40{rust['commit-hash']}",
         'description': "Rust's standard library (std, core, alloc), linked statically", 'properties': {'ksip:commit': rust['commit-hash']}},
        {'name': 'msvc-runtime', 'version': msvc, 'supplier': 'Microsoft Corporation',
         'licenses': [{'license': {'name': 'Microsoft Software License Terms (Visual Studio Build Tools)'}}],
         'purl': f'pkg:generic/msvc-runtime@{msvc}',
         'description': 'The MSVC C and C++ runtime (vcruntime, libcmt, libcpmt), linked statically',
         'properties': {f'ksip:sha256:{name}': crt[name] for name in ('libcmt.lib', 'libvcruntime.lib', 'libcpmt.lib')}},
        {'name': 'ucrt', 'version': ucrt, 'supplier': 'Microsoft Corporation',
         'licenses': [{'license': {'name': 'Microsoft Software License Terms (Windows SDK)'}}],
         'purl': f'pkg:generic/ucrt@{ucrt}', 'hashes': [{'alg': 'SHA-256', 'content': crt['libucrt.lib']}],
         'description': 'The Universal C Runtime of the Windows SDK (libucrt), linked statically',
         'properties': {'ksip:sdk_build': tools['sdk_build'], 'ksip:sha256:libucrt.lib': crt['libucrt.lib']}},
        {'name': 'compiler-rt', 'version': clang, 'supplier': 'LLVM Project',
         'licenses': [{'expression': 'Apache-2.0 WITH LLVM-exception AND NCSA AND MIT'}],
         'purl': f'pkg:generic/compiler-rt@{clang}',
         'description': "clang's builtins (clang_rt.builtins-x86_64), from the clang WebRTC's build fetches"},
    ]


def environment_properties(record):
    """Where the build ran, as the build recorded it: how the Windows SDK came
    to the machine (the installer the release workflow fetched and checked,
    by version and hash, or one already there), the runner image when
    GitHub's, and the build tools."""
    environment = record['environment']
    installer = environment['sdk_installer']
    image = environment['runner_image']
    return [
        {'name': 'ksip:sdk_installer', 'value': f"winsdksetup.exe {installer['version']} sha256:{installer['sha256']}" if isinstance(installer, dict) else installer},
        {'name': 'ksip:runner_image', 'value': f"{image['os']} {image['version']}" if image else 'none (not a GitHub runner)'},
        {'name': 'ksip:cmake', 'value': record['toolchain']['cmake'] or 'unknown'},
        {'name': 'ksip:ninja', 'value': record['toolchain']['ninja'] or 'unknown'},
    ]


def component(c, scope='required'):
    out = {'type': c.get('type', 'library'), 'bom-ref': c['purl'], 'name': c['name']}
    if c.get('version'):
        out['version'] = c['version']
    if c.get('supplier'):
        out['supplier'] = {'name': c['supplier']}
    if c.get('description'):
        out['description'] = c['description']
    out['scope'] = scope
    for key in ('hashes', 'licenses', 'purl', 'externalReferences', 'pedigree'):
        if c.get(key):
            out[key] = c[key]
    if c.get('properties'):
        out['properties'] = [{'name': k, 'value': v} for k, v in c['properties'].items()]
    return out


def build(exe, record_path=ROOT / 'temp/build/build-record.json'):
    record = build_record(exe, record_path)
    meta = json.loads(run('cargo', 'metadata', '--locked', '--format-version', '1', '--filter-platform', TARGET,
                          '--manifest-path', str(ROOT / 'src-tauri/Cargo.toml')))
    packages = {(p['name'], p['version']): p for p in meta['packages']}
    root, edges, scopes = crate_scopes()
    ksip = packages[root]
    checksums = {(p['name'], p['version']): p.get('checksum') for p in tomllib.loads((ROOT / 'src-tauri/Cargo.lock').read_text(encoding='utf-8'))['package']}
    lock = json.loads((ROOT / 'deps/native-sources.lock.json').read_text(encoding='utf-8'))
    commit = record['commit']
    top = f"pkg:github/{ksip['repository'].removeprefix('https://github.com/')}@v{ksip['version']}"

    components, dependencies, ref_of = [], [], {}
    for pid, scope in sorted(scopes.items()):
        p = packages[pid]
        ref = f"pkg:cargo/{p['name']}@{p['version']}"
        ref_of[pid] = ref
        refs = [{'type': 'distribution', 'url': f"https://crates.io/crates/{p['name']}/{p['version']}"}]
        if p.get('repository'):
            refs.append({'type': 'vcs', 'url': p['repository']})
        components.append(component({'name': p['name'], 'version': p['version'], 'purl': ref, 'licenses': crate_license(p),
                                     'hashes': [{'alg': 'SHA-256', 'content': checksums[pid]}] if checksums.get(pid) else None,
                                     'externalReferences': refs}, scope))
    for pid in sorted(scopes, key=lambda pid: ref_of[pid]):
        dependencies.append({'ref': ref_of[pid], 'dependsOn': sorted(ref_of[d] for d in edges.get(pid, ()))})

    native = {}
    for name, (supplier, license) in NATIVE.items():
        r = lock[name]
        if r['url'].startswith('https://codeload.github.com/'):
            purl, version = f"pkg:github/{r['repository']}@{r['version']}", r['version']
        else:
            # A git source is known by its commit; the lock's own label (a
            # date) is not the project's version and is kept as a property.
            version = r.get('commit') or r['version']
            purl = f"pkg:generic/{name}@{version}?vcs_url=" + (f"git%2B{r['url']}%40{r['commit']}" if r.get('commit') else r['url'])
        props = {'ksip:source_date': r['source_date']}
        if r.get('commit'):
            props['ksip:commit'] = r['commit']
        if r.get('source_type') == 'git':
            props['ksip:lock_label'] = r['version']
        if name == 'google-webrtc':
            props.update({'ksip:deps_sha256': r['deps_sha256'], 'ksip:build_commit': r['build_commit']})
        if name == 'depot-tools':
            props['ksip:used_for'] = 'fetching and building WebRTC only; not in the exe'
        native[name] = purl
        components.append(component({
            'type': 'application' if name == 'depot-tools' else 'library', 'name': name, 'version': version, 'purl': purl,
            'supplier': supplier, 'licenses': license_entry(license),
            'hashes': [{'alg': 'SHA-256', 'content': r['sha256']}] if r.get('sha256') else None,
            'externalReferences': [{'type': 'distribution' if r.get('sha256') else 'vcs', 'url': r['url']}],
            'pedigree': {'notes': f'Patched before it is built, by {PATCHED[name]} in the KSIP repository.'} if name in PATCHED else None,
            'properties': props}, 'excluded' if name == 'depot-tools' else 'required'))

    # As the build resolved them (build-record.py), not from the WebRTC tree
    # as it is now: that tree may have been updated, or another one chosen.
    if 'webrtc' not in record:
        raise SystemExit('the build record has no WebRTC parts (made by an older build-record.py): build the exe again')
    parts, unlinked = record['webrtc']['parts'], record['webrtc']['not_linked']
    bundled = []
    for part in parts:
        purl = f"pkg:generic/{part['name']}@{part['version']}"
        if part['url'].startswith('http'):
            purl += f"?download_url={part['url']}"
        props = {'ksip:bundled_in': 'google-webrtc', 'ksip:webrtc_folder': part['folder'], 'ksip:revision': part['revision']}
        if part['in_tree']:
            props['ksip:version_note'] = 'kept in the WebRTC source tree, with no version of its own: the version is the WebRTC commit'
        bundled.append(purl)
        refs = [{'type': 'website', 'url': part['url']}] if part['url'].startswith('http') else []
        components.append(component({'name': part['name'], 'version': part['version'], 'purl': purl, 'description': part['title'],
                                     'licenses': part['licenses'], 'externalReferences': refs, 'properties': props}))
    runtimes = []
    for c in toolchain(record):
        runtimes.append(c['purl'])
        components.append(component(c))

    direct = sorted(ref_of[d] for d in edges.get(root, ()))
    dependencies = [
        {'ref': top, 'dependsOn': sorted(direct + [native[n] for n in ('baresip', 're', 'libressl', 'google-webrtc')] + runtimes)},
        *dependencies,
        {'ref': native['baresip'], 'dependsOn': sorted(native[n] for n in ('re', 'opus', 'libg722', 'libressl'))},
        {'ref': native['re'], 'dependsOn': [native['libressl']]},
        {'ref': native['google-webrtc'], 'dependsOn': sorted(bundled)},
    ]
    return {
        'bomFormat': 'CycloneDX',
        'specVersion': SPEC,
        # A new document each time it is made, as the specification asks;
        # what it describes is fixed by the exe's hash below.
        'serialNumber': f'urn:uuid:{uuid.uuid4()}',
        'version': 1,
        'metadata': {
            'timestamp': datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0).isoformat().replace('+00:00', 'Z'),
            'tools': {'components': [{'type': 'application', 'name': 'ksip scripts/build/sbom.py', 'version': run('git', 'rev-parse', 'HEAD')}]},
            'component': {
                'type': 'application', 'bom-ref': top, 'name': ksip['name'], 'version': ksip['version'],
                'description': ksip.get('description') or '', 'supplier': {'name': 'Kashihara City'}, 'licenses': crate_license(ksip),
                'purl': top, 'hashes': [{'alg': 'SHA-256', 'content': record['exe_sha256']}],
                'externalReferences': [{'type': 'vcs', 'url': ksip['repository']}],
                'properties': [{'name': 'ksip:commit', 'value': commit},
                               {'name': 'ksip:uncommitted_changes', 'value': str(record['uncommitted_changes'])},
                               {'name': 'ksip:target', 'value': TARGET},
                               {'name': 'ksip:not_included', 'value': 'Microsoft Edge WebView2 Runtime (part of Windows, not shipped)'},
                               {'name': 'ksip:webrtc_listed_not_linked', 'value': ', '.join(unlinked) or 'none'},
                               *environment_properties(record)],
            },
        },
        'components': components,
        'dependencies': dependencies,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--exe', default=str(ROOT / 'release/ksip.exe'), help='the ksip.exe the SBOM describes')
    parser.add_argument('--out', default=str(ROOT / 'temp/build/sbom/ksip.cdx.json'))
    parser.add_argument('--record', default=str(ROOT / 'temp/build/build-record.json'), help='what app.ps1 recorded of the build')
    args = parser.parse_args()
    bom = build(Path(args.exe), Path(args.record))
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(bom, indent=2, ensure_ascii=False) + '\n', encoding='utf-8', newline='\n')
    scopes = [c.get('scope') for c in bom['components']]
    print(f"SBOM: {out} ({len(bom['components'])} components: {scopes.count('required')} in the exe, {scopes.count('excluded')} build only)")


if __name__ == '__main__':
    main()
