"""Fetch the pinned official CycloneDX 1.6 JSON schemas into deps/cyclonedx/, checking or recording their SHA-256 in its lock."""
import datetime, hashlib, json, pathlib, urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[2]
FOLDER = ROOT / 'deps/cyclonedx'
LOCK = FOLDER / 'schema.lock.json'
REPOSITORY = 'CycloneDX/specification'
TAG = '1.6.2'
# The schema of the BOM, and the two it refers to (SPDX license ids, JSON signatures).
FILES = ['bom-1.6.schema.json', 'spdx.schema.json', 'jsf-0.82.schema.json']
# No release younger than this is taken (the same rule as the other dependencies).
MIN_AGE = datetime.timedelta(days=7)


def get(url):
    req = urllib.request.Request(url, headers={'User-Agent': 'ksip-build/0.1'})
    return urllib.request.urlopen(req, timeout=60).read()


def main():
    old = json.loads(LOCK.read_text(encoding='utf-8')) if LOCK.exists() else {}
    commit = json.loads(get(f'https://api.github.com/repos/{REPOSITORY}/commits/{TAG}'))
    sha, date = commit['sha'], commit['commit']['committer']['date']
    if old.get('commit') and old['commit'] != sha:
        raise SystemExit(f'{TAG} now points at {sha}, the lock at {old["commit"]}: review before taking it')
    if datetime.datetime.now(datetime.timezone.utc) - datetime.datetime.fromisoformat(date.replace('Z', '+00:00')) < MIN_AGE:
        raise SystemExit(f'{TAG} ({date}) is younger than {MIN_AGE.days} days')
    record = {'repository': REPOSITORY, 'version': TAG, 'commit': sha, 'source_date': date, 'files': {}}
    FOLDER.mkdir(parents=True, exist_ok=True)
    for name in FILES:
        url = f'https://raw.githubusercontent.com/{REPOSITORY}/{sha}/schema/{name}'
        data = get(url)
        digest = hashlib.sha256(data).hexdigest()
        pinned = old.get('files', {}).get(name, {}).get('sha256')
        if pinned and pinned != digest:
            raise SystemExit(f'{name}: {digest} is not the pinned {pinned}')
        (FOLDER / name).write_bytes(data)
        record['files'][name] = {'url': url, 'sha256': digest}
    LOCK.write_text(json.dumps(record, indent=2) + '\n', encoding='utf-8', newline='\n')
    print('CycloneDX schemas', TAG, sha, ', '.join(FILES))


if __name__ == '__main__':
    main()
