"""Package the single executable and a separate source/notices archive.
Uses only fixed installed/build outputs. No downloads or package resolution.
"""
from pathlib import Path
import hashlib,json,os,re,shutil,subprocess,tomllib,zipfile
ROOT=Path(__file__).resolve().parents[2]
VERSION=tomllib.loads((ROOT/'src-tauri/Cargo.toml').read_text(encoding='utf-8'))['package']['version']
EXE_NAME=f'ksip-v{VERSION}.exe'
OUT=ROOT/f'temp/build/single-v{VERSION}/KSIP-source'

def copy(src,dst):
    dst.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(src,dst)

def notices(src,dst,recursive=False):
    files=[]
    for p in (src.rglob('*') if recursive else src.glob('*')):
        if p.is_file() and p.name.upper().startswith(('LICENSE','LICENCE','COPYING','NOTICE','PATENTS','COPYRIGHT','AUTHORS')):
            copy(p,dst/p.relative_to(src));files.append(p.relative_to(src).as_posix())
    return files

def main():
    # Do not overwrite a user's data if they have used the output folder.
    if (OUT/'data').exists():raise RuntimeError('Output has user data; package into a new workspace or relocate the used folder first.')
    OUT.mkdir(parents=True,exist_ok=True)
    executable=OUT.parent/EXE_NAME
    copy(ROOT/'release'/EXE_NAME,executable)
    copy(ROOT/'temp/build/third-party-notices.txt',OUT/'THIRD_PARTY_NOTICES.txt')
    for directory in ['.cargo','src-web','src-native','scripts','licenses']:
        for p in (ROOT/directory).rglob('*'):
            if p.is_file() and '__pycache__' not in p.parts:copy(p,OUT/p.relative_to(ROOT))
    for p in (ROOT/'src-tauri').rglob('*'):
        if p.is_file() and not {'target','gen'}.intersection(p.relative_to(ROOT/'src-tauri').parts):copy(p,OUT/p.relative_to(ROOT))
    for n in ['README.md','PROJECT.md','AGENTS.md','.gitignore','deps/native-sources.lock.json']:
        copy(ROOT/n,OUT/n)
    # Native libraries include notices from bundled third-party source trees.
    for name in ['baresip','re']:
        notices(ROOT/'temp/vendor'/name,OUT/'licenses/native'/name,True)
    copy(ROOT/'temp/build/webrtc-notices/LICENSE.md',OUT/'licenses/native/google-webrtc/LICENSE.md')
    # Target-specific dependency tree, including build-time tools. No fetching.
    tree=subprocess.check_output(['cargo','tree','--locked','--offline','--manifest-path',str(ROOT/'src-tauri/Cargo.toml'),'--target','x86_64-pc-windows-msvc','--prefix','none','--format','{p}|{l}'],encoding='utf-8')
    cache=next(Path.home().glob('.cargo/registry/src/index.crates.io-*'))
    packages={}
    for line in tree.splitlines():
        m=re.match(r'([\w-]+) v([\w.+-]+).*?\|(.*)',line)
        if not m or m[1]=='ksip':continue
        n,v,lic=m.groups();key=f'{n}-{v}'
        if key in packages:continue
        src=cache/key;assert src.exists(),src
        files=notices(src,OUT/'licenses/rust'/key,True)
        if not files:
            files=notices(ROOT/'licenses/upstream'/key,OUT/'licenses/rust'/key,True)
            assert files,f'{key}: run scripts/deps/fetch-crate-licenses.py first'
        if 'MPL' in lic:
            for p in src.rglob('*'):
                if p.is_file():copy(p,OUT/'licenses/rust'/key/'source'/p.relative_to(src))
        packages[key]={'name':n,'version':v,'license':lic.removesuffix(' (*)'),'notice_files':files}
    (OUT/'reports').mkdir(parents=True, exist_ok=True)
    (OUT/'reports/windows-dependencies.json').write_text(json.dumps(list(packages.values()),indent=2),encoding='utf-8')
    (OUT/'THIRD_PARTY_NOTICES.md').write_text('''# Third-party notices

Native: baresip/re (BSD) and pinned official Google WebRTC components
(see the generated target-specific WebRTC LICENSE.md).
Front end: local HTML/CSS/JavaScript, with no third-party UI dependencies.
Rust and build-time dependencies are listed in
reports/windows-dependencies.json; corresponding notices are under licenses/.
The target-specific inventory includes build tools as well as runtime code.

CRT and the Google WebRTC audio library are statically linked into the versioned KSIP executable.
Readable third-party notices are embedded and accessible from the tray menu.

WebView2 Runtime is supplied separately by Microsoft and is not in this ZIP.
Source modifications and their build scripts are included; original native
source URLs and hashes are in deps/native-sources.lock.json.
''',encoding='utf-8')
    manifest={p.relative_to(OUT).as_posix():hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(OUT.rglob('*')) if p.is_file() and p.name!='SHA256SUMS.json'}
    (OUT/'SHA256SUMS.json').write_text(json.dumps(manifest,indent=2),encoding='utf-8')
    archive=OUT.parent/f'KSIP-v{VERSION}-source.zip'
    with zipfile.ZipFile(archive,'w',zipfile.ZIP_DEFLATED,compresslevel=9,strict_timestamps=False) as z:
        for p in sorted(OUT.rglob('*')):
            if p.is_file():z.write(p,p.relative_to(OUT.parent))
    digest=hashlib.sha256(archive.read_bytes()).hexdigest()
    archive.with_suffix('.zip.sha256').write_text(digest+'  '+archive.name+'\n')
    print(f'{archive}\n{archive.stat().st_size:,} bytes\nSHA256 {digest}')
    digest=hashlib.sha256(executable.read_bytes()).hexdigest()
    executable.with_suffix('.exe.sha256').write_text(digest+'  '+executable.name+'\n')
    print(f'{executable}\n{executable.stat().st_size:,} bytes\nSHA256 {digest}')
if __name__=='__main__':main()
