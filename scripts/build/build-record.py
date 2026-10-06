"""Record what release/ksip.exe was built from (its hash, version, commit, inputs, toolchain, WebRTC's linked parts) in temp/build/build-record.json, for the SBOM; run by app.ps1 in its MSVC shell."""
import ctypes, hashlib, importlib.util, json, os, subprocess
from ctypes import wintypes
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
EXE = ROOT / 'release/ksip.exe'
RECORD = ROOT / 'temp/build/build-record.json'
WEBRTC = Path(os.environ.get('KSIP_WEBRTC_SOURCE') or ROOT / 'temp/w/src')
# What the SBOM reads besides the exe, and what the exe is linked from: the
# SBOM is only made while these are as they were at the build (see sbom.py).
LINKED = ['ksip_entry', 'ksip_modules', 'libbaresip', 'opus', 'g722_static', 'ssl', 'crypto', 're-static',
          'ksip_webrtc_audio', 'clang_rt.builtins-x86_64']
INPUTS = ['src-tauri/Cargo.toml', 'src-tauri/Cargo.lock', 'deps/native-sources.lock.json', 'temp/build/webrtc-notices/LICENSE.md',
          *[f'temp/build/native/lib/{name}.lib' for name in LINKED]]


def sha256(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for block in iter(lambda: f.read(1 << 20), b''):
            h.update(block)
    return h.hexdigest()


def exe_version(path):
    """The product version in the exe's version resource, as major.minor.patch."""
    version = ctypes.windll.version
    size = version.GetFileVersionInfoSizeW(str(path), None)
    if not size:
        raise SystemExit(f'{path}: no version resource')
    data = ctypes.create_string_buffer(size)
    if not version.GetFileVersionInfoW(str(path), 0, size, data):
        raise SystemExit(f'{path}: the version resource cannot be read')
    info, length = ctypes.c_void_p(), wintypes.UINT()
    if not version.VerQueryValueW(data, '\\', ctypes.byref(info), ctypes.byref(length)):
        raise SystemExit(f'{path}: no fixed version information')
    # VS_FIXEDFILEINFO: signature, struct version, file MS/LS, product MS/LS, ...
    fixed = (wintypes.DWORD * 7).from_address(info.value)
    ms, ls = fixed[4], fixed[5]
    return f'{ms >> 16}.{ms & 0xFFFF}.{ls >> 16}'


def file_version(path):
    """The file version in a file's version resource, all four parts: a
    tool of the Windows SDK says the SDK's own build this way (rc.exe
    10.0.28000.2705), where the folder name says only 10.0.28000.0."""
    version = ctypes.windll.version
    size = version.GetFileVersionInfoSizeW(str(path), None)
    if not size:
        raise SystemExit(f'{path}: no version resource')
    data = ctypes.create_string_buffer(size)
    if not version.GetFileVersionInfoW(str(path), 0, size, data):
        raise SystemExit(f'{path}: the version resource cannot be read')
    info, length = ctypes.c_void_p(), wintypes.UINT()
    if not version.VerQueryValueW(data, '\\', ctypes.byref(info), ctypes.byref(length)):
        raise SystemExit(f'{path}: no fixed version information')
    fixed = (wintypes.DWORD * 7).from_address(info.value)
    ms, ls = fixed[2], fixed[3]
    return f'{ms >> 16}.{ms & 0xFFFF}.{ls >> 16}.{ls & 0xFFFF}'


def tool_version(*args):
    """The first line a tool prints for --version, or nothing when it is not there."""
    try:
        return run(*args).splitlines()[0]
    except (OSError, subprocess.CalledProcessError, IndexError):
        return ''


def run(*args):
    return subprocess.check_output(list(args), cwd=ROOT, encoding='utf-8').strip()


def main():
    manifest = (ROOT / 'src-tauri/Cargo.toml').read_text(encoding='utf-8')
    version = next(line.split('"')[1] for line in manifest.splitlines() if line.startswith('version'))
    built = exe_version(EXE)
    if built != version:
        raise SystemExit(f'{EXE} says {built}, Cargo.toml {version}')
    rust = dict(line.split(': ', 1) for line in run('rustc', '-vV').splitlines()[1:] if ': ' in line)
    msvc, ucrt = os.environ.get('VCToolsVersion', '').strip('\\'), os.environ.get('UCRTVersion', '').strip('\\')
    if not msvc or not ucrt:
        raise SystemExit('Run from the MSVC shell (app.ps1): VCToolsVersion and UCRTVersion say what the exe was linked with')
    changed = [line for line in run('git', 'status', '--porcelain', '--untracked-files=no').splitlines() if line]
    # The SDK's own build (its folder says only 10.0.28000.0; the tools say
    # 10.0.28000.2705), and the runtime libraries the exe is linked from
    # statically (/MT, +crt-static), by hash: a serviced SDK or toolset with
    # the same version folder changes these, and the exe with them.
    sdk_bin = Path(os.environ.get('WindowsSdkVerBinPath', '')) / 'x64/rc.exe'
    crt_paths = {
        'libucrt.lib': Path(os.environ.get('UniversalCRTSdkDir', '')) / 'Lib' / ucrt / 'ucrt/x64/libucrt.lib',
        **{name: Path(os.environ.get('VCToolsInstallDir', '')) / 'lib/x64' / name for name in ('libcmt.lib', 'libvcruntime.lib', 'libcpmt.lib')},
    }
    missing = [str(path) for path in [sdk_bin, *crt_paths.values()] if not path.is_file()]
    if missing:
        raise SystemExit('Not found (the MSVC shell names where the SDK and the toolset are): ' + ', '.join(missing))
    # How the SDK came to this machine: the installer the release workflow
    # fetched and checked (its version and hash, from its environment), or
    # one already there. And the runner image, when GitHub's.
    installer = os.environ.get('KSIP_SDK_INSTALLER_VERSION', '')
    environment = {
        'sdk_installer': {'version': installer, 'sha256': os.environ.get('KSIP_SDK_INSTALLER_SHA256', '').lower()} if installer else 'preinstalled',
        'runner_image': {'os': os.environ['ImageOS'], 'version': os.environ.get('ImageVersion', '')} if os.environ.get('ImageOS') else None,
    }
    # WebRTC's parts, resolved now from the tree the exe was built from (its
    # README.chromium files, DEPS and license generator), so that the SBOM
    # takes them from here and never from a tree changed or chosen later.
    spec = importlib.util.spec_from_file_location('sbom', ROOT / 'scripts/build/sbom.py')
    sbom = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(sbom)
    webrtc_commit = json.loads((ROOT / 'deps/native-sources.lock.json').read_text(encoding='utf-8'))['google-webrtc']['commit']
    parts, not_linked = sbom.webrtc_parts(webrtc_commit)
    record = {
        'exe_sha256': sha256(EXE),
        'version': version,
        'commit': run('git', 'rev-parse', 'HEAD'),
        # Built with changes not yet committed: the commit alone does not say what was built.
        'uncommitted_changes': len(changed),
        'inputs': {name: sha256(ROOT / name) for name in INPUTS},
        'toolchain': {
            'rustc': rust['release'], 'rustc_commit': rust['commit-hash'],
            'msvc': msvc, 'ucrt': ucrt,
            'sdk_build': file_version(sdk_bin),
            'crt_libs': {name: sha256(path) for name, path in crt_paths.items()},
            'clang': (WEBRTC / 'third_party/llvm-build/Release+Asserts/cr_build_revision').read_text().strip(),
            'cmake': tool_version('cmake', '--version'),
            'ninja': tool_version('ninja', '--version'),
        },
        'environment': environment,
        'webrtc': {'parts': parts, 'not_linked': not_linked},
    }
    RECORD.parent.mkdir(parents=True, exist_ok=True)
    RECORD.write_text(json.dumps(record, indent=2) + '\n', encoding='utf-8', newline='\n')
    print(f"Build record: {record['exe_sha256'][:12]} v{version} {record['commit'][:12]}" + (f" (+{len(changed)} uncommitted)" if changed else ''))


if __name__ == '__main__':
    main()
