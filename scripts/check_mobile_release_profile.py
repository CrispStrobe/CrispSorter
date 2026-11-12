#!/usr/bin/env python3
"""Describe the current signed mobile recipe and reject unsupported OCR requests.

The ordinary release recipe omits ocr-mobile and its native/ORT preparation.
This guard must be replaced with final-artifact provenance validation when that
recipe is integrated; changing an input or adding a runtime receipt is not enough.
"""
import argparse
import json
import os


def profile(requested=None):
    requested = requested or os.environ.get('CRISPSORTER_MOBILE_RELEASE_PROFILE', 'standard')
    if requested not in ('standard', 'ocr'):
        raise ValueError('Unknown mobile release profile')
    if requested == 'ocr':
        raise ValueError('Signed mobile OCR release integration is not implemented: use the target-scoped '
                         'ocr-mobile-native.yml draft qualification route. Standard mobile releases '
                         'remain available; they do not include the native OCR feature profile.')
    return {'requested': 'standard', 'recipe': 'standard',
            'native_ocr': 'not included by this release recipe',
            'ocr_qualification': 'not established',
            'scope': 'Package identity/signature inspection only; no OCR or device behavior qualification'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--requested', choices=['standard', 'ocr'], default=None)
    args = parser.parse_args()
    print(json.dumps(profile(args.requested), sort_keys=True))


if __name__ == '__main__':
    main()
