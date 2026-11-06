#!/usr/bin/env python3
"""Copy public upstream refs and release bytes without running upstream code."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import urllib.request

UPSTREAM = 'https://rasterhub.com/rasterstate/fj'
API = 'https://rasterhub.com/api/v1/repos/rasterstate/fj'
REPO = os.environ['GITHUB_REPOSITORY']


def command(*args):
    return subprocess.check_output(args, text=True).strip()


def github(path, method='GET', data=None):
    args = ['gh', 'api', f'repos/{REPO}/{path}', '--method', method]
    if data is not None:
        args += ['--input', '-']
    result = subprocess.run(args, input=json.dumps(data) if data is not None else None,
                            text=True, capture_output=True, check=True)
    return json.loads(result.stdout) if result.stdout else None


def releases():
    result = []
    for page in range(1, 10000):
        with urllib.request.urlopen(API + f'/releases?limit=50&page={page}', timeout=90) as response:
            batch = json.load(response)
        if not batch:
            return [r for r in result if not r['draft']]
        result.extend(batch)
    raise RuntimeError('Release pagination did not terminate')


def download(asset, directory):
    name = asset['name']
    if not name or Path(name).name != name or name in ('.', '..') or chr(92) in name:
        raise ValueError(f'Unsafe asset name: {name!r}')
    path = directory / name
    with urllib.request.urlopen(asset['browser_download_url'], timeout=180) as response:
        with path.open('wb') as output:
            while chunk := response.read(1024 * 1024):
                output.write(chunk)
    if path.stat().st_size != asset['size']:
        raise ValueError(f'Incomplete asset: {name}')
    return path


def mirror_refs():
    with tempfile.TemporaryDirectory() as tmp:
        gitdir = str(Path(tmp) / 'upstream.git')
        command('git', 'clone', '--bare', UPSTREAM, gitdir)
        refs = command('git', '-C', gitdir, 'for-each-ref', '--format=%(refname)',
                       'refs/heads', 'refs/tags').splitlines()
        if 'refs/heads/mirror' in refs:
            raise RuntimeError('Upstream branch conflicts with reserved mirror branch')
        command('git', '-C', gitdir, 'push', '--atomic',
                f'https://github.com/{REPO}.git', *[f'+{ref}:{ref}' for ref in refs])
        print(f'Synced {len(refs)} branches and tags', flush=True)


def mirror_releases():
    published = releases()
    latest = next((r['tag_name'] for r in published if not r['prerelease']), None)
    existing = {}
    for page in range(1, 10000):
        batch = github(f'releases?per_page=100&page={page}')
        if not batch:
            break
        existing.update({r['tag_name']: r for r in batch})
    for release in reversed(published):
        tag = release['tag_name']
        print(f'Syncing {tag}', flush=True)
        previous = existing.get(tag)
        body = (f"Unofficial mirror of [upstream release]({release['html_url']}). "
                'Assets are copied unchanged.\n\n---\n\n' + (release['body'] or ''))
        data = dict(tag_name=tag, name=release['name'] or tag, body=body,
                    prerelease=release['prerelease'], draft=False,
                    make_latest='true' if tag == latest else 'false')
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            files = [download(a, directory) for a in release['assets']]
            checksums = directory / 'SHA256SUMS'
            if checksums.exists():
                for line in checksums.read_text().splitlines():
                    if not line.strip():
                        continue
                    digest, name = line.split(maxsplit=1)
                    name = name.lstrip('*')
                    path = directory / name
                    if path not in files or hashlib.sha256(path.read_bytes()).hexdigest() != digest:
                        raise ValueError(f'Checksum mismatch: {tag}/{name}')
            if previous:
                current = github(f"releases/{previous['id']}", 'PATCH', data)
            else:
                command('git', 'ls-remote', '--exit-code', '--tags',
                        f'https://github.com/{REPO}.git', f'refs/tags/{tag}')
                current = github('releases', 'POST', data)
            assets = {a['name']: a for a in current['assets']}
            for path in files:
                old = assets.get(path.name)
                digest = 'sha256:' + hashlib.sha256(path.read_bytes()).hexdigest()
                if old and old.get('digest') == digest and old['size'] == path.stat().st_size:
                    continue
                subprocess.run(['gh', 'release', 'upload', tag, str(path),
                                '--repo', REPO, '--clobber'], check=True)
    print(f'Synced {len(published)} releases', flush=True)


if __name__ == '__main__':
    mirror_refs()
    mirror_releases()
