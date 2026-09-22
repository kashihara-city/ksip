"""Fetch pinned official sources; record hashes before any build executes."""
import datetime, hashlib, io, json, pathlib, tarfile, urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[2]
CUTOFF = datetime.datetime(2026, 9, 1, tzinfo=datetime.timezone.utc)
SOURCES = [('re', 'baresip/re', 'v4.11.0'),
           ('baresip', 'baresip/baresip', 'v4.11.0'),
           # Audio codecs for baresip's opus and libg722 modules.
           ('opus', 'xiph/opus', 'v1.5.2'),
           ('libg722', 'sippy/libg722', 'v1.2.8')]
# Sources that are not published on GitHub. The release date is written here
# because there is no API to read it from, and the hash is pinned in the lock.
RELEASES = [('libressl', '4.3.2', '2026-05-26T00:00:00Z',
             'https://cdn.openbsd.org/pub/OpenBSD/LibreSSL/libressl-4.3.2.tar.gz')]

def get(url):
    req = urllib.request.Request(url, headers={'User-Agent': 'ksip-build/0.1'})
    return urllib.request.urlopen(req, timeout=60).read()

def main():
    lockpath = ROOT / 'deps/native-sources.lock.json'
    old = json.loads(lockpath.read_text()) if lockpath.exists() else {}
    records = {}
    for name, repo, tag in SOURCES:
        if name in old:
            records[name] = old[name]
            continue
        ref = json.loads(get(f'https://api.github.com/repos/{repo}/commits/{tag}'))
        date = ref['commit']['committer']['date']
        if datetime.datetime.fromisoformat(date.replace('Z', '+00:00')) > CUTOFF:
            raise RuntimeError(f'{name}: source younger than seven-day cutoff')
        sha = ref['sha']
        records[name] = {'repository': repo, 'version': tag, 'commit': sha,
                         'source_date': date, 'url': f'https://codeload.github.com/{repo}/tar.gz/{sha}'}
    for name, version, date, url in RELEASES:
        if name in old:
            records[name] = old[name]
            continue
        if datetime.datetime.fromisoformat(date.replace('Z', '+00:00')) > CUTOFF:
            raise RuntimeError(f'{name}: source younger than seven-day cutoff')
        records[name] = {'repository': 'openbsd/libressl', 'version': version,
                         'source_date': date, 'url': url}
    records.update({name: value for name, value in old.items()
                    if value.get('source_type') == 'git'})
    for name, record in records.items():
        if record.get('source_type') == 'git':
            continue
        dest = ROOT / 'temp' / 'vendor' / name
        archive = ROOT / 'deps' / (name + '.tar.gz')
        if archive.exists():
            data = archive.read_bytes()
        else:
            data = get(record['url'])
        digest = hashlib.sha256(data).hexdigest()
        if record.get('sha256') and digest != record['sha256']:
            raise RuntimeError(f'{name}: SHA256 mismatch')
        record['sha256'] = digest
        archive.parent.mkdir(parents=True, exist_ok=True)
        if not archive.exists():
            archive.write_bytes(data)
        if dest.exists():
            continue
        dest.mkdir(parents=True, exist_ok=True)
        with tarfile.open(fileobj=io.BytesIO(data)) as tar:
            for member in tar.getmembers():
                parts = pathlib.PurePosixPath(member.name).parts
                if len(parts) < 2:
                    continue
                member.name = str(pathlib.PurePosixPath(*parts[1:]))
                tar.extract(member, dest, filter='data')
        print(name, record['version'], digest, flush=True)
    lockpath.write_text(json.dumps(records, indent=2) + '\n')

if __name__ == '__main__':
    main()
