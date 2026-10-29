#!/usr/bin/env python3
"""Require an unpublished release to resolve to the checked-out source commit."""
import argparse
import json
import re
import subprocess
import sys


SHA = re.compile(r'[0-9a-f]{40}')


def gh(*args, missing_ok=False):
    result = subprocess.run(['gh', *args], capture_output=True, text=True)
    if result.returncode:
        if missing_ok and re.search(r'\bHTTP 404\b', result.stderr):
            return None
        raise RuntimeError('GitHub lookup failed: ' + result.stderr.strip())
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise RuntimeError('GitHub lookup returned invalid JSON') from error


def validate_release_target(repo, tag, source):
    if not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', repo):
        raise ValueError('Invalid repository name')
    if not re.fullmatch(r'v[0-9A-Za-z._+\-]+', tag):
        raise ValueError('Invalid release tag')
    if not SHA.fullmatch(source):
        raise ValueError('Source must be a full lowercase commit SHA')
    release = gh('release', 'view', tag, '--repo', repo, '--json', 'isDraft,targetCommitish')
    if not isinstance(release, dict) or release.get('isDraft') is not True:
        raise ValueError('Existing release is not a draft')
    ref = gh('api', f'repos/{repo}/git/ref/tags/{tag}', missing_ok=True)
    if ref is None:
        if release.get('targetCommitish') != source:
            raise ValueError('Draft target must be the exact current commit when the tag does not exist')
        return 'draft target'
    if not isinstance(ref, dict) or not isinstance(ref.get('object'), dict):
        raise ValueError('Git tag reference has an invalid object')
    obj = ref['object']
    seen = set()
    for _ in range(32):
        sha = obj.get('sha')
        kind = obj.get('type')
        if not isinstance(sha, str) or not SHA.fullmatch(sha):
            raise ValueError('Git tag object has an invalid SHA')
        if kind == 'commit':
            if sha != source:
                raise ValueError('Release tag points to a different commit')
            return 'existing tag'
        if kind != 'tag' or sha in seen:
            raise ValueError('Git tag does not resolve to a commit')
        seen.add(sha)
        annotated = gh('api', f'repos/{repo}/git/tags/{sha}')
        if not isinstance(annotated, dict) or not isinstance(annotated.get('object'), dict):
            raise ValueError('Annotated tag has an invalid object')
        obj = annotated['object']
    raise ValueError('Git tag nesting exceeds the safety limit')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', required=True)
    parser.add_argument('--tag', required=True)
    parser.add_argument('--source', required=True)
    args = parser.parse_args()
    try:
        result = validate_release_target(args.repo, args.tag, args.source)
    except (RuntimeError, ValueError) as error:
        print(f'::error::{error}', file=sys.stderr)
        sys.exit(1)
    print(f'Verified {result} resolves to the current source commit.')


if __name__ == '__main__':
    main()
