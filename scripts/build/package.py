"""Package the single executable and a separate source/notices archive.
Uses only fixed installed/build outputs. No downloads or package resolution.
"""
from pathlib import Path
import hashlib,json,os,re,shutil,subprocess,sys,tomllib,zipfile
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
    # A fresh staging folder every time: nothing from an earlier run can stay behind.
    shutil.rmtree(OUT,ignore_errors=True)
    OUT.mkdir(parents=True,exist_ok=True)
    executable=OUT.parent/EXE_NAME
    copy(ROOT/'release'/EXE_NAME,executable)
    copy(ROOT/'temp/build/third-party-notices.txt',OUT/'THIRD_PARTY_NOTICES.txt')
    # The source is exactly what Git tracks: no untracked file in a source folder
    # is picked up, and nothing tracked (the pinned toolchain included) is left out.
    tracked=[n for n in subprocess.check_output(['git','ls-files','-z'],cwd=ROOT).decode('utf-8').split('\0') if n]
    for n in tracked:
        if (ROOT/n).is_file():copy(ROOT/n,OUT/n)
    # Where this source came from, for whoever compares it with the repository.
    origin={'version':VERSION,
            'commit':subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip(),
            'describe':subprocess.check_output(['git','describe','--tags','--always','--dirty'],cwd=ROOT,text=True).strip(),
            'uncommitted_changes':bool(subprocess.check_output(['git','status','--porcelain'],cwd=ROOT,text=True).strip()),
            'tracked_files':len(tracked),
            'rust_toolchain':(ROOT/'rust-toolchain.toml').read_text(encoding='utf-8'),
            'python':sys.version.split()[0]}
    (OUT/'reports').mkdir(parents=True,exist_ok=True)
    (OUT/'reports/source-origin.json').write_text(json.dumps(origin,indent=2)+'\n',encoding='utf-8')
    # Native libraries include notices from bundled third-party source trees.
    for name in ['baresip','re']:
        notices(ROOT/'temp/vendor'/name,OUT/'licenses/native'/name,True)
    copy(ROOT/'temp/build/webrtc-notices/LICENSE.md',OUT/'licenses/native/google-webrtc/LICENSE.md')
    # Target-specific dependency tree, including build-time tools. No fetching.
    tree=subprocess.check_output(['cargo','tree','--locked','--offline','--manifest-path',str(ROOT/'src-tauri/Cargo.toml'),'--target','x86_64-pc-windows-msvc','--prefix','none','--format','{p}|{l}'],encoding='utf-8')
    # The cargo home is where cargo says it is (CARGO_HOME), not always under the user's folder.
    cargo_home=Path(os.environ.get('CARGO_HOME') or Path.home()/'.cargo')
    cache=next(cargo_home.glob('registry/src/index.crates.io-*'))
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
