#!/usr/bin/env python3
"""Skip uploading an app version/build already visible in App Store Connect."""
import base64
import os
from pathlib import Path
import re
import sys
from urllib.parse import urlencode, urlsplit

from testflight_distribute import Asc, make_token, API

BUNDLE = 'com.crispstrobe.crispsorter'


def check_app(asc, app_id):
    if not re.fullmatch(r'[0-9]+', app_id):
        raise ValueError('Invalid App Store Connect app selection')
    app, status = asc.get(f'/apps/{app_id}?fields[apps]=bundleId')
    if status != 200 or app.get('data', {}).get('attributes', {}).get('bundleId') != BUNDLE:
        raise ValueError('Configured App Store Connect app does not match the IPA bundle')


def needs_upload(asc, app_id, marketing_version, build_number):
    if (not re.fullmatch(r'[0-9]+', app_id)
            or not re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+', marketing_version)
            or not re.fullmatch(r'[0-9]+(?:\.[0-9]+){0,2}', build_number)):
        raise ValueError('Invalid TestFlight app/version/build selection')
    check_app(asc, app_id)
    query = urlencode({'filter[app]': app_id, 'filter[version]': build_number,
        'filter[preReleaseVersion.platform]': 'IOS', 'include': 'preReleaseVersion',
        'fields[builds]': 'version,preReleaseVersion', 'fields[preReleaseVersions]': 'version,platform', 'limit': '200'})
    path, seen = '/builds?' + query, set()
    while path:
        if path in seen:
            raise ValueError('App Store Connect build pagination repeated')
        seen.add(path)
        payload, status = asc.get(path)
        if status != 200 or payload.get('errors') or not isinstance(payload.get('data'), list):
            raise ValueError('App Store Connect build lookup failed')
        versions = {item['id']: item.get('attributes', {}) for item in payload.get('included', [])
                    if item.get('type') == 'preReleaseVersions'}
        for build in payload['data']:
            if build.get('attributes', {}).get('version') != build_number:
                raise ValueError('App Store Connect returned an unexpected build number')
            version_id = build.get('relationships', {}).get('preReleaseVersion', {}).get('data', {}).get('id')
            attrs = versions.get(version_id)
            if not attrs or not attrs.get('version') or attrs.get('platform') != 'IOS':
                raise ValueError('Existing build marketing version is unavailable')
            if attrs['version'] == marketing_version:
                # PROCESSING and INVALID builds also consume this version/build.
                return False
        path = payload.get('links', {}).get('next')
        if path:
            parsed = urlsplit(path)
            if (parsed.scheme != 'https' or parsed.netloc != urlsplit(API).netloc
                    or parsed.path != '/v1/builds' or parsed.fragment):
                raise ValueError('Unexpected App Store Connect build pagination')
    return True


def main():
    try:
        token = make_token(os.environ['ASC_KEY_ID'], os.environ['ASC_ISSUER_ID'],
                           base64.b64decode(os.environ['ASC_KEY_P8_BASE64'], validate=True))
        mode = os.environ.get('TESTFLIGHT_SELECTION_MODE', 'upload')
        if mode not in ('upload', 'resume'):
            raise ValueError('Invalid TestFlight selection mode')
        asc = Asc(token)
        if mode == 'resume':
            check_app(asc, os.environ['ASC_APP_ID'])
            required = False
        else:
            required = needs_upload(asc, os.environ['ASC_APP_ID'], os.environ['IPA_VERSION'], os.environ['BUILD_NUMBER'])
        with Path(os.environ['GITHUB_ENV']).open('a') as stream:
            stream.write('TESTFLIGHT_UPLOAD_REQUIRED=' + str(required).lower() + '\n')
            if mode == 'upload' and not required:
                stream.write('UPLOADED_AFTER=\n')
        print('Upload required' if required else 'Continue exact build assignment without uploading again')
        return 0
    except Exception:
        print('App Store Connect upload check failed; upload was not attempted', file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
