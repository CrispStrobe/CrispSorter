#!/usr/bin/env python3
"""Describe the current signed mobile recipe and reject unsupported OCR requests.

The ordinary release recipe omits ocr-mobile and its native/ORT preparation.
Public OCR remains unsupported. Explicit draft selectors use a separate
unpublished signed build recipe with mandatory native/ORT provenance checks;
changing an input or adding a runtime receipt cannot enable public OCR.
"""
import argparse
import json
import os


def profile(requested=None):
    requested = requested or os.environ.get('CRISPSORTER_MOBILE_RELEASE_PROFILE', 'standard')
    if requested not in ('standard', 'ocr', 'ocr-draft'):
        raise ValueError('Unknown mobile release profile')
    if requested == 'ocr-draft':
        if (os.environ.get('GITHUB_EVENT_NAME') != 'workflow_dispatch'
                or os.environ.get('CRISPSORTER_RELEASE_PUBLISH') != 'false'
                or os.environ.get('CRISPSORTER_RELEASE_MAC_APPSTORE') != 'false'
                or os.environ.get('CRISPSORTER_RELEASE_SELECTOR') not in ('android_ocr_draft', 'ios_ocr_draft')):
            raise ValueError('OCR drafts require the explicit manual draft selector, publish=false and mac_appstore=false')
        return {'requested': 'ocr-draft', 'recipe': 'ocr-mobile-draft',
                'native_ocr': 'requested; final native/ORT provenance checks are mandatory',
                'ocr_qualification': 'not established',
                'scope': 'Unpublished signed build preparation only; no device or OCR quality qualification'}
    if requested == 'ocr':
        raise ValueError('Public signed mobile OCR release integration is not implemented: use the explicit draft selectors or target-scoped '
                         'ocr-mobile-native.yml draft qualification route. Standard mobile releases '
                         'remain available; they do not include the native OCR feature profile.')
    return {'requested': 'standard', 'recipe': 'standard',
            'native_ocr': 'not included by this release recipe',
            'ocr_qualification': 'not established',
            'scope': 'Package identity/signature inspection only; no OCR or device behavior qualification'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--requested', choices=['standard', 'ocr', 'ocr-draft'], default=None)
    args = parser.parse_args()
    print(json.dumps(profile(args.requested), sort_keys=True))


if __name__ == '__main__':
    main()
